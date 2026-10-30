//! DirectWrite's Unix glyph service over the session's cached Text engine.
use super::*;
fn word(env: &Env<'_>, p: u32, at: u32) -> u32 {
    env.cpu.read_u32(p + at).unwrap()
}
fn put(env: &mut Env<'_>, p: u32, v: u32) {
    env.cpu.write_u32(p, v).unwrap();
}
pub(super) fn run(env: &mut Env<'_>, name: &str, a: &[u32]) -> Outcome {
    if matches!(name, "process_attach" | "process_detach") {
        return Outcome::Return(0);
    }
    let p = a[0];
    if name == "create_font_object" {
        let data = word(env, p, 0);
        let size = word(env, p, 8);
        let index = word(env, p, 16);
        let output = word(env, p, 20);
        let Ok(bytes) = placement::read(env, data, size, false) else {
            return Outcome::Return(0xc000000d);
        };
        let fonts = env.ext.get_mut::<Fonts>();
        let Ok((id, _)) = fonts.text.add_resource(&bytes) else {
            return Outcome::Return(0xc000000d);
        };
        fonts.next_memory += 1;
        let handle = fonts.next_memory;
        fonts.dwrite.insert(handle, (id, index));
        env.cpu
            .write(output, &u64::from(handle).to_le_bytes())
            .unwrap();
        return Outcome::Return(0);
    }
    let handle = word(env, p, 0);
    if name == "release_font_object" {
        if let Some((id, _)) = env.ext.get_mut::<Fonts>().dwrite.remove(&handle) {
            let _ = env.ext.get_mut::<Fonts>().text.remove_resource(id);
        }
        return Outcome::Return(0);
    }
    let Some((id, index)) = env.ext.get::<Fonts>().dwrite.get(&handle).copied() else {
        return Outcome::Return(0xc000000d);
    };
    let simulations = if matches!(name, "get_glyph_count" | "get_glyph_advance") {
        0
    } else {
        word(env, p, 8)
    };
    let em = if name == "get_design_glyph_metrics" {
        word(env, p, 16) as f32
    } else if name == "get_glyph_bitmap" {
        f32::from_bits(word(env, p, 20))
    } else {
        f32::from_bits(word(env, p, 16))
    };
    let height = if name == "get_glyph_count" {
        -16
    } else {
        -(em.round().max(1.0) as i32)
    };
    let Ok(request) = env
        .ext
        .get_mut::<Fonts>()
        .text
        .resource_font(id, index, height, simulations)
    else {
        return Outcome::Return(0xc000000d);
    };
    if name == "get_glyph_count" {
        let file = env.ext.get_mut::<Fonts>().text.font_file(&request).unwrap();
        let maxp = file.table(*b"maxp").unwrap();
        let count = u16::from_be_bytes(maxp[4..6].try_into().unwrap());
        put(env, word(env, p, 8), u32::from(count));
        return Outcome::Return(0);
    }
    let glyph = word(env, p, if name == "get_glyph_advance" { 8 } else { 12 });
    let mono = name == "get_glyph_bitmap" && word(env, p, 16) == 1;
    let format = if name == "get_glyph_outline" {
        GlyphFormat::CubicOutline
    } else if name == "get_glyph_bitmap" {
        if mono {
            GlyphFormat::Monochrome
        } else {
            GlyphFormat::Gray8
        }
    } else {
        GlyphFormat::Metrics
    };
    let mut transform = Transform::IDENTITY;
    if matches!(name, "get_glyph_bitmap" | "get_glyph_bbox") {
        let at: u32 = if name == "get_glyph_bitmap" { 24 } else { 20 };
        let f = |n: u32| f32::from_bits(word(env, p, at + n));
        transform = Transform {
            xx: (f(0) * 65536.0).round() as i32,
            yx: (-f(4) * 65536.0).round() as i32,
            xy: (-f(8) * 65536.0).round() as i32,
            yy: (f(12) * 65536.0).round() as i32,
        };
    }
    let Ok(g) = env.ext.get_mut::<Fonts>().text.glyph(
        &request,
        GlyphRequest {
            character: Character::GlyphIndex(glyph),
            format,
            hinted: name != "get_design_glyph_metrics",
            transform,
        },
    ) else {
        return Outcome::Return(0xc0000001);
    };
    let m = g.metrics;
    match name {
        "get_glyph_advance" => {
            put(env, word(env, p, 20), i32::from(m.advance_x) as u32);
            put(
                env,
                word(env, p, 24),
                u32::from(m.width != 0 && m.height != 0),
            );
        }
        "get_glyph_bbox" => {
            let bytes: Vec<_> = [
                m.origin_x,
                -m.origin_y,
                m.origin_x + m.width as i32,
                -m.origin_y + m.height as i32,
            ]
            .into_iter()
            .flat_map(i32::to_le_bytes)
            .collect();
            env.cpu.write(word(env, p, 36), &bytes).unwrap();
        }
        "get_design_glyph_metrics" => {
            let upem = word(env, p, 16);
            let ascent = word(env, p, 20) as i32;
            let top = ascent - m.origin_y;
            let bytes: Vec<_> = [
                m.origin_x,
                i32::from(m.advance_x),
                i32::from(m.advance_x) - m.origin_x - m.width as i32,
                top,
                upem as i32,
                upem as i32 - m.height as i32 - top,
                ascent,
            ]
            .into_iter()
            .flat_map(i32::to_le_bytes)
            .collect();
            env.cpu.write(word(env, p, 24), &bytes).unwrap();
        }
        "get_glyph_bitmap" => {
            let bbox: [i32; 4] = std::array::from_fn(|i| word(env, p, 40 + i as u32 * 4) as i32);
            let pitch = word(env, p, 56) as usize;
            let target = word(env, p, 60);
            let h = (bbox[3] - bbox[1]).max(0) as usize;
            let w = (bbox[2] - bbox[0]).max(0) as usize;
            let mut bytes = vec![0; pitch * h];
            let GlyphData::Bitmap(data) = &g.data else {
                unreachable!()
            };
            let src_pitch = if mono {
                (m.width.div_ceil(8) + 3) & !3
            } else {
                (m.width + 3) & !3
            } as usize;
            for y in 0..h {
                for x in 0..w {
                    let sx = x as i32 + bbox[0] - m.origin_x;
                    let sy = y as i32 + bbox[1] + m.origin_y;
                    if sx < 0 || sy < 0 || sx >= m.width as i32 || sy >= m.height as i32 {
                        continue;
                    }
                    if mono {
                        if data[sy as usize * src_pitch + sx as usize / 8] & (0x80 >> (sx & 7)) != 0
                        {
                            bytes[y * pitch + x / 8] |= 0x80 >> (x & 7);
                        }
                    } else {
                        bytes[y * pitch + x] =
                            ((u32::from(data[sy as usize * src_pitch + sx as usize]) * 255 + 32)
                                / 64) as u8;
                    }
                }
            }
            env.cpu.write(target, &bytes).unwrap();
            put(env, word(env, p, 64), u32::from(mono));
        }
        "get_glyph_outline" => {
            let out = word(env, p, 20);
            let points = word(env, out, 12);
            let tags = word(env, out, 0);
            let mut values = Vec::new();
            let mut kinds = Vec::new();
            let GlyphData::Outline(contours) = &g.data else {
                unreachable!()
            };
            for contour in contours {
                if contour.curves.is_empty() {
                    continue;
                }
                kinds.push(0);
                append(&mut values, contour.start);
                for curve in &contour.curves {
                    match curve {
                        Curve::Line(points) => {
                            for &point in points {
                                kinds.push(2);
                                append(&mut values, point)
                            }
                        }
                        Curve::Cubic(points) => {
                            for chunk in points.as_chunks::<3>().0.iter() {
                                kinds.push(3);
                                for &point in chunk {
                                    append(&mut values, point)
                                }
                            }
                        }
                        Curve::Quadratic(_) => unreachable!(),
                    }
                }
                kinds.push(1);
            }
            put(env, out + 4, kinds.len() as u32);
            put(env, out + 16, (values.len() / 8) as u32);
            if points != 0 {
                env.cpu.write(points, &values).unwrap();
                env.cpu.write(tags, &kinds).unwrap();
            }
        }
        _ => unreachable!(),
    }
    Outcome::Return(0)
}
fn append(bytes: &mut Vec<u8>, point: text::Point) {
    bytes.extend((point.x as f32 / 65536.0).to_le_bytes());
    bytes.extend((-point.y as f32 / 65536.0).to_le_bytes());
}

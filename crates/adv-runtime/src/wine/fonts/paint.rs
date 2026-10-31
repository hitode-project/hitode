//! Position once in logical coordinates, compose our cached glyphs on DIBs.
use super::*;
use std::sync::Arc;

pub(super) fn register(_registry: &mut Registry) {}
#[derive(Clone, Copy)]
struct Matrix([f64; 6]);
impl Matrix {
    fn point(self, x: f64, y: f64) -> [i32; 2] {
        let m = self.0;
        [
            (x * m[0] + y * m[2] + m[4] + 0.5).floor() as i32,
            (x * m[1] + y * m[3] + m[5] + 0.5).floor() as i32,
        ]
    }
    fn glyph(self) -> Transform {
        let m = self.0;
        Transform {
            xx: (m[0] * 65536.0).round() as i32,
            xy: (-m[1] * 65536.0).round() as i32,
            yx: (-m[2] * 65536.0).round() as i32,
            yy: (m[3] * 65536.0).round() as i32,
        }
    }
}
fn matrix(env: &Env<'_>, dc: u32) -> Matrix {
    let mut bytes = [0; 24];
    env.cpu
        .read(dc + layout::dc::xformWorld2Vport, &mut bytes)
        .unwrap();
    Matrix(std::array::from_fn(|i| {
        f64::from(f32::from_le_bytes(
            bytes[i * 4..i * 4 + 4].try_into().unwrap(),
        ))
    }))
}
fn rect(env: &Env<'_>, at: u32) -> [i32; 4] {
    std::array::from_fn(|i| env.cpu.read_u32(at + i as u32 * 4).unwrap() as i32)
}
fn color(env: &Env<'_>, dc: u32, value: u32) -> [u8; 3] {
    if value >> 24 == 1 {
        let handle = env.cpu.read_u32(dc + layout::dc::hPalette).unwrap();
        if let Some(pal) = object(env, handle, 8) {
            let count = env.cpu.read_u16(pal + 14).unwrap();
            let index = (value & 0xffff).min(u32::from(count.saturating_sub(1)));
            let entries = env.cpu.read_u32(pal + 16).unwrap();
            let v = env.cpu.read_u32(entries + index * 4).unwrap();
            return [v as u8, (v >> 8) as u8, (v >> 16) as u8];
        }
    }
    [value as u8, (value >> 8) as u8, (value >> 16) as u8]
}
fn physdev(env: &Env<'_>, dc: u32) -> Option<(u32, u32)> {
    let mut dev = env.cpu.read_u32(dc + layout::dc::physDev).ok()?;
    while dev != 0 {
        let table = env.cpu.read_u32(dev).ok()?;
        let f = env.cpu.read_u32(table + layout::funcs::pExtTextOut).ok()?;
        if f != 0 {
            return Some((dev, f));
        }
        dev = env.cpu.read_u32(dev + 4).ok()?;
    }
    None
}
struct Run {
    glyphs: Vec<(Arc<text::Glyph>, [i32; 2])>,
    fills: Vec<([[i32; 2]; 4], [u8; 3])>,
    clip: Option<[i32; 4]>,
    foreground: [u8; 3],
    mono: bool,
}
#[allow(clippy::too_many_arguments)]
pub(super) fn render(
    env: &mut Env<'_>,
    hdc: u32,
    x: i32,
    y: i32,
    flags: u32,
    rectangle: Option<[i32; 4]>,
    units: &[u16],
    dx: Option<&[i32]>,
) -> Outcome {
    let Some(dc) = dc_pointer(env, hdc) else {
        set_last_error(env, 6);
        return Outcome::Return(0);
    };
    let attr = env.cpu.read_u32(dc + layout::dc::attr).unwrap();
    let mut request = font_request(env, hdc);
    let align = u32::from(env.cpu.read_u16(attr + layout::attr::text_align).unwrap());
    let matrix = matrix(env, dc);
    let text = &mut env.ext.get_mut::<Fonts>().text;
    let Ok(metrics) = text.metrics(&request) else {
        return Outcome::Return(0);
    };
    let extra = env.cpu.read_u32(attr + layout::attr::char_extra).unwrap() as i32;
    let justify = env.cpu.read_u32(dc + layout::dc::breakExtra).unwrap() as i32;
    let rem = env.cpu.read_u32(dc + layout::dc::breakRem).unwrap() as i32;
    let indices = flags & 0x10 != 0;
    let positions = match driver::positions(env, &request, units, indices, extra, justify, rem) {
        Ok((p, _)) => p,
        Err(_) => return Outcome::Return(0),
    };
    let angle = f64::from(request.baseline_angle) / 1800.0 * std::f64::consts::PI;
    let (sin, cos) = angle.sin_cos();
    let mut advance = Vec::with_capacity(units.len() + 1);
    advance.push([0.0, 0.0]);
    let mut previous = 0;
    for i in 0..units.len() {
        let (x, y) = if let Some(dx) = dx {
            if flags & 0x2000 != 0 {
                (dx[i * 2], dx[i * 2 + 1])
            } else {
                (dx[i], 0)
            }
        } else {
            (positions[i] - previous, 0)
        };
        previous = positions[i];
        let p = advance[i];
        advance.push([
            p[0] + f64::from(x) * cos + f64::from(y) * sin,
            p[1] - f64::from(x) * sin + f64::from(y) * cos,
        ]);
    }
    let total = *advance.last().unwrap();
    let mut origin = if align & 1 != 0 {
        [
            env.cpu.read_u32(attr + layout::attr::cur_pos).unwrap() as i32,
            env.cpu.read_u32(attr + layout::attr::cur_pos + 4).unwrap() as i32,
        ]
    } else {
        [x, y]
    };
    if align & 1 != 0 && align & 6 != 6 {
        let p = if align & 6 == 2 {
            [-total[0], -total[1]]
        } else {
            total
        };
        env.cpu
            .write_u32(
                attr + layout::attr::cur_pos,
                (f64::from(origin[0]) + p[0]).round() as i32 as u32,
            )
            .unwrap();
        env.cpu
            .write_u32(
                attr + layout::attr::cur_pos + 4,
                (f64::from(origin[1]) + p[1]).round() as i32 as u32,
            )
            .unwrap();
    }
    let h = match align & 6 {
        2 => 1.0,
        6 => 0.5,
        _ => 0.0,
    };
    let v = match align & 24 {
        24 => 0.0,
        8 => -f64::from(metrics.descent),
        _ => f64::from(metrics.ascent),
    };
    let base = [
        f64::from(origin[0]) - total[0] * h + v * sin,
        f64::from(origin[1]) - total[1] * h + v * cos,
    ];
    origin = matrix.point(base[0], base[1]);
    let foreground = color(
        env,
        dc,
        env.cpu.read_u32(attr + layout::attr::text_color).unwrap(),
    );
    let background = color(
        env,
        dc,
        env.cpu
            .read_u32(attr + layout::attr::background_color)
            .unwrap(),
    );
    let quality = env
        .ext
        .get_mut::<Fonts>()
        .text
        .logical_font(&request)
        .map_or(request.quality, |f| f.quality);
    let target = physdev(env, dc);
    let monochrome = target.is_some_and(|(dev, _)| {
        env.cpu.read_u32(dev).ok() == module(env).map(|base| base + layout::DIB_DRIVER)
            && palette_dib(env, dev)
    });
    let mut run = Run {
        glyphs: Vec::with_capacity(units.len()),
        fills: Vec::new(),
        clip: None,
        foreground,
        mono: monochrome || quality == Quality::Monochrome,
    };
    if let Some(r) = rectangle {
        let p = matrix.point(f64::from(r[0]), f64::from(r[1]));
        let q = matrix.point(f64::from(r[2]), f64::from(r[3]));
        let r = [
            p[0].min(q[0]),
            p[1].min(q[1]),
            p[0].max(q[0]),
            p[1].max(q[1]),
        ];
        if flags & 4 != 0 {
            run.clip = Some(r)
        }
        if flags & 2 != 0 {
            run.fills.push((
                [[r[0], r[1]], [r[2], r[1]], [r[2], r[3]], [r[0], r[3]]],
                background,
            ));
        }
    }
    let quad = |top: f64, bottom: f64| {
        std::array::from_fn(|i| {
            let p = if i == 1 || i == 2 { total } else { [0.0, 0.0] };
            let y = if i < 2 { top } else { bottom };
            matrix.point(base[0] + p[0] + y * sin, base[1] + p[1] + y * cos)
        })
    };
    if env
        .cpu
        .read_u16(attr + layout::attr::background_mode)
        .unwrap()
        == 2
        && !units.is_empty()
    {
        run.fills.push((
            quad(-f64::from(metrics.ascent), f64::from(metrics.descent)),
            background,
        ));
    }
    let thickness = f64::from((metrics.height / 16).max(1));
    if request.underline {
        run.fills
            .push((quad(thickness, thickness * 2.0), foreground));
    }
    if request.strikeout {
        let top = -f64::from(metrics.ascent) / 3.0;
        run.fills.push((quad(top, top + thickness), foreground));
    }
    // The cached glyphs include the LOGFONT orientation and DC linear transform.
    request.underline = false;
    request.strikeout = false;
    let mut codes = Vec::with_capacity(units.len());
    if indices {
        codes.extend(units.iter().enumerate().map(|(i, &u)| (i, u32::from(u))));
    } else {
        let mut i = 0;
        for scalar in char::decode_utf16(units.iter().copied()) {
            let Ok(c) = scalar else {
                return Outcome::Return(0);
            };
            codes.push((i, u32::from(c)));
            i += c.len_utf16();
        }
    }
    for (i, unit) in codes {
        let glyph = match env.ext.get_mut::<Fonts>().text.glyph(
            &request,
            GlyphRequest {
                character: if indices {
                    Character::GlyphIndex(unit)
                } else {
                    Character::Unicode(unit)
                },
                format: if run.mono {
                    GlyphFormat::Monochrome
                } else {
                    GlyphFormat::Gray8
                },
                hinted: true,
                transform: matrix.glyph(),
            },
        ) {
            Ok(g) => g,
            Err(_) => return Outcome::Return(0),
        };
        let p = matrix.point(base[0] + advance[i][0], base[1] + advance[i][1]);
        run.glyphs.push((glyph, p));
    }
    let Some((dev, function)) = physdev(env, dc) else {
        return Outcome::Return(0);
    };
    if env.cpu.read_u32(dev).unwrap() == module(env).unwrap() + layout::DIB_DRIVER {
        return compose(env, dev, &run);
    }
    // Metafile/path/printer drivers receive the positioned glyph run, and obtain
    // contours through our font driver when converting the run into a path.
    let allocator = super::super::allocator();
    let count = units.len() as u32;
    let size = count * 10 + 16;
    let Some(scratch) = (allocator.allocate)(env, size) else {
        return Outcome::Return(0);
    };
    let mut bytes: Vec<_> = units.iter().flat_map(|u| u.to_le_bytes()).collect();
    let dp = scratch + count * 2;
    for i in 0..units.len() {
        let p = matrix.point(base[0] + advance[i][0], base[1] + advance[i][1]);
        let q = matrix.point(base[0] + advance[i + 1][0], base[1] + advance[i + 1][1]);
        bytes.extend((q[0] - p[0]).to_le_bytes());
        bytes.extend((q[1] - p[1]).to_le_bytes());
    }
    let rp = scratch + count * 10;
    if let Some(r) = rectangle {
        let p = matrix.point(f64::from(r[0]), f64::from(r[1]));
        let q = matrix.point(f64::from(r[2]), f64::from(r[3]));
        bytes.extend(
            [
                p[0].min(q[0]),
                p[1].min(q[1]),
                p[0].max(q[0]),
                p[1].max(q[1]),
            ]
            .into_iter()
            .flat_map(i32::to_le_bytes),
        );
    }
    env.cpu.write(scratch, &bytes).unwrap();
    Outcome::call_aligned(
        env,
        function,
        [
            dev,
            origin[0] as u32,
            origin[1] as u32,
            flags | 0x2000,
            if rectangle.is_some() { rp } else { 0 },
            scratch,
            count,
            dp,
        ],
        move |env, result| {
            (allocator.free)(env, scratch);
            Outcome::Return(result.value())
        },
    )
}
pub(super) fn output(env: &mut Env<'_>, a: &[u32]) -> Outcome {
    let units = match placement::read(env, a[5], a[6] * 2, false) {
        Ok(b) => placement::units(b),
        Err(_) => return Outcome::Return(0),
    };
    let rectangle = (a[4] != 0).then(|| rect(env, a[4]));
    let dx = if a[7] != 0 {
        match placement::read(
            env,
            a[7],
            a[6] * if a[3] & 0x2000 != 0 { 8 } else { 4 },
            false,
        ) {
            Ok(b) => Some(
                placement::words(b)
                    .into_iter()
                    .map(|v| v as i32)
                    .collect::<Vec<_>>(),
            ),
            Err(_) => return Outcome::Return(0),
        }
    } else {
        None
    };
    render(
        env,
        a[0],
        a[1] as i32,
        a[2] as i32,
        a[3],
        rectangle,
        &units,
        dx.as_deref(),
    )
}
pub(super) fn driver_output(env: &mut Env<'_>, a: &[u32]) -> Outcome {
    let hdc = env.cpu.read_u32(a[0] + layout::physdev::hdc).unwrap();
    let units = placement::units(placement::read(env, a[5], a[6] * 2, false).unwrap());
    let request = font_request(env, hdc);
    let mut run = Run {
        glyphs: Vec::new(),
        fills: Vec::new(),
        clip: None,
        foreground: [0; 3],
        mono: request.quality == Quality::Monochrome || palette_dib(env, a[0]),
    };
    let dc = dc_pointer(env, hdc).unwrap();
    let attr = env.cpu.read_u32(dc + layout::dc::attr).unwrap();
    run.foreground = color(
        env,
        dc,
        env.cpu.read_u32(attr + layout::attr::text_color).unwrap(),
    );
    if a[4] != 0 {
        let r = rect(env, a[4]);
        if a[3] & 4 != 0 {
            run.clip = Some(r)
        }
        if a[3] & 2 != 0 {
            let bg = color(
                env,
                dc,
                env.cpu
                    .read_u32(attr + layout::attr::background_color)
                    .unwrap(),
            );
            run.fills
                .push(([[r[0], r[1]], [r[2], r[1]], [r[2], r[3]], [r[0], r[3]]], bg));
        }
    }
    let mut point = [a[1] as i32, a[2] as i32];
    for (i, &unit) in units.iter().enumerate() {
        let Ok(glyph) = env.ext.get_mut::<Fonts>().text.glyph(
            &request,
            GlyphRequest {
                character: if a[3] & 16 != 0 {
                    Character::GlyphIndex(u32::from(unit))
                } else {
                    Character::Unicode(u32::from(unit))
                },
                format: if run.mono {
                    GlyphFormat::Monochrome
                } else {
                    GlyphFormat::Gray8
                },
                hinted: true,
                transform: Transform::IDENTITY,
            },
        ) else {
            return Outcome::Return(0);
        };
        let advance = if a[7] != 0 {
            let at = a[7] + i as u32 * if a[3] & 0x2000 != 0 { 8 } else { 4 };
            [
                env.cpu.read_u32(at).unwrap() as i32,
                if a[3] & 0x2000 != 0 {
                    env.cpu.read_u32(at + 4).unwrap() as i32
                } else {
                    0
                },
            ]
        } else {
            [
                i32::from(glyph.metrics.advance_x),
                i32::from(glyph.metrics.advance_y),
            ]
        };
        run.glyphs.push((glyph, point));
        point[0] += advance[0];
        point[1] += advance[1];
    }
    compose(env, a[0], &run)
}
pub(super) enum Drawing {
    Text {
        hdc: u32,
        x: i32,
        y: i32,
        flags: u32,
        rectangle: Option<[i32; 4]>,
        units: Vec<u16>,
        dx: Option<Vec<i32>>,
    },
    Rule {
        hdc: u32,
        rectangle: [i32; 4],
        clip: Option<[i32; 4]>,
    },
}
pub(super) struct Paragraph {
    pub drawing: std::collections::VecDeque<Drawing>,
    pub result: u32,
    pub alignment: Option<(u32, u16)>,
}
impl Paragraph {
    pub fn draw(mut self, env: &mut Env<'_>) -> Outcome {
        while let Some(drawing) = self.drawing.pop_front() {
            let outcome = match drawing {
                Drawing::Text {
                    hdc,
                    x,
                    y,
                    flags,
                    rectangle,
                    units,
                    dx,
                } => render(env, hdc, x, y, flags, rectangle, &units, dx.as_deref()),
                Drawing::Rule {
                    hdc,
                    rectangle,
                    clip,
                } => rule(env, hdc, rectangle, clip),
            };
            if !matches!(outcome, Outcome::Return(1)) {
                return self.resume(env, outcome);
            }
        }
        let result = self.result;
        self.finish(env, result)
    }
    fn finish(self, env: &mut Env<'_>, result: u32) -> Outcome {
        if let Some((at, align)) = self.alignment {
            env.cpu.write(at, &align.to_le_bytes()).unwrap();
        }
        Outcome::Return(result)
    }
    fn resume(self, env: &mut Env<'_>, outcome: Outcome) -> Outcome {
        match outcome {
            Outcome::Return(1) => self.draw(env),
            Outcome::Return(_) => self.finish(env, 0),
            Outcome::Call {
                target,
                args,
                then,
                stack,
                unwindable,
            } => Outcome::Call {
                target,
                args,
                stack,
                unwindable,
                then: Box::new(move |env, result| {
                    let next = then(env, result);
                    self.resume(env, next)
                }),
            },
            other => other,
        }
    }
}
pub(super) fn poly(env: &mut Env<'_>, a: &[u32]) -> Outcome {
    let mut drawing = std::collections::VecDeque::new();
    for i in 0..a[2] {
        let at = a[1] + i * 40;
        let r: Vec<_> = (0..10)
            .map(|j| env.cpu.read_u32(at + j * 4).unwrap())
            .collect();
        let units = placement::units(placement::read(env, r[3], r[2] * 2, false).unwrap());
        let dx = (r[9] != 0).then(|| {
            placement::words(
                placement::read(
                    env,
                    r[9],
                    r[2] * if r[4] & 0x2000 != 0 { 8 } else { 4 },
                    false,
                )
                .unwrap(),
            )
            .into_iter()
            .map(|v| v as i32)
            .collect()
        });
        drawing.push_back(Drawing::Text {
            hdc: a[0],
            x: r[0] as i32,
            y: r[1] as i32,
            flags: r[4],
            rectangle: Some(rect(env, at + 20)),
            units,
            dx,
        });
    }
    Paragraph {
        drawing,
        result: 1,
        alignment: None,
    }
    .draw(env)
}
/// Windows draws text on DIBs of 8 bits per pixel or fewer without
/// antialiasing, whatever quality the font asks for (WIN32-GDI-16).
fn palette_dib(env: &mut Env<'_>, dev: u32) -> bool {
    env.cpu
        .read_u32(dev + layout::dibdev::dib + layout::dib::bit_count)
        .is_ok_and(|bits| bits <= 8)
}

fn compose(env: &mut Env<'_>, dev: u32, run: &Run) -> Outcome {
    let dib = dev + layout::dibdev::dib;
    let get = |at| env.cpu.read_u32(dib + at).unwrap();
    let bpp = get(layout::dib::bit_count);
    let width = get(layout::dib::width) as i32;
    let height = get(layout::dib::height) as i32;
    let stride = get(layout::dib::stride) as i32;
    let bits = get(layout::dib::bits);
    let masks = if bpp == 24 {
        [0xff0000, 0xff00, 0xff]
    } else {
        [
            get(layout::dib::red_mask),
            get(layout::dib::green_mask),
            get(layout::dib::blue_mask),
        ]
    };
    let colors = get(layout::dib::color_table);
    let count = get(layout::dib::color_table_size);
    let mut table = vec![0; count as usize * 4];
    env.cpu.read(colors, &mut table).unwrap();
    let palette: Vec<[u8; 3]> = table
        .as_chunks::<4>()
        .0
        .iter()
        .map(|&[b, g, r, _]| [r, g, b])
        .collect();
    let region = env.cpu.read_u32(dev + layout::dibdev::clip).unwrap();
    let clips = if let Some(p) = object(env, region, 4) {
        let count = env.cpu.read_u32(p + layout::region::numRects).unwrap();
        let r = env.cpu.read_u32(p + layout::region::rects).unwrap();
        (0..count)
            .map(|i| rect(env, r + i * 16))
            .collect::<Vec<_>>()
    } else {
        vec![[0, 0, width, height]]
    };
    let mut bounds = [width, height, 0, 0];
    let mut include = |r: [i32; 4]| {
        bounds[0] = bounds[0].min(r[0]);
        bounds[1] = bounds[1].min(r[1]);
        bounds[2] = bounds[2].max(r[2]);
        bounds[3] = bounds[3].max(r[3]);
    };
    for (g, p) in &run.glyphs {
        let m = g.metrics;
        include([
            p[0] + m.origin_x,
            p[1] - m.origin_y,
            p[0] + m.origin_x + m.width as i32,
            p[1] - m.origin_y + m.height as i32,
        ]);
    }
    for (q, _) in &run.fills {
        include([
            q.iter().map(|p| p[0]).min().unwrap(),
            q.iter().map(|p| p[1]).min().unwrap(),
            q.iter().map(|p| p[0]).max().unwrap(),
            q.iter().map(|p| p[1]).max().unwrap(),
        ]);
    }
    bounds[0] = bounds[0].max(0);
    bounds[1] = bounds[1].max(0);
    bounds[2] = bounds[2].min(width);
    bounds[3] = bounds[3].min(height);
    if let Some(r) = run.clip {
        bounds[0] = bounds[0].max(r[0]);
        bounds[1] = bounds[1].max(r[1]);
        bounds[2] = bounds[2].min(r[2]);
        bounds[3] = bounds[3].min(r[3]);
    }
    if bounds[0] >= bounds[2] || bounds[1] >= bounds[3] {
        return Outcome::Return(1);
    }
    let origin = rect(env, dib + layout::dib::rect);
    let first = ((bounds[0] + origin[0]) as u32 * bpp / 8) as usize;
    let end = ((bounds[2] + origin[0]) as u32 * bpp).div_ceil(8) as usize;
    let pitch = end - first;
    let rows = (bounds[3] - bounds[1]) as usize;
    let mut bytes = vec![0; pitch * rows];
    for row in 0..rows {
        let at = bits.wrapping_add(((origin[1] + bounds[1] + row as i32) * stride) as u32)
            + first as u32;
        env.cpu
            .read(at, &mut bytes[row * pitch..(row + 1) * pitch])
            .unwrap();
    }
    let mut nearest = NEAREST.take();
    nearest.adopt(&palette);
    let mut blend = |x: i32, y: i32, rgb: [u8; 3], coverage: u32| {
        if x < bounds[0]
            || x >= bounds[2]
            || y < bounds[1]
            || y >= bounds[3]
            || !clips
                .iter()
                .any(|r| x >= r[0] && x < r[2] && y >= r[1] && y < r[3])
        {
            return;
        }
        let x = x + origin[0];
        let offset = (y - bounds[1]) as usize * pitch + (x as usize * bpp as usize / 8 - first);
        let n = bpp.div_ceil(8) as usize;
        let raw = match bpp {
            1 => u32::from((bytes[offset] >> (7 - (x & 7))) & 1),
            4 => u32::from((bytes[offset] >> if x & 1 == 0 { 4 } else { 0 }) & 15),
            8 => u32::from(bytes[offset]),
            _ => {
                let mut v = [0; 4];
                v[..n].copy_from_slice(&bytes[offset..offset + n]);
                u32::from_le_bytes(v)
            }
        };
        let old: [u8; 3] = if bpp <= 8 {
            palette
                .get(raw as usize)
                .copied()
                .unwrap_or(if raw == 0 { [0; 3] } else { [255; 3] })
        } else {
            std::array::from_fn(|i| {
                let m = masks[i];
                if m == 0 {
                    0
                } else {
                    (((raw & m) >> m.trailing_zeros()) * 255 / (m >> m.trailing_zeros())) as u8
                }
            })
        };
        let rgb: [u8; 3] = std::array::from_fn(|i| {
            ((u32::from(old[i]) * (255 - coverage) + u32::from(rgb[i]) * coverage + 127) / 255)
                as u8
        });
        let value = if bpp <= 8 {
            nearest.index(rgb).unwrap_or(u32::from(rgb[0] >= 128))
        } else {
            let mut v = if bpp == 16 {
                0
            } else {
                raw & !(masks[0] | masks[1] | masks[2])
            };
            for (m, c) in masks.into_iter().zip(rgb) {
                if m != 0 {
                    let shift = m.trailing_zeros();
                    v |= (u32::from(c) * (m >> shift) / 255) << shift;
                }
            }
            v
        };
        match bpp {
            1 => {
                let mask = 0x80 >> (x & 7);
                bytes[offset] = (bytes[offset] & !mask) | if value != 0 { mask } else { 0 }
            }
            4 => {
                let shift = if x & 1 == 0 { 4 } else { 0 };
                bytes[offset] = (bytes[offset] & !(15 << shift)) | ((value as u8) << shift)
            }
            8 => bytes[offset] = value as u8,
            _ => bytes[offset..offset + n].copy_from_slice(&value.to_le_bytes()[..n]),
        }
    };
    for (q, color) in &run.fills {
        let top = q.iter().map(|p| p[1]).min().unwrap().max(bounds[1]);
        let bottom = q.iter().map(|p| p[1]).max().unwrap().min(bounds[3]);
        for y in top..bottom {
            for x in bounds[0]..bounds[2] {
                let mut inside = false;
                for i in 0..4 {
                    let a = q[i];
                    let b = q[(i + 1) % 4];
                    let py = f64::from(y) + 0.5;
                    if (f64::from(a[1]) > py) != (f64::from(b[1]) > py)
                        && f64::from(x) + 0.5
                            < f64::from(b[0] - a[0]) * (py - f64::from(a[1]))
                                / f64::from(b[1] - a[1])
                                + f64::from(a[0])
                    {
                        inside = !inside;
                    }
                }
                if inside {
                    blend(x, y, *color, 255);
                }
            }
        }
    }
    for (g, p) in &run.glyphs {
        let GlyphData::Bitmap(data) = &g.data else {
            continue;
        };
        let m = g.metrics;
        let pitch = if run.mono {
            (m.width.div_ceil(8) + 3) & !3
        } else {
            (m.width + 3) & !3
        } as usize;
        for y in 0..m.height as usize {
            for x in 0..m.width as usize {
                let value = if run.mono {
                    if data[y * pitch + x / 8] & (0x80 >> (x & 7)) != 0 {
                        255
                    } else {
                        0
                    }
                } else {
                    (u32::from(data[y * pitch + x]) * 255 + 32) / 64
                };
                if value != 0 {
                    blend(
                        p[0] + m.origin_x + x as i32,
                        p[1] - m.origin_y + y as i32,
                        run.foreground,
                        value,
                    );
                }
            }
        }
    }
    NEAREST.set(nearest);
    for row in 0..rows {
        let at = bits.wrapping_add(((origin[1] + bounds[1] + row as i32) * stride) as u32)
            + first as u32;
        env.cpu
            .write(at, &bytes[row * pitch..(row + 1) * pitch])
            .unwrap();
    }
    Outcome::Return(1)
}

/// The palette entry nearest to `rgb` by squared distance, the first on ties.
fn nearest(palette: &[[u8; 3]], rgb: [u8; 3]) -> Option<u32> {
    palette
        .iter()
        .enumerate()
        .min_by_key(|(_, p)| {
            p.iter()
                .zip(rgb)
                .map(|(&a, b)| {
                    let d = i32::from(a) - i32::from(b);
                    d * d
                })
                .sum::<i32>()
        })
        .map(|(i, _)| i as u32)
}

/// Antialiased text on a palette DIB asks for the same few blends pixel after
/// pixel and call after call; a full scan of the palette for each pixel stalled
/// whole frames. Answers are kept in a direct-mapped table, since a hash per
/// pixel costs as much as the rest of the blend, until the palette changes.
struct Nearest {
    palette: Vec<[u8; 3]>,
    slots: Box<[Slot]>,
}

/// A blend and the palette entry nearest to it.
type Slot = Option<([u8; 3], Option<u32>)>;

/// Slots of the table, a power of two; a collision only repeats a scan.
const NEAREST_SLOTS: usize = 4096;

impl Default for Nearest {
    fn default() -> Self {
        Nearest {
            palette: Vec::new(),
            slots: vec![None; NEAREST_SLOTS].into_boxed_slice(),
        }
    }
}

impl Nearest {
    fn adopt(&mut self, palette: &[[u8; 3]]) {
        if self.palette != palette {
            self.palette = palette.to_vec();
            self.slots.fill(None);
        }
    }

    fn index(&mut self, rgb: [u8; 3]) -> Option<u32> {
        let key = u32::from_le_bytes([rgb[0], rgb[1], rgb[2], 0]);
        let slot = (key.wrapping_mul(0x9e37_79b1) >> 20) as usize % NEAREST_SLOTS;
        match self.slots[slot] {
            Some((kept, index)) if kept == rgb => index,
            _ => {
                let index = nearest(&self.palette, rgb);
                self.slots[slot] = Some((rgb, index));
                index
            }
        }
    }
}

thread_local! {
    static NEAREST: std::cell::Cell<Nearest> = std::cell::Cell::default();
}

pub(super) fn rule(
    env: &mut Env<'_>,
    hdc: u32,
    rectangle: [i32; 4],
    clip: Option<[i32; 4]>,
) -> Outcome {
    let dc = dc_pointer(env, hdc).unwrap();
    let attr = env.cpu.read_u32(dc + layout::dc::attr).unwrap();
    let transform = matrix(env, dc);
    let r = rectangle;
    let q = [[r[0], r[1]], [r[2], r[1]], [r[2], r[3]], [r[0], r[3]]]
        .map(|p| transform.point(f64::from(p[0]), f64::from(p[1])));
    let color = color(
        env,
        dc,
        env.cpu.read_u32(attr + layout::attr::text_color).unwrap(),
    );
    let clip = clip.map(|r| {
        let p = transform.point(f64::from(r[0]), f64::from(r[1]));
        let q = transform.point(f64::from(r[2]), f64::from(r[3]));
        [
            p[0].min(q[0]),
            p[1].min(q[1]),
            p[0].max(q[0]),
            p[1].max(q[1]),
        ]
    });
    let run = Run {
        glyphs: Vec::new(),
        fills: vec![(q, color)],
        clip,
        foreground: color,
        mono: true,
    };
    let Some((dev, _)) = physdev(env, dc) else {
        return Outcome::Return(0);
    };
    if env.cpu.read_u32(dev).unwrap() == module(env).unwrap() + layout::DIB_DRIVER {
        compose(env, dev, &run)
    } else {
        fill_driver(env, hdc, q, color, clip)
    }
}

pub(super) fn pe_output(env: &mut Env<'_>, name: &str, args: &[u32]) -> Outcome {
    let extended = name.starts_with("Ext");
    let ansi = name.ends_with('A');
    let mut a = [
        args[0],
        args[1],
        args[2],
        if extended { args[3] } else { 0 },
        if extended { args[4] } else { 0 },
        if extended { args[5] } else { args[3] },
        if extended { args[6] } else { args[4] },
        if extended { args[7] } else { 0 },
        0,
    ];
    if (a[6] as i32) < 0 || reject_dc(env, a[0]) {
        return Outcome::Return(0);
    }
    let request = font_request(env, a[0]);
    let mut converted_dx = None;
    let units = if ansi && a[3] & 16 == 0 {
        let Ok(bytes) = placement::read(env, a[5], a[6], false) else {
            return Outcome::Return(0);
        };
        let units = ansi_text(env, &request, &bytes)
            .unwrap_or_else(|| env.ext.get::<Fonts>().codec.to_utf16(0, &bytes));
        if a[7] != 0 {
            let pdy = a[3] & 0x2000 != 0;
            let stride = if pdy { 2 } else { 1 };
            let mut advances = Vec::new();
            let cp = ansi_codepage(env, &request).unwrap_or(1252);
            let mut at = 0;
            while at < bytes.len() {
                let n = if env.ext.get::<Fonts>().codec.is_lead_byte(cp, bytes[at])
                    && at + 1 < bytes.len()
                {
                    2
                } else {
                    1
                };
                for axis in 0..stride {
                    let mut advance = 0i32;
                    for i in at..at + n {
                        advance += env
                            .cpu
                            .read_u32(a[7] + (i * stride + axis) as u32 * 4)
                            .unwrap() as i32;
                    }
                    advances.push(advance);
                }
                at += n;
            }
            converted_dx = Some(advances);
        }
        units
    } else {
        let Ok(bytes) = placement::read(env, a[5], a[6] * 2, false) else {
            return Outcome::Return(0);
        };
        placement::units(bytes)
    };
    let dx = if let Some(dx) = converted_dx {
        Some(dx)
    } else if a[7] != 0 {
        Some(
            placement::words(
                placement::read(
                    env,
                    a[7],
                    units.len() as u32 * if a[3] & 0x2000 != 0 { 8 } else { 4 },
                    false,
                )
                .unwrap(),
            )
            .into_iter()
            .map(|v| v as i32)
            .collect(),
        )
    } else {
        None
    };
    let rectangle = (a[4] != 0).then(|| rect(env, a[4]));
    // Reordering and contextual glyph selection are requested on Unicode runs;
    // explicit glyph-index and ignore-language runs retain caller positioning.
    let align = env
        .cpu
        .read_u16(
            env.cpu
                .read_u32(dc_pointer(env, a[0]).unwrap() + layout::dc::attr)
                .unwrap()
                + layout::attr::text_align,
        )
        .unwrap();
    if a[3] & (16 | 0x1000) == 0
        && dx.is_none()
        && (align & 0x100 != 0
            || a[3] & 0x80 != 0
            || units.iter().any(|&u| matches!(u, 0x590..=0x8ff)))
    {
        let capacity = units.len() as u32;
        let buffers = text::PlacementBuffers {
            text: None,
            encoded_text: None,
            order: None,
            advances: Some(vec![0; units.len()]),
            caret: None,
            classes: None,
            glyphs: Some(vec![0; units.len()]),
            capacity,
            max_fit: 0,
        };
        let acp = env.ext.get::<Fonts>().codec.acp;
        let Ok(shaped) = env.ext.get_mut::<Fonts>().text.placement(
            &request,
            text::TextInput::Unicode(&units),
            i32::MAX,
            2 | 0x10 | 0x20,
            Some(buffers),
            acp,
        ) else {
            return Outcome::Return(0);
        };
        let buffers = shaped.buffers.unwrap();
        let count = buffers.capacity as usize;
        let glyphs = &buffers.glyphs.unwrap()[..count];
        let dx = &buffers.advances.unwrap()[..count];
        a[3] |= 16;
        return render(
            env,
            a[0],
            a[1] as i32,
            a[2] as i32,
            a[3],
            rectangle,
            glyphs,
            Some(dx),
        );
    }
    render(
        env,
        a[0],
        a[1] as i32,
        a[2] as i32,
        a[3],
        rectangle,
        &units,
        dx.as_deref(),
    )
}

// Underlines are filled geometry for path, printer and metafile drivers.
fn fill_driver(
    env: &mut Env<'_>,
    hdc: u32,
    quad: [[i32; 2]; 4],
    rgb: [u8; 3],
    clip: Option<[i32; 4]>,
) -> Outcome {
    let mut points: Vec<[f64; 2]> = quad.map(|p| p.map(f64::from)).into();
    if let Some(rect) = clip {
        for (axis, bound, greater) in [
            (0, rect[0], true),
            (1, rect[1], true),
            (0, rect[2], false),
            (1, rect[3], false),
        ] {
            let input = std::mem::take(&mut points);
            for i in 0..input.len() {
                let a = input[i];
                let b = input[(i + 1) % input.len()];
                let inside = |p: [f64; 2]| {
                    if greater {
                        p[axis] >= f64::from(bound)
                    } else {
                        p[axis] <= f64::from(bound)
                    }
                };
                if inside(a) {
                    points.push(a);
                }
                if inside(a) != inside(b) {
                    let t = (f64::from(bound) - a[axis]) / (b[axis] - a[axis]);
                    points.push([a[0] + t * (b[0] - a[0]), a[1] + t * (b[1] - a[1])]);
                }
            }
        }
    }
    if points.len() < 3 {
        return Outcome::Return(1);
    }
    let image = env
        .ext
        .get::<adv_nt::identity::Images>()
        .0
        .values()
        .find(|m| m.name == "win32u.so")
        .unwrap();
    let table = image.table().unwrap();
    let entry = |name| image.base + table.named(name).unwrap().rva;
    let create = entry("NtGdiCreateSolidBrush");
    let select = entry("NtGdiSelectBrush");
    let select_pen = entry("NtGdiSelectPen");
    let stock = entry("GetStockObject");
    let delete = entry("NtGdiDeleteObjectApp");
    let dc = dc_pointer(env, hdc).unwrap();
    let mut dev = env.cpu.read_u32(dc + layout::dc::physDev).unwrap();
    let function = loop {
        let table = env.cpu.read_u32(dev).unwrap();
        let function = env
            .cpu
            .read_u32(table + layout::funcs::pPolyPolygon)
            .unwrap();
        if function != 0 {
            break function;
        }
        dev = env.cpu.read_u32(dev + 4).unwrap();
    };
    let allocator = super::super::allocator();
    let count = points.len() as u32;
    let Some(scratch) = (allocator.allocate)(env, count * 8 + 4) else {
        return Outcome::Return(0);
    };
    let mut bytes: Vec<u8> = points
        .into_iter()
        .flat_map(|p| p.map(|n| n.round() as i32))
        .flat_map(i32::to_le_bytes)
        .collect();
    bytes.extend(count.to_le_bytes());
    env.cpu.write(scratch, &bytes).unwrap();
    let color = u32::from(rgb[0]) | (u32::from(rgb[1]) << 8) | (u32::from(rgb[2]) << 16);
    Outcome::call_aligned(env, create, [color, 0], move |env, result| {
        let brush = result.value();
        Outcome::call_aligned(env, select, [hdc, brush], move |env, result| {
            let old_brush = result.value();
            Outcome::call_aligned(env, stock, [8], move |env, result| {
                Outcome::call_aligned(
                    env,
                    select_pen,
                    [hdc, result.value()],
                    move |env, result| {
                        let old_pen = result.value();
                        Outcome::call_aligned(
                            env,
                            function,
                            [dev, scratch, scratch + count * 8, 1],
                            move |env, result| {
                                let success = result.value();
                                Outcome::call_aligned(
                                    env,
                                    select_pen,
                                    [hdc, old_pen],
                                    move |env, _| {
                                        Outcome::call_aligned(
                                            env,
                                            select,
                                            [hdc, old_brush],
                                            move |env, _| {
                                                Outcome::call_aligned(
                                                    env,
                                                    delete,
                                                    [brush],
                                                    move |env, _| {
                                                        (allocator.free)(env, scratch);
                                                        Outcome::Return(success)
                                                    },
                                                )
                                            },
                                        )
                                    },
                                )
                            },
                        )
                    },
                )
            })
        })
    })
}

#[cfg(test)]
mod nearest_tests {
    use super::{Nearest, nearest};

    fn cube() -> Vec<[u8; 3]> {
        let mut palette: Vec<[u8; 3]> = (0..216u32)
            .map(|i| {
                [
                    (i / 36 * 51) as u8,
                    (i / 6 % 6 * 51) as u8,
                    (i % 6 * 51) as u8,
                ]
            })
            .collect();
        palette.extend((0..40u32).map(|i| [(i * 6) as u8; 3]));
        palette
    }

    fn blends() -> impl Iterator<Item = [u8; 3]> {
        let colors: [[u8; 3]; 4] = [[0, 0, 0], [255, 255, 255], [200, 40, 90], [12, 140, 230]];
        colors.into_iter().flat_map(move |under| {
            colors.into_iter().flat_map(move |over| {
                (0..=64u32).map(move |level| {
                    let coverage = (level * 255 + 32) / 64;
                    std::array::from_fn(|i| {
                        let (a, b) = (u32::from(under[i]), u32::from(over[i]));
                        ((a * (255 - coverage) + b * coverage + 127) / 255) as u8
                    })
                })
            })
        })
    }

    #[test]
    fn kept_answers_match_a_full_scan_until_the_palette_changes() {
        let mut kept = Nearest::default();
        let palette = cube();
        kept.adopt(&palette);
        for rgb in blends().chain(blends()) {
            assert_eq!(kept.index(rgb), nearest(&palette, rgb));
        }
        let reversed: Vec<_> = palette.iter().rev().copied().collect();
        kept.adopt(&reversed);
        for rgb in blends() {
            assert_eq!(kept.index(rgb), nearest(&reversed, rgb));
        }
    }

    #[test]
    fn ties_take_the_first_entry_and_an_empty_palette_has_none() {
        let palette = [[10, 10, 10], [30, 30, 30], [10, 10, 10]];
        assert_eq!(nearest(&palette, [20, 20, 20]), Some(0));
        assert_eq!(nearest(&[], [20, 20, 20]), None);
    }
}

//! Straight-alpha bitmap storage, path coverage and image sampling.
use super::*;

pub(super) fn execute(
    object: &Object,
    graphics: Graphics,
    operation: Op<'_>,
) -> Result<Reply, i32> {
    match operation {
        Op::Clip(rectangles) => {
            if rectangles.iter().flatten().any(|v| !v.is_finite()) {
                return Err(2);
            }
            if let Data::Graphics(g) = &mut *object.data.lock().map_err(|_| 1)? {
                g.clip = Some(rectangles);
            }
        }
        Op::Smoothing(value) | Op::TextHint(value) => {
            if !(0..=5).contains(&value) {
                return Err(2);
            }
            if let Data::Graphics(g) = &mut *object.data.lock().map_err(|_| 1)? {
                if matches!(operation, Op::Smoothing(_)) {
                    g.smoothing = value;
                } else {
                    g.text_hint = value;
                }
            }
        }
        Op::PixelOffset(value) => {
            if let Some(value) = value {
                if !(0..=4).contains(&value) {
                    return Err(2);
                }
                if let Data::Graphics(g) = &mut *object.data.lock().map_err(|_| 1)? {
                    g.pixel_offset = value;
                }
                return Ok(Reply::Count(value));
            }
            return Ok(Reply::Count(graphics.pixel_offset));
        }
        Op::Clear(color) => {
            let color = color.to_le_bytes();
            paint(&graphics, |_, _| Some((color, true)))?;
        }
        Op::FillPath(brush, path) => {
            let color = match &*brush.data.lock().map_err(|_| 1)? {
                Data::Solid(c) => c.to_le_bytes(),
                _ => return Err(2),
            };
            let path = match &*path.data.lock().map_err(|_| 1)? {
                Data::Path(p) => p.clone(),
                _ => return Err(2),
            };
            let (width, height) = dimensions(&graphics)?;
            let antialias = matches!(graphics.smoothing, 2 | 4 | 5);
            let offset = if matches!(graphics.pixel_offset, 2 | 4) {
                0.5
            } else {
                0.
            };
            let mask = if antialias {
                antialias_mask(&path, width, height, offset, graphics.smoothing == 5)
            } else {
                path.coverage(width, height, offset)
            };
            paint(&graphics, |x, y| {
                let coverage = mask[y as usize * width as usize + x as usize];
                let mut pixel = color;
                pixel[3] = ((u16::from(pixel[3]) * u16::from(coverage) + 127) / 255) as u8;
                Some((pixel, false))
            })?;
        }
        Op::DrawImage(image, [dx, dy, dw, dh]) => {
            if [dx, dy, dw, dh].iter().any(|v| !v.is_finite()) {
                return Err(2);
            }
            let (width, height, pixels) = match &*image.data.lock().map_err(|_| 1)? {
                Data::Image {
                    width,
                    height,
                    pixels,
                } if image.kind == Kind::Image => (*width, *height, pixels.clone()),
                _ => return Err(2),
            };
            if dw == 0. || dh == 0. {
                return Ok(Reply::None);
            }
            paint(&graphics, |x, y| {
                let u = (x as f32 + 0.5 - dx) / dw;
                let v = (y as f32 + 0.5 - dy) / dh;
                if !(0. ..1.).contains(&u) || !(0. ..1.).contains(&v) {
                    return None;
                }
                Some((
                    sample(
                        &pixels,
                        width,
                        height,
                        u * width as f32 - 0.5,
                        v * height as f32 - 0.5,
                    ),
                    false,
                ))
            })?;
        }
        Op::DrawString { .. } | Op::Measure { .. } => return Err(6),
        _ => return Err(2),
    }
    Ok(Reply::None)
}
fn antialias_mask(path: &Path, width: u32, height: u32, center: f32, square: bool) -> Vec<u8> {
    // GDI+ specifies an 8x4 box filter, or 8x8 in AntiAlias8x8 mode.
    // Tile the zeno mask so a long, narrow image cannot multiply temporary
    // storage by the supersampling factor for its entire surface.
    let sy = if square { 8 } else { 4 };
    let mut output = vec![0; width as usize * height as usize];
    for top in (0..height).step_by(16) {
        for left in (0..width).step_by(256) {
            let w = (width - left).min(256);
            let h = (height - top).min(16);
            let pitch = w * 8;
            let mut samples = vec![0; pitch as usize * (h * sy) as usize];
            zeno::Mask::new(&path.commands)
                .style(path.fill())
                .size(pitch, h * sy)
                .transform(Some(zeno::Transform::scale(8., sy as f32)))
                .offset([
                    (0.5 - center - left as f32) * 8.,
                    (0.5 - center - top as f32) * sy as f32,
                ])
                .render_into(&mut samples, None);
            for y in 0..h {
                for x in 0..w {
                    let mut count = 0u32;
                    for py in 0..sy {
                        for px in 0..8 {
                            count += u32::from(
                                samples[((y * sy + py) * pitch + x * 8 + px) as usize] >= 128,
                            );
                        }
                    }
                    output[((top + y) * width + left + x) as usize] =
                        ((count * 255 + 4 * sy) / (8 * sy)) as u8;
                }
            }
        }
    }
    output
}
fn dimensions(graphics: &Graphics) -> Result<(u32, u32), i32> {
    match &*graphics.target.data.lock().map_err(|_| 1)? {
        Data::Image { width, height, .. } => Ok((*width, *height)),
        _ => Err(2),
    }
}
fn paint(
    graphics: &Graphics,
    mut source: impl FnMut(u32, u32) -> Option<([u8; 4], bool)>,
) -> Result<(), i32> {
    let canvas = graphics.target.kind == Kind::Canvas;
    let mut target = graphics.target.data.lock().map_err(|_| 1)?;
    let Data::Image {
        width,
        height,
        pixels,
    } = &mut *target
    else {
        return Err(2);
    };
    for y in 0..*height {
        for x in 0..*width {
            let visible = graphics.clip.as_ref().is_none_or(|rectangles| {
                rectangles.iter().any(|r| {
                    let px = x as f32 + 0.5;
                    let py = y as f32 + 0.5;
                    px >= r[0] && py >= r[1] && px < r[0] + r[2] && py < r[1] + r[3]
                })
            });
            if !visible {
                continue;
            }
            if let Some((mut color, replace)) = source(x, y) {
                let pixel = &mut pixels[(y as usize * *width as usize + x as usize) * 4..][..4];
                if !replace {
                    color = blend(color, (&*pixel).try_into().unwrap(), canvas);
                }
                if canvas {
                    color[3] = 0;
                }
                pixel.copy_from_slice(&color);
            }
        }
    }
    Ok(())
}
fn blend(source: [u8; 4], dest: [u8; 4], canvas: bool) -> [u8; 4] {
    let sa = u32::from(source[3]);
    let da = if canvas { 255 } else { u32::from(dest[3]) };
    let alpha = sa * 255 + da * (255 - sa);
    if alpha == 0 {
        return dest;
    }
    let mut result = [0; 4];
    for channel in 0..3 {
        result[channel] = ((u32::from(source[channel]) * sa * 255
            + u32::from(dest[channel]) * da * (255 - sa)
            + alpha / 2)
            / alpha) as u8;
    }
    result[3] = ((alpha + 127) / 255) as u8;
    result
}
fn sample(pixels: &[u8], width: u32, height: u32, x: f32, y: f32) -> [u8; 4] {
    let (left, top) = (x.floor() as i64, y.floor() as i64);
    let (fx, fy) = (x - x.floor(), y - y.floor());
    let mut sums = [0.; 4];
    for (ox, oy, weight) in [
        (0, 0, (1. - fx) * (1. - fy)),
        (1, 0, fx * (1. - fy)),
        (0, 1, (1. - fx) * fy),
        (1, 1, fx * fy),
    ] {
        let (px, py) = (left + ox, top + oy);
        if px < 0 || py < 0 || px >= i64::from(width) || py >= i64::from(height) {
            continue;
        }
        let pixel = &pixels[(py as usize * width as usize + px as usize) * 4..][..4];
        let alpha = f32::from(pixel[3]) * weight;
        for c in 0..3 {
            sums[c] += f32::from(pixel[c]) * alpha;
        }
        sums[3] += alpha;
    }
    if sums[3] == 0. {
        return [0; 4];
    }
    [
        (sums[0] / sums[3]).round() as u8,
        (sums[1] / sums[3]).round() as u8,
        (sums[2] / sums[3]).round() as u8,
        sums[3].round() as u8,
    ]
}

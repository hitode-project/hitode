//! Straight-alpha bitmap storage, path coverage and image sampling.
use super::*;

pub(super) fn execute(
    object: &Object,
    graphics: Graphics,
    operation: Op<'_>,
) -> Result<Reply, i32> {
    match operation {
        Op::World(value) => {
            if let Some(matrix) = value {
                if !matrix.invertible() {
                    return Err(2);
                }
                if let Data::Graphics(g) = &mut *object.data.lock().map_err(|_| 1)? {
                    g.world = matrix;
                }
                return Ok(Reply::None);
            }
            return Ok(Reply::Matrix(graphics.world));
        }
        Op::ComposeWorld(matrix, prepend) => {
            if !matrix.finite() {
                return Err(2);
            }
            let next = if prepend {
                matrix.multiply(graphics.world)
            } else {
                graphics.world.multiply(matrix)
            };
            if !next.invertible() {
                return Err(2);
            }
            if let Data::Graphics(g) = &mut *object.data.lock().map_err(|_| 1)? {
                g.world = next;
            }
        }
        Op::Clip(rectangles) => {
            if rectangles.iter().flatten().any(|v| !v.is_finite()) {
                return Err(2);
            }
            if let Data::Graphics(g) = &mut *object.data.lock().map_err(|_| 1)? {
                g.clip = Some(rectangles);
            }
        }
        Op::Smoothing(value) => return property(object, GraphicsProperty::Smoothing, Some(value)),
        Op::TextHint(value) => return property(object, GraphicsProperty::TextHint, Some(value)),
        Op::GraphicsProperty(kind, value) => return property(object, kind, value),
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
            paint(&graphics, |_, _| Some((color, true, 255)))?;
        }
        Op::FillPath(brush, path) => {
            let color = match &*brush.data.lock().map_err(|_| 1)? {
                Data::Solid(c) => c.to_le_bytes(),
                _ => return Err(2),
            };
            let path = match &*path.data.lock().map_err(|_| 1)? {
                Data::Path(p) => p.transformed(graphics.world),
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
                (coverage != 0).then_some((color, false, coverage))
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
            let Some(inverse) = graphics.world.inverse() else {
                return Ok(Reply::None);
            };
            paint(&graphics, |x, y| {
                let [px, py] = inverse.point([x as f32 + 0.5, y as f32 + 0.5], false);
                let u = (px - dx) / dw;
                let v = (py - dy) / dh;
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
                    255,
                ))
            })?;
        }
        Op::DrawString { .. } if graphics.compositing == 1 => return Err(2),
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
    mut source: impl FnMut(u32, u32) -> Option<([u8; 4], bool, u8)>,
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
            if let Some((mut color, replace, coverage)) = source(x, y) {
                let pixel = &mut pixels[(y as usize * *width as usize + x as usize) * 4..][..4];
                if !replace {
                    color = blend(
                        color,
                        (&*pixel).try_into().unwrap(),
                        coverage,
                        canvas,
                        graphics.compositing == 0,
                    );
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
fn blend(source: [u8; 4], dest: [u8; 4], coverage: u8, canvas: bool, over: bool) -> [u8; 4] {
    if !over && coverage == 255 {
        return source;
    }
    let scale = |value: u32, factor: u32| (value * factor + 127) / 255;
    let sa = u32::from(source[3]);
    let da = if canvas { 255 } else { u32::from(dest[3]) };
    let coverage = u32::from(coverage);
    let source_alpha = scale(sa, coverage);
    let remaining = if over {
        255 - source_alpha
    } else {
        255 - coverage
    };
    let alpha = source_alpha + scale(da, remaining);
    if alpha == 0 {
        return if over { dest } else { [0; 4] };
    }
    let mut result = [0; 4];
    for channel in 0..3 {
        let source = scale(scale(u32::from(source[channel]), sa), coverage);
        let dest = scale(scale(u32::from(dest[channel]), da), remaining);
        result[channel] = ((source + dest) * 255 + alpha / 2)
            .checked_div(alpha)
            .unwrap_or(0)
            .min(255) as u8;
    }
    result[3] = alpha.min(255) as u8;
    result
}
fn property(object: &Object, kind: GraphicsProperty, value: Option<i32>) -> Result<Reply, i32> {
    let mut data = object.data.lock().map_err(|_| 1)?;
    let Data::Graphics(graphics) = &mut *data else {
        return Err(2);
    };
    let property = match kind {
        GraphicsProperty::CompositingMode => &mut graphics.compositing,
        GraphicsProperty::Smoothing => &mut graphics.smoothing,
        GraphicsProperty::TextHint => &mut graphics.text_hint,
    };
    if let Some(value) = value {
        let maximum = if matches!(kind, GraphicsProperty::CompositingMode) {
            1
        } else {
            5
        };
        if !(0..=maximum).contains(&value) {
            return Err(2);
        }
        *property = match (kind, value) {
            (GraphicsProperty::Smoothing, 0 | 1) => 3,
            (GraphicsProperty::Smoothing, 2) => 4,
            _ => value,
        };
    }
    Ok(Reply::Count(*property))
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

//! Bounded GDI regions use the existing shared GDI object owner. Curved and
//! polygonal shapes are scan-converted per row into the same disjoint rectangles.
use super::*;
use adv_abi::types::RECT;
const LIMIT: usize = 4096;

pub(crate) fn rectangles<'a>(env: &'a Env<'_>, h: u32) -> Option<&'a Vec<[i32; 4]>> {
    match get(env, h)? {
        GdiObj::Region(r) => Some(r),
        _ => None,
    }
}
fn normalized(l: i32, t: i32, r: i32, b: i32) -> Vec<[i32; 4]> {
    let rect = [l.min(r), t.min(b), l.max(r), t.max(b)];
    if rect[0] == rect[2] || rect[1] == rect[3] {
        Vec::new()
    } else {
        vec![rect]
    }
}
pub(crate) fn complexity(rects: &[[i32; 4]]) -> i32 {
    match rects.len() {
        0 => 1,
        1 => 2,
        _ => 3,
    }
}
pub(crate) fn bounds(rects: &[[i32; 4]]) -> [i32; 4] {
    rects
        .iter()
        .copied()
        .reduce(|a, b| {
            [
                a[0].min(b[0]),
                a[1].min(b[1]),
                a[2].max(b[2]),
                a[3].max(b[3]),
            ]
        })
        .unwrap_or([0; 4])
}
#[win32(gdi32, name="CreateRectRgn", level=state)]
fn create(e: &mut Env<'_>, l: i32, t: i32, r: i32, b: i32) -> Handle {
    Handle(add(e, GdiObj::Region(normalized(l, t, r, b))))
}
#[win32(gdi32, name="CreateRectRgnIndirect", level=state)]
fn create_indirect(e: &mut Env<'_>, pointer: GuestPtr<RECT>) -> Handle {
    let Ok(r) = pointer.read(e.cpu) else {
        return Handle(0);
    };
    create(e, r.left, r.top, r.right, r.bottom)
}
#[win32(gdi32, name="SetRectRgn", level=state)]
fn set(e: &mut Env<'_>, h: Handle, l: i32, t: i32, r: i32, b: i32) -> i32 {
    let Some(GdiObj::Region(rects)) = get_mut(e, h.0) else {
        return 0;
    };
    *rects = normalized(l, t, r, b);
    1
}
#[win32(gdi32, name="GetRgnBox", level=state)]
fn bbox(e: &mut Env<'_>, h: Handle, output: GuestPtr<RECT>) -> i32 {
    let Some(r) = rectangles(e, h.0) else {
        return 0;
    };
    let kind = complexity(r);
    let [left, top, right, bottom] = bounds(r);
    output
        .write(
            e.cpu,
            RECT {
                left,
                top,
                right,
                bottom,
            },
        )
        .map_or(0, |_| kind)
}
#[win32(gdi32, name="ExtCreateRegion", level=state)]
fn external(e: &mut Env<'_>, transform: GuestPtr<()>, size: u32, data: GuestPtr<()>) -> Handle {
    if size < 32 || !e.cpu.accessible(data.addr(), size, Prot::READ) {
        return Handle(0);
    }
    let mut header = [0; 32];
    if e.cpu.read(data.addr(), &mut header).is_err() {
        return Handle(0);
    }
    let word = |at| u32::from_le_bytes(header[at..at + 4].try_into().unwrap());
    let count = word(8) as usize;
    if word(0) != 32 || word(4) != 1 || count > LIMIT || 32u64 + count as u64 * 16 > u64::from(size)
    {
        return Handle(0);
    }
    // XFORM eM11, eM12, eM21, eM22, eDx, eDy; identity when absent.
    let mut f = [1.0f32, 0.0, 0.0, 1.0, 0.0, 0.0];
    if !transform.is_null() {
        let mut bytes = [0; 24];
        if e.cpu.read(transform.addr(), &mut bytes).is_err() {
            return Handle(0);
        }
        for (value, b) in f.iter_mut().zip(bytes.as_chunks::<4>().0) {
            *value = f32::from_le_bytes(*b);
        }
        if f[1] != 0.0 || f[2] != 0.0 || !f.iter().all(|v| v.is_finite()) {
            adv_abi::warn_unavailable(
                e,
                "ExtCreateRegion",
                "rotating or shearing region transforms are unavailable",
            );
            crate::util::set_last_error(e, 50);
            return Handle(0);
        }
    }
    let map = |v: i32, scale: f32, offset: f32| {
        (f64::from(v) * f64::from(scale) + f64::from(offset)).round() as i32
    };
    let mut rects = Vec::with_capacity(count);
    let mut raw = Vec::with_capacity(count);
    for i in 0..count {
        let mut bytes = [0; 16];
        if e.cpu
            .read(data.addr() + 32 + i as u32 * 16, &mut bytes)
            .is_err()
        {
            return Handle(0);
        }
        let r: [i32; 4] = std::array::from_fn(|i| {
            i32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap())
        });
        if r[0] >= r[2] || r[1] >= r[3] || raw.iter().any(|a| intersects(*a, r)) {
            return Handle(0);
        }
        raw.push(r);
        let (l, rr) = (map(r[0], f[0], f[4]), map(r[2], f[0], f[4]));
        let (t, b) = (map(r[1], f[3], f[5]), map(r[3], f[3], f[5]));
        rects.extend(normalized(l, t, rr, b));
    }
    let Some(rects) = canonical(rects) else {
        adv_abi::warn_unavailable(
            e,
            "ExtCreateRegion",
            "canonical region exceeds the bounded rectangle limit",
        );
        return Handle(0);
    };
    Handle(add(e, GdiObj::Region(rects)))
}
#[win32(gdi32, name="GetRegionData", level=state)]
fn data(e: &mut Env<'_>, h: Handle, size: u32, output: GuestPtr<()>) -> u32 {
    let Some(r) = rectangles(e, h.0) else {
        return 0;
    };
    let length = 32 + r.len() * 16;
    if size == 0 {
        return length as u32;
    }
    if size < (length as u32) || !e.cpu.accessible(output.addr(), length as u32, Prot::WRITE) {
        return 0;
    }
    let mut bytes = Vec::with_capacity(length);
    for v in [32, 1, r.len() as u32, (r.len() * 16) as u32] {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    for v in bounds(r) {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    for rect in r {
        for v in rect {
            bytes.extend_from_slice(&v.to_le_bytes());
        }
    }
    e.cpu
        .write(output.addr(), &bytes)
        .map_or(0, |_| length as u32)
}
fn intersects(a: [i32; 4], b: [i32; 4]) -> bool {
    a[0] < b[2] && b[0] < a[2] && a[1] < b[3] && b[1] < a[3]
}
fn canonical(rects: Vec<[i32; 4]>) -> Option<Vec<[i32; 4]>> {
    let mut edges: Vec<i32> = rects.iter().flat_map(|r| [r[1], r[3]]).collect();
    edges.sort_unstable();
    edges.dedup();
    let mut result: Vec<[i32; 4]> = Vec::new();
    let mut previous = (0, 0);
    for ys in edges.windows(2) {
        let (top, bottom) = (ys[0], ys[1]);
        let mut spans: Vec<[i32; 2]> = rects
            .iter()
            .filter(|r| r[1] <= top && r[3] >= bottom)
            .map(|r| [r[0], r[2]])
            .collect();
        spans.sort_unstable();
        let mut merged: Vec<[i32; 2]> = Vec::new();
        for span in spans {
            if let Some(last) = merged.last_mut().filter(|last| last[1] >= span[0]) {
                last[1] = last[1].max(span[1]);
            } else {
                merged.push(span);
            }
        }
        let (start, count) = previous;
        if count != 0
            && count == merged.len()
            && merged.iter().enumerate().all(|(i, x)| {
                let r = result[start + i];
                r[0] == x[0] && r[2] == x[1] && r[3] == top
            })
        {
            for r in &mut result[start..start + count] {
                r[3] = bottom;
            }
        } else {
            previous = (result.len(), merged.len());
            result.extend(merged.into_iter().map(|x| [x[0], top, x[1], bottom]));
            if result.len() > LIMIT {
                return None;
            }
        }
    }
    Some(result)
}
#[win32(gdi32, name="PtInRegion", level=state)]
fn point(e: &mut Env<'_>, h: Handle, x: i32, y: i32) -> i32 {
    i32::from(rectangles(e, h.0).is_some_and(|r| {
        r.iter()
            .any(|a| x >= a[0] && x < a[2] && y >= a[1] && y < a[3])
    }))
}
#[win32(gdi32, name="RectInRegion", level=state)]
fn rect(e: &mut Env<'_>, h: Handle, p: GuestPtr<RECT>) -> i32 {
    let Ok(r) = p.read(e.cpu) else {
        return 0;
    };
    let r = normalized(r.left, r.top, r.right, r.bottom);
    i32::from(r.first().is_some_and(|r| {
        rectangles(e, h.0).is_some_and(|rs| rs.iter().any(|a| intersects(*a, *r)))
    }))
}
#[win32(gdi32, name="OffsetRgn", level=state)]
fn offset(e: &mut Env<'_>, h: Handle, x: i32, y: i32) -> i32 {
    let Some(rs) = rectangles(e, h.0) else {
        return 0;
    };
    let next: Option<Vec<[i32; 4]>> = rs
        .iter()
        .map(|r| {
            Some([
                r[0].checked_add(x)?,
                r[1].checked_add(y)?,
                r[2].checked_add(x)?,
                r[3].checked_add(y)?,
            ])
        })
        .collect();
    let Some(next) = next else {
        return 0;
    };
    let kind = complexity(&next);
    let Some(GdiObj::Region(rs)) = get_mut(e, h.0) else {
        return 0;
    };
    *rs = next;
    kind
}
/// Disjoint rectangles of an ellipse inscribed in `l..r` x `t..b`, sampled at
/// row centres.
pub(crate) fn ellipse(l: i32, t: i32, r: i32, b: i32) -> Vec<[i32; 4]> {
    let [l, t, r, b] = [l.min(r), t.min(b), l.max(r), t.max(b)];
    let (cx, cy) = (
        (f64::from(l) + f64::from(r)) / 2.0,
        (f64::from(t) + f64::from(b)) / 2.0,
    );
    let (rx, ry) = (
        (f64::from(r) - f64::from(l)) / 2.0,
        (f64::from(b) - f64::from(t)) / 2.0,
    );
    if rx <= 0.0 || ry <= 0.0 {
        return Vec::new();
    }
    rows(t, b, |y| {
        let dy = (f64::from(y) + 0.5 - cy) / ry;
        let half = rx * (1.0 - dy * dy).max(0.0).sqrt();
        Some(((cx - half).round() as i32, (cx + half).round() as i32))
    })
}

fn round_rect(l: i32, t: i32, r: i32, b: i32, w: i32, h: i32) -> Vec<[i32; 4]> {
    let [l, t, r, b] = [l.min(r), t.min(b), l.max(r), t.max(b)];
    let (rx, ry) = (
        f64::from(w.unsigned_abs().min((r - l) as u32)) / 2.0,
        f64::from(h.unsigned_abs().min((b - t) as u32)) / 2.0,
    );
    if rx <= 0.0 || ry <= 0.0 {
        return normalized(l, t, r, b);
    }
    rows(t, b, |y| {
        let y = f64::from(y) + 0.5;
        let dy = if y < f64::from(t) + ry {
            f64::from(t) + ry - y
        } else if y > f64::from(b) - ry {
            y - (f64::from(b) - ry)
        } else {
            0.0
        } / ry;
        let inset = (rx - rx * (1.0 - dy * dy).max(0.0).sqrt()).round() as i32;
        Some((l + inset, r - inset))
    })
}

/// Scan-converts a closed polygon; `winding` selects WINDING over ALTERNATE.
fn polygon(points: &[[i32; 2]], winding: bool) -> Vec<[i32; 4]> {
    let (Some(top), Some(bottom)) = (
        points.iter().map(|p| p[1]).min(),
        points.iter().map(|p| p[1]).max(),
    ) else {
        return Vec::new();
    };
    let mut rects = Vec::new();
    for y in top..bottom {
        let centre = f64::from(y) + 0.5;
        let mut crossings: Vec<(f64, i32)> = Vec::new();
        for (i, a) in points.iter().enumerate() {
            let b = points[(i + 1) % points.len()];
            let (ay, by) = (f64::from(a[1]), f64::from(b[1]));
            if (ay <= centre) != (by <= centre) {
                let x = f64::from(a[0]) + (centre - ay) * f64::from(b[0] - a[0]) / (by - ay);
                crossings.push((x, if by > ay { 1 } else { -1 }));
            }
        }
        crossings.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut count = 0;
        for pair in crossings.windows(2) {
            count += pair[0].1;
            let inside = if winding { count != 0 } else { count % 2 != 0 };
            // Pixels whose centres lie inside, by the top-left fill convention.
            let (a, z) = (
                (pair[0].0 - 0.5).ceil() as i32,
                (pair[1].0 - 0.5).ceil() as i32,
            );
            if inside && a < z {
                rects.push([a, y, z, y + 1]);
            }
        }
    }
    canonical(rects).unwrap_or_default()
}

fn rows(t: i32, b: i32, span: impl Fn(i32) -> Option<(i32, i32)>) -> Vec<[i32; 4]> {
    let rects = (t..b)
        .filter_map(|y| {
            span(y)
                .filter(|(a, z)| a < z)
                .map(|(a, z)| [a, y, z, y + 1])
        })
        .collect();
    canonical(rects).unwrap_or_default()
}

/// RGN_AND, RGN_OR, RGN_XOR and RGN_DIFF over two disjoint rectangle sets.
fn combine(a: &[[i32; 4]], b: &[[i32; 4]], mode: u32) -> Option<Vec<[i32; 4]>> {
    let mut edges: Vec<i32> = a.iter().chain(b).flat_map(|r| [r[1], r[3]]).collect();
    edges.sort_unstable();
    edges.dedup();
    let spans = |rects: &[[i32; 4]], y: i32| {
        let mut xs: Vec<(i32, i32)> = rects
            .iter()
            .filter(|r| r[1] <= y && y < r[3])
            .map(|r| (r[0], r[2]))
            .collect();
        xs.sort_unstable();
        xs
    };
    let mut rects = Vec::new();
    for band in edges.windows(2) {
        let (sa, sb) = (spans(a, band[0]), spans(b, band[0]));
        let mut xs: Vec<i32> = sa.iter().chain(&sb).flat_map(|s| [s.0, s.1]).collect();
        xs.sort_unstable();
        xs.dedup();
        let inside = |spans: &[(i32, i32)], x: i32| spans.iter().any(|s| s.0 <= x && x < s.1);
        for pair in xs.windows(2) {
            let (ia, ib) = (inside(&sa, pair[0]), inside(&sb, pair[0]));
            let keep = match mode {
                1 => ia && ib,
                2 => ia || ib,
                3 => ia != ib,
                _ => ia && !ib,
            };
            if keep {
                rects.push([pair[0], band[0], pair[1], band[1]]);
            }
        }
    }
    canonical(rects)
}

/// The band of `rects` within `w` columns or `h` rows of its outside.
pub(crate) fn frame(rects: &[[i32; 4]], w: i32, h: i32) -> Vec<[i32; 4]> {
    combine(rects, &erode(rects, w, h), 4).unwrap_or_default()
}

fn erode(rects: &[[i32; 4]], w: i32, h: i32) -> Vec<[i32; 4]> {
    let shifted = |dx: i32, dy: i32| -> Vec<[i32; 4]> {
        rects
            .iter()
            .map(|r| [r[0] + dx, r[1] + dy, r[2] + dx, r[3] + dy])
            .collect()
    };
    let mut inner = rects.to_vec();
    for (dx, dy) in [(w, 0), (-w, 0), (0, h), (0, -h)] {
        inner = combine(&inner, &shifted(dx, dy), 1).unwrap_or_default();
    }
    inner
}

fn shape(e: &mut Env<'_>, rects: Vec<[i32; 4]>) -> Handle {
    if rects.len() > LIMIT {
        adv_abi::warn_unavailable(
            e,
            "gdi32.dll!CreateRgn",
            "region exceeds the bounded rectangle limit",
        );
        return Handle(0);
    }
    Handle(add(e, GdiObj::Region(rects)))
}
#[win32(gdi32, name="CreateEllipticRgn", level=state)]
fn create_elliptic(e: &mut Env<'_>, l: i32, t: i32, r: i32, b: i32) -> Handle {
    shape(e, ellipse(l, t, r, b))
}
#[win32(gdi32, name="CreateEllipticRgnIndirect", level=state)]
fn create_elliptic_indirect(e: &mut Env<'_>, pointer: GuestPtr<RECT>) -> Handle {
    let Ok(r) = pointer.read(e.cpu) else {
        return Handle(0);
    };
    create_elliptic(e, r.left, r.top, r.right, r.bottom)
}
#[win32(gdi32, name="CreateRoundRectRgn", level=state)]
fn create_round(e: &mut Env<'_>, l: i32, t: i32, r: i32, b: i32, w: i32, h: i32) -> Handle {
    shape(e, round_rect(l, t, r, b, w, h))
}
#[win32(gdi32, name="CreatePolygonRgn", level=state)]
fn create_polygon(e: &mut Env<'_>, points: GuestPtr<()>, count: i32, mode: i32) -> Handle {
    if !(2..=LIMIT as i32).contains(&count) || !matches!(mode, 1 | 2) {
        return Handle(0);
    }
    let mut bytes = vec![0; count as usize * 8];
    if e.cpu.read(points.addr(), &mut bytes).is_err() {
        return Handle(0);
    }
    let points: Vec<[i32; 2]> = bytes
        .as_chunks::<8>()
        .0
        .iter()
        .map(|p| {
            [
                i32::from_le_bytes(p[..4].try_into().unwrap()),
                i32::from_le_bytes(p[4..].try_into().unwrap()),
            ]
        })
        .collect();
    shape(e, polygon(&points, mode == 2))
}
#[win32(gdi32, name="CombineRgn", level=state)]
fn combine_regions(e: &mut Env<'_>, target: Handle, a: Handle, b: Handle, mode: i32) -> i32 {
    let Some(first) = rectangles(e, a.0).cloned() else {
        return 0;
    };
    let next = match mode {
        5 => Some(first),
        1..=4 => {
            let Some(second) = rectangles(e, b.0) else {
                return 0;
            };
            combine(&first, second, mode as u32)
        }
        _ => return 0,
    };
    let Some(next) = next else {
        adv_abi::warn_unavailable(
            e,
            "gdi32.dll!CombineRgn",
            "region exceeds the bounded rectangle limit",
        );
        return 0;
    };
    let kind = complexity(&next);
    let Some(GdiObj::Region(rects)) = get_mut(e, target.0) else {
        return 0;
    };
    *rects = next;
    kind
}
pub(super) fn register(r: &mut Registry) {
    for api in [
        create_elliptic_ABI,
        create_elliptic_indirect_ABI,
        create_round_ABI,
        create_polygon_ABI,
        combine_regions_ABI,
        create_ABI,
        create_indirect_ABI,
        set_ABI,
        bbox_ABI,
        external_ABI,
        data_ABI,
        point_ABI,
        rect_ABI,
        offset_ABI,
    ] {
        r.register(api);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(rects: &[[i32; 4]]) -> i64 {
        rects
            .iter()
            .map(|r| i64::from(r[2] - r[0]) * i64::from(r[3] - r[1]))
            .sum()
    }

    fn inside(rects: &[[i32; 4]], x: i32, y: i32) -> bool {
        rects
            .iter()
            .any(|r| x >= r[0] && x < r[2] && y >= r[1] && y < r[3])
    }

    // spec: WIN32-GDI-2
    #[test]
    fn curved_and_polygonal_shapes_become_disjoint_bounded_rectangles() {
        let circle = ellipse(0, 0, 100, 100);
        assert!(inside(&circle, 50, 50) && !inside(&circle, 2, 2) && !inside(&circle, 97, 97));
        assert_eq!(bounds(&circle), [0, 0, 100, 100]);
        let expected = std::f64::consts::PI * 2500.0;
        assert!((area(&circle) as f64 - expected).abs() < expected * 0.02);
        let rounded = round_rect(0, 0, 40, 20, 10, 10);
        assert!(inside(&rounded, 20, 0) && !inside(&rounded, 0, 0) && inside(&rounded, 0, 10));
        // Row y keeps the 9 - y pixel centres left of the hypotenuse.
        let triangle = polygon(&[[0, 0], [10, 0], [0, 10]], false);
        assert_eq!(area(&triangle), 45);
        // A self-overlapping star keeps its centre only under WINDING.
        let star = [[5, 0], [8, 10], [0, 4], [10, 4], [2, 10]];
        assert!(!inside(&polygon(&star, false), 5, 5));
        assert!(inside(&polygon(&star, true), 5, 5));
        for shape in [&circle, &rounded, &triangle] {
            for (i, a) in shape.iter().enumerate() {
                assert!(shape[i + 1..].iter().all(|b| !intersects(*a, *b)));
            }
        }
    }

    // spec: WIN32-GDI-2
    #[test]
    fn combinations_and_frames_follow_set_algebra() {
        let a = vec![[0, 0, 10, 10]];
        let b = vec![[5, 5, 15, 15]];
        assert_eq!(area(&combine(&a, &b, 1).unwrap()), 25);
        assert_eq!(area(&combine(&a, &b, 2).unwrap()), 175);
        assert_eq!(area(&combine(&a, &b, 3).unwrap()), 150);
        assert_eq!(area(&combine(&a, &b, 4).unwrap()), 75);
        let border = frame(&a, 2, 1);
        assert_eq!(area(&border), 100 - 6 * 8);
        assert!(inside(&border, 0, 5) && inside(&border, 5, 0) && !inside(&border, 5, 5));
    }
}

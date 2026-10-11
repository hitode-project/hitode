//! Bounded rectangular GDI regions use the existing shared GDI object owner.
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
    if !transform.is_null() {
        let mut bytes = [0; 24];
        if e.cpu.read(transform.addr(), &mut bytes).is_err() {
            return Handle(0);
        }
        let f: Vec<f32> = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
            .collect();
        if f != [1.0, 0.0, 0.0, 1.0, 0.0, 0.0] {
            adv_abi::warn_unavailable(
                e,
                "ExtCreateRegion",
                "non-identity region transforms are unavailable",
            );
            crate::util::set_last_error(e, 50);
            return Handle(0);
        }
    }
    let mut rects = Vec::with_capacity(count);
    for i in 0..count {
        let mut bytes = [0; 16];
        if e.cpu
            .read(data.addr() + 32 + i as u32 * 16, &mut bytes)
            .is_err()
        {
            return Handle(0);
        }
        let r = std::array::from_fn(|i| {
            i32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap())
        });
        if r[0] >= r[2] || r[1] >= r[3] || rects.iter().any(|a| intersects(*a, r)) {
            return Handle(0);
        }
        rects.push(r);
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
pub(super) fn register(r: &mut Registry) {
    for api in [
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

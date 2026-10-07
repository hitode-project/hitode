//! Exact, bounded client update rectangles belong to the existing Window owner.
//! The bounding box is query output only; painting never replaces a union by it.
use super::window::Window;
use crate::{Win32, util::set_last_error};
use adv_abi::{Env, GuestPtr, Handle, Outcome, Registry, types::RECT, win32};
use adv_cpu::backend::Prot;

pub(crate) const MAX_RECTS: usize = 64;
#[derive(Default)]
pub(crate) struct UpdateRegion {
    rects: Vec<[i32; 4]>,
    erase: bool,
    unerased: bool,
    erase_generation: u64,
}
#[derive(Clone)]
pub(crate) struct Snapshot {
    pub rects: Vec<[i32; 4]>,
    pub erase: bool,
    pub unerased: bool,
    generation: u64,
}
impl Snapshot {
    pub fn bounds(&self) -> [i32; 4] {
        self.rects
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
}
fn clipped(w: &Window, r: Option<[i32; 4]>) -> Option<[i32; 4]> {
    let (width, height) = w.client_size();
    let [l, t, r, b] = r.unwrap_or([0, 0, width, height]);
    let r = [
        l.min(r).max(0),
        t.min(b).max(0),
        l.max(r).min(width),
        t.max(b).min(height),
    ];
    (r[0] < r[2] && r[1] < r[3]).then_some(r)
}
fn subtract(a: [i32; 4], b: [i32; 4], out: &mut Vec<[i32; 4]>) {
    let x = [
        a[0].max(b[0]),
        a[1].max(b[1]),
        a[2].min(b[2]),
        a[3].min(b[3]),
    ];
    if x[0] >= x[2] || x[1] >= x[3] {
        out.push(a);
        return;
    }
    for r in [
        [a[0], a[1], a[2], x[1]],
        [a[0], x[3], a[2], a[3]],
        [a[0], x[1], x[0], x[3]],
        [x[2], x[1], a[2], x[3]],
    ] {
        if r[0] < r[2] && r[1] < r[3] {
            out.push(r);
        }
    }
}
fn merge(rects: &mut Vec<[i32; 4]>) {
    loop {
        let mut joined = false;
        'pairs: for i in 0..rects.len() {
            for j in i + 1..rects.len() {
                let (a, b) = (rects[i], rects[j]);
                let horizontal = a[1] == b[1] && a[3] == b[3] && (a[2] == b[0] || b[2] == a[0]);
                let vertical = a[0] == b[0] && a[2] == b[2] && (a[3] == b[1] || b[3] == a[1]);
                if horizontal || vertical {
                    rects[i] = [
                        a[0].min(b[0]),
                        a[1].min(b[1]),
                        a[2].max(b[2]),
                        a[3].max(b[3]),
                    ];
                    rects.remove(j);
                    joined = true;
                    break 'pairs;
                }
            }
        }
        if !joined {
            break;
        }
    }
}
pub(crate) fn snapshot(w: &Window) -> Snapshot {
    let mut rects = w
        .control
        .update
        .rects
        .iter()
        .filter_map(|&r| clipped(w, Some(r)))
        .collect::<Vec<_>>();
    // Legacy external fixtures can still initialize the public Boolean. Every
    // production dirty producer now uses invalidate_client or this same owner.
    if w.needs_paint && rects.is_empty() && w.control.update.rects.is_empty() {
        rects.extend(clipped(w, None));
    }
    Snapshot {
        rects,
        erase: w.control.update.erase,
        unerased: w.control.update.unerased,
        generation: w.control.update.erase_generation,
    }
}
pub(crate) fn invalidate(w: &mut Window, rectangle: Option<[i32; 4]>, erase: bool) -> bool {
    let Some(r) = clipped(w, rectangle) else {
        return true;
    };
    let mut next = snapshot(w).rects;
    let mut fragments = vec![r];
    for &old in &next {
        let mut remainder = Vec::new();
        for r in fragments {
            subtract(r, old, &mut remainder);
        }
        if remainder.len() > MAX_RECTS {
            return false;
        }
        fragments = remainder;
    }
    next.extend(fragments);
    merge(&mut next);
    if next.len() > MAX_RECTS {
        return false;
    }
    w.control.update.rects = next;
    w.needs_paint = true;
    if erase {
        w.control.update.erase = true;
        w.control.update.unerased = true;
        w.control.update.erase_generation = w.control.update.erase_generation.wrapping_add(1);
    }
    true
}
pub(crate) fn invalidate_client(w: &mut Window, erase: bool) {
    w.control.update.rects.clear();
    w.control.update.rects.extend(clipped(w, None));
    w.needs_paint = !w.control.update.rects.is_empty();
    if erase && w.needs_paint {
        w.control.update.erase = true;
        w.control.update.unerased = true;
        w.control.update.erase_generation = w.control.update.erase_generation.wrapping_add(1);
    }
}
pub(crate) fn clear(w: &mut Window) {
    w.control.update.rects.clear();
    w.control.update.erase = false;
    w.needs_paint = false;
}
fn validate(w: &mut Window, rectangle: Option<[i32; 4]>) -> bool {
    if rectangle.is_none() {
        clear(w);
        return true;
    }
    let Some(r) = clipped(w, rectangle) else {
        return true;
    };
    let mut next = Vec::new();
    for old in snapshot(w).rects {
        subtract(old, r, &mut next);
    }
    merge(&mut next);
    if next.len() > MAX_RECTS {
        return false;
    }
    w.control.update.rects = next;
    w.needs_paint = !w.control.update.rects.is_empty();
    if !w.needs_paint {
        w.control.update.erase = false;
    }
    true
}
pub(crate) fn erased(env: &mut Env<'_>, hwnd: u32, old: &Snapshot, handled: bool) {
    if !handled {
        return;
    }
    if let Some(w) = env
        .ext
        .get::<Win32>()
        .user
        .windows
        .get_mut(&mut env.sys.objects, &hwnd)
        && w.control.update.erase_generation == old.generation
    {
        w.control.update.erase = false;
        w.control.update.unerased = false;
    }
}
fn rectangle(env: &Env<'_>, input: GuestPtr<RECT>) -> Result<Option<[i32; 4]>, ()> {
    if input.is_null() {
        return Ok(None);
    }
    adv_abi::guest::check_range(env.cpu, input.addr(), 16, Prot::READ).map_err(|_| ())?;
    let r = input.read(env.cpu).map_err(|_| ())?;
    Ok(Some([r.left, r.top, r.right, r.bottom]))
}
fn failure(env: &mut Env<'_>, error: u32) -> Outcome {
    set_last_error(env, error);
    Outcome::Return(0)
}
fn capacity(env: &mut Env<'_>, api: &str) -> Outcome {
    adv_abi::warn_unavailable(
        env,
        api,
        "exact update region exceeds 64 rectangles; prior region is unchanged",
    );
    failure(env, 50)
}
#[win32(user32, name="InvalidateRect", level=state)]
fn invalidate_rect(env: &mut Env<'_>, hwnd: Handle, input: GuestPtr<RECT>, erase: i32) -> Outcome {
    let Ok(r) = rectangle(env, input) else {
        return failure(env, 87);
    };
    if hwnd.0 == 0 {
        // Existing virtual-desktop profile invalidates this process's windows.
        // Cross-process nonclient/background broadcasts are unavailable.
        let windows = &env.ext.get::<Win32>().user.windows;
        windows.for_each_mut(&mut env.sys.objects, |w| w.invalidate_client(erase != 0));
        return Outcome::Return(1);
    }
    let Some(w) = env
        .ext
        .get::<Win32>()
        .user
        .windows
        .get_mut(&mut env.sys.objects, &hwnd.0)
    else {
        return failure(env, 1400);
    };
    let thread = w.thread;
    if !invalidate(w, r, erase != 0) {
        return capacity(env, "user32.dll!InvalidateRect");
    }
    super::message::queue_status_for(env, thread);
    Outcome::Return(1)
}
#[win32(user32, name="ValidateRect", level=state)]
fn validate_rect(env: &mut Env<'_>, hwnd: Handle, input: GuestPtr<RECT>) -> Outcome {
    let Ok(r) = rectangle(env, input) else {
        return failure(env, 87);
    };
    let Some(w) = env
        .ext
        .get::<Win32>()
        .user
        .windows
        .get_mut(&mut env.sys.objects, &hwnd.0)
    else {
        return failure(env, 1400);
    };
    let thread = w.thread;
    if !validate(w, r) {
        return capacity(env, "user32.dll!ValidateRect");
    }
    super::message::queue_status_for(env, thread);
    Outcome::Return(1)
}
#[win32(user32, name="GetUpdateRect", level=state)]
fn get_update_rect(env: &mut Env<'_>, hwnd: Handle, output: GuestPtr<RECT>, erase: i32) -> Outcome {
    let Some(w) = env
        .ext
        .get::<Win32>()
        .user
        .windows
        .get(&env.sys.objects, &hwnd.0)
    else {
        return failure(env, 1400);
    };
    let old = snapshot(w);
    if !output.is_null() {
        if adv_abi::guest::check_range(env.cpu, output.addr(), 16, Prot::WRITE).is_err() {
            return failure(env, 87);
        }
        let [left, top, right, bottom] = old.bounds();
        if output
            .write(
                env.cpu,
                RECT {
                    left,
                    top,
                    right,
                    bottom,
                },
            )
            .is_err()
        {
            return failure(env, 87);
        }
    }
    let pending = !old.rects.is_empty();
    if erase != 0 && pending && old.erase {
        return crate::gdi32::raster::front::erase_update(env, hwnd.0, old, move |_, _| {
            Outcome::Return(1)
        });
    }
    Outcome::Return(u32::from(pending))
}
pub(super) fn register(registry: &mut Registry) {
    registry.register(invalidate_rect_ABI);
    registry.register(validate_rect_ABI);
    registry.register(get_update_rect_ABI);
}

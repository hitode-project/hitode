//! Application clipping borrows the existing DC and CPU surface; no pixel owner.
use crate::{Win32, util::set_last_error};
use adv_abi::{
    Env, GuestPtr, Handle, Outcome, Registry,
    types::{POINT, RECT},
    win32,
};
use adv_cpu::backend::Prot;
use adv_device::surface::{Surface, SurfaceMemory, SurfaceStore};
use adv_os::gdi::{DcClip, GdiObj};
use std::borrow::Cow;

pub(super) fn register(reg: &mut Registry) {
    reg.register(exclude_ABI);
    reg.register(intersect_ABI);
    reg.register(rect_visible_ABI);
    reg.register(window_origin_ABI);
    reg.register(set_window_origin_ABI);
    reg.register(set_viewport_origin_ABI);
}
fn unsupported(env: &mut Env<'_>, reason: &str) -> Outcome {
    adv_abi::warn_unavailable(env, "gdi32.dll!ExcludeClipRect", reason);
    set_last_error(env, 50);
    Outcome::Return(0)
}
#[win32(gdi32, name="ExcludeClipRect", level=state)]
fn exclude(env: &mut Env<'_>, dc: Handle, left: i32, top: i32, right: i32, bottom: i32) -> Outcome {
    let gdi = env.ext.get::<Win32>().gdi.clone();
    let Some(GdiObj::Dc {
        selected,
        modes,
        clip,
        ..
    }) = gdi.get(&env.sys.objects, dc.0)
    else {
        set_last_error(env, 6);
        return Outcome::Return(0);
    };
    if modes.map != 1 {
        return unsupported(env, "only MM_TEXT clipping is available");
    }
    let Some(surface) = env
        .ext
        .try_get::<SurfaceStore>()
        .and_then(|s| s.get(selected[3]))
    else {
        return unsupported(env, "the DC has no shared raster surface");
    };
    if super::front::provider(env.ext).is_none() || surface.pixels == 0 {
        return unsupported(env, "the DC has no CPU raster provider");
    }
    let (w, h) = (surface.width, surface.height);
    let (Some([left, top]), Some([right, bottom])) = (
        modes.device_point(left, top),
        modes.device_point(right, bottom),
    ) else {
        return unsupported(
            env,
            "the translated clipping coordinates exceed the supported device range",
        );
    };
    let mut next = clip.clone();
    if !next.exclude([left, top, right, bottom]) {
        return unsupported(
            env,
            "the bounded application clipping capacity is exhausted",
        );
    }
    // Native ExcludeClipRect reports COMPLEXREGION for the application region,
    // even when its intersection with the bitmap is a single rectangle.
    let result = if next.visible(w, h) { 3 } else { 1 };
    if let Some(GdiObj::Dc { clip, .. }) = gdi.get_mut(&mut env.sys.objects, dc.0) {
        *clip = next;
    }
    Outcome::Return(result)
}

pub(crate) fn snapshot(env: &Env<'_>, dc: u32) -> DcClip {
    match env.ext.get::<Win32>().gdi.get(&env.sys.objects, dc) {
        Some(GdiObj::Dc { clip, .. }) => clip.clone(),
        _ => DcClip::default(),
    }
}

#[win32(gdi32,name="IntersectClipRect",level=state)]
fn intersect(
    env: &mut Env<'_>,
    dc: Handle,
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
) -> Outcome {
    let unavailable = |env: &mut Env<'_>, reason: &str| {
        adv_abi::warn_unavailable(env, "gdi32.dll!IntersectClipRect", reason);
        set_last_error(env, 50);
        Outcome::Return(0)
    };
    let gdi = env.ext.get::<Win32>().gdi.clone();
    let Some(GdiObj::Dc {
        selected,
        modes,
        clip,
        ..
    }) = gdi.get(&env.sys.objects, dc.0)
    else {
        set_last_error(env, 6);
        return Outcome::Return(0);
    };
    if modes.map != 1 {
        return unavailable(env, "only MM_TEXT clipping is available");
    }
    let Some(surface) = env
        .ext
        .try_get::<SurfaceStore>()
        .and_then(|s| s.get(selected[3]))
    else {
        return unavailable(env, "the DC has no shared raster surface");
    };
    if super::front::provider(env.ext).is_none() || surface.pixels == 0 {
        return unavailable(env, "the DC has no CPU raster provider");
    }
    let (Some([left, top]), Some([right, bottom])) = (
        modes.device_point(left, top),
        modes.device_point(right, bottom),
    ) else {
        return unavailable(
            env,
            "the translated clipping coordinates exceed the supported device range",
        );
    };
    let mut next = clip.clone();
    next.intersect([left, top, right, bottom]);
    let Some(GdiObj::Dc { clip, .. }) = gdi.get_mut(&mut env.sys.objects, dc.0) else {
        unreachable!("checked DC")
    };
    *clip = next;
    // The tested Windows memory-DC profile returns COMPLEXREGION here even for
    // simple/empty visible intersections. GetClipBox's result is distinct;
    // actual visibility and pixel writes always use the real updated mask.
    Outcome::Return(3)
}

#[win32(gdi32, name="GetWindowOrgEx", level=state)]
fn window_origin(env: &mut Env<'_>, dc: Handle, output: GuestPtr<POINT>) -> Outcome {
    let Some(GdiObj::Dc { modes, .. }) = env.ext.get::<Win32>().gdi.get(&env.sys.objects, dc.0)
    else {
        // Native invalid-HDC query leaves both the POINT and LastError alone.
        return Outcome::Return(0);
    };
    if modes.map != 1 {
        adv_abi::warn_unavailable(
            env,
            "gdi32.dll!GetWindowOrgEx",
            "only MM_TEXT window origins are available",
        );
        set_last_error(env, 50);
        return Outcome::Return(0);
    }
    let [x, y] = modes.window_origin;
    if adv_abi::guest::check_range(env.cpu, output.addr(), 8, Prot::WRITE).is_err()
        || output.write(env.cpu, POINT { x, y }).is_err()
    {
        set_last_error(env, 87);
        return Outcome::Return(0);
    }
    Outcome::Return(1)
}

#[win32(gdi32,name="SetWindowOrgEx",level=state)]
fn set_window_origin(
    env: &mut Env<'_>,
    dc: Handle,
    x: i32,
    y: i32,
    old: GuestPtr<POINT>,
) -> Outcome {
    set_origin(env, dc, [x, y], old, false)
}
#[win32(gdi32,name="SetViewportOrgEx",level=state)]
fn set_viewport_origin(
    env: &mut Env<'_>,
    dc: Handle,
    x: i32,
    y: i32,
    old: GuestPtr<POINT>,
) -> Outcome {
    set_origin(env, dc, [x, y], old, true)
}
fn set_origin(
    env: &mut Env<'_>,
    dc: Handle,
    next: [i32; 2],
    output: GuestPtr<POINT>,
    viewport: bool,
) -> Outcome {
    let api = if viewport {
        "gdi32.dll!SetViewportOrgEx"
    } else {
        "gdi32.dll!SetWindowOrgEx"
    };
    let gdi = env.ext.get::<Win32>().gdi.clone();
    let Some(GdiObj::Dc { modes, .. }) = gdi.get(&env.sys.objects, dc.0) else {
        set_last_error(env, 6);
        return Outcome::Return(0);
    };
    if modes.map != 1 {
        adv_abi::warn_unavailable(env, api, "only MM_TEXT origin translation is available");
        set_last_error(env, 50);
        return Outcome::Return(0);
    }
    let [x, y] = if viewport {
        modes.viewport_origin
    } else {
        modes.window_origin
    };
    // Optional previous POINT is completely preflighted before either output or
    // DC state changes. The origins never relocate the already stored device clip.
    if !output.is_null()
        && (adv_abi::guest::check_range(env.cpu, output.addr(), 8, Prot::WRITE).is_err()
            || output.write(env.cpu, POINT { x, y }).is_err())
    {
        set_last_error(env, 87);
        return Outcome::Return(0);
    }
    let Some(GdiObj::Dc { modes, .. }) = gdi.get_mut(&mut env.sys.objects, dc.0) else {
        unreachable!("checked DC")
    };
    if viewport {
        modes.viewport_origin = next;
    } else {
        modes.window_origin = next;
    }
    Outcome::Return(1)
}

#[win32(gdi32, name="RectVisible", level=state)]
fn rect_visible(env: &mut Env<'_>, dc: Handle, rectangle: GuestPtr<RECT>) -> Outcome {
    let Some(GdiObj::Dc {
        selected,
        modes,
        clip,
        ..
    }) = env.ext.get::<Win32>().gdi.get(&env.sys.objects, dc.0)
    else {
        set_last_error(env, 6);
        return Outcome::Return(0);
    };
    if modes.map != 1 {
        adv_abi::warn_unavailable(
            env,
            "gdi32.dll!RectVisible",
            "only MM_TEXT visibility queries are available",
        );
        set_last_error(env, 50);
        return Outcome::Return(0);
    }
    let (target, clip, modes) = (selected[3], clip.clone(), *modes);
    if adv_abi::guest::check_range(env.cpu, rectangle.addr(), 16, Prot::READ).is_err() {
        set_last_error(env, 87);
        return Outcome::Return(0);
    }
    let Ok(r) = rectangle.read(env.cpu) else {
        set_last_error(env, 87);
        return Outcome::Return(0);
    };
    let Some(surface) = env
        .ext
        .try_get::<SurfaceStore>()
        .and_then(|s| s.get(target))
    else {
        adv_abi::warn_unavailable(
            env,
            "gdi32.dll!RectVisible",
            "the DC has no shared surface geometry",
        );
        set_last_error(env, 50);
        return Outcome::Return(0);
    };
    // This query needs owned geometry, not CPU pixel access or host window state.
    let [left, top] = modes.to_device(i64::from(r.left), i64::from(r.top));
    let [right, bottom] = modes.to_device(i64::from(r.right), i64::from(r.bottom));
    Outcome::Return(u32::from(clip.intersects_device(
        [left, top, right, bottom],
        surface.width,
        surface.height,
    )))
}

/// Filter destination writes only. Reads retain the original transfer sampling
/// phase and self-copy snapshot; the selected bitmap remains the sole pixel owner.
pub(crate) struct ClippedMemory<'a> {
    memory: &'a mut dyn SurfaceMemory,
    surface: Surface,
    clip: &'a DcClip,
}
impl<'a> ClippedMemory<'a> {
    pub(crate) fn new(
        memory: &'a mut dyn SurfaceMemory,
        surface: Surface,
        clip: &'a DcClip,
    ) -> Self {
        Self {
            memory,
            surface,
            clip,
        }
    }
    fn visit(
        surface: &Surface,
        clip: &DcClip,
        addr: u32,
        len: usize,
        mut f: impl FnMut(usize, usize) -> Result<(), String>,
    ) -> Result<(), String> {
        if clip.is_default() {
            return f(0, len);
        }
        let start = u64::from(addr);
        let end = start
            .checked_add(len as u64)
            .filter(|&e| e <= u64::from(u32::MAX) + 1)
            .ok_or("clipped write overflows")?;
        let base = u64::from(surface.pixels);
        let limit = base + u64::from(surface.pitch) * u64::from(surface.height);
        if end <= base || start >= limit {
            return f(0, len);
        }
        if start < base {
            f(0, (base - start) as usize)?;
        }
        let first = start.max(base);
        let last = end.min(limit);
        let bytes = u64::from(surface.format.bytes());
        let pitch = u64::from(surface.pitch);
        let a = (first - base) / pitch;
        let z = (last - base).div_ceil(pitch);
        for row in a..z {
            let y = if surface.bottom_up {
                u64::from(surface.height) - row - 1
            } else {
                row
            };
            let rowbase = base + row * pitch;
            for (l, r) in clip.spans(y as i32, surface.width) {
                let l = (rowbase + u64::from(l) * bytes).max(first);
                let r = (rowbase + u64::from(r) * bytes).min(last);
                if l < r {
                    f((l - start) as usize, (r - l) as usize)?;
                }
            }
        }
        if end > limit {
            f((limit - start) as usize, (end - limit) as usize)?;
        }
        Ok(())
    }
}
impl SurfaceMemory for ClippedMemory<'_> {
    fn read(&self, addr: u32, len: usize) -> Result<Cow<'_, [u8]>, String> {
        self.memory.read(addr, len)
    }
    fn writable(&self, addr: u32, len: usize) -> Result<(), String> {
        Self::visit(&self.surface, self.clip, addr, len, |o, n| {
            self.memory.writable(addr + o as u32, n)
        })
    }
    fn write(&mut self, addr: u32, data: &[u8]) -> Result<(), String> {
        Self::visit(&self.surface, self.clip, addr, data.len(), |o, n| {
            self.memory.write(addr + o as u32, &data[o..o + n])
        })
    }
    fn copy(&mut self, src: u32, dst: u32, len: usize) -> Result<(), String> {
        if self.clip.is_default() {
            return self.memory.copy(src, dst, len);
        }
        // Snapshot the row before splitting it, so overlapping aliases are memmove.
        let bytes = self.memory.read(src, len)?.into_owned();
        self.write(dst, &bytes)
    }
}

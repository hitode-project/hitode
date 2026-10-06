//! Application clipping borrows the existing DC and CPU surface; no pixel owner.
use crate::{Win32, util::set_last_error};
use adv_abi::{Env, Handle, Outcome, Registry, win32};
use adv_device::surface::{Surface, SurfaceMemory, SurfaceStore};
use adv_os::gdi::{DcClip, GdiObj};
use std::borrow::Cow;

pub(super) fn register(reg: &mut Registry) {
    reg.register(exclude_ABI);
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

//! Display modes of the software DirectDraw device, independent of surfaces.
use super::{ComState, Kind, State};
use adv_abi::{Env, GuestPtr, Outcome, win32};
use adv_cpu::backend::{Prot, Reg};
use adv_device::surface::{self, PixelFormat, SurfaceStore};

const INVALID: u32 = 0x80070057;
const INVALID_OBJECT: u32 = 0x88760082;
const HEIGHT: u32 = 2;
const WIDTH: u32 = 4;
const PIXEL_FORMAT: u32 = 0x1000;
const REFRESH: u32 = 0x40000;
type Descriptor = [u8; 124];

fn word(d: &Descriptor, offset: usize) -> u32 {
    u32::from_le_bytes(d[offset..offset + 4].try_into().unwrap())
}

fn matches(filter: &Descriptor, mode: &Descriptor) -> bool {
    let flags = word(filter, 4);
    for (flag, offset) in [(HEIGHT, 8), (WIDTH, 12), (REFRESH, 24)] {
        if flags & flag != 0 && word(filter, offset) != word(mode, offset) {
            return false;
        }
    }
    // Native enumeration uses RGB bit depth, not input RGB masks, to select
    // display modes. Surface-only fields (caps, back buffers, etc.) are ignored.
    flags & PIXEL_FORMAT == 0 || word(filter, 84) == word(mode, 84)
}

fn catalog(current: (u32, u32), legacy: bool) -> Vec<Descriptor> {
    // A bounded virtual display profile. All entries use formats accepted by
    // SetDisplayMode; this catalog neither allocates nor changes the primary.
    let mut sizes = vec![
        (640, 480),
        (800, 600),
        (1024, 768),
        (1280, 720),
        (1600, 900),
        (1920, 1080),
        current,
    ];
    sizes.sort_unstable();
    sizes.dedup();
    let mut modes = Vec::new();
    for (width, height) in sizes {
        if surface::size(width, height).is_err() {
            continue;
        }
        for format in [PixelFormat::Rgb565, PixelFormat::Bgra32] {
            let mut d = [0; 124];
            let (red, green, blue) = format.masks();
            for (offset, value) in [
                (0, if legacy { 108 } else { 124 }),
                (
                    4,
                    HEIGHT | WIDTH | 8 | PIXEL_FORMAT | if legacy { 0 } else { REFRESH },
                ),
                (8, height),
                (12, width),
                (16, (width * format.bytes() + 3) & !3),
                (24, 60),
                (72, 32),
                (76, 0x40),
                (84, format.bytes() * 8),
                (88, red),
                (92, green),
                (96, blue),
            ] {
                d[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
            }
            modes.push(d);
        }
    }
    modes
}

fn next(
    env: &mut Env<'_>,
    mut modes: std::vec::IntoIter<Descriptor>,
    callback: u32,
    context: u32,
    scratch: u32,
    length: usize,
) -> Outcome {
    let Some(mode) = modes.next() else {
        return Outcome::Return(0);
    };
    if adv_abi::guest::check_range(env.cpu, scratch, length as u32, Prot::WRITE).is_err()
        || env.cpu.write(scratch, &mode[..length]).is_err()
    {
        return Outcome::Return(INVALID);
    }
    // Each continuation owns a snapshot; callbacks may enumerate again or wait
    // without retaining a mutable device/store borrow or allocating guest heaps.
    Outcome::call_on_stack(
        callback,
        [scratch, context],
        scratch,
        move |env, returned| {
            if returned.value() == 0 {
                Outcome::Return(0)
            } else {
                next(env, modes, callback, context, scratch, length)
            }
        },
    )
}

#[win32(ddraw, name = "IDirectDraw7::EnumDisplayModes", level = state)]
pub(super) fn enum_modes(
    env: &mut Env<'_>,
    this: GuestPtr<()>,
    flags: u32,
    filter: GuestPtr<()>,
    context: GuestPtr<()>,
    callback: GuestPtr<()>,
) -> Outcome {
    let draw = &env.ext.get::<State>().draw;
    let Some(object) = draw
        .com
        .as_ref()
        .and_then(|com| com.get::<ComState>(&env.sys.objects, this.addr()))
        .filter(|o| matches!(o.kind, Kind::Device))
    else {
        return Outcome::Return(INVALID_OBJECT);
    };
    let legacy = object.legacy_descriptors.contains(&this.addr());
    let length = if legacy { 108 } else { 124 };
    // Standard VGA requests are accepted, but this device exposes no 8-bit
    // display modes. REFRESHRATES exposes the single supported 60 Hz profile.
    if flags & !3 != 0 || callback.is_null() || !env.cpu.accessible(callback.addr(), 1, Prot::EXEC)
    {
        return Outcome::Return(INVALID);
    }
    let filter = if filter.is_null() {
        None
    } else {
        let mut d = [0; 124];
        if adv_abi::guest::check_range(env.cpu, filter.addr(), length as u32, Prot::READ).is_err()
            || env.cpu.read(filter.addr(), &mut d[..length]).is_err()
            || word(&d, 0) as usize != length
        {
            return Outcome::Return(INVALID);
        }
        Some(d)
    };
    let store = env.ext.get::<SurfaceStore>();
    let mut modes = catalog((store.width, store.height), legacy);
    modes.retain(|mode| filter.as_ref().is_none_or(|f| matches(f, mode)));
    for mode in &mut modes {
        if legacy || flags & 1 == 0 {
            mode[24..28].fill(0);
        }
    }
    if modes.is_empty() {
        return Outcome::Return(0);
    }
    let Some(scratch) = env
        .cpu
        .reg(Reg::Esp)
        .checked_sub(length as u32)
        .map(|p| p & !3)
    else {
        return Outcome::Return(INVALID);
    };
    if adv_abi::guest::check_range(env.cpu, scratch, length as u32, Prot::WRITE).is_err() {
        return Outcome::Return(INVALID);
    }
    next(
        env,
        modes.into_iter(),
        callback.addr(),
        context.addr(),
        scratch,
        length,
    )
}

#[cfg(test)]
mod tests;

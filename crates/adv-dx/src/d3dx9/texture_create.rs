//! D3DX parameter correction and empty textures reuse the shared D3D9 owner.
use adv_abi::{GuestPtr, Registry, guest::check_range, win32};
use adv_cpu::backend::Prot;
use adv_machine::{Env, Outcome};

const INVALID: u32 = 0x8876086c;
const UNAVAILABLE: u32 = 0x8876086a;
const DEFAULT: u32 = u32::MAX;

pub(super) fn register(registry: &mut Registry) {
    for api in [check_ABI, create_ABI] {
        registry.register(api);
        // The February 2006 and June 2010 SDK versions share these contracts.
        registry.register(adv_abi::Api {
            module: "d3dx9_29.dll",
            ..api
        });
    }
}

#[derive(Clone, Copy)]
struct Requirements {
    width: u32,
    height: u32,
    mips: u32,
    format: u32,
}

#[allow(clippy::too_many_arguments)]
fn requirements(
    env: &mut Env<'_>,
    device: u32,
    width: u32,
    height: u32,
    mips: u32,
    usage: u32,
    format: i32,
    pool: i32,
) -> Result<Requirements, u32> {
    if !(0..=3).contains(&pool) {
        return Err(INVALID);
    }
    #[cfg(not(feature = "gpu"))]
    {
        let _ = (device, width, height, mips, usage, format);
        Err(UNAVAILABLE)
    }
    #[cfg(feature = "gpu")]
    {
        let caps = crate::d3d9::loading::texture_caps(env, device)?;
        if usage & !(1 | 0x200) != 0 {
            return Err(unavailable(
                env,
                "texture usage is outside the implemented D3D9 profile",
            ));
        }
        let (width, height) = match (width, height) {
            (DEFAULT, DEFAULT) => (256, 256),
            (DEFAULT, h) => (h.max(1), h.max(1)),
            (w, DEFAULT) => (w.max(1), w.max(1)),
            (w, h) => (w.max(1), h.max(1)),
        };
        let dimension = |n: u32, maximum: u32| {
            if caps.TextureCaps & 2 != 0 {
                n.checked_next_power_of_two()
                    .unwrap_or(maximum)
                    .min(maximum)
            } else {
                n.min(maximum)
            }
        };
        let mut width = dimension(width, caps.MaxTextureWidth);
        let mut height = dimension(height, caps.MaxTextureHeight);
        if caps.TextureCaps & 0x20 != 0 {
            width = width
                .max(height)
                .min(caps.MaxTextureWidth.min(caps.MaxTextureHeight));
            height = width;
        }
        let aspect = caps.MaxTextureAspectRatio.max(1);
        if u64::from(width) > u64::from(height) * u64::from(aspect) {
            height = width.div_ceil(aspect);
        }
        if u64::from(height) > u64::from(width) * u64::from(aspect) {
            width = height.div_ceil(aspect);
        }
        let maximum = if caps.TextureCaps & 0x4000 != 0 {
            32 - width.max(height).leading_zeros()
        } else {
            1
        };
        let mips = if matches!(mips, 0 | DEFAULT) {
            maximum
        } else {
            mips.min(maximum)
        };
        // Prefer an exact supported format, then preserve color/alpha channels.
        // Floating-point/depth/FourCC conversions are outside this bounded profile.
        let format = match format {
            0 | -1 => 21,
            21 | 22 => format as u32,
            23 | 26 | 28 if usage & 1 == 0 => format as u32,
            20 | 23 | 24 | 30 | 33 | 41 => 22,
            25 | 26 | 28 | 29 | 31 | 32 | 35 | 40 => 21,
            _ => {
                return Err(unavailable(
                    env,
                    "requested texture format has no implemented compatible format",
                ));
            }
        };
        Ok(Requirements {
            width,
            height,
            mips,
            format,
        })
    }
}

fn unavailable(env: &mut Env<'_>, reason: &str) -> u32 {
    let name = env.sys.thunks.get(env.thunk).name.clone();
    adv_abi::warn_unavailable(env, &name, reason);
    UNAVAILABLE
}

#[allow(clippy::too_many_arguments)]
#[win32(d3dx9_43, name = "D3DXCheckTextureRequirements", level = implement)]
fn check(
    env: &mut Env<'_>,
    device: GuestPtr<()>,
    width: GuestPtr<u32>,
    height: GuestPtr<u32>,
    mips: GuestPtr<u32>,
    usage: u32,
    format: GuestPtr<i32>,
    pool: i32,
) -> u32 {
    for address in [width.addr(), height.addr(), mips.addr(), format.addr()] {
        if address != 0 && check_range(env.cpu, address, 4, Prot::READ | Prot::WRITE).is_err() {
            return INVALID;
        }
    }
    let value = |pointer: GuestPtr<u32>| {
        if pointer.is_null() {
            DEFAULT
        } else {
            pointer.read(env.cpu).unwrap()
        }
    };
    let (width_value, height_value, mips_value) = (value(width), value(height), value(mips));
    let format_value = if format.is_null() {
        0
    } else {
        format.read(env.cpu).unwrap()
    };
    let result = requirements(
        env,
        device.addr(),
        width_value,
        height_value,
        mips_value,
        usage,
        format_value,
        pool,
    );
    let result = match result {
        Ok(r) => r,
        Err(error) => return error,
    };
    for (pointer, value) in [
        (width, result.width),
        (height, result.height),
        (mips, result.mips),
        (GuestPtr::new(format.addr()), result.format),
    ] {
        if !pointer.is_null() && pointer.write(env.cpu, value).is_err() {
            return INVALID;
        }
    }
    0
}

#[allow(clippy::too_many_arguments)]
#[win32(d3dx9_43, name = "D3DXCreateTexture", level = implement)]
fn create(
    env: &mut Env<'_>,
    device: GuestPtr<()>,
    width: u32,
    height: u32,
    mips: u32,
    usage: u32,
    format: i32,
    pool: i32,
    output: GuestPtr<u32>,
) -> Outcome {
    if output.is_null() || check_range(env.cpu, output.addr(), 4, Prot::WRITE).is_err() {
        return Outcome::Return(INVALID);
    }
    let result = match requirements(env, device.addr(), width, height, mips, usage, format, pool) {
        Ok(r) => r,
        Err(error) => return Outcome::Return(error),
    };
    if (usage != 0 && pool != 0) || usage & (1 | 0x200) == (1 | 0x200) {
        return Outcome::Return(INVALID);
    }
    #[cfg(feature = "gpu")]
    {
        crate::d3d9::loading::create_empty(
            env,
            device.addr(),
            result.width,
            result.height,
            result.mips,
            usage,
            result.format,
            pool,
            output,
        )
    }
    #[cfg(not(feature = "gpu"))]
    {
        let _ = result;
        Outcome::Return(UNAVAILABLE)
    }
}

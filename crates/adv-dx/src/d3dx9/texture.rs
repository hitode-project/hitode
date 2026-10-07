//! D3DX image metadata and one-level texture loading over the shared D3D9 owner.
use adv_abi::{GuestPtr, Registry, win32};
use adv_cpu::backend::Prot;
use adv_machine::{Env, Outcome};
mod decode;

const INVALID: u32 = 0x8876086c;
const INVALID_DATA: u32 = 0x88760b59;
const UNAVAILABLE: u32 = 0x8876086a;
const DEFAULT: u32 = u32::MAX;
const NONPOW2: u32 = u32::MAX - 1;

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, bytemuck::Pod, bytemuck::Zeroable)]
struct ImageInfo {
    width: u32,
    height: u32,
    depth: u32,
    mips: u32,
    format: u32,
    resource_type: u32,
    file_format: u32,
}

pub(super) fn register(registry: &mut Registry) {
    registry.register(info_ABI);
    registry.register(create_ABI);
}

fn api(env: &Env<'_>) -> &'static str {
    if env.sys.thunks.get(env.thunk).name == "D3DXGetImageInfoFromFileInMemory" {
        "d3dx9_43.dll!D3DXGetImageInfoFromFileInMemory"
    } else {
        "d3dx9_43.dll!D3DXCreateTextureFromFileInMemoryEx"
    }
}
fn unavailable(env: &mut Env<'_>, reason: &str) -> u32 {
    adv_abi::warn_unavailable(env, api(env), reason);
    UNAVAILABLE
}
fn source(env: &mut Env<'_>, pointer: GuestPtr<()>, size: u32) -> Result<Vec<u8>, u32> {
    if size == 0 || pointer.is_null() {
        return Err(INVALID);
    }
    if size as usize > decode::LIMIT {
        return Err(unavailable(
            env,
            "encoded image exceeds the bounded 64 MiB CPU loader",
        ));
    }
    if adv_abi::guest::check_range(env.cpu, pointer.addr(), size, Prot::READ).is_err() {
        return Err(INVALID);
    }
    let mut bytes = vec![0; size as usize];
    env.cpu
        .read(pointer.addr(), &mut bytes)
        .map_err(|_| INVALID)?;
    Ok(bytes)
}
fn decoded_error(env: &mut Env<'_>, bytes: &[u8], error: decode::Error) -> u32 {
    match error {
        decode::Error::Invalid => INVALID_DATA,
        decode::Error::Unsupported(reason) => {
            // Only a bounded container signature is diagnostic; guest image
            // content and source addresses are never copied to the report.
            let prefix = bytes
                .iter()
                .take(4)
                .map(|b| format!("{b:02x}"))
                .collect::<String>();
            unavailable(env, &format!("{reason}; container magic={prefix}"))
        }
    }
}

#[win32(d3dx9_43, name = "D3DXGetImageInfoFromFileInMemory", level = implement)]
fn info(env: &mut Env<'_>, data: GuestPtr<()>, size: u32, output: GuestPtr<ImageInfo>) -> u32 {
    if !output.is_null()
        && adv_abi::guest::check_range(env.cpu, output.addr(), 28, Prot::WRITE).is_err()
    {
        return INVALID;
    }
    let bytes = match source(env, data, size) {
        Ok(b) => b,
        Err(e) => return e,
    };
    let image = match decode::inspect(&bytes) {
        Ok(i) => i,
        Err(e) => return decoded_error(env, &bytes, e),
    };
    if !output.is_null() && output.write(env.cpu, image).is_err() {
        INVALID
    } else {
        0
    }
}

#[allow(clippy::too_many_arguments)]
#[win32(d3dx9_43, name = "D3DXCreateTextureFromFileInMemoryEx", level = implement)]
fn create(
    env: &mut Env<'_>,
    device: GuestPtr<()>,
    data: GuestPtr<()>,
    size: u32,
    width: u32,
    height: u32,
    mips: u32,
    usage: u32,
    format: i32,
    pool: i32,
    filter: u32,
    mip_filter: u32,
    color_key: u32,
    image_info: GuestPtr<ImageInfo>,
    palette: GuestPtr<()>,
    output: GuestPtr<u32>,
) -> Outcome {
    if output.is_null()
        || output.write(env.cpu, 0).is_err()
        || (!image_info.is_null()
            && adv_abi::guest::check_range(env.cpu, image_info.addr(), 28, Prot::WRITE).is_err())
    {
        return Outcome::Return(INVALID);
    }
    #[cfg(not(feature = "gpu"))]
    {
        let _ = (
            device, data, size, width, height, mips, usage, format, pool, filter, mip_filter,
            color_key, palette,
        );
        Outcome::Return(unavailable(
            env,
            "D3D9 texture loading requires the installed GPU personality",
        ))
    }
    #[cfg(feature = "gpu")]
    {
        if !crate::d3d9::loading::valid_device(env, device.addr()) {
            return Outcome::Return(INVALID);
        }
        if usage != 0 || !(0..=3).contains(&pool) || !palette.is_null() {
            return Outcome::Return(unavailable(
                env,
                "loader supports Usage=0, ordinary D3D9 pools and no palette output",
            ));
        }
        if mips != 1 || !matches!(filter, 1..=3) || !matches!(mip_filter, 1..=3) {
            return Outcome::Return(unavailable(
                env,
                "loader supports one mip and NONE/POINT/LINEAR without extra filter flags",
            ));
        }
        let bytes = match source(env, data, size) {
            Ok(b) => b,
            Err(e) => return Outcome::Return(e),
        };
        let image = match decode::inspect(&bytes) {
            Ok(i) => i,
            Err(e) => return Outcome::Return(decoded_error(env, &bytes, e)),
        };
        let dimension = |requested, source: u32| match requested {
            NONPOW2 | 0 => Some(source),
            DEFAULT if source.is_power_of_two() => Some(source),
            n if n == source => Some(source),
            _ => None,
        };
        if dimension(width, image.width).is_none() || dimension(height, image.height).is_none() {
            return Outcome::Return(unavailable(
                env,
                "resizing or DEFAULT power-of-two expansion is not implemented; NONPOW2 preserves source dimensions",
            ));
        }
        let format = match format {
            0 | -1 => {
                if image.format == 22 || image.format == 20 || image.format == 23 {
                    22
                } else {
                    21
                }
            }
            -3 if matches!(image.format, 21 | 22 | 26) => image.format,
            21 | 22 | 26 => format as u32,
            _ => {
                return Outcome::Return(unavailable(
                    env,
                    "destination format is unsupported by the shared texture storage (21/22/26)",
                ));
            }
        };
        let mut pixels = match decode::decode(&bytes, image) {
            Ok(p) => p,
            Err(e) => return Outcome::Return(decoded_error(env, &bytes, e)),
        };
        if color_key != 0 {
            for pixel in pixels.as_chunks_mut::<4>().0.iter_mut() {
                if u32::from_le_bytes(*pixel) == color_key {
                    pixel.fill(0);
                }
            }
        }
        let outcome = crate::d3d9::loading::create(
            env,
            device.addr(),
            image.width,
            image.height,
            format,
            pool,
            pixels,
        );
        publish(env, output, image_info, image, outcome)
    }
}

#[cfg(feature = "gpu")]
fn publish(
    env: &mut Env<'_>,
    output: GuestPtr<u32>,
    info: GuestPtr<ImageInfo>,
    image: ImageInfo,
    outcome: Outcome,
) -> Outcome {
    match outcome {
        Outcome::Block {
            wait,
            then,
            allow_time_skip,
        } => Outcome::Block {
            wait,
            allow_time_skip,
            then: Box::new(move |env, resume| {
                let result = then(env, resume);
                publish(env, output, info, image, result)
            }),
        },
        Outcome::Return(pointer) if pointer != 0 && pointer & 0x80000000 == 0 => {
            if adv_abi::guest::check_range(env.cpu, output.addr(), 4, Prot::WRITE).is_err()
                || (!info.is_null()
                    && adv_abi::guest::check_range(env.cpu, info.addr(), 28, Prot::WRITE).is_err())
                || (!info.is_null() && info.write(env.cpu, image).is_err())
                || output.write(env.cpu, pointer).is_err()
            {
                crate::d3d9::loading::release_texture(env, pointer);
                Outcome::Return(INVALID)
            } else {
                Outcome::Return(0)
            }
        }
        other => other,
    }
}

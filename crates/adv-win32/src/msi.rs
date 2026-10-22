//! Windows Installer discovery for the profile's empty product/component set.
//! No installation record or package handle is invented. File hashing still
//! operates on real guest files through the existing VFS/file/section owners.
use crate::{
    Win32,
    advapi32::Digest,
    util::{last_error, set_last_error},
};
use adv_abi::{AnsiStr, GuestPtr, Registry, WideStr, guest::check_range};
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::{Env, Outcome};
const INVALID_PARAMETER: u32 = 87;
const UNKNOWN_PRODUCT: u32 = 1605;
const UNKNOWN_COMPONENT: u32 = 1607;
const INVALID_STATE: u32 = (-2i32) as u32;
const UNKNOWN_STATE: u32 = u32::MAX;
fn text(env: &Env<'_>, pointer: u32, wide: bool) -> Result<Vec<u16>, u32> {
    if pointer == 0 {
        return Err(INVALID_PARAMETER);
    }
    if wide {
        WideStr(GuestPtr::new(pointer))
            .read(env.cpu, 32768)
            .map_err(|_| INVALID_PARAMETER)
    } else {
        AnsiStr(GuestPtr::new(pointer))
            .read(env.cpu, 32768)
            .map(|bytes| env.ext.get::<Win32>().codec.to_utf16(0, &bytes))
            .map_err(|_| INVALID_PARAMETER)
    }
}
fn guid(env: &Env<'_>, pointer: u32, wide: bool) -> bool {
    let Ok(value) = text(env, pointer, wide) else {
        return false;
    };
    value.len() == 38
        && value.iter().enumerate().all(|(i, &unit)| match i {
            0 => unit == u16::from(b'{'),
            37 => unit == u16::from(b'}'),
            9 | 14 | 19 | 24 => unit == u16::from(b'-'),
            _ => unit <= 127 && (unit as u8).is_ascii_hexdigit(),
        })
}
fn query_product(env: &mut Env<'_>, wide: bool) -> Outcome {
    Outcome::Return(if guid(env, env.arg(0), wide) {
        UNKNOWN_STATE
    } else {
        INVALID_STATE
    })
}
fn query_feature(env: &mut Env<'_>, wide: bool) -> Outcome {
    Outcome::Return(
        if guid(env, env.arg(0), wide) && text(env, env.arg(1), wide).is_ok() {
            UNKNOWN_STATE
        } else {
            INVALID_STATE
        },
    )
}
fn enum_empty(env: &mut Env<'_>) -> Outcome {
    Outcome::Return(if env.arg(1) == 0 {
        INVALID_PARAMETER
    } else {
        259
    })
}
fn enum_features(env: &mut Env<'_>, wide: bool) -> Outcome {
    Outcome::Return(if !guid(env, env.arg(0), wide) || env.arg(2) == 0 {
        INVALID_PARAMETER
    } else {
        UNKNOWN_PRODUCT
    })
}
fn enum_clients(env: &mut Env<'_>, wide: bool) -> Outcome {
    Outcome::Return(if !guid(env, env.arg(0), wide) || env.arg(2) == 0 {
        INVALID_PARAMETER
    } else {
        UNKNOWN_COMPONENT
    })
}
fn component_path(env: &mut Env<'_>, wide: bool) -> Outcome {
    Outcome::Return(
        if !guid(env, env.arg(0), wide)
            || !guid(env, env.arg(1), wide)
            || env.arg(2) != 0 && env.arg(3) == 0
        {
            INVALID_STATE
        } else {
            UNKNOWN_STATE
        },
    )
}
fn product_code(env: &mut Env<'_>, wide: bool) -> Outcome {
    Outcome::Return(if !guid(env, env.arg(0), wide) || env.arg(1) == 0 {
        INVALID_PARAMETER
    } else {
        UNKNOWN_COMPONENT
    })
}
fn product_info(env: &mut Env<'_>, wide: bool) -> Outcome {
    Outcome::Return(
        if !guid(env, env.arg(0), wide)
            || text(env, env.arg(1), wide).is_err()
            || env.arg(2) != 0 && env.arg(3) == 0
        {
            INVALID_PARAMETER
        } else {
            UNKNOWN_PRODUCT
        },
    )
}
fn elevated(env: &mut Env<'_>, wide: bool) -> Outcome {
    Outcome::Return(if !guid(env, env.arg(0), wide) || env.arg(1) == 0 {
        INVALID_PARAMETER
    } else {
        UNKNOWN_PRODUCT
    })
}
fn feature_usage(env: &mut Env<'_>, wide: bool) -> Outcome {
    Outcome::Return(
        if !guid(env, env.arg(0), wide)
            || text(env, env.arg(1), wide).is_err()
            || env.arg(2) == 0
            || env.arg(3) == 0
        {
            INVALID_PARAMETER
        } else {
            UNKNOWN_PRODUCT
        },
    )
}
fn file_hash(env: &mut Env<'_>, wide: bool) -> Outcome {
    let (path, options, result) = (env.arg(0), env.arg(1), env.arg(2));
    if options != 0
        || check_range(env.cpu, result, 20, Prot::WRITE).is_err()
        || check_range(env.cpu, result, 4, Prot::READ).is_err()
        || env.cpu.read_u32(result).ok() != Some(20)
    {
        return Outcome::Return(INVALID_PARAMETER);
    }
    let path = match text(env, path, wide) {
        Ok(value) if !value.is_empty() => String::from_utf16_lossy(&value),
        _ => return Outcome::Return(INVALID_PARAMETER),
    };
    // Internal file APIs use LastError. MSI returns a status code directly, so
    // preserve the caller's LastError while using that shared file owner.
    let saved = last_error(env);
    let handle = crate::kernel32::open_with_share(env, &path, 0x80000000, 3, 7);
    let error = last_error(env);
    set_last_error(env, saved);
    if handle == u32::MAX {
        return Outcome::Return(error);
    }
    let mut digest = Digest::new(0x8003).unwrap();
    let mut buffer = [0; 4096];
    let status = loop {
        let file = env.sys.objects.host_mut::<crate::FileObj>(handle).unwrap();
        match crate::kernel32::read_coherent(
            file,
            &env.ext.get::<Win32>().vfs.sections,
            &mut buffer,
        ) {
            Ok(0) => break 0,
            Ok(count) => digest.update(&buffer[..count]),
            Err(_) => break 1117,
        }
    };
    env.sys.objects.close(handle);
    if status != 0 {
        return Outcome::Return(status);
    }
    if env.cpu.write(result + 4, &digest.finish()).is_err() {
        return Outcome::Return(INVALID_PARAMETER);
    }
    Outcome::Return(0)
}
pub(super) fn register(registry: &mut Registry) {
    for (name, words, handler) in [
        (
            "MsiQueryProductStateA",
            1,
            (|env: &mut Env<'_>| query_product(env, false)) as adv_machine::Handler,
        ),
        ("MsiQueryProductStateW", 1, |env| query_product(env, true)),
        ("MsiQueryFeatureStateA", 2, |env| query_feature(env, false)),
        ("MsiQueryFeatureStateW", 2, |env| query_feature(env, true)),
        ("MsiEnumProductsA", 2, enum_empty),
        ("MsiEnumProductsW", 2, enum_empty),
        ("MsiEnumComponentsA", 2, enum_empty),
        ("MsiEnumComponentsW", 2, enum_empty),
        ("MsiEnumFeaturesA", 4, |env| enum_features(env, false)),
        ("MsiEnumFeaturesW", 4, |env| enum_features(env, true)),
        ("MsiEnumClientsA", 3, |env| enum_clients(env, false)),
        ("MsiEnumClientsW", 3, |env| enum_clients(env, true)),
        ("MsiGetComponentPathA", 4, |env| component_path(env, false)),
        ("MsiGetComponentPathW", 4, |env| component_path(env, true)),
        ("MsiGetProductCodeA", 2, |env| product_code(env, false)),
        ("MsiGetProductCodeW", 2, |env| product_code(env, true)),
        ("MsiGetProductInfoA", 4, |env| product_info(env, false)),
        ("MsiGetProductInfoW", 4, |env| product_info(env, true)),
        ("MsiIsProductElevatedA", 2, |env| elevated(env, false)),
        ("MsiIsProductElevatedW", 2, |env| elevated(env, true)),
        ("MsiGetFeatureUsageA", 4, |env| feature_usage(env, false)),
        ("MsiGetFeatureUsageW", 4, |env| feature_usage(env, true)),
        ("MsiGetFileHashA", 3, |env| file_hash(env, false)),
        ("MsiGetFileHashW", 3, |env| file_hash(env, true)),
    ] {
        crate::stdcall(registry, "msi.dll", name, words, handler);
    }
}
#[cfg(test)]
mod tests;

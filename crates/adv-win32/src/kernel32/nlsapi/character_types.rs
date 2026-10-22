//! A/W character classification shares Unicode properties and locale decoding.
use super::locales;
use adv_abi::{AnsiStr, GuestPtr, Registry, WideStr, guest::check_range};
use adv_cpu::backend::Prot;
use adv_machine::{Env, Outcome};
fn classify(env: &mut Env<'_>, wide: bool, extended: bool) -> Outcome {
    let offset = u32::from(!wide || extended);
    let kind = env.arg(offset);
    let source = env.arg(offset + 1);
    let length = env.arg(offset + 2) as i32;
    let output = env.arg(offset + 3);
    if !matches!(kind, 1 | 2 | 4) {
        return locales::fail(env, 1004);
    }
    if length == 0 || length < -1 || source == 0 || output == source {
        return locales::fail(env, 87);
    }
    if kind != 1 {
        adv_abi::warn_unavailable(
            env,
            "kernel32.dll!GetStringTypeEx",
            "bidirectional and shaping classification data is unavailable",
        );
        return locales::fail(env, 50);
    }
    let units = if wide {
        if length == -1 {
            match WideStr(GuestPtr::new(source)).read(env.cpu, 1 << 20) {
                Ok(mut units) => {
                    units.push(0);
                    units
                }
                Err(_) => return locales::fail(env, 87),
            }
        } else {
            let Some(bytes) = (length as u32).checked_mul(2) else {
                return locales::fail(env, 87);
            };
            if check_range(env.cpu, source, bytes, Prot::READ).is_err() {
                return locales::fail(env, 87);
            }
            let mut bytes = vec![0; bytes as usize];
            if env.cpu.read(source, &mut bytes).is_err() {
                return locales::fail(env, 87);
            }
            bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|b| u16::from_le_bytes([b[0], b[1]]))
                .collect()
        }
    } else {
        let locale = match locales::by_id(env, env.arg(0)) {
            Ok(locale) => locale,
            Err(error) => return locales::fail(env, error),
        };
        let bytes = if length == -1 {
            match AnsiStr(GuestPtr::new(source)).read(env.cpu, 1 << 20) {
                Ok(mut bytes) => {
                    bytes.push(0);
                    bytes
                }
                Err(_) => return locales::fail(env, 87),
            }
        } else {
            if check_range(env.cpu, source, length as u32, Prot::READ).is_err() {
                return locales::fail(env, 87);
            }
            let mut bytes = vec![0; length as usize];
            if env.cpu.read(source, &mut bytes).is_err() {
                return locales::fail(env, 87);
            }
            bytes
        };
        env.ext
            .get::<crate::Win32>()
            .codec
            .to_utf16(locale.cp, &bytes)
    };
    let bytes = units
        .into_iter()
        .flat_map(|u| adv_os::nls::ctype1(u).to_le_bytes())
        .collect::<Vec<_>>();
    if check_range(env.cpu, output, bytes.len() as u32, Prot::WRITE).is_err() {
        return locales::fail(env, 87);
    }
    let _ = env.cpu.write(output, &bytes);
    Outcome::Return(1)
}
/// msvcr90's internal `__crtGetStringTypeW` takes GetStringTypeExW's first five
/// arguments; its code page and LCID serve only the narrow fallback.
pub(crate) fn crt_string_type_w(env: &mut Env<'_>) -> Outcome {
    classify(env, true, true)
}
pub(super) fn register(registry: &mut Registry) {
    crate::stdcall(registry, super::K32, "GetStringTypeW", 4, |e| {
        classify(e, true, false)
    });
    crate::stdcall(registry, super::K32, "GetStringTypeA", 5, |e| {
        classify(e, false, false)
    });
    crate::stdcall(registry, super::K32, "GetStringTypeExW", 5, |e| {
        classify(e, true, true)
    });
    crate::stdcall(registry, super::K32, "GetStringTypeExA", 5, |e| {
        classify(e, false, true)
    });
}

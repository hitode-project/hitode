//! Built-in Microsoft software CSP discovery. Verify contexts own no persistent
//! private keys; opening a missing key container is an ordinary CSP failure.
use super::*;
use adv_abi::{GuestPtr, Registry, win32};
use adv_machine::Outcome;

const PROVIDERS: [(u32, &str); 4] = [
    (1, "Microsoft Enhanced Cryptographic Provider v1.0"),
    (1, "Microsoft Base Cryptographic Provider v1.0"),
    (1, "Microsoft Strong Cryptographic Provider"),
    (24, "Microsoft Enhanced RSA and AES Cryptographic Provider"),
];
fn default(kind: u32) -> Option<&'static str> {
    match kind {
        1 => Some(PROVIDERS[2].1),
        24 => Some(PROVIDERS[3].1),
        _ => None,
    }
}
fn encoded(value: &str, wide: bool) -> Vec<u8> {
    if wide {
        value
            .encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect()
    } else {
        value.bytes().chain([0]).collect()
    }
}
fn acquire(env: &mut Env<'_>, wide: bool) -> Outcome {
    let (result, container, name, kind, flags) =
        (env.arg(0), env.arg(1), env.arg(2), env.arg(3), env.arg(4));
    let returned = (|| {
        if !output(env, result) {
            return fail(env, 87);
        }
        if flags & !(0xf0000000 | 0x78) != 0
            || flags & 0xf0000000 != 0 && flags & 0xf0000000 != 0xf0000000
        {
            return fail(env, BAD_FLAGS);
        }
        let Some(default_name) = default(kind) else {
            return fail(env, 0x80090017);
        };
        let requested = match crate::network::text(env, name, wide) {
            Ok(value) => String::from_utf16_lossy(&value),
            Err(error) => return fail(env, error),
        };
        let Some((_, name)) = PROVIDERS.iter().find(|(provider_kind, provider_name)| {
            *provider_kind == kind
                && (requested.is_empty() && *provider_name == default_name
                    || provider_name.eq_ignore_ascii_case(&requested))
        }) else {
            return fail(env, 0x80090013);
        };
        let container = match crate::network::text(env, container, wide) {
            Ok(value) => value,
            Err(error) => return fail(env, error),
        };
        if flags & 0xf0000000 != 0 && (!container.is_empty() || flags & 0x18 != 0) {
            return fail(env, BAD_FLAGS);
        }
        if flags & 0x18 != 0 {
            adv_abi::warn_unavailable(
                env,
                if wide {
                    "advapi32.dll!CryptAcquireContextW"
                } else {
                    "advapi32.dll!CryptAcquireContextA"
                },
                "persistent CSP key-container creation and deletion are unavailable",
            );
            return fail(env, 0x80090020);
        }
        if flags & 0xf0000000 == 0 {
            return fail(env, 0x80090016);
        } // NTE_BAD_KEYSET: no stored keys.
        publish(
            env,
            Context {
                provider: Arc::new(Provider {
                    kind,
                    name,
                    machine: flags & 0x20 != 0,
                }),
                references: 1,
            },
            result,
        )
    })();
    Outcome::Return(returned as u32)
}
fn release(env: &mut Env<'_>) -> Outcome {
    let (handle, flags) = (env.arg(0), env.arg(1));
    if flags != 0 {
        return Outcome::Return(fail(env, BAD_FLAGS) as u32);
    }
    let Some(id) = identity::<Context>(env, handle) else {
        return Outcome::Return(fail(env, BAD_UID) as u32);
    };
    let value = env.sys.objects.owned_mut(id).unwrap();
    value.references -= 1;
    if value.references == 0 {
        let ns = env.ext.get::<Store>().0;
        env.sys.objects.close_alias(ns, handle);
    }
    Outcome::Return(1)
}
fn add_ref(env: &mut Env<'_>) -> Outcome {
    let (handle, reserved, flags) = (env.arg(0), env.arg(1), env.arg(2));
    let Some(id) = identity::<Context>(env, handle) else {
        return Outcome::Return(fail(env, BAD_UID) as u32);
    };
    if reserved != 0 {
        return Outcome::Return(fail(env, 87) as u32);
    }
    if flags != 0 {
        return Outcome::Return(fail(env, BAD_FLAGS) as u32);
    }
    let value = env.sys.objects.owned_mut(id).unwrap();
    let Some(next) = value.references.checked_add(1) else {
        return Outcome::Return(fail(env, NO_MEMORY) as u32);
    };
    value.references = next;
    Outcome::Return(1)
}
fn get_default(env: &mut Env<'_>, wide: bool) -> Outcome {
    let (kind, reserved, flags, name, length) =
        (env.arg(0), env.arg(1), env.arg(2), env.arg(3), env.arg(4));
    if reserved != 0 {
        return Outcome::Return(fail(env, 87) as u32);
    }
    if !matches!(flags, 1 | 2) {
        return Outcome::Return(fail(env, BAD_FLAGS) as u32);
    }
    let Some(provider) = default(kind) else {
        return Outcome::Return(fail(env, 0x80090017) as u32);
    };
    Outcome::Return(sized(env, &encoded(provider, wide), name, length) as u32)
}
fn parameter(env: &mut Env<'_>) -> Outcome {
    let (handle, param, data, length, flags) =
        (env.arg(0), env.arg(1), env.arg(2), env.arg(3), env.arg(4));
    let Some(provider) = context(env, handle) else {
        return Outcome::Return(fail(env, BAD_UID) as u32);
    };
    if flags != 0 {
        return Outcome::Return(fail(env, BAD_FLAGS) as u32);
    }
    let bytes = match param {
        4 => encoded(provider.name, false),         // PP_NAME
        5 => 0x200u32.to_le_bytes().to_vec(),       // PP_VERSION
        6 => vec![0],                               // PP_CONTAINER: empty verify-context name
        16 => provider.kind.to_le_bytes().to_vec(), // PP_PROVTYPE
        27 => (if provider.machine { 0x20u32 } else { 0 })
            .to_le_bytes()
            .to_vec(), // PP_KEYSET_TYPE
        3 => 1u32.to_le_bytes().to_vec(),           // PP_IMPTYPE: CRYPT_IMPL_SOFTWARE
        _ => {
            adv_abi::warn_unavailable(
                env,
                "advapi32.dll!CryptGetProvParam",
                "CSP parameter is unavailable",
            );
            return Outcome::Return(fail(env, BAD_TYPE) as u32);
        }
    };
    Outcome::Return(sized(env, &bytes, data, length) as u32)
}

#[win32(advapi32,name="CryptGenRandom",level=implement)]
fn random(env: &mut Env<'_>, handle: u32, length: u32, result: GuestPtr<u8>) -> i32 {
    if context(env, handle).is_none() {
        return fail(env, BAD_UID);
    }
    if check_range(env.cpu, result.addr(), length, Prot::WRITE).is_err() {
        return fail(env, 87);
    }
    let mut bytes = Vec::new();
    if bytes.try_reserve_exact(length as usize).is_err() {
        return fail(env, NO_MEMORY);
    }
    bytes.resize(length as usize, 0);
    if adv_os::entropy::fill(&mut bytes).is_err() {
        return fail(env, 0x80090020);
    }
    if length != 0 && env.cpu.write(result.addr(), &bytes).is_err() {
        return fail(env, 87);
    }
    1
}

pub(super) fn register(registry: &mut Registry) {
    registry.register(random_ABI);
    use crate::stdcall;
    for (name, argc, handler) in [
        (
            "CryptAcquireContextA",
            5,
            (|e: &mut Env<'_>| acquire(e, false)) as adv_machine::Handler,
        ),
        ("CryptAcquireContextW", 5, |e| acquire(e, true)),
        ("CryptReleaseContext", 2, release),
        ("CryptContextAddRef", 3, add_ref),
        ("CryptGetDefaultProviderA", 5, |e| get_default(e, false)),
        ("CryptGetDefaultProviderW", 5, |e| get_default(e, true)),
        ("CryptGetProvParam", 5, parameter),
    ] {
        stdcall(registry, "advapi32.dll", name, argc, handler);
    }
}

//! Session-key construction and plaintext blobs. No asymmetric key or successful
//! private-key operation is synthesized when only symmetric material is owned.
mod parameters;
mod transform;
use super::*;
use adv_abi::{GuestPtr, win32};

const BAD_KEY: u32 = 0x80090003;
const BAD_DATA: u32 = 0x80090005;
#[derive(Clone)]
struct Key {
    _provider: Arc<Provider>,
    algorithm: u32,
    material: Vec<u8>,
    effective: u32,
    initial: cipher::Cipher,
    active: cipher::Cipher,
    iv: Vec<u8>,
    chain: Vec<u8>,
    mode: u32,
    permissions: u32,
    salt: Vec<u8>,
}
pub(super) fn hmac_material(env: &Env<'_>, handle: u32) -> Option<Vec<u8>> {
    Some(env.sys.objects.owned(key(env, handle)?)?.material.clone())
}
fn key(env: &Env<'_>, handle: u32) -> Option<Identity<Key>> {
    identity(env, handle)
}
fn construct(
    provider: Arc<Provider>,
    algorithm: u32,
    mut material: Vec<u8>,
    effective: u32,
    flags: u32,
) -> Result<Key, u32> {
    if provider.kind != 24 && matches!(algorithm, 0x660e..=0x6610) {
        return Err(BAD_ALGID);
    }
    let hmac_only = algorithm == 0x6602 && flags & 0x100 != 0;
    let valid = match algorithm {
        0x660e => material.len() == 16,
        0x660f => material.len() == 24,
        0x6610 => material.len() == 32,
        0x6601 => material.len() == 8,
        0x6603 => material.len() == 24,
        0x6609 => material.len() == 16,
        0x6602 | 0x6801 => hmac_only && !material.is_empty() || (5..=16).contains(&material.len()),
        _ => return Err(BAD_ALGID),
    };
    if provider.base()
        && (matches!(algorithm, 0x6603 | 0x6609)
            || matches!(algorithm, 0x6602 | 0x6801)
                && !hmac_only
                && material.len() * 8 > provider.rc_maximum() as usize)
    {
        return Err(BAD_ALGID);
    }
    if !valid {
        return Err(BAD_DATA);
    }
    if matches!(algorithm, 0x6601 | 0x6603 | 0x6609) {
        for byte in &mut material {
            *byte = (*byte & 0xfe) | u8::from((*byte & 0xfe).count_ones() % 2 == 0);
        }
    }
    let salt = if material.len() == 5
        && matches!(algorithm, 0x6602 | 0x6801)
        && flags & 0x10 == 0
        && !hmac_only
    {
        vec![0; 11]
    } else {
        Vec::new()
    };
    let mut expanded = material.clone();
    expanded.extend_from_slice(&salt);
    let initial = if hmac_only {
        cipher::Cipher::HmacOnly
    } else {
        cipher::Cipher::new(algorithm, &expanded, effective).ok_or(BAD_DATA)?
    };
    let iv = vec![0; initial.block_size()];
    Ok(Key {
        _provider: provider,
        algorithm,
        material,
        effective,
        active: initial.clone(),
        initial,
        chain: iv.clone(),
        iv,
        mode: 1,
        permissions: 0x3b | ((flags & 1) * 4),
        salt,
    })
}
#[win32(advapi32,name="CryptImportKey",level=state)]
fn import(
    env: &mut Env<'_>,
    provider: u32,
    input: GuestPtr<u8>,
    length: u32,
    public_key: u32,
    flags: u32,
    result: GuestPtr<u32>,
) -> i32 {
    let Some(provider) = context(env, provider) else {
        return fail(env, BAD_UID);
    };
    if flags & !0x111 != 0 {
        return fail(env, BAD_FLAGS);
    }
    if public_key != 0 {
        return fail(env, BAD_KEY);
    }
    if !output(env, result.addr()) {
        return fail(env, 87);
    }
    let bytes = match readable(env, input.addr(), length) {
        Ok(value) => value,
        Err(error) => return fail(env, error),
    };
    if bytes.len() < 12 || bytes[1] != 2 || bytes[2..4] != [0, 0] {
        return fail(env, BAD_DATA);
    }
    if bytes[0] != 8 {
        adv_abi::warn_unavailable(
            env,
            "advapi32.dll!CryptImportKey",
            "asymmetric and encrypted key blobs are unavailable",
        );
        return fail(env, BAD_TYPE);
    }
    let algorithm = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
    let length = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
    if length != bytes.len() - 12 {
        return fail(env, BAD_DATA);
    }
    if flags & 0x100 != 0 && algorithm != 0x6602 {
        return fail(env, BAD_FLAGS);
    }
    let effective = if algorithm == 0x6602 {
        40
    } else {
        length as u32 * 8
    };
    let value = match construct(provider, algorithm, bytes[12..].to_vec(), effective, flags) {
        Ok(value) => value,
        Err(error) => return fail(env, error),
    };
    publish(env, value, result.addr())
}
#[win32(advapi32,name="CryptDeriveKey",level=state)]
fn derive(
    env: &mut Env<'_>,
    provider: u32,
    algorithm: u32,
    base: u32,
    flags: u32,
    result: GuestPtr<u32>,
) -> i32 {
    let Some(provider) = context(env, provider) else {
        return fail(env, BAD_UID);
    };
    let Some(id) = identity::<Hash>(env, base) else {
        return fail(env, BAD_HASH);
    };
    if flags & 0xffff & !0x1d != 0 {
        return fail(env, BAD_FLAGS);
    }
    if flags & 0x14 == 0x14 {
        return fail(env, BAD_FLAGS);
    }
    if !output(env, result.addr()) {
        return fail(env, 87);
    }
    let specified = flags >> 16;
    let bits = match algorithm {
        0x6601 => 56,
        0x6603 => 168,
        0x6609 => 112,
        0x660e => 128,
        0x660f => 192,
        0x6610 => 256,
        0x6602 | 0x6801 => {
            if specified == 0 {
                provider.rc_default()
            } else {
                specified
            }
        }
        _ => return fail(env, BAD_ALGID),
    };
    if specified != 0 && specified != bits
        || matches!(algorithm, 0x6602 | 0x6801)
            && (!(40..=provider.rc_maximum()).contains(&bits) || bits % 8 != 0)
    {
        return fail(env, BAD_FLAGS);
    }
    let hash = env.sys.objects.owned(id).unwrap();
    let base = hash.value();
    let hash_algorithm = hash.digest.algorithm();
    let length = match algorithm {
        0x6601 => 8,
        0x6603 => 24,
        0x6609 => 16,
        _ => (bits / 8) as usize,
    };
    let mut material = expand(&base, hash_algorithm, algorithm);
    if material.len() < length {
        return fail(env, BAD_HASH);
    }
    let salt = if flags & 4 != 0 && matches!(algorithm, 0x6602 | 0x6801) {
        material[length..].to_vec()
    } else if bits == 40 && matches!(algorithm, 0x6602 | 0x6801) && flags & 0x10 == 0 {
        vec![0; 11]
    } else {
        Vec::new()
    };
    material.truncate(length);
    let mut value = match construct(provider, algorithm, material, bits, flags) {
        Ok(value) => value,
        Err(error) => return fail(env, error),
    };
    value.salt = salt;
    if !value.salt.is_empty() {
        let mut expanded = value.material.clone();
        expanded.extend_from_slice(&value.salt);
        let Some(cipher) = cipher::Cipher::new(algorithm, &expanded, bits) else {
            return fail(env, BAD_DATA);
        };
        value.initial = cipher.clone();
        value.active = cipher;
    }
    if publish(env, value, result.addr()) == 0 {
        return 0;
    }
    env.sys.objects.owned_mut(id).unwrap().finished = Some(base);
    1
}
fn expand(base: &[u8], hash_algorithm: u32, algorithm: u32) -> Vec<u8> {
    if !matches!(hash_algorithm, 0x800c..=0x800e)
        && matches!(algorithm, 0x6603 | 0x6609 | 0x660e..=0x6610)
    {
        let mut material = Vec::new();
        for pad in [0x36, 0x5c] {
            let mut input = vec![pad; 64];
            for (dst, src) in input.iter_mut().zip(base) {
                *dst ^= src;
            }
            let mut digest = digest::Digest::new(hash_algorithm).unwrap();
            digest.update(&input);
            material.extend(digest.finish());
        }
        // Keep spare bytes for CRYPT_CREATE_SALT; the caller chooses the key length.
        material
    } else {
        base.to_vec()
    }
}
#[win32(advapi32,name="CryptExportKey",level=state)]
fn export(
    env: &mut Env<'_>,
    handle: u32,
    public_key: u32,
    kind: u32,
    flags: u32,
    result: GuestPtr<u8>,
    length: GuestPtr<u32>,
) -> i32 {
    let Some(id) = key(env, handle) else {
        return fail(env, BAD_KEY);
    };
    if public_key != 0 {
        return fail(env, BAD_KEY);
    }
    if flags != 0 {
        return fail(env, BAD_FLAGS);
    }
    let key = env.sys.objects.owned(id).unwrap();
    if key.permissions & 4 == 0 {
        return fail(env, 0x8009000b);
    }
    if kind != 8 {
        adv_abi::warn_unavailable(
            env,
            "advapi32.dll!CryptExportKey",
            "encrypted and asymmetric key-blob export is unavailable",
        );
        return fail(env, BAD_TYPE);
    }
    let mut bytes = vec![8, 2, 0, 0];
    bytes.extend_from_slice(&key.algorithm.to_le_bytes());
    bytes.extend_from_slice(&(key.material.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&key.material);
    sized(env, &bytes, result.addr(), length.addr())
}
#[win32(advapi32,name="CryptDuplicateKey",level=state)]
fn duplicate(
    env: &mut Env<'_>,
    handle: u32,
    reserved: GuestPtr<u32>,
    flags: u32,
    result: GuestPtr<u32>,
) -> i32 {
    let Some(id) = key(env, handle) else {
        return fail(env, BAD_KEY);
    };
    if !reserved.is_null() {
        return fail(env, 87);
    }
    if flags != 0 {
        return fail(env, BAD_FLAGS);
    }
    if !output(env, result.addr()) {
        return fail(env, 87);
    }
    let value = env.sys.objects.owned(id).unwrap().clone();
    publish(env, value, result.addr())
}
#[win32(advapi32,name="CryptDestroyKey",level=state)]
fn destroy(env: &mut Env<'_>, handle: u32) -> i32 {
    if key(env, handle).is_none() {
        return fail(env, BAD_KEY);
    }
    let ns = env.ext.get::<Store>().0;
    env.sys.objects.close_alias(ns, handle);
    1
}

#[win32(advapi32,name="CryptGenKey",level=state)]
fn generate(
    env: &mut Env<'_>,
    handle: u32,
    algorithm: u32,
    flags: u32,
    result: GuestPtr<u32>,
) -> i32 {
    let Some(provider) = context(env, handle) else {
        return fail(env, BAD_UID);
    };
    if flags & 0xffff & !0x11 != 0 {
        return fail(env, BAD_FLAGS);
    }
    if !output(env, result.addr()) {
        return fail(env, 87);
    }
    let specified = flags >> 16;
    let bits = match algorithm {
        0x6601 => 56,
        0x6603 => 168,
        0x6609 => 112,
        0x660e => 128,
        0x660f => 192,
        0x6610 => 256,
        0x6602 | 0x6801 => {
            if specified == 0 {
                provider.rc_default()
            } else {
                specified
            }
        }
        1 | 2 | 0xa400 | 0x2400 => {
            adv_abi::warn_unavailable(
                env,
                "advapi32.dll!CryptGenKey",
                "asymmetric key generation is unavailable",
            );
            return fail(env, BAD_ALGID);
        }
        _ => return fail(env, BAD_ALGID),
    };
    if specified != 0 && specified != bits
        || matches!(algorithm, 0x6602 | 0x6801)
            && (!(40..=provider.rc_maximum()).contains(&bits) || bits % 8 != 0)
    {
        return fail(env, BAD_FLAGS);
    }
    let length = match algorithm {
        0x6601 => 8,
        0x6603 => 24,
        0x6609 => 16,
        _ => (bits / 8) as usize,
    };
    let mut material = vec![0; length];
    if adv_os::entropy::fill(&mut material).is_err() {
        return fail(env, 0x80090020);
    }
    let mut value = match construct(provider, algorithm, material, bits, flags) {
        Ok(value) => value,
        Err(error) => return fail(env, error),
    };
    if bits == 40 && matches!(algorithm, 0x6602 | 0x6801) && flags & 0x10 == 0 {
        value.salt = vec![0; 11];
        let mut material = value.material.clone();
        material.extend_from_slice(&value.salt);
        value.initial = cipher::Cipher::new(algorithm, &material, bits).unwrap();
        value.active = value.initial.clone();
    }
    publish(env, value, result.addr())
}

pub(super) fn register(registry: &mut Registry) {
    for api in [
        generate_ABI,
        import_ABI,
        derive_ABI,
        export_ABI,
        duplicate_ABI,
        destroy_ABI,
    ] {
        registry.register(api);
    }
    parameters::register(registry);
    transform::register(registry);
}
#[cfg(test)]
mod tests;

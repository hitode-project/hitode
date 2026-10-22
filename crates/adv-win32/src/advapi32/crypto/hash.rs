//! Hash lifecycle is shared by aliases, parameter queries and duplication.
use super::*;
use adv_abi::{GuestPtr, win32};

#[win32(advapi32, name="CryptCreateHash", level=state)]
fn create(
    env: &mut Env<'_>,
    provider: u32,
    algorithm: u32,
    key: u32,
    flags: u32,
    result: GuestPtr<u32>,
) -> i32 {
    let Some(provider) = context(env, provider) else {
        return fail(env, BAD_UID);
    };
    if flags != 0 {
        return fail(env, BAD_FLAGS);
    }
    if algorithm != 0x8009 && key != 0 {
        return fail(env, 0x80090003);
    }
    let hmac_material = if algorithm == 0x8009 {
        match key::hmac_material(env, key) {
            Some(value) => Some(value),
            None => return fail(env, 0x80090003),
        }
    } else {
        None
    };
    if provider.kind != 24 && matches!(algorithm, 0x800c..=0x800e) {
        return fail(env, BAD_ALGID);
    }
    let Some(mut digest) = digest::Digest::new(if algorithm == 0x8009 {
        0x8004
    } else {
        algorithm
    }) else {
        return fail(env, BAD_ALGID);
    };
    let hmac = if let Some(material) = hmac_material {
        let (hmac, inner) = hmac::Hmac::new(material, 0x8004, None, None).unwrap();
        digest = inner;
        Some(hmac)
    } else {
        None
    };
    if !output(env, result.addr()) {
        return fail(env, 87);
    }
    publish(
        env,
        Hash {
            _provider: provider,
            algorithm,
            digest,
            finished: None,
            hmac,
            started: false,
        },
        result.addr(),
    )
}
#[win32(advapi32, name="CryptHashData", level=state)]
fn data(env: &mut Env<'_>, handle: u32, input: GuestPtr<u8>, length: u32, flags: u32) -> i32 {
    let Some(id) = identity::<Hash>(env, handle) else {
        return fail(env, BAD_HASH);
    };
    if flags != 0 {
        return fail(env, BAD_FLAGS);
    }
    if env.sys.objects.owned(id).unwrap().finished.is_some() {
        return fail(env, BAD_HASH_STATE);
    }
    if check_range(env.cpu, input.addr(), length, Prot::READ).is_err() {
        return fail(env, 87);
    }
    let mut value = env.sys.objects.owned(id).unwrap().clone();
    let mut offset = 0;
    let mut buffer = [0u8; 4096];
    while offset < length {
        let count = (length - offset).min(buffer.len() as u32);
        if env
            .cpu
            .read(input.addr() + offset, &mut buffer[..count as usize])
            .is_err()
        {
            return fail(env, 87);
        }
        value.digest.update(&buffer[..count as usize]);
        offset += count;
    }
    value.started = true;
    *env.sys.objects.owned_mut(id).unwrap() = value;
    1
}
#[win32(advapi32, name="CryptGetHashParam", level=state)]
fn get(
    env: &mut Env<'_>,
    handle: u32,
    parameter: u32,
    result: GuestPtr<u8>,
    length: GuestPtr<u32>,
    flags: u32,
) -> i32 {
    let Some(id) = identity::<Hash>(env, handle) else {
        return fail(env, BAD_HASH);
    };
    if flags != 0 {
        return fail(env, BAD_FLAGS);
    }
    let hash = env.sys.objects.owned(id).unwrap();
    let bytes = match parameter {
        1 => hash.algorithm.to_le_bytes().to_vec(), // HP_ALGID
        2 => hash.value(),                          // HP_HASHVAL
        4 => (hash.digest.size() as u32).to_le_bytes().to_vec(), // HP_HASHSIZE
        _ => return fail(env, BAD_TYPE),
    };
    let success = sized(env, &bytes, result.addr(), length.addr());
    if success != 0 && parameter == 2 && !result.is_null() {
        env.sys.objects.owned_mut(id).unwrap().finished = Some(bytes);
    }
    success
}
#[win32(advapi32, name="CryptSetHashParam", level=state)]
fn set(env: &mut Env<'_>, handle: u32, parameter: u32, input: GuestPtr<u8>, flags: u32) -> i32 {
    let Some(id) = identity::<Hash>(env, handle) else {
        return fail(env, BAD_HASH);
    };
    if flags != 0 {
        return fail(env, BAD_FLAGS);
    }
    if parameter == 5 {
        return set_hmac(env, id, input.addr());
    }
    if parameter != 2 {
        return fail(env, BAD_TYPE);
    }
    let hash = env.sys.objects.owned(id).unwrap();
    if hash.finished.is_some() {
        return fail(env, BAD_HASH_STATE);
    }
    let length = hash.digest.size() as u32;
    let bytes = match readable(env, input.addr(), length) {
        Ok(bytes) => bytes,
        Err(error) => return fail(env, error),
    };
    env.sys.objects.owned_mut(id).unwrap().finished = Some(bytes);
    1
}
#[win32(advapi32, name="CryptDuplicateHash", level=state)]
fn duplicate(
    env: &mut Env<'_>,
    handle: u32,
    reserved: GuestPtr<u32>,
    flags: u32,
    result: GuestPtr<u32>,
) -> i32 {
    let Some(id) = identity::<Hash>(env, handle) else {
        return fail(env, BAD_HASH);
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
    let hash = env.sys.objects.owned(id).unwrap().clone();
    publish(env, hash, result.addr())
}
#[win32(advapi32, name="CryptDestroyHash", level=state)]
fn destroy(env: &mut Env<'_>, handle: u32) -> i32 {
    if identity::<Hash>(env, handle).is_none() {
        return fail(env, BAD_HASH);
    }
    let ns = env.ext.get::<Store>().0;
    env.sys.objects.close_alias(ns, handle);
    1
}

fn set_hmac(env: &mut Env<'_>, id: Identity<Hash>, input: u32) -> i32 {
    let hash = env.sys.objects.owned(id).unwrap();
    let Some(hmac) = &hash.hmac else {
        return fail(env, BAD_TYPE);
    };
    if hash.started || hash.finished.is_some() {
        return fail(env, BAD_HASH_STATE);
    }
    let key = hmac.key();
    let info = match readable(env, input, 20) {
        Ok(bytes) => bytes,
        Err(error) => return fail(env, error),
    };
    let fields: [u32; 5] =
        std::array::from_fn(|i| u32::from_le_bytes(info[i * 4..i * 4 + 4].try_into().unwrap()));
    let mut pads = Vec::new();
    for (pointer, length) in [(fields[1], fields[2]), (fields[3], fields[4])] {
        if pointer == 0 {
            pads.push(None);
        } else {
            match readable(env, pointer, length) {
                Ok(bytes) => pads.push(Some(bytes)),
                Err(error) => return fail(env, error),
            }
        }
    }
    let outer = pads.pop().unwrap();
    let inner = pads.pop().unwrap();
    if env.sys.objects.owned(id).unwrap()._provider.kind != 24
        && matches!(fields[0], 0x800c..=0x800e)
    {
        return fail(env, BAD_ALGID);
    }
    let Some((hmac, digest)) = hmac::Hmac::new(key, fields[0], inner, outer) else {
        return fail(env, BAD_ALGID);
    };
    let hash = env.sys.objects.owned_mut(id).unwrap();
    hash.hmac = Some(hmac);
    hash.digest = digest;
    1
}
#[win32(advapi32,name="CryptHashSessionKey",level=state)]
fn session_key(env: &mut Env<'_>, handle: u32, key: u32, flags: u32) -> i32 {
    let Some(id) = identity::<Hash>(env, handle) else {
        return fail(env, BAD_HASH);
    };
    if flags & !1 != 0 {
        return fail(env, BAD_FLAGS);
    }
    if env.sys.objects.owned(id).unwrap().finished.is_some() {
        return fail(env, BAD_HASH_STATE);
    }
    let Some(mut bytes) = key::hmac_material(env, key) else {
        return fail(env, 0x80090003);
    };
    if flags & 1 == 0 {
        bytes.reverse();
    }
    let hash = env.sys.objects.owned_mut(id).unwrap();
    hash.digest.update(&bytes);
    hash.started = true;
    1
}

pub(super) fn register(registry: &mut Registry) {
    for api in [
        session_key_ABI,
        create_ABI,
        data_ABI,
        get_ABI,
        set_ABI,
        duplicate_ABI,
        destroy_ABI,
    ] {
        registry.register(api);
    }
}

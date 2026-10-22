//! CBC/ECB/CFB8 and PKCS#5/#7 padding. Work on copies until every guest output
//! is validated, so failed encryption/decryption cannot consume key/hash state.
use super::*;
#[allow(clippy::too_many_arguments)]
fn transform(
    env: &mut Env<'_>,
    handle: u32,
    hash: u32,
    final_block: bool,
    flags: u32,
    pointer: u32,
    length: u32,
    capacity: u32,
    decrypt: bool,
) -> i32 {
    let Some(id) = key(env, handle) else {
        return fail(env, BAD_KEY);
    };
    if flags != 0 {
        return fail(env, BAD_FLAGS);
    }
    if !output(env, length) {
        return fail(env, 87);
    }
    let Ok(count) = env.cpu.read_u32(length) else {
        return fail(env, 87);
    };
    let mut key = env.sys.objects.owned(id).unwrap().clone();
    if matches!(key.initial, cipher::Cipher::HmacOnly) {
        return fail(env, BAD_KEY);
    }
    if key.permissions & if decrypt { 2 } else { 1 } == 0 {
        return fail(env, 0x80090010);
    }
    let hash_id = if hash == 0 {
        None
    } else {
        let Some(hash) = identity::<Hash>(env, hash) else {
            return fail(env, BAD_HASH);
        };
        if env.sys.objects.owned(hash).unwrap().finished.is_some() {
            return fail(env, BAD_HASH_STATE);
        }
        Some(hash)
    };
    let block = key.initial.block_size() as u32;
    if block > 1 && count % block != 0 && (decrypt || !final_block) {
        return fail(env, BAD_DATA);
    }
    if decrypt && final_block && count == 0 && block > 1 {
        return fail(env, BAD_DATA);
    }
    let required = if !decrypt && final_block && block > 1 {
        count.checked_add(block - count % block)
    } else {
        Some(count)
    };
    let Some(required) = required else {
        return fail(env, BAD_DATA);
    };
    if pointer == 0 && !decrypt {
        let _ = env.cpu.write_u32(length, required);
        return 1;
    }
    if !decrypt && capacity < required {
        let _ = env.cpu.write_u32(length, required);
        return fail(env, 234);
    }
    if check_range(env.cpu, pointer, required, Prot::WRITE).is_err() {
        return fail(env, 87);
    }
    let mut bytes = match readable(env, pointer, count) {
        Ok(bytes) => bytes,
        Err(error) => return fail(env, error),
    };
    let plain = if !decrypt { bytes.clone() } else { Vec::new() };
    bytes = match apply(&mut key, bytes, final_block, decrypt) {
        Ok(bytes) => bytes,
        Err(error) => return fail(env, error),
    };
    if env.cpu.write(pointer, &bytes).is_err() {
        return fail(env, 87);
    }
    let _ = env.cpu.write_u32(length, bytes.len() as u32);
    if let Some(hash) = hash_id {
        let hash = env.sys.objects.owned_mut(hash).unwrap();
        hash.digest.update(if decrypt { &bytes } else { &plain });
        hash.started = true;
    }
    *env.sys.objects.owned_mut(id).unwrap() = key;
    1
}
fn apply(
    key: &mut Key,
    mut bytes: Vec<u8>,
    final_block: bool,
    decrypt: bool,
) -> Result<Vec<u8>, u32> {
    let block = key.initial.block_size();
    if !decrypt && final_block && block > 1 {
        let padding = block - bytes.len() % block;
        bytes.resize(bytes.len() + padding, padding as u8);
    }
    if block == 1 {
        key.active.transform(&mut bytes, decrypt);
    } else if key.mode == 4 {
        for byte in &mut bytes {
            let original = *byte;
            let mut feedback = key.chain.clone();
            key.active.transform(&mut feedback, false);
            *byte ^= feedback[0];
            key.chain.rotate_left(1);
            *key.chain.last_mut().unwrap() = if decrypt { original } else { *byte };
        }
    } else {
        for chunk in bytes.chunks_exact_mut(block) {
            if decrypt {
                let ciphertext = chunk.to_vec();
                key.active.transform(chunk, true);
                if key.mode == 1 {
                    for (dst, src) in chunk.iter_mut().zip(&key.chain) {
                        *dst ^= src;
                    }
                    key.chain = ciphertext;
                }
            } else {
                if key.mode == 1 {
                    for (dst, src) in chunk.iter_mut().zip(&key.chain) {
                        *dst ^= src;
                    }
                }
                key.active.transform(chunk, false);
                if key.mode == 1 {
                    key.chain = chunk.to_vec();
                }
            }
        }
    }
    if decrypt && final_block && block > 1 {
        let padding = usize::from(*bytes.last().unwrap());
        if padding == 0
            || padding > block
            || bytes[bytes.len() - padding..]
                .iter()
                .any(|byte| usize::from(*byte) != padding)
        {
            return Err(BAD_DATA);
        }
        bytes.truncate(bytes.len() - padding);
    }
    if final_block {
        key.active = key.initial.clone();
        key.chain = key.iv.clone();
    }
    Ok(bytes)
}
#[win32(advapi32,name="CryptEncrypt",level=state)]
fn encrypt(
    env: &mut Env<'_>,
    key: u32,
    hash: u32,
    final_block: i32,
    flags: u32,
    data: GuestPtr<u8>,
    length: GuestPtr<u32>,
    capacity: u32,
) -> i32 {
    transform(
        env,
        key,
        hash,
        final_block != 0,
        flags,
        data.addr(),
        length.addr(),
        capacity,
        false,
    )
}
#[win32(advapi32,name="CryptDecrypt",level=state)]
fn decrypt(
    env: &mut Env<'_>,
    key: u32,
    hash: u32,
    final_block: i32,
    flags: u32,
    data: GuestPtr<u8>,
    length: GuestPtr<u32>,
) -> i32 {
    transform(
        env,
        key,
        hash,
        final_block != 0,
        flags,
        data.addr(),
        length.addr(),
        0,
        true,
    )
}
pub(super) fn register(registry: &mut Registry) {
    registry.register(encrypt_ABI);
    registry.register(decrypt_ABI);
}

#[cfg(test)]
mod tests;

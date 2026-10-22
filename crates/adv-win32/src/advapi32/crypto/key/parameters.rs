//! Session-key parameters are actual owned state, not canned success values.
use super::*;
#[win32(advapi32,name="CryptGetKeyParam",level=state)]
fn get(
    env: &mut Env<'_>,
    handle: u32,
    parameter: u32,
    result: GuestPtr<u8>,
    length: GuestPtr<u32>,
    flags: u32,
) -> i32 {
    let Some(id) = key(env, handle) else {
        return fail(env, BAD_KEY);
    };
    if flags != 0 {
        return fail(env, BAD_FLAGS);
    }
    let key = env.sys.objects.owned(id).unwrap();
    let bytes = match parameter {
        1 => key.iv.clone(),
        2 => key.salt.clone(),
        3 => 1u32.to_le_bytes().to_vec(),
        4 => key.mode.to_le_bytes().to_vec(),
        5 => 8u32.to_le_bytes().to_vec(),
        6 => key.permissions.to_le_bytes().to_vec(),
        7 => key.algorithm.to_le_bytes().to_vec(),
        8 => if key.algorithm == 0x6801 {
            0u32
        } else {
            key.initial.block_size() as u32 * 8
        }
        .to_le_bytes()
        .to_vec(),
        9 => (if matches!(key.algorithm, 0x6601 | 0x6603 | 0x6609) {
            key.material.len() as u32 * 7
        } else {
            key.material.len() as u32 * 8
        })
        .to_le_bytes()
        .to_vec(),
        19 if key.algorithm == 0x6602 => key.effective.to_le_bytes().to_vec(),
        _ => return fail(env, BAD_TYPE),
    };
    sized(env, &bytes, result.addr(), length.addr())
}
#[win32(advapi32,name="CryptSetKeyParam",level=state)]
fn set(env: &mut Env<'_>, handle: u32, parameter: u32, input: GuestPtr<u8>, flags: u32) -> i32 {
    let Some(id) = key(env, handle) else {
        return fail(env, BAD_KEY);
    };
    if flags != 0 {
        return fail(env, BAD_FLAGS);
    }
    let mut value = env.sys.objects.owned(id).unwrap().clone();
    match parameter {
        1 => {
            if value.algorithm == 0x6801 {
                return fail(env, BAD_TYPE);
            }
            value.iv = match readable(env, input.addr(), value.initial.block_size() as u32) {
                Ok(bytes) => bytes,
                Err(error) => return fail(env, error),
            };
            value.chain = value.iv.clone();
        }
        3 | 4 | 5 | 6 | 19 => {
            let bytes = match readable(env, input.addr(), 4) {
                Ok(bytes) => bytes,
                Err(error) => return fail(env, error),
            };
            let number = u32::from_le_bytes(bytes.try_into().unwrap());
            match parameter {
                3 if number == 1 => {}
                4 if matches!(number, 1 | 2 | 4) && value.algorithm != 0x6801 => {
                    value.mode = number
                }
                5 if number == 8 => {}
                6 if number & !value.permissions == 0 => value.permissions = number,
                19 if value.algorithm == 0x6602
                    && !matches!(value.initial, cipher::Cipher::HmacOnly)
                    && (1..=1024).contains(&number) =>
                {
                    value.effective = number;
                    let mut material = value.material.clone();
                    material.extend_from_slice(&value.salt);
                    value.initial =
                        cipher::Cipher::new(value.algorithm, &material, number).unwrap();
                    value.active = value.initial.clone();
                }
                _ => return fail(env, BAD_DATA),
            }
        }
        2 | 10 => {
            let (pointer, length) = if parameter == 10 {
                let bytes = match readable(env, input.addr(), 8) {
                    Ok(bytes) => bytes,
                    Err(error) => return fail(env, error),
                };
                (
                    u32::from_le_bytes(bytes[4..].try_into().unwrap()),
                    u32::from_le_bytes(bytes[..4].try_into().unwrap()),
                )
            } else {
                (input.addr(), 11)
            };
            if !matches!(value.algorithm, 0x6602 | 0x6801)
                || matches!(value.initial, cipher::Cipher::HmacOnly)
                || length > 128u32.saturating_sub(value.material.len() as u32)
            {
                return fail(env, BAD_DATA);
            }
            value.salt = match readable(env, pointer, length) {
                Ok(bytes) => bytes,
                Err(error) => return fail(env, error),
            };
            let mut material = value.material.clone();
            material.extend_from_slice(&value.salt);
            let Some(cipher) = cipher::Cipher::new(value.algorithm, &material, value.effective)
            else {
                return fail(env, BAD_DATA);
            };
            value.initial = cipher.clone();
            value.active = cipher;
        }
        _ => return fail(env, BAD_TYPE),
    }
    *env.sys.objects.owned_mut(id).unwrap() = value;
    1
}
pub(super) fn register(registry: &mut Registry) {
    registry.register(get_ABI);
    registry.register(set_ABI);
}

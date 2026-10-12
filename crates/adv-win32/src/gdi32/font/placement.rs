//! Checked GCP arrays stay in guest memory; shaping uses the selected font owner.
use super::*;
use adv_cpu::backend::{GuestExt, Prot};

pub(super) fn register(registry: &mut Registry) {
    registry.register(placement_a_ABI);
    registry.register(placement_w_ABI);
}
#[win32(gdi32, name = "GetCharacterPlacementA", level = implement)]
fn placement_a(
    env: &mut Env<'_>,
    dc: Handle,
    input: AnsiStr,
    count: i32,
    maximum: i32,
    result: GuestPtr<u8>,
    flags: u32,
) -> Outcome {
    placement(
        env,
        dc.0,
        input.0.addr(),
        count,
        maximum,
        result.addr(),
        flags,
        false,
    )
}
#[win32(gdi32, name = "GetCharacterPlacementW", level = implement)]
fn placement_w(
    env: &mut Env<'_>,
    dc: Handle,
    input: WideStr,
    count: i32,
    maximum: i32,
    result: GuestPtr<u8>,
    flags: u32,
) -> Outcome {
    placement(
        env,
        dc.0,
        input.0.addr(),
        count,
        maximum,
        result.addr(),
        flags,
        true,
    )
}

fn read(env: &Env<'_>, pointer: u32, size: u32, writable: bool) -> Result<Vec<u8>, ()> {
    if size != 0
        && adv_abi::guest::check_range(
            env.cpu,
            pointer,
            size,
            if writable { Prot::RW } else { Prot::READ },
        )
        .is_err()
    {
        return Err(());
    }
    let mut bytes = vec![0; size as usize];
    if size != 0 && env.cpu.read(pointer, &mut bytes).is_err() {
        return Err(());
    }
    Ok(bytes)
}
fn arrays(env: &Env<'_>, pointer: u32, count: u32, stride: u32) -> Result<Option<Vec<u8>>, ()> {
    if pointer == 0 {
        Ok(None)
    } else {
        read(env, pointer, count * stride, true).map(Some)
    }
}
fn units(bytes: Vec<u8>) -> Vec<u16> {
    bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes(pair.try_into().unwrap()))
        .collect()
}
fn words(bytes: Vec<u8>) -> Vec<u32> {
    bytes
        .chunks_exact(4)
        .map(|word| u32::from_le_bytes(word.try_into().unwrap()))
        .collect()
}

#[expect(clippy::too_many_arguments, reason = "shared A/W placement boundary")]
fn placement(
    env: &mut Env<'_>,
    dc: u32,
    pointer: u32,
    count: i32,
    maximum: i32,
    output: u32,
    flags: u32,
    wide: bool,
) -> Outcome {
    if reject_dc(env, dc) {
        return Outcome::Return(0);
    }
    let request = font_request(env, dc);
    let failure = |env: &mut Env<'_>| {
        set_last_error(env, 87);
        Outcome::Return(0)
    };
    let Ok(count) = u32::try_from(count) else {
        return failure(env);
    };
    if count > 65536 {
        return failure(env);
    }
    let Ok(input) = read(env, pointer, count * if wide { 2 } else { 1 }, false) else {
        return failure(env);
    };
    let mut fields = [0u32; 9];
    let buffers = if output == 0 {
        None
    } else {
        let Ok(bytes) = read(env, output, 36, true) else {
            return failure(env);
        };
        for (field, bytes) in fields.iter_mut().zip(bytes.chunks_exact(4)) {
            *field = u32::from_le_bytes(bytes.try_into().unwrap());
        }
        if fields[0] != 36 || fields[7] > 65536 {
            return failure(env);
        }
        let cap = fields[7];
        let span = cap.max(count);
        let result = (|| -> Result<text::PlacementBuffers, ()> {
            let string = arrays(env, fields[1], span, if wide { 2 } else { 1 })?;
            Ok(text::PlacementBuffers {
                text: if wide {
                    string.clone().map(units)
                } else {
                    None
                },
                encoded_text: if wide { None } else { string },
                order: arrays(env, fields[2], span, 4)?.map(words),
                advances: arrays(env, fields[3], span, 4)?
                    .map(|bytes| words(bytes).into_iter().map(|word| word as i32).collect()),
                caret: arrays(env, fields[4], span, 4)?
                    .map(|bytes| words(bytes).into_iter().map(|word| word as i32).collect()),
                classes: arrays(env, fields[5], span, 1)?,
                glyphs: arrays(env, fields[6], span, 2)?.map(units),
                capacity: cap,
                max_fit: fields[8] as i32,
            })
        })();
        match result {
            Ok(buffers) => Some(buffers),
            Err(()) => return failure(env),
        }
    };
    let utf16 = if wide {
        units(input.clone())
    } else {
        Vec::new()
    };
    let input = if wide {
        text::TextInput::Unicode(&utf16)
    } else {
        text::TextInput::Encoded(&input)
    };
    let codepage = env.ext.get::<Win32>().codec.acp;
    let result = env
        .ext
        .get_mut::<Fonts>()
        .text
        .placement(&request, input, maximum, flags, buffers, codepage);
    let result = match result {
        Ok(result) => result,
        Err(error) => {
            adv_abi::warn_unavailable(
                env,
                if wide {
                    "gdi32.dll!GetCharacterPlacementW"
                } else {
                    "gdi32.dll!GetCharacterPlacementA"
                },
                &error.to_string(),
            );
            set_last_error(env, 50);
            return Outcome::Return(0);
        }
    };
    if let Some(buffers) = result.buffers {
        let data = [
            if wide {
                buffers
                    .text
                    .map(|values| values.into_iter().flat_map(u16::to_le_bytes).collect())
            } else {
                buffers.encoded_text
            },
            buffers
                .order
                .map(|values| values.into_iter().flat_map(u32::to_le_bytes).collect()),
            buffers
                .advances
                .map(|values| values.into_iter().flat_map(i32::to_le_bytes).collect()),
            buffers
                .caret
                .map(|values| values.into_iter().flat_map(i32::to_le_bytes).collect()),
            buffers.classes,
            buffers
                .glyphs
                .map(|values| values.into_iter().flat_map(u16::to_le_bytes).collect()),
        ];
        for (pointer, bytes) in fields[1..7].iter().zip(data) {
            if let Some(bytes) = bytes
                && !bytes.is_empty()
                && env.cpu.write(*pointer, &bytes).is_err()
            {
                return failure(env);
            }
        }
        if env.cpu.write_u32(output + 28, buffers.capacity).is_err()
            || env
                .cpu
                .write_u32(output + 32, buffers.max_fit as u32)
                .is_err()
        {
            return failure(env);
        }
    }
    Outcome::Return(result.extent)
}

//! DispCallFunc invokes actual guest code using the pinned x86 scalar ABI.
use adv_abi::{GuestPtr, Registry, guest::check_range, win32};
use adv_cpu::backend::{GuestExt, Prot, Reg};
use adv_machine::{Env, Outcome};
const INVALID: u32 = 0x80070057;
const BAD_TYPE: u32 = 0x80020008;
fn capability(env: &mut Env<'_>, reason: &str) -> u32 {
    adv_abi::warn_unavailable(env, "oleaut32.dll!DispCallFunc", reason);
    0x80004001
}
fn width(kind: u16) -> Result<usize, u32> {
    if kind & 0x9000 != 0 {
        return Err(BAD_TYPE);
    }
    if kind & 0x6000 != 0 {
        return Ok(4);
    }
    match kind {
        0 => Ok(0),
        16 | 17 => Ok(1),
        2 | 11 | 18 => Ok(2),
        3 | 4 | 8 | 9 | 10 | 13 | 19 | 22 | 23 => Ok(4),
        5 | 6 | 7 | 20 | 21 => Ok(8),
        12 | 14 => Ok(16),
        _ => Err(BAD_TYPE),
    }
}
fn argument(env: &mut Env<'_>, kind: u16, address: u32) -> Result<Vec<u32>, u32> {
    let size = width(kind)?;
    if size == 0 {
        return Err(INVALID);
    }
    if kind == 14 {
        return Err(capability(
            env,
            "by-value DECIMAL argument ABI is unavailable",
        ));
    }
    check_range(env.cpu, address, 16, Prot::READ).map_err(|_| INVALID)?;
    let mut bytes = [0; 16];
    env.cpu.read(address, &mut bytes).map_err(|_| INVALID)?;
    let payload = if kind == 12 {
        &bytes[..]
    } else {
        &bytes[8..8 + size]
    };
    if size == 16 || size == 8 {
        return Ok(payload
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| u32::from_le_bytes(*b))
            .collect());
    }
    let value = match (kind, size) {
        (16, _) => i32::from(payload[0] as i8) as u32,
        (2 | 11, _) => i32::from(i16::from_le_bytes(payload.try_into().unwrap())) as u32,
        (_, 1) => u32::from(payload[0]),
        (_, 2) => u32::from(u16::from_le_bytes(payload.try_into().unwrap())),
        _ => u32::from_le_bytes(payload.try_into().unwrap()),
    };
    Ok(vec![value])
}
#[win32(oleaut32,name="DispCallFunc",level=implement)]
fn call(
    env: &mut Env<'_>,
    instance: GuestPtr<()>,
    offset: u32,
    convention: i32,
    result_type: u32,
    count: u32,
    types: GuestPtr<u16>,
    values: GuestPtr<u32>,
    output: GuestPtr<[u8; 16]>,
) -> Outcome {
    if !matches!(convention, 1 | 4) {
        return Outcome::Return(capability(
            env,
            "only x86 cdecl and stdcall dispatch are available",
        ));
    }
    let kind = result_type as u16;
    let size = match width(kind) {
        Ok(size) => size,
        Err(error) => return Outcome::Return(error),
    };
    if size == 16 {
        return Outcome::Return(capability(
            env,
            "aggregate return-buffer dispatch is unavailable",
        ));
    }
    let validation = (|| {
        check_range(env.cpu, output.addr(), 16, Prot::WRITE).map_err(|_| INVALID)?;
        check_range(
            env.cpu,
            types.addr(),
            count.checked_mul(2).ok_or(INVALID)?,
            Prot::READ,
        )
        .map_err(|_| INVALID)?;
        check_range(
            env.cpu,
            values.addr(),
            count.checked_mul(4).ok_or(INVALID)?,
            Prot::READ,
        )
        .map_err(|_| INVALID)?;
        let target = if instance.is_null() {
            check_range(env.cpu, offset, 1, Prot::EXEC).map_err(|_| INVALID)?;
            offset
        } else {
            if !offset.is_multiple_of(4) {
                return Err(INVALID);
            }
            crate::ole32::calls::method(env, instance.addr(), offset / 4).map_err(|_| INVALID)?
        };
        let mut args = Vec::new();
        if !instance.is_null() {
            args.push(instance.addr());
        }
        for index in 0..count {
            let kind = env
                .cpu
                .read_u16(types.addr() + index * 2)
                .map_err(|_| INVALID)?;
            let address = env
                .cpu
                .read_u32(values.addr() + index * 4)
                .map_err(|_| INVALID)?;
            args.extend(argument(env, kind, address)?);
        }
        Ok((target, args))
    })();
    let (target, args) = match validation {
        Ok(value) => value,
        Err(error) => return Outcome::Return(error),
    };
    Outcome::call(target, args, move |env, result| {
        let value = result.value();
        let payload = if kind == 4 || kind == 5 || kind == 7 {
            let Some(value) = env.cpu.pop_f64() else {
                return Outcome::Return(capability(env, "guest callee returned no x87 result"));
            };
            if kind == 4 {
                (value as f32).to_le_bytes().to_vec()
            } else {
                value.to_le_bytes().to_vec()
            }
        } else if size == 8 {
            (u64::from(value) | (u64::from(env.cpu.reg(Reg::Edx)) << 32))
                .to_le_bytes()
                .to_vec()
        } else {
            value.to_le_bytes()[..size].to_vec()
        };
        if check_range(env.cpu, output.addr(), 16, Prot::WRITE).is_err() {
            return Outcome::Return(INVALID);
        }
        if env.cpu.write(output.addr(), &kind.to_le_bytes()).is_err()
            || env.cpu.write(output.addr() + 8, &payload).is_err()
        {
            return Outcome::Return(INVALID);
        }
        Outcome::Return(0)
    })
}
pub(super) fn register(registry: &mut Registry) {
    registry.register(call_ABI);
}

//! BYREF resolution borrows its input and stages deep ownership before replacing a destination.
use super::*;
pub(super) fn register(registry: &mut Registry) {
    registry.register(copy_ind_ABI);
}
pub(super) fn resolve(env: &Env<'_>, mut bytes: Variant) -> Result<Variant, u32> {
    let mut visited = std::collections::BTreeSet::new();
    while vt(&bytes) & 0x4000 != 0 {
        let kind = vt(&bytes);
        let address = pointer(&bytes);
        if address == 0 || !visited.insert(address) {
            return Err(INVALID);
        }
        if kind == 0x400c {
            bytes = read(env, address, false)?;
            continue;
        }
        let base = kind & 0x0fff;
        let size = if kind & 0x2000 != 0 {
            4
        } else {
            match base {
                16 | 17 => 1,
                2 | 11 | 18 => 2,
                3 | 4 | 8 | 9 | 10 | 13 | 19 | 22 | 23 => 4,
                5 | 6 | 7 | 20 | 21 => 8,
                14 => 16,
                _ => return Err(BAD_TYPE),
            }
        };
        check_range(env.cpu, address, size, Prot::READ).map_err(|_| INVALID)?;
        let mut result = [0; 16];
        let payload = if base == 14 && kind & 0x2000 == 0 {
            &mut result[..]
        } else {
            &mut result[8..8 + size as usize]
        };
        env.cpu.read(address, payload).map_err(|_| INVALID)?;
        result[..2].copy_from_slice(&(kind & !0x4000).to_le_bytes());
        bytes = result;
    }
    Ok(bytes)
}
fn free(env: &mut Env<'_>, address: u32) {
    env.ext
        .get_mut::<adv_os::Heaps>()
        .free(env.cpu, &mut env.sys.vm, 0, address);
}
#[win32(oleaut32,name="VariantCopyInd",level=implement)]
fn copy_ind(
    env: &mut Env<'_>,
    destination: GuestPtr<Variant>,
    source: GuestPtr<Variant>,
) -> Outcome {
    copy_ind_value(env, destination.addr(), source.addr())
}
pub(super) fn copy_ind_value(env: &mut Env<'_>, destination: u32, source: u32) -> Outcome {
    let bytes = match read(env, source, false) {
        Ok(bytes) => bytes,
        Err(error) => return Outcome::Return(error),
    };
    if vt(&bytes) & 0x4000 == 0 {
        return copy_value(env, destination, source);
    }
    let bytes = match resolve(env, bytes) {
        Ok(bytes) => bytes,
        Err(error) => return Outcome::Return(error),
    };
    if let Err(error) = supported(env, vt(&bytes), "oleaut32.dll!VariantCopyInd") {
        return Outcome::Return(error);
    }
    if read(env, destination, true).is_err() {
        return Outcome::Return(INVALID);
    }
    let Some(temporary) =
        env.ext
            .get_mut::<adv_os::Heaps>()
            .alloc(env.cpu, &mut env.sys.vm, 0, 32, true)
    else {
        return Outcome::Return(OUT_OF_MEMORY);
    };
    if env.cpu.write(temporary, &bytes).is_err() {
        free(env, temporary);
        return Outcome::Return(INVALID);
    }
    // Materialize ownership first: a BYREF BSTR may name the destination's own BSTR.
    let outcome = copy_value(env, temporary + 16, temporary);
    complete(
        env,
        outcome,
        Box::new(move |env, result| {
            if result != 0 {
                free(env, temporary);
                return Outcome::Return(result);
            }
            let outcome = clear_value(env, destination);
            complete(
                env,
                outcome,
                Box::new(move |env, result| {
                    if result == 0
                        && let Ok(bytes) = read(env, temporary + 16, false)
                        && env.cpu.write(destination, &bytes).is_ok()
                    {
                        free(env, temporary);
                        return Outcome::Return(0);
                    }
                    let failure = if result == 0 { INVALID } else { result };
                    let outcome = clear_value(env, temporary + 16);
                    complete(
                        env,
                        outcome,
                        Box::new(move |env, _| {
                            free(env, temporary);
                            Outcome::Return(failure)
                        }),
                    )
                }),
            )
        }),
    )
}

//! x86 CRT abstract control bits translated to the existing guest FPU environment.
//! Never execute a host FLDCW/LDMXCSR or keep a second floating-control owner.
use super::*;
use adv_abi::{GuestPtr, win32};
use adv_cpu::backend::FpuEnvironment;

const EM: u32 = 0x0008_001f;
const RC: u32 = 0x300;
const PC: u32 = 0x30000;
const IC: u32 = 0x40000;
const DN: u32 = 0x0300_0000;
const DENORMAL: u32 = 0x80000;
const VALID: u32 = EM | RC | PC | IC | DN;
const AMBIGUOUS: u32 = 0x8000_0000;
const EXCEPTIONS: [u32; 6] = [0x10, DENORMAL, 8, 4, 2, 1];

fn exceptions(bits: u32) -> u32 {
    EXCEPTIONS
        .iter()
        .enumerate()
        .fold(0, |out, (i, abstract_bit)| {
            out | if bits & (1 << i) != 0 {
                *abstract_bit
            } else {
                0
            }
        })
}
fn x87(control: u16) -> u32 {
    exceptions(u32::from(control))
        | ((u32::from(control) >> 2) & RC)
        | match (control >> 8) & 3 {
            0 => 0x20000,
            2 => 0x10000,
            _ => 0,
        }
        | if control & 0x1000 != 0 { IC } else { 0 }
}
fn sse(control: u32) -> u32 {
    exceptions(control >> 7)
        | ((control >> 5) & RC)
        | match (control & (1 << 6) != 0, control & (1 << 15) != 0) {
            (false, false) => 0,
            (true, true) => 0x0100_0000,
            (true, false) => 0x0200_0000,
            (false, true) => 0x0300_0000,
        }
}
fn merge(environment: FpuEnvironment) -> u32 {
    let x = x87(environment.x87_control);
    let s = sse(environment.mxcsr);
    x | s
        | if (x ^ s) & (EM | RC) != 0 {
            AMBIGUOUS
        } else {
            0
        }
}
fn update_x87(mut control: u16, value: u32, mask: u32) -> u16 {
    let value = (x87(control) & !mask) | (value & mask);
    for (i, bit) in EXCEPTIONS.into_iter().enumerate() {
        if mask & bit != 0 {
            control = (control & !(1 << i)) | (u16::from(value & bit != 0) << i);
        }
    }
    if mask & RC != 0 {
        control = (control & !0xc00) | (((value & RC) << 2) as u16);
    }
    if mask & PC != 0 {
        control = (control & !0x300)
            | match value & PC {
                0x20000 => 0,
                0x10000 => 0x200,
                _ => 0x300,
            };
    }
    if mask & IC != 0 {
        control = (control & !0x1000) | if value & IC != 0 { 0x1000 } else { 0 };
    }
    control
}
fn update_sse(mut control: u32, value: u32, mask: u32) -> u32 {
    let value = (sse(control) & !mask) | (value & mask);
    for (i, bit) in EXCEPTIONS.into_iter().enumerate() {
        if mask & bit != 0 {
            control = (control & !(1 << (i + 7))) | (u32::from(value & bit != 0) << (i + 7));
        }
    }
    if mask & RC != 0 {
        control = (control & !0x6000) | ((value & RC) << 5);
    }
    if mask & DN != 0 {
        control &= !0x8040;
        control |= match value & DN {
            0x0100_0000 => 0x8040,
            0x0200_0000 => 0x40,
            0x0300_0000 => 0x8000,
            _ => 0,
        };
    }
    control
}
fn apply(
    env: &mut Env<'_>,
    value: u32,
    mask: u32,
    x: bool,
    s: bool,
) -> Result<FpuEnvironment, String> {
    let mut current = env.cpu.fpu_environment().map_err(|e| e.to_string())?;
    let previous = current;
    if mask != 0 && (x || s) {
        if x {
            current.x87_control = update_x87(current.x87_control, value, mask);
        }
        if s {
            let updated = update_sse(current.mxcsr, value, mask);
            if updated != current.mxcsr {
                current.mxcsr = updated & !0x3f;
            }
        }
        if current != previous {
            env.cpu
                .set_fpu_environment(current)
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(current)
}
fn control(env: &mut Env<'_>, value: u32, mask: u32, denormal: bool) -> Outcome {
    let mask = if denormal { mask } else { mask & !DENORMAL } & VALID;
    match apply(env, value, mask, true, true) {
        Ok(current) => Outcome::Return(merge(current)),
        Err(e) => Outcome::Fail(e),
    }
}
fn secure(env: &mut Env<'_>, output: GuestPtr<u32>, value: u32, mask: u32) -> Outcome {
    if !output.is_null()
        && adv_abi::guest::check_range(env.cpu, output.addr(), 4, adv_cpu::backend::Prot::WRITE)
            .is_err()
    {
        return Outcome::Fail("_controlfp_s: unwritable output".into());
    }
    if value & mask & !VALID != 0 {
        let current = match env.cpu.fpu_environment() {
            Ok(current) => current,
            Err(e) => return Outcome::Fail(e.to_string()),
        };
        if !output.is_null() && output.write(env.cpu, merge(current)).is_err() {
            return Outcome::Fail("_controlfp_s: output write failed".into());
        }
        return invalid_parameter(env, "_controlfp_s", 22);
    }
    match apply(env, value, mask & VALID & !DENORMAL, true, true) {
        Ok(current) => {
            if !output.is_null() && output.write(env.cpu, merge(current)).is_err() {
                return Outcome::Fail("_controlfp_s: output write failed".into());
            }
            Outcome::Return(0)
        }
        Err(e) => Outcome::Fail(e),
    }
}
pub(super) fn errno_address(env: &mut Env<'_>) -> GuestPtr<i32> {
    if env.ext.try_get::<CrtState>().is_none() {
        env.ext.insert(CrtState::default());
    }
    let module = env.sys.thunks.get(env.thunk).module.as_str();
    let module = crt_module(module);
    let key = (module.to_owned(), env.context);
    let address = if let Some(&address) = env.ext.get::<CrtState>().errno.get(&key) {
        address
    } else {
        // One existing module/thread owner holds errno and its adjacent DWORD
        // DOS error, then the signal FP-code and exception-pointer cells.
        // The published _errno pointer remains the first int32 cell;
        // the ordinary thread/process detach path frees this same allocation.
        let address = allocate(env, 16, true);
        if address != 0 {
            env.ext.get_mut::<CrtState>().errno.insert(key, address);
        }
        address
    };
    GuestPtr::new(address)
}
pub(super) fn invalid_set(env: &mut Env<'_>, handler: GuestPtr<u8>) -> GuestPtr<u8> {
    if env.ext.try_get::<CrtState>().is_none() {
        env.ext.insert(CrtState::default());
    }
    let module = crt_module(&env.sys.thunks.get(env.thunk).module).to_owned();
    GuestPtr::new(
        env.ext
            .get_mut::<CrtState>()
            .invalid_handlers
            .insert(module, handler.addr())
            .unwrap_or(0),
    )
}
fn invalid_get(env: &mut Env<'_>) -> GuestPtr<u8> {
    GuestPtr::new(
        env.ext
            .try_get::<CrtState>()
            .and_then(|s| {
                s.invalid_handlers
                    .get(crt_module(&env.sys.thunks.get(env.thunk).module))
                    .copied()
            })
            .unwrap_or(0),
    )
}
fn separate(
    env: &mut Env<'_>,
    value: u32,
    mask: u32,
    x: GuestPtr<u32>,
    s: GuestPtr<u32>,
) -> Outcome {
    for pointer in [x, s].into_iter().filter(|p| !p.is_null()) {
        if adv_abi::guest::check_range(env.cpu, pointer.addr(), 4, adv_cpu::backend::Prot::WRITE)
            .is_err()
        {
            return Outcome::Fail("__control87_2: invalid output".into());
        }
    }
    match apply(env, value, mask & VALID, !x.is_null(), !s.is_null()) {
        Ok(current) => {
            if !x.is_null() && x.write(env.cpu, x87(current.x87_control)).is_err()
                || !s.is_null() && s.write(env.cpu, sse(current.mxcsr)).is_err()
            {
                return Outcome::Fail("__control87_2: output write failed".into());
            }
            Outcome::Return(1)
        }
        Err(e) => Outcome::Fail(e),
    }
}

#[win32(msvcrt, name = "_controlfp", level = implement)]
fn crt_controlfp(env: &mut Env<'_>, value: u32, mask: u32) -> Outcome {
    control(env, value, mask, false)
}

#[win32(msvcrt, name = "_control87", level = implement)]
fn crt_control87(env: &mut Env<'_>, value: u32, mask: u32) -> Outcome {
    control(env, value, mask, true)
}

#[win32(msvcrt, name = "_controlfp_s", level = implement)]
fn crt_controlfp_s(env: &mut Env<'_>, output: GuestPtr<u32>, value: u32, mask: u32) -> Outcome {
    secure(env, output, value, mask)
}

#[win32(msvcr80, name = "_controlfp", level = implement)]
fn vc80_controlfp(env: &mut Env<'_>, value: u32, mask: u32) -> Outcome {
    control(env, value, mask, false)
}

#[win32(msvcr80, name = "_control87", level = implement)]
fn vc80_control87(env: &mut Env<'_>, value: u32, mask: u32) -> Outcome {
    control(env, value, mask, true)
}

#[win32(msvcr80, name = "_controlfp_s", level = implement)]
fn vc80_controlfp_s(env: &mut Env<'_>, output: GuestPtr<u32>, value: u32, mask: u32) -> Outcome {
    secure(env, output, value, mask)
}

#[win32(msvcr80, name = "__control87_2", level = implement)]
fn vc80_control87_2(
    env: &mut Env<'_>,
    value: u32,
    mask: u32,
    x: GuestPtr<u32>,
    s: GuestPtr<u32>,
) -> Outcome {
    separate(env, value, mask, x, s)
}

#[win32(msvcr90, name = "_controlfp", level = implement)]
fn vc90_controlfp(env: &mut Env<'_>, value: u32, mask: u32) -> Outcome {
    control(env, value, mask, false)
}

#[win32(msvcr90, name = "_control87", level = implement)]
fn vc90_control87(env: &mut Env<'_>, value: u32, mask: u32) -> Outcome {
    control(env, value, mask, true)
}

#[win32(msvcr90, name = "_controlfp_s", level = implement)]
fn vc90_controlfp_s(env: &mut Env<'_>, output: GuestPtr<u32>, value: u32, mask: u32) -> Outcome {
    secure(env, output, value, mask)
}

#[win32(msvcr90, name = "__control87_2", level = implement)]
fn vc90_control87_2(
    env: &mut Env<'_>,
    value: u32,
    mask: u32,
    x: GuestPtr<u32>,
    s: GuestPtr<u32>,
) -> Outcome {
    separate(env, value, mask, x, s)
}

#[win32(ucrtbase, name = "_controlfp", level = implement)]
fn ucrt_controlfp(env: &mut Env<'_>, value: u32, mask: u32) -> Outcome {
    control(env, value, mask, false)
}

#[win32(ucrtbase, name = "_control87", level = implement)]
fn ucrt_control87(env: &mut Env<'_>, value: u32, mask: u32) -> Outcome {
    control(env, value, mask, true)
}

#[win32(ucrtbase, name = "_controlfp_s", level = implement)]
fn ucrt_controlfp_s(env: &mut Env<'_>, output: GuestPtr<u32>, value: u32, mask: u32) -> Outcome {
    secure(env, output, value, mask)
}

#[win32(ucrtbase, name = "__control87_2", level = implement)]
fn ucrt_control87_2(
    env: &mut Env<'_>,
    value: u32,
    mask: u32,
    x: GuestPtr<u32>,
    s: GuestPtr<u32>,
) -> Outcome {
    separate(env, value, mask, x, s)
}

pub(super) fn register(reg: &mut Registry) {
    for api in [
        crt_errno_ABI,
        vc80_errno_ABI,
        vc90_errno_ABI,
        ucrt_errno_ABI,
        vc80_set_invalid_parameter_handler_ABI,
        vc80_get_invalid_parameter_handler_ABI,
        vc90_set_invalid_parameter_handler_ABI,
        vc90_get_invalid_parameter_handler_ABI,
        ucrt_set_invalid_parameter_handler_ABI,
        ucrt_get_invalid_parameter_handler_ABI,
        ucrt_controlfp_ABI,
        ucrt_control87_ABI,
        ucrt_controlfp_s_ABI,
        ucrt_control87_2_ABI,
        crt_controlfp_ABI,
        crt_control87_ABI,
        crt_controlfp_s_ABI,
        vc80_controlfp_ABI,
        vc80_control87_ABI,
        vc80_controlfp_s_ABI,
        vc80_control87_2_ABI,
        vc90_controlfp_ABI,
        vc90_control87_ABI,
        vc90_controlfp_s_ABI,
        vc90_control87_2_ABI,
    ] {
        reg.register(api);
    }
}

#[win32(msvcr80, name = "_set_invalid_parameter_handler", level = implement)]
fn vc80_set_invalid_parameter_handler(env: &mut Env<'_>, handler: GuestPtr<u8>) -> GuestPtr<u8> {
    invalid_set(env, handler)
}

#[win32(msvcr80, name = "_get_invalid_parameter_handler", level = implement)]
fn vc80_get_invalid_parameter_handler(env: &mut Env<'_>) -> GuestPtr<u8> {
    invalid_get(env)
}

#[win32(msvcr90, name = "_set_invalid_parameter_handler", level = implement)]
fn vc90_set_invalid_parameter_handler(env: &mut Env<'_>, handler: GuestPtr<u8>) -> GuestPtr<u8> {
    invalid_set(env, handler)
}

#[win32(msvcr90, name = "_get_invalid_parameter_handler", level = implement)]
fn vc90_get_invalid_parameter_handler(env: &mut Env<'_>) -> GuestPtr<u8> {
    invalid_get(env)
}

#[win32(ucrtbase, name = "_set_invalid_parameter_handler", level = implement)]
fn ucrt_set_invalid_parameter_handler(env: &mut Env<'_>, handler: GuestPtr<u8>) -> GuestPtr<u8> {
    invalid_set(env, handler)
}

#[win32(ucrtbase, name = "_get_invalid_parameter_handler", level = implement)]
fn ucrt_get_invalid_parameter_handler(env: &mut Env<'_>) -> GuestPtr<u8> {
    invalid_get(env)
}

#[win32(msvcrt, name = "_errno", level = implement)]
fn crt_errno(env: &mut Env<'_>) -> GuestPtr<i32> {
    errno_address(env)
}

#[win32(msvcr80, name = "_errno", level = implement)]
fn vc80_errno(env: &mut Env<'_>) -> GuestPtr<i32> {
    errno_address(env)
}

#[win32(msvcr90, name = "_errno", level = implement)]
fn vc90_errno(env: &mut Env<'_>) -> GuestPtr<i32> {
    errno_address(env)
}

#[win32(ucrtbase, name = "_errno", level = implement)]
fn ucrt_errno(env: &mut Env<'_>) -> GuestPtr<i32> {
    errno_address(env)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abstract_controls_preserve_unselected_hardware_bits() {
        assert_eq!(x87(0x027f), 0x9001f);
        assert_eq!(sse(0x1f80), 0x8001f);
        // A partial rounding mask preserves the other rounding bit.
        assert_eq!(update_x87(0x0a7f, 0x100, 0x100), 0x0e7f);
        assert_eq!(update_sse(0x5fbf, 0x100, 0x100), 0x7fbf);
        for mode in [0, 0x1000000, 0x2000000, 0x3000000] {
            assert_eq!(sse(update_sse(0x1fbf, mode, DN)) & DN, mode);
            assert_eq!(update_sse(0x1fbf, mode, DN) & 0x3f, 0x3f);
        }
        assert_eq!(update_x87(0x127f, 0, RC) & !0xc00, 0x127f & !0xc00);
        let environment = FpuEnvironment {
            x87_control: 0x067f,
            mxcsr: 0x5f80,
        };
        assert_eq!(merge(environment), AMBIGUOUS | 0x9031f);
    }
}

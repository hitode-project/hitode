//! Legacy CRT buffer and measurement fronts over one checked formatter.
use super::*;
use crate::printf::crt::{self, Error};
use adv_abi::{
    Api, Arguments, GuestPtr, ImplementationLevel,
    guest::{check_range, memory_failure},
};
use adv_cpu::backend::Prot;

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Unbounded,
    Secure,
    Bounded,
    Measure,
}
#[derive(Clone, Copy)]
struct Front {
    name: &'static str,
    wide: bool,
    va_list: bool,
    mode: Mode,
    fixed: u8,
}
const FRONTS: [Front; 14] = [
    Front {
        name: "sprintf",
        wide: false,
        va_list: false,
        mode: Mode::Unbounded,
        fixed: 2,
    },
    Front {
        name: "vsprintf",
        wide: false,
        va_list: true,
        mode: Mode::Unbounded,
        fixed: 3,
    },
    Front {
        name: "sprintf_s",
        wide: false,
        va_list: false,
        mode: Mode::Secure,
        fixed: 3,
    },
    Front {
        name: "vsprintf_s",
        wide: false,
        va_list: true,
        mode: Mode::Secure,
        fixed: 4,
    },
    Front {
        name: "_snprintf_s",
        wide: false,
        va_list: false,
        mode: Mode::Bounded,
        fixed: 4,
    },
    Front {
        name: "_vsnprintf_s",
        wide: false,
        va_list: true,
        mode: Mode::Bounded,
        fixed: 5,
    },
    Front {
        name: "swprintf_s",
        wide: true,
        va_list: false,
        mode: Mode::Secure,
        fixed: 3,
    },
    Front {
        name: "vswprintf_s",
        wide: true,
        va_list: true,
        mode: Mode::Secure,
        fixed: 4,
    },
    Front {
        name: "_snwprintf_s",
        wide: true,
        va_list: false,
        mode: Mode::Bounded,
        fixed: 4,
    },
    Front {
        name: "_vsnwprintf_s",
        wide: true,
        va_list: true,
        mode: Mode::Bounded,
        fixed: 5,
    },
    Front {
        name: "_scprintf",
        wide: false,
        va_list: false,
        mode: Mode::Measure,
        fixed: 1,
    },
    Front {
        name: "_vscprintf",
        wide: false,
        va_list: true,
        mode: Mode::Measure,
        fixed: 2,
    },
    Front {
        name: "_scwprintf",
        wide: true,
        va_list: false,
        mode: Mode::Measure,
        fixed: 1,
    },
    Front {
        name: "_vscwprintf",
        wide: true,
        va_list: true,
        mode: Mode::Measure,
        fixed: 2,
    },
];
pub(super) fn register(registry: &mut Registry) {
    for module in ["msvcrt.dll", "msvcr80.dll", "msvcr90.dll"] {
        for front in FRONTS {
            if module == "msvcrt.dll" && front.mode != Mode::Unbounded {
                continue;
            }
            registry.register(Api {
                module,
                name: front.name,
                conv: if front.va_list {
                    Conv::Cdecl { argc: front.fixed }
                } else {
                    Conv::CdeclVariadic { fixed: front.fixed }
                },
                env_leaf: None,
                handler: buffer,
                level: ImplementationLevel::Implement,
            });
        }
    }
}
fn write(env: &mut Env<'_>, pointer: u32, bytes: &[u8]) -> Result<(), adv_cpu::backend::MemFault> {
    check_range(env.cpu, pointer, bytes.len() as u32, Prot::WRITE)?;
    env.cpu.write(pointer, bytes)
}
fn empty(
    env: &mut Env<'_>,
    front: Front,
    destination: u32,
    capacity: u32,
) -> Result<(), adv_cpu::backend::MemFault> {
    if front.mode != Mode::Measure && destination != 0 && capacity != 0 {
        write(env, destination, if front.wide { &[0, 0] } else { &[0] })?;
    }
    Ok(())
}
fn failure(
    env: &mut Env<'_>,
    front: Front,
    destination: u32,
    capacity: u32,
    error: Error,
) -> Outcome {
    if let Error::Memory(error) = error {
        return memory_failure(error);
    }
    if front.mode == Mode::Unbounded {
        // Native legacy %n invokes a callback after writing an unterminated
        // prefix, and unknown/trailing conversions have permissive behavior.
        // The staged checked formatter cannot reproduce those side effects;
        // reject this boundary without fabricating a native invalid callback.
        return match error {
            Error::Invalid(_) => {
                warning(
                    env,
                    front,
                    "legacy format syntax/count-output callback ordering is unavailable",
                );
                set_errno(env, 40)
            }
            Error::Unsupported(reason) => {
                warning(env, front, reason);
                set_errno(env, 40)
            }
            Error::Encoding => set_errno(env, 42),
            Error::Memory(_) => unreachable!(),
        };
    }
    if let Err(error) = empty(env, front, destination, capacity) {
        return memory_failure(error);
    }
    match error {
        Error::Invalid(reason) => {
            if reason == "%n count-output is disabled" {
                warning(env, front, reason);
            }
            invalid_parameter_result(env, front.name, 22, u32::MAX)
        }
        Error::Unsupported(reason) => {
            warning(env, front, reason);
            set_errno(env, 40) // VC errno.h ENOSYS; no fabricated invalid argument callback.
        }
        Error::Encoding => set_errno(env, 42), // EILSEQ is not an invalid parameter.
        Error::Memory(_) => unreachable!(),
    }
}
fn set_errno(env: &mut Env<'_>, value: i32) -> Outcome {
    if let Err(outcome) = store_errno(env, value) {
        return outcome;
    }
    Outcome::Return(u32::MAX)
}
fn warning(env: &mut Env<'_>, front: Front, reason: &str) {
    let api = format!("{}!{}", env.sys.thunks.get(env.thunk).module, front.name);
    adv_abi::warn_unavailable(env, &api, reason);
}
fn store_errno(env: &mut Env<'_>, value: i32) -> Result<(), Outcome> {
    let errno = floating::errno_address(env);
    if errno.is_null() || errno.write(env.cpu, value).is_err() {
        return Err(Outcome::Fail(
            "CRT formatting errno allocation/write failed".into(),
        ));
    }
    Ok(())
}
fn finish(
    env: &mut Env<'_>,
    front: Front,
    destination: u32,
    capacity: u32,
    count: u32,
    bytes: Vec<u8>,
    length: usize,
) -> Outcome {
    if front.mode == Mode::Measure {
        return Outcome::Return(length as u32);
    }
    if front.mode == Mode::Unbounded {
        // The ABI has no capacity argument. The shared formatter has already
        // bounded its output; check exactly the bytes it produced plus NUL.
        let mut output = bytes;
        output.push(0);
        if let Err(error) = write(env, destination, &output) {
            return memory_failure(error);
        }
        return Outcome::Return(length as u32);
    }
    let maximum = if front.mode == Mode::Bounded {
        count.min(capacity - 1)
    } else {
        capacity - 1
    } as usize;
    let copied = length.min(maximum);
    let width = if front.wide { 2 } else { 1 };
    let mut output = bytes[..copied * width].to_vec();
    output.resize(output.len() + width, 0);
    if let Err(error) = write(env, destination, &output) {
        return memory_failure(error);
    }
    if length <= maximum {
        return Outcome::Return(length as u32);
    }
    // _TRUNCATE, or a count smaller than capacity, permits a terminated prefix.
    // That result is -1, with no invalid callback and no new errno.
    if front.mode == Mode::Bounded && (count == u32::MAX || count < capacity) {
        return Outcome::Return(u32::MAX);
    }
    // Overflow empties the string before invoking the returning invalid handler.
    if let Err(error) = empty(env, front, destination, capacity) {
        return memory_failure(error);
    }
    invalid_parameter_result(env, front.name, 34, u32::MAX)
}
fn buffer(env: &mut Env<'_>) -> Outcome {
    let name = &env.sys.thunks.get(env.thunk).name;
    let front = *FRONTS
        .iter()
        .find(|front| front.name == name)
        .expect("registered CRT format front");
    let decode = || -> Result<_, adv_cpu::backend::MemFault> {
        let mut arguments = Arguments::new(env.cpu);
        let (destination, capacity) = if front.mode == Mode::Measure {
            (0, 0)
        } else if front.mode == Mode::Unbounded {
            (arguments.next::<u32>()?, u32::MAX)
        } else {
            (arguments.next::<u32>()?, arguments.next::<u32>()?)
        };
        let count = if front.mode == Mode::Bounded {
            arguments.next::<u32>()?
        } else {
            0
        };
        let format = arguments.next::<u32>()?;
        let values = if front.va_list {
            arguments.next::<u32>()?
        } else {
            env.cpu
                .reg(Reg::Esp)
                .checked_add(4 + u32::from(front.fixed) * 4)
                .ok_or(adv_cpu::backend::MemFault {
                    addr: env.cpu.reg(Reg::Esp),
                    len: 4,
                })?
        };
        Ok((destination, capacity, count, format, values))
    };
    let (destination, capacity, count, format, values) = match decode() {
        Ok(values) => values,
        Err(error) => return memory_failure(error),
    };
    // VC90 validates the format pointer before resetting a valid destination.
    if format == 0 {
        return invalid_parameter_result(env, front.name, 22, u32::MAX);
    }
    if front.mode == Mode::Bounded && destination == 0 && capacity == 0 && count == 0 {
        return Outcome::Return(0);
    }
    if front.mode != Mode::Measure && (destination == 0 || capacity == 0) {
        return invalid_parameter_result(env, front.name, 22, u32::MAX);
    }
    let cp = locale::formatting_codepage(env);
    // Integer/string conversions retain their existing transport even when a
    // floating profile is unavailable. Only a floating conversion consumes
    // this reason and reports the qualified API warning through failure().
    let float_profile = if locale::scalar_float_profile(env).is_none() {
        Some("LC_NUMERIC/LC_CTYPE floating formatting profile is unavailable")
    } else {
        match env.cpu.fpu_environment() {
            Ok(control) if control.x87_control & 0xc00 == 0 && control.mxcsr & 0x6000 == 0 => None,
            Ok(_) => Some("non-nearest floating rounding mode is unavailable"),
            Err(_) => Some("guest floating control state is unavailable"),
        }
    };
    if front.wide {
        let format = match adv_abi::WideStr(GuestPtr::new(format)).read(env.cpu, crt::LIMIT + 1) {
            Ok(format) => format,
            Err(error) => {
                if error.addr == format && error.len == ((crt::LIMIT + 1) * 2) as u32 {
                    return failure(
                        env,
                        front,
                        destination,
                        capacity,
                        Error::Unsupported("format scan exceeds the bounded formatter profile"),
                    );
                }
                return memory_failure(error);
            }
        };
        match crt::wide_with_float_profile(env.cpu, cp, &format, values, float_profile) {
            Ok((output, encoding_error)) => {
                if encoding_error {
                    if let Err(outcome) = store_errno(env, 42) {
                        return outcome;
                    }
                }
                let count_units = output.len();
                finish(
                    env,
                    front,
                    destination,
                    capacity,
                    count,
                    output.iter().flat_map(|unit| unit.to_le_bytes()).collect(),
                    count_units,
                )
            }
            Err(error) => failure(env, front, destination, capacity, error),
        }
    } else {
        let format = match adv_abi::AnsiStr(GuestPtr::new(format)).read(env.cpu, crt::LIMIT + 1) {
            Ok(format) => format,
            Err(error) => {
                if error.addr == format && error.len == (crt::LIMIT + 1) as u32 {
                    return failure(
                        env,
                        front,
                        destination,
                        capacity,
                        Error::Unsupported("format scan exceeds the bounded formatter profile"),
                    );
                }
                return memory_failure(error);
            }
        };
        match crt::narrow_with_float_profile(env.cpu, cp, &format, values, float_profile) {
            Ok((output, encoding_error)) => {
                if encoding_error {
                    if let Err(outcome) = store_errno(env, 42) {
                        return outcome;
                    }
                }
                let length = output.len();
                finish(env, front, destination, capacity, count, output, length)
            }
            Err(error) => failure(env, front, destination, capacity, error),
        }
    }
}

#[cfg(test)]
mod tests;

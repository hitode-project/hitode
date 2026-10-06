//! WINAPIV formatting and va_list transport over the shared numeric parser.
use super::*;
use crate::printf::{self, Args};
use adv_abi::{AnsiStr, GuestPtr, WideStr, win32};
use adv_cpu::backend::{MemFault, Reg};

pub(super) fn register(registry: &mut Registry) {
    // Pinned metadata declares both mandatory string arguments. Additional
    // arguments remain caller-owned x86 cdecl slots; no guessed fixed arity.
    registry.add(
        USER32,
        "wsprintfA",
        Conv::CdeclVariadic { fixed: 2 },
        wsprintf_a,
    );
    registry.add(
        USER32,
        "wsprintfW",
        Conv::CdeclVariadic { fixed: 2 },
        wsprintf_w,
    );
    registry.register(wvsprintf_a_ABI);
    registry.register(wvsprintf_w_ABI);
}
struct Arguments<'a> {
    cpu: &'a dyn adv_cpu::backend::Guest,
    address: Option<u32>,
    fault: Option<MemFault>,
}
impl Args for Arguments<'_> {
    fn next(&mut self) -> u32 {
        let pointer = self.address.unwrap_or(u32::MAX);
        let value = GuestPtr::<u32>::new(pointer).read(self.cpu);
        self.address = pointer.checked_add(4);
        match value {
            Ok(value) => value,
            Err(fault) => {
                self.fault = Some(fault);
                0
            }
        }
    }
}
fn failed(env: &mut Env<'_>) -> Outcome {
    crate::util::set_last_error(env, 87);
    Outcome::Return(0)
}
fn publish(env: &mut Env<'_>, destination: u32, data: &[u8], count: usize) -> Outcome {
    if adv_abi::guest::check_range(
        env.cpu,
        destination,
        data.len() as u32,
        adv_cpu::backend::Prot::WRITE,
    )
    .is_err()
        || env.cpu.write(destination, data).is_err()
    {
        return failed(env);
    }
    Outcome::Return(count as u32)
}
fn format_a(
    env: &mut Env<'_>,
    buffer: AnsiStr,
    format: AnsiStr,
    arguments: Option<u32>,
) -> Outcome {
    let Ok(format) = format.read(env.cpu, crate::util::MAX_STR) else {
        return failed(env);
    };
    let codec = env.ext.get::<Win32>().codec;
    let mut args = Arguments {
        cpu: env.cpu,
        address: arguments,
        fault: None,
    };
    let result = printf::windows_a(env.cpu, &codec, &format, &mut args);
    let Ok(mut out) = result else {
        return failed(env);
    };
    if args.fault.is_some() {
        return failed(env);
    }
    let count = out.len();
    out.push(0);
    publish(env, buffer.0.addr(), &out, count)
}
fn format_w(
    env: &mut Env<'_>,
    buffer: WideStr,
    format: WideStr,
    arguments: Option<u32>,
) -> Outcome {
    let Ok(format) = format.read(env.cpu, crate::util::MAX_STR) else {
        return failed(env);
    };
    let codec = env.ext.get::<Win32>().codec;
    let mut args = Arguments {
        cpu: env.cpu,
        address: arguments,
        fault: None,
    };
    let result = printf::windows_w(env.cpu, &codec, &format, &mut args);
    let Ok(mut out) = result else {
        return failed(env);
    };
    if args.fault.is_some() {
        return failed(env);
    }
    let count = out.len();
    out.push(0);
    let bytes: Vec<_> = out.into_iter().flat_map(u16::to_le_bytes).collect();
    publish(env, buffer.0.addr(), &bytes, count)
}
fn wsprintf_a(env: &mut Env<'_>) -> Outcome {
    format_a(
        env,
        AnsiStr(GuestPtr::new(env.arg(0))),
        AnsiStr(GuestPtr::new(env.arg(1))),
        env.cpu.reg(Reg::Esp).checked_add(12),
    )
}
fn wsprintf_w(env: &mut Env<'_>) -> Outcome {
    format_w(
        env,
        WideStr(GuestPtr::new(env.arg(0))),
        WideStr(GuestPtr::new(env.arg(1))),
        env.cpu.reg(Reg::Esp).checked_add(12),
    )
}
#[win32(user32, name = "wvsprintfA", level = implement)]
fn wvsprintf_a(
    env: &mut Env<'_>,
    buffer: AnsiStr,
    format: AnsiStr,
    arguments: GuestPtr<()>,
) -> Outcome {
    format_a(env, buffer, format, Some(arguments.addr()))
}
#[win32(user32, name = "wvsprintfW", level = implement)]
fn wvsprintf_w(
    env: &mut Env<'_>,
    buffer: WideStr,
    format: WideStr,
    arguments: GuestPtr<()>,
) -> Outcome {
    format_w(env, buffer, format, Some(arguments.addr()))
}

#[cfg(test)]
mod tests;

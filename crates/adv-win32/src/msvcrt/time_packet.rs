//! timeb fields and clock ticks come from the existing Windows clock/timezone.
//! Field writes preserve x86 padding and the native observable access order.
use super::*;
use adv_abi::{GuestPtr, guest::memory_failure};
use adv_cpu::backend::MemFault;

const UNIX_EPOCH_FT: u64 = 116_444_736_000_000_000;
const NAMES: &[&str] = &[
    "clock",
    "_ftime",
    "_ftime32",
    "_ftime64",
    "_ftime32_s",
    "_ftime64_s",
    "_o__ftime32",
    "_o__ftime64",
    "_o__ftime32_s",
    "_o__ftime64_s",
];
pub(super) fn register(registry: &mut Registry) {
    let exports: Vec<_> = NAMES
        .iter()
        .map(|&n| (n, call as adv_machine::Handler))
        .collect();
    versions::register(registry, &exports);
}
fn packet(env: &mut Env<'_>, pointer: u32, wide: bool) -> Result<(), MemFault> {
    let ft = crate::kernel32::filetime_now(env);
    let ticks = ft.wrapping_sub(UNIX_EPOCH_FT);
    let seconds = ticks / 10_000_000;
    let milliseconds = ((ticks / 10_000) % 1000) as u16;
    let timezone = env
        .ext
        .get::<crate::Win32>()
        .cfg
        .utc_offset_min
        .wrapping_neg() as i16;
    let pointer = GuestPtr::<u8>::new(pointer);
    let offset = if wide { 8 } else { 4 };
    // Native VC90/UCRT write timezone and DST, then milliseconds, then time.
    pointer
        .offset(offset + 2)?
        .cast::<i16>()
        .write(env.cpu, timezone)?;
    pointer
        .offset(offset + 4)?
        .cast::<i16>()
        .write(env.cpu, 0)?;
    pointer
        .offset(offset)?
        .cast::<u16>()
        .write(env.cpu, milliseconds)?;
    pointer.cast::<u32>().write(env.cpu, seconds as u32)?;
    if wide {
        pointer
            .offset(4)?
            .cast::<u32>()
            .write(env.cpu, (seconds >> 32) as u32)?;
    }
    Ok(())
}
fn call(env: &mut Env<'_>) -> Outcome {
    let thunk = env.sys.thunks.get(env.thunk);
    let profile = versions::profile(&thunk.module);
    let name = thunk.name.strip_prefix("_o_").unwrap_or(&thunk.name);
    if name == "clock" {
        let clock = &mut env.sys.clock;
        let epoch = clock.epoch_us;
        let milliseconds = clock.guest_now_us().saturating_sub(epoch) / 1000;
        return Outcome::Return(
            if profile.clock_overflow_failure && milliseconds > i32::MAX as u64 {
                u32::MAX
            } else {
                milliseconds as u32
            },
        );
    }
    let wide = name.contains("64");
    let pointer = env.arg(0);
    if pointer == 0 && profile.invalid_parameters {
        return errors::invalid(env, 22);
    }
    match packet(env, pointer, wide) {
        Ok(()) => Outcome::Return(0),
        Err(fault) => memory_failure(fault),
    }
}

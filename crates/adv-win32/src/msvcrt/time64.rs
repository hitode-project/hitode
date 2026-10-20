//! CRT time64 reads the existing machine wall clock and fixed virtual timezone.
//! One process-heap tm block per CRT module/thread is shared by local/gmtime;
//! DLL/TLS detach releases it through the existing CRT owner, not a new cache.
use super::*;
use adv_abi::{
    GuestPtr,
    guest::{check_range, memory_failure},
    win32,
};
use adv_cpu::backend::Prot;

const UNIX_EPOCH_FT: u64 = 116_444_736_000_000_000;
const MAX_CALENDAR_UTC: i64 = 32_535_215_999; // 3000-12-31T23:59:59Z
const TM_BYTES: u32 = 9 * 4;
const MODULES: [&str; 3] = ["msvcrt.dll", "msvcr80.dll", "msvcr90.dll"];

pub(super) fn register(registry: &mut Registry) {
    for module in MODULES {
        for api in [time64_ABI, gmtime64_ABI, localtime64_ABI, difftime64_ABI] {
            registry.register(adv_abi::Api { module, ..api });
        }
    }
}
fn warning(env: &mut Env<'_>, reason: &str) {
    let thunk = env.sys.thunks.get(env.thunk);
    let api = format!("{}!{}", thunk.module, thunk.name);
    adv_abi::warn_unavailable(env, &api, reason);
}
fn errno(env: &mut Env<'_>, error: i32) -> Result<(), Outcome> {
    floating::errno_address(env)
        .write(env.cpu, error)
        .map_err(memory_failure)
}
fn tm_address(env: &mut Env<'_>) -> Result<GuestPtr<[i32; 9]>, Outcome> {
    if env.ext.try_get::<CrtState>().is_none() {
        env.ext.insert(CrtState::default());
    }
    let module = crt_module(&env.sys.thunks.get(env.thunk).module).to_owned();
    let key = (module, env.context);
    if let Some(&address) = env.ext.get::<CrtState>().tm_buffers.get(&key) {
        return Ok(GuestPtr::new(address));
    }
    let address = allocate(env, TM_BYTES, true);
    if address == 0 {
        errno(env, 12)?;
        return Err(Outcome::Return(0));
    }
    env.ext
        .get_mut::<CrtState>()
        .tm_buffers
        .insert(key, address);
    Ok(GuestPtr::new(address))
}
fn invalid_tm(env: &mut Env<'_>, output: GuestPtr<[i32; 9]>, error: i32) -> Outcome {
    if let Err(error) = output.write(env.cpu, [-1; 9]) {
        return memory_failure(error);
    }
    match errno(env, error) {
        Ok(()) => Outcome::Return(0),
        Err(outcome) => outcome,
    }
}
/// Converts the 64-bit (`wide`) or 32-bit `time_t` at `input` into the shared tm.
pub(super) fn convert(env: &mut Env<'_>, input: u32, wide: bool, local: bool) -> Outcome {
    // Snapshot readable input before touching the cached output (the guest may
    // alias them). An actual read fault leaves any published tm unchanged.
    let read = if input == 0 {
        Ok(None)
    } else if wide {
        GuestPtr::<i64>::new(input).read(env.cpu).map(Some)
    } else {
        GuestPtr::<i32>::new(input)
            .read(env.cpu)
            .map(|seconds| Some(seconds.into()))
    };
    let seconds = match read {
        Ok(seconds) => seconds,
        Err(error) => return memory_failure(error),
    };
    let output = match tm_address(env) {
        Ok(output) => output,
        Err(outcome) => return outcome,
    };
    let Some(seconds) = seconds else {
        if let Err(error) = output.write(env.cpu, [-1; 9]) {
            return memory_failure(error);
        }
        return invalid_parameter_result(
            env,
            if local { "_localtime64" } else { "_gmtime64" },
            22,
            0,
        );
    };
    if seconds < 0 {
        return invalid_tm(env, output, 22);
    } // VC80: no invalid callback.
    if seconds > MAX_CALENDAR_UTC {
        warning(
            env,
            "calendar UTC inputs after year 3000 are outside the supported profile",
        );
        return invalid_tm(env, output, 40);
    }
    let offset = if local {
        i64::from(env.ext.get::<crate::Win32>().cfg.utc_offset_min) * 60
    } else {
        0
    };
    let seconds = seconds + offset;
    let days = seconds.div_euclid(86_400);
    let day_seconds = seconds.rem_euclid(86_400);
    let (year, month, day) = crate::kernel32::civil_from_days(days);
    let year_day = days - crate::kernel32::days_from_civil(year, 1, 1);
    let tm = [
        (day_seconds % 60) as i32,
        (day_seconds / 60 % 60) as i32,
        (day_seconds / 3600) as i32,
        day as i32,
        month as i32 - 1,
        (year - 1900) as i32,
        (days + 4).rem_euclid(7) as i32,
        year_day as i32,
        0,
    ];
    // tm_isdst=0 belongs to the existing fixed-offset virtual timezone. No CRT
    // TZ environment/DST database or host local calendar is claimed here.
    match output.write(env.cpu, tm) {
        Ok(()) => Outcome::Return(output.addr()),
        Err(error) => memory_failure(error),
    }
}

#[win32(msvcrt, name = "_time64", level = implement)]
fn time64(env: &mut Env<'_>, output: GuestPtr<i64>) -> Outcome {
    if !output.is_null()
        && let Err(error) = check_range(env.cpu, output.addr(), 8, Prot::WRITE)
    {
        return memory_failure(error);
    }
    let ft = crate::kernel32::filetime_now(env);
    let Some(seconds) = ft
        .checked_sub(UNIX_EPOCH_FT)
        .map(|ticks| ticks / 10_000_000)
    else {
        warning(env, "wall clock preceding the Unix epoch is unavailable");
        if !output.is_null()
            && let Err(error) = output.write(env.cpu, -1)
        {
            return memory_failure(error);
        }
        return match errno(env, 40) {
            Ok(()) => Outcome::Return64(u64::MAX),
            Err(outcome) => outcome,
        };
    };
    if !output.is_null()
        && let Err(error) = output.write(env.cpu, seconds as i64)
    {
        return memory_failure(error);
    }
    Outcome::Return64(seconds)
}

#[win32(msvcrt, name = "_gmtime64", level = implement)]
fn gmtime64(env: &mut Env<'_>, input: GuestPtr<i64>) -> Outcome {
    convert(env, input.addr(), true, false)
}

#[win32(msvcrt, name = "_localtime64", level = implement)]
fn localtime64(env: &mut Env<'_>, input: GuestPtr<i64>) -> Outcome {
    convert(env, input.addr(), true, true)
}

#[win32(msvcrt, name = "_difftime64", level = implement)]
fn difftime64(env: &mut Env<'_>, end: u64, begin: u64) -> Outcome {
    let (end, begin) = (end as i64, begin as i64);
    if end < 0 || begin < 0 {
        return match errno(env, 22) {
            Ok(()) => Outcome::ReturnF64(0.0),
            Err(outcome) => outcome,
        };
    }
    // Nonnegative signed inputs make this subtraction non-overflowing. Native
    // VC80 subtracts integers before converting (2^53+1 minus 2^53 remains 1).
    Outcome::ReturnF64((end - begin) as f64)
}

pub(super) fn release(
    env: &mut Env<'_>,
    context: Option<adv_machine::CtxId>,
) -> Result<(), String> {
    let Some(state) = env.ext.try_get::<CrtState>() else {
        return Ok(());
    };
    let cells: Vec<_> = state
        .tm_buffers
        .iter()
        .filter(|((_, id), _)| context.is_none_or(|ctx| *id == ctx))
        .map(|(key, &address)| (key.clone(), address))
        .collect();
    for (key, address) in cells {
        let heaps = env.ext.get_mut::<Heaps>();
        if !heaps.free(env.cpu, &mut env.sys.vm, heaps.process_heap, address) {
            return Err(format!("CRT tm detach: invalid heap block {address:#x}"));
        }
        env.ext.get_mut::<CrtState>().tm_buffers.remove(&key);
    }
    Ok(())
}

#[cfg(test)]
mod tests;

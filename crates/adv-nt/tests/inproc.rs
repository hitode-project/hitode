#![cfg(any(target_vendor = "apple", target_os = "android"))]
use adv_wineserver as _;
mod common;
use adv_cpu::backend::Reg;
use adv_machine::{Resolver, thunk::HostFn};
struct Measured;
struct Baseline(adv_nt::counters::Boundaries);
impl Resolver for Measured {
    fn provides(&self, module: &str) -> bool {
        adv_nt::dispatch::Platform.provides(module)
    }
    fn host_fn(&self, module: &str, name: &str) -> Option<HostFn> {
        let mut function = adv_nt::dispatch::Platform.host_fn(module, name)?;
        if module == adv_nt::dispatch::MODULE && name == adv_nt::dispatch::SYSCALL {
            function.handler = measure;
        }
        Some(function)
    }
}
fn measure(env: &mut adv_machine::Env<'_>) -> adv_machine::Outcome {
    let number = env.cpu.reg(Reg::Eax) as usize;
    if adv_nt::syscalls::SYSCALLS
        .get(number)
        .is_some_and(|&(name, _)| name == "NtReleaseSemaphore")
        && env.ext.try_get::<Baseline>().is_none()
    {
        env.ext
            .insert(Baseline(adv_nt::counters::Boundaries::of(env.ext)));
    }
    (adv_nt::dispatch::Platform
        .host_fn(adv_nt::dispatch::MODULE, adv_nt::dispatch::SYSCALL)
        .unwrap()
        .handler)(env)
}

// spec: SRV-SYNC-1, SRV-SYNC-2, SRV-SYNC-3, SRV-SYNC-4, WINE-THR-1, WINE-THR-2
#[test]
fn semaphore_event_ping_pong_avoids_server_operations() {
    let Some((result, output)) = common::run_probe_inspect(
        "inproc.exe",
        false,
        "null",
        Measured,
        |_, _| {},
        |machine| {
            let counters = adv_nt::counters::Boundaries::of(&machine.ext);
            for name in ["select", "release_semaphore"] {
                let number = adv_nt::protocol::REQUEST_NAMES
                    .iter()
                    .position(|&s| s == name)
                    .unwrap() as i32;
                let before = &machine.ext.get::<Baseline>().0;
                assert_eq!(
                    counters.server.get(&number).map_or(0, |t| t.calls)
                        - before.server.get(&number).map_or(0, |t| t.calls),
                    0,
                    "{name}"
                );
            }
        },
    ) else {
        return;
    };
    assert!(
        matches!(result, adv_machine::RunResult::Exited(0)),
        "{result:?}"
    );
    assert_eq!(output.trim(), "rounds=10000 code=0 read=MZ");
}

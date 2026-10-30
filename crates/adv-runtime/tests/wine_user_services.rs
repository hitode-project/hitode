//! USER services use shared state and preserve the measured Windows behavior.
#![cfg(any(target_vendor = "apple", target_os = "android"))]
#[path = "../../adv-nt/tests/common/mod.rs"]
mod common;
use adv_abi::Registry;
use adv_machine::{HostFn, Resolver, RunResult};
struct Platform(Registry);
#[derive(Default)]
struct Audits {
    begin: u64,
    done: u32,
}
static SYSCALL: std::sync::OnceLock<adv_machine::Handler> = std::sync::OnceLock::new();

impl Resolver for Platform {
    fn host_fn(&self, module: &str, name: &str) -> Option<HostFn> {
        let mut binding = adv_unix::Platform
            .host_fn(module, name)
            .or_else(|| self.0.get(module, name))?;
        if module == adv_nt::dispatch::MODULE && name == adv_nt::dispatch::SYSCALL {
            SYSCALL.get_or_init(|| binding.handler);
            binding.handler = |env| {
                use adv_cpu::backend::{GuestExt, Reg};
                if adv_nt::syscalls::SYSCALLS
                    .get(env.cpu.reg(Reg::Eax) as usize)
                    .is_some_and(|&(name, _)| name == "NtTestAlert")
                {
                    let marker = env.cpu.read_u32(env.ctx().teb + 0x34).unwrap();
                    if (0x5a17e40..0x5a17e4c).contains(&marker) {
                        let which = (marker - 0x5a17e40) / 2;
                        let after = marker & 1 != 0;
                        let tid = adv_nt::server::current_tid(env).unwrap();
                        assert_eq!(
                            adv_wineserver::has_queue(tid),
                            after && which != 5,
                            "cold service {which} after={after}"
                        );
                    }
                }
                if adv_nt::syscalls::SYSCALLS
                    .get(env.cpu.reg(Reg::Eax) as usize)
                    .is_some_and(|&(name, _)| name == "NtTestAlert")
                {
                    let marker = env.cpu.read_u32(env.ctx().teb + 0x34).unwrap();
                    if (0x5a17e80..0x5a17e94).contains(&marker) {
                        let calls = env.sys.host_calls;
                        let traced = env.sys.trace.enabled();
                        let audit = env.ext.get_or_default::<Audits>();
                        if marker & 1 == 0 {
                            audit.begin = calls;
                        } else {
                            if marker == 0x5a17e93 {
                                assert!(
                                    calls - audit.begin > 1,
                                    "timer must request server deadline processing"
                                );
                            } else if traced {
                                assert!(
                                    calls - audit.begin > 1,
                                    "guest service must cross the boundary"
                                );
                            } else {
                                assert_eq!(
                                    calls - audit.begin,
                                    1,
                                    "service {} crossed the boundary",
                                    (marker - 0x5a17e80) / 2
                                );
                            }
                            audit.done |= 1 << ((marker - 0x5a17e80) / 2);
                        }
                    }
                }
                SYSCALL.get().unwrap()(env)
            };
        }
        Some(binding)
    }
    fn provides(&self, module: &str) -> bool {
        adv_unix::Platform.provides(module) || self.0.provides(module)
    }
}
fn worker(reference: bool) {
    let mut registry = Registry::default();
    adv_runtime::wine::register(&mut registry);
    let name = if reference {
        "user_services_reference.exe"
    } else {
        "user_services.exe"
    };
    let mut done = 0;
    let Some((result, output)) = common::run_probe_engine(
        name,
        true,
        adv_runtime::wine::GRAPHICS_DRIVER,
        Platform(registry),
        |machine, prefix| {
            adv_unix::install(machine);
            adv_runtime::wine::install(machine, &prefix.join("drive_c"), 640, 480).unwrap();
            if reference {
                machine.sys.trace = adv_machine::Trace::to_writer(Box::new(std::io::sink()));
                machine.sys.trace.all = true;
            }
        },
        |machine| {
            done = machine
                .ext
                .try_get::<Audits>()
                .map_or(0, |audit| audit.done);
        },
        true,
    ) else {
        return;
    };
    eprintln!("{output}");
    assert!(
        matches!(result, RunResult::Exited(0)),
        "{result:?}: {output}"
    );
    assert!(output.contains("CHECK user:"));
    assert_eq!(done, 1023);
}
#[test]
#[ignore = "isolated native worker"]
fn user_native() {
    worker(false);
}
#[test]
#[ignore = "isolated guest worker"]
fn user_reference() {
    worker(true);
}
// spec: NAT-BODY-18, A64-CALL-5, A64-CALL-6, PROC-CLOCK-7
#[test]
fn user_services_preserve_windows_empty_queue_behavior() {
    if std::env::var_os("ADV_WINE_DIST").is_none() || std::env::var_os("ADV_WINE_PROBES").is_none()
    {
        return;
    }
    for name in ["user_native", "user_reference"] {
        let output = std::process::Command::new(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/test-timeout.sh"),
        )
        .arg(std::env::current_exe().unwrap())
        .args(["--exact", name, "--ignored", "--nocapture"])
        .env("ADV_TEST_LIMIT", "120")
        .output()
        .unwrap();
        assert!(
            output.status.success(),
            "{name}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

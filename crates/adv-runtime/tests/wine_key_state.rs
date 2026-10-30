//! Queue key state agrees with Wine before and after SetKeyboardState.
#![cfg(any(target_vendor = "apple", target_os = "android"))]
#[path = "../../adv-nt/tests/common/mod.rs"]
mod common;

fn probe(jit: bool) -> Option<String> {
    let (result, output) = common::run_probe_inspect(
        "key_state.exe",
        false,
        "null",
        adv_unix::Platform,
        |machine, _| {
            if jit {
                *machine = adv_machine::Machine::new(
                    adv_cpu::Cpu::new(adv_machine::layout::PARK_ADDR, true).unwrap(),
                    adv_unix::Platform,
                    adv_machine::Clock::virtual_time(),
                )
                .unwrap();
            } else {
                // Interpreted runs follow redirects too (A64-CTL-14); tracing
                // keeps the reference on Wine's own code.
                machine.sys.trace = adv_machine::Trace::to_writer(Box::new(std::io::sink()));
            }
            adv_unix::install(machine);
        },
        |machine| {
            let syscalls = adv_nt::counters::Boundaries::of(&machine.ext)
                .syscalls
                .get(&0x1410)
                .map_or(0, |tally| tally.calls);
            eprintln!("jit={jit}: key-state syscalls={syscalls}");
            if jit {
                assert_eq!(syscalls, 0);
            } else {
                assert!(syscalls >= 512);
            }
        },
    )?;
    assert!(
        matches!(result, adv_machine::RunResult::Exited(0)),
        "{result:?}: {output}"
    );
    assert_eq!(
        output.trim().strip_prefix("keys=").unwrap().len(),
        256 * 4 * 2
    );
    Some(output)
}

// spec: NAT-BODY-12
#[test]
#[ignore = "interpreter subprocess of key_state_matches_wine"]
fn key_state_interpreter() {
    if let Some(output) = probe(false) {
        print!("{output}");
    }
}

// spec: NAT-BODY-12
#[test]
fn key_state_matches_wine() {
    if std::env::var_os("ADV_WINE_DIST").is_none() || std::env::var_os("ADV_WINE_PROBES").is_none()
    {
        return;
    }
    let reference = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "key_state_interpreter",
            "--ignored",
            "--nocapture",
        ])
        .output()
        .unwrap();
    assert!(
        reference.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&reference.stdout),
        String::from_utf8_lossy(&reference.stderr)
    );
    let text = String::from_utf8(reference.stdout).unwrap();
    let expected = text.lines().find(|line| line.starts_with("keys=")).unwrap();
    assert_eq!(probe(true).unwrap().trim(), expected);
}

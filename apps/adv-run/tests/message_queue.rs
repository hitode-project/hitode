//! The same repository-owned x86 program checks native Windows and both guest backends.
#![cfg(windows)]

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use adv_machine::{Clock, RunResult, SliceResult};
use adv_runtime::boot::Runtime;

#[test]
fn native_and_guest_thread_message_observation_and_nine_object_priority_match() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = root.join("target/message-queue-tests");
    let build = Command::new("powershell.exe")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(root.join("tools/fixtures/build.ps1"))
        .arg("-OutputDirectory")
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let exe = output.join("fixture_message_queue.exe");
    let native = Command::new(&exe).status().unwrap();
    assert_eq!(native.code(), Some(0), "native Windows check failed");
    for jit in [false, true] {
        let mut runtime = Runtime::new(
            &exe,
            output.join(if jit { "jit" } else { "uc" }),
            jit,
            32,
            32,
        )
        .unwrap();
        runtime.machine.sys.clock = Clock::virtual_time();
        runtime
            .machine
            .set_execution_budget(Some(Duration::from_secs(2)));
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(
                Instant::now() < until,
                "message fixture exceeded wall deadline"
            );
            match runtime.step_for(Duration::from_millis(8)) {
                SliceResult::Yielded => {}
                SliceResult::Complete(RunResult::Idle {
                    next_deadline: Some(deadline),
                }) => {
                    runtime.machine.sys.clock.advance_to(deadline);
                }
                SliceResult::Complete(RunResult::Exited(code)) => {
                    assert_eq!(code, 0, "guest native-compatible check failed, jit={jit}");
                    break;
                }
                other => panic!("unexpected guest queue result, jit={jit}: {other:?}"),
            }
        }
    }
}

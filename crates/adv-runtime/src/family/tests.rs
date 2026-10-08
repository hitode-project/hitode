//! Failure paths are tested with an authored loop PE and no native tools.
use super::*;
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt},
};

fn fixture() -> (Runtime<JitCpu>, Spawn, std::path::PathBuf) {
    let mut pe = vec![0u8; 1024];
    let put16 = |pe: &mut [u8], offset: usize, value: u16| {
        pe[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    };
    let put32 = |pe: &mut [u8], offset: usize, value: u32| {
        pe[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    };
    put16(&mut pe, 0, 0x5a4d);
    put32(&mut pe, 0x3c, 0x80);
    pe[0x80..0x84].copy_from_slice(b"PE\0\0");
    for (offset, value) in [
        (0x84, 0x14c),
        (0x86, 1),
        (0x94, 224),
        (0x96, 0x102),
        (0x98, 0x10b),
    ] {
        put16(&mut pe, offset, value);
    }
    for (offset, value) in [
        (0x9c, 0x200),
        (0xa8, 0x1000),
        (0xac, 0x1000),
        (0xb4, 0x0040_0000),
        (0xb8, 4096),
        (0xbc, 512),
        (0xd0, 0x2000),
        (0xd4, 0x200),
        (0xe0, 0x100000),
        (0xe4, 4096),
        (0xe8, 0x100000),
        (0xec, 4096),
        (0xf4, 16),
        (0x180, 2),
        (0x184, 0x1000),
        (0x188, 0x200),
        (0x18c, 0x200),
        (0x19c, 0x60000020),
    ] {
        put32(&mut pe, offset, value);
    }
    put16(&mut pe, 0xc0, 4);
    put16(&mut pe, 0xc8, 4);
    put16(&mut pe, 0xdc, 3);
    pe[0x178..0x17d].copy_from_slice(b".text");
    pe[0x200..0x202].copy_from_slice(&[0xeb, 0xfe]); // jmp $

    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "adv-process-family-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&path).unwrap();
    let exe = path.join("loop.exe");
    std::fs::write(&exe, pe).unwrap();
    let runtime = Runtime::new(&exe, path.join("data"), false, 4, 2).unwrap();
    let config = runtime.machine.ext.get::<Win32>().cfg.os.clone();
    let spec = Spawn {
        parent: runtime.machine.sys.pid,
        directory: config.guest_root.clone(),
        command_line: "loop.exe".into(),
        environment: Vec::new(),
        config,
        suspended: false,
        inherited: Vec::new(),
    };
    (runtime, spec, exe)
}
fn cleanup(exe: std::path::PathBuf) {
    std::fs::remove_file(&exe).unwrap();
    let _ = std::fs::remove_dir(exe.parent().unwrap());
}
fn pump(runtime: &mut Runtime<JitCpu>) {
    for _ in 0..4 {
        assert!(!matches!(
            runtime.step_for(Duration::from_millis(2)),
            SliceResult::Complete(RunResult::Failed(_) | RunResult::Exited(_))
        ));
    }
}

#[test]
fn ordinary_process_receives_the_requested_slice_before_any_child_exists() {
    let (mut runtime, _, exe) = fixture();
    runtime.machine.set_execution_budget(None);
    // Warm the authored infinite loop before measuring its scheduling boundary.
    assert!(matches!(
        runtime.step_for(Duration::from_millis(10)),
        SliceResult::Yielded
    ));
    let requested = Duration::from_millis(40);
    let started = Instant::now();
    assert!(matches!(runtime.step_for(requested), SliceResult::Yielded));
    // A busy runnable guest must consume the caller's slice. This is a lower
    // bound only: compilation and host scheduling may legitimately overrun it.
    assert!(started.elapsed() >= requested);
    assert_eq!(runtime.process_statuses().len(), 1);
    assert_eq!(runtime.process_statuses()[0].exit_code, None);
    cleanup(exe);
}

#[test]
fn ordinary_process_keeps_its_terminal_result_on_repeated_steps() {
    let (mut runtime, _, exe) = fixture();
    runtime.machine.sys.terminate_process_stopped(17);
    for _ in 0..2 {
        assert!(matches!(
            runtime.step_for(Duration::from_millis(10)),
            SliceResult::Complete(RunResult::Exited(17))
        ));
        assert!(matches!(runtime.step(), RunResult::Exited(17)));
    }
    assert_eq!(runtime.process_statuses()[0].exit_code, Some(17));
    cleanup(exe);

    let (mut runtime, _, exe) = fixture();
    runtime
        .machine
        .cpu
        .write(0x401000, &[0xc7, 0x05, 0, 0, 0, 0, 1, 0, 0, 0])
        .unwrap();
    for _ in 0..2 {
        assert!(matches!(runtime.step(), RunResult::Failed(_)));
        assert!(matches!(
            runtime.step_for(Duration::from_millis(10)),
            SliceResult::Complete(RunResult::Failed(_))
        ));
    }
    assert_eq!(runtime.process_statuses()[0].exit_code, Some(0xc0000005));
    cleanup(exe);
}

#[test]
fn unpublished_and_abandoned_creation_never_executes_and_releases_address_space() {
    let (mut runtime, spec, exe) = fixture();
    let created = runtime.create_child(spec).unwrap();
    let pid = created.process.pid;
    pump(&mut runtime);
    assert_eq!(runtime.family.parked[&pid].sys.host_calls, 0);
    assert!(runtime.family.prepared.contains_key(&pid));
    drop(created); // The caller thread can exit while its continuation owns this lease.
    pump(&mut runtime);
    assert_eq!(runtime.family.records[1].exit_code, Some(995));
    assert!(!runtime.family.parked.contains_key(&pid));
    assert!(!runtime.family.prepared.contains_key(&pid));
    cleanup(exe);
}

struct PublishAndWait {
    ticket: Rc<Cell<processes::Publication>>,
    process: u32,
}
fn publish_and_wait(env: &mut adv_machine::Env<'_>) -> adv_machine::Outcome {
    let publication = env.ext.get::<PublishAndWait>();
    publication.ticket.set(processes::Publication::Published);
    adv_machine::Outcome::block_then_return(
        adv_machine::Wait::Object {
            handle: publication.process,
            deadline: Some(env.sys.clock.now_us() + 5_000_000),
        },
        0,
    )
}
fn call_and_loop(machine: &mut Machine<JitCpu>, entry: u32) {
    let mut code = vec![0xb8]; // mov eax, entry; call eax; jmp $
    code.extend_from_slice(&entry.to_le_bytes());
    code.extend_from_slice(&[0xff, 0xd0, 0xeb, 0xfe]);
    machine.cpu.write(0x401000, &code).unwrap();
    machine.cpu.set_reg(Reg::Eip, 0x401000);
}
fn install_call(machine: &mut Machine<JitCpu>, name: &str, handler: adv_machine::Handler) {
    machine
        .sys
        .thunks
        .resolver_mut::<adv_abi::Registry>()
        .unwrap()
        .add(
            "family-test",
            name,
            adv_machine::Conv::Cdecl { argc: 0 },
            handler,
        );
    let entry = machine.resolve_import("family-test", name).unwrap();
    call_and_loop(machine, entry);
}

#[test]
fn publication_after_a_pending_visit_wakes_only_runnable_children() {
    for suspended in [false, true] {
        let (mut runtime, mut spec, exe) = fixture();
        // Finish startup before installing the parent's authored publisher.
        assert!(matches!(
            runtime.machine.run_slice(Duration::from_millis(2)),
            SliceResult::Yielded
        ));
        spec.suspended = suspended;
        let created = runtime.create_child(spec).unwrap();
        let pid = created.process.pid;
        let process = runtime
            .machine
            .sys
            .objects
            .insert(adv_machine::Object::Process {
                identity: created.process.clone(),
                access: 0x100000,
            });
        runtime.machine.ext.insert(PublishAndWait {
            ticket: created.publication.clone(),
            process,
        });
        install_call(&mut runtime.machine, "publish_and_wait", publish_and_wait);
        // The child is considered first while Pending; the parent's handler
        // publishes it later in the same fairness round and blocks for it.
        runtime.family.last = runtime.machine.sys.pid;
        let result = runtime.step_for(Duration::from_millis(40));
        assert_eq!(created.publication.get(), processes::Publication::Published);
        assert_eq!(runtime.family.parked[&pid].sys.host_calls, 0);
        if suspended {
            assert!(matches!(
                result,
                SliceResult::Complete(RunResult::Idle {
                    next_deadline: Some(_)
                })
            ));
        } else {
            assert!(matches!(result, SliceResult::Yielded));
            let _ = runtime.step_for(Duration::from_millis(2));
            assert!(!runtime.family.prepared.contains_key(&pid));
        }
        drop(created);
        cleanup(exe);
    }
}

#[test]
fn child_exit_revisits_a_parent_wait_before_host_sleep() {
    let (mut runtime, spec, exe) = fixture();
    assert!(matches!(
        runtime.machine.run_slice(Duration::from_millis(2)),
        SliceResult::Yielded
    ));
    let created = runtime.create_child(spec).unwrap();
    let pid = created.process.pid;
    let process = runtime
        .machine
        .sys
        .objects
        .insert(adv_machine::Object::Process {
            identity: created.process.clone(),
            access: 0x100000,
        });
    runtime.machine.ext.insert(PublishAndWait {
        ticket: created.publication.clone(),
        process,
    });
    created.publication.set(processes::Publication::Published);
    install_call(&mut runtime.machine, "wait_for_child", publish_and_wait);
    let child = runtime.family.parked.get_mut(&pid).unwrap();
    install_call(child, "exit_child", |_| {
        adv_machine::Outcome::ExitProcess(37)
    });
    // The parent first blocks; the child exits later in the same round.
    runtime.family.last = pid;
    let result = runtime.step_for(Duration::from_millis(40));
    assert_eq!(created.process.completion.exit_code(), Some(37));
    assert!(matches!(result, SliceResult::Yielded));
    let _ = runtime.step_for(Duration::from_millis(40));
    runtime.select_process(runtime.family.records[0].pid);
    assert!(
        runtime.machine.sys.contexts[0].blocked_thunk().is_none(),
        "parent state: {}, wait: {:?}",
        runtime.machine.sys.contexts[0].state_name(),
        runtime.machine.sys.contexts[0].waiting_on()
    );
    drop(created);
    cleanup(exe);
}

#[test]
fn failed_publication_reaps_even_when_the_parent_remains_live() {
    let (mut runtime, spec, exe) = fixture();
    let created = runtime.create_child(spec).unwrap();
    let pid = created.process.pid;
    runtime
        .family
        .parked
        .get_mut(&pid)
        .unwrap()
        .sys
        .terminate_process_stopped(8);
    drop(created);
    pump(&mut runtime);
    assert_eq!(runtime.family.records[1].exit_code, Some(8));
    assert!(!runtime.family.parked.contains_key(&pid));
    assert_eq!(runtime.family.records[0].exit_code, None);
    cleanup(exe);
}

#[test]
fn malformed_remote_headers_end_only_the_child_and_retain_diagnostics() {
    let (mut runtime, spec, exe) = fixture();
    let created = runtime.create_child(spec).unwrap();
    let pid = created.process.pid;
    runtime
        .family
        .parked
        .get_mut(&pid)
        .unwrap()
        .cpu
        .write_u32(0x400000 + 0x80, 0)
        .unwrap();
    created.publication.set(processes::Publication::Published);
    drop(created);
    pump(&mut runtime);
    assert_eq!(runtime.family.records[1].exit_code, Some(0xc0000142));
    assert!(
        runtime.family.records[1]
            .failure
            .as_ref()
            .unwrap()
            .contains("invalid mapped")
    );
    assert_eq!(runtime.family.records[0].exit_code, None);
    assert!(!runtime.api_acceptance_eligible());
    runtime.machine.sys.terminate_process_stopped(0);
    assert!(matches!(
        runtime.step_for(Duration::from_millis(2)),
        SliceResult::Complete(RunResult::Failed(_))
    ));
    cleanup(exe);
}

#[test]
fn structural_mismatch_in_a_child_disqualifies_the_family_after_retirement() {
    let (mut runtime, spec, exe) = fixture();
    let parent = runtime.machine.sys.pid;
    let created = runtime.create_child(spec).unwrap();
    let pid = created.process.pid;
    let diagnostics = runtime
        .family
        .parked
        .get_mut(&pid)
        .unwrap()
        .ext
        .get_or_default::<adv_abi::ApiDiagnostics>();
    diagnostics.structural_mismatches = 1;
    diagnostics.warnings.insert(
        (
            "fixture.dll!frontend".into(),
            "authored structural mismatch".into(),
        ),
        1,
    );
    assert!(!runtime.api_acceptance_eligible());
    assert_eq!(runtime.process_structural_diagnostics(pid).0, 1);
    created.publication.set(processes::Publication::Published);
    drop(created);
    assert!(runtime.select_process(pid));
    runtime.record_exit(0, None);
    assert!(runtime.select_process(parent));
    runtime.reap();
    assert!(!runtime.family.parked.contains_key(&pid));
    assert!(!runtime.api_acceptance_eligible());
    let (count, warnings) = runtime.process_structural_diagnostics(pid);
    assert_eq!(count, 1);
    assert_eq!(
        warnings,
        vec![(
            "fixture.dll!frontend".into(),
            "authored structural mismatch".into(),
            1
        )]
    );
    cleanup(exe);
}

#[test]
fn live_process_limit_fails_without_publishing_an_extra_machine() {
    let (mut runtime, spec, exe) = fixture();
    let mut children = Vec::new();
    for _ in 1..processes::MAX_LIVE_PROCESSES {
        children.push(runtime.create_child(spec.clone()).unwrap());
    }
    assert_eq!(runtime.create_child(spec.clone()).err().unwrap().code, 8);
    assert_eq!(
        runtime.family.parked.len(),
        processes::MAX_LIVE_PROCESSES - 1
    );
    drop(children);
    pump(&mut runtime);
    assert!(runtime.family.parked.is_empty());
    let replacement = runtime.create_child(spec).unwrap();
    assert!(replacement.process.pid > 0x80000);
    drop(replacement);
    cleanup(exe);
}

#[test]
fn unhandled_child_exception_preserves_native_status_while_parent_runs() {
    let (mut runtime, spec, exe) = fixture();
    let created = runtime.create_child(spec).unwrap();
    let pid = created.process.pid;
    // MOV dword ptr [0], 1: an actual guest write access violation.
    runtime
        .family
        .parked
        .get_mut(&pid)
        .unwrap()
        .cpu
        .write(0x401000, &[0xc7, 0x05, 0, 0, 0, 0, 1, 0, 0, 0])
        .unwrap();
    created.publication.set(processes::Publication::Published);
    drop(created);
    pump(&mut runtime);
    assert_eq!(runtime.family.records[1].exit_code, Some(0xc0000005));
    assert!(
        runtime.family.records[1]
            .failure
            .as_ref()
            .unwrap()
            .contains("unhandled Windows exception")
    );
    assert_eq!(runtime.family.records[0].exit_code, None);
    let record = &runtime.family.records[1];
    let first = record
        .first_cpu_fault
        .as_ref()
        .expect("retired child fault evidence");
    assert_eq!(first.fault.pc, 0x401000);
    assert_eq!(first.access(), Some(("write", 0)));
    assert_eq!(record.latest_cpu_fault.as_ref(), Some(first));
    assert!(runtime.family.records[0].first_cpu_fault.is_none());
    assert!(
        runtime.machine.sys.first_cpu_fault.is_none(),
        "child evidence must not become parent state"
    );
    assert!(
        !runtime.family.parked.contains_key(&pid),
        "evidence must survive child retirement"
    );
    cleanup(exe);
}

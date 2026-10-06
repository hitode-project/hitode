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
    cleanup(exe);
}

//! NT call packages through Wine's Win32 DLLs in headless sessions.
#![cfg(any(target_vendor = "apple", target_os = "android"))]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use adv_runtime::wine::session::{Config, Session, Stop};

/// Runs the probe `exe` and returns how it stopped and what it printed, or
/// `None` in the parent process and when no Wine dist is configured. Each
/// package runs in its own process: the in-process Wine server is global.
fn probe(test: &str, exe: &str, setup: impl FnOnce(&mut Session, &Path)) -> Option<(Stop, String)> {
    if std::env::var_os("ADV_NT_PROBE_CHILD").is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--nocapture"])
            .env("ADV_NT_PROBE_CHILD", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return None;
    }
    let (Some(dist), Some(probes)) = (
        std::env::var_os("ADV_WINE_DIST").map(PathBuf::from),
        std::env::var_os("ADV_WINE_PROBES").map(PathBuf::from),
    ) else {
        eprintln!("skipped: ADV_WINE_DIST or ADV_WINE_PROBES is not set");
        return None;
    };
    let root = std::env::temp_dir().join(format!("adv-nt-{test}-{}", std::process::id()));
    let game = root.join("game");
    std::fs::create_dir_all(&game).unwrap();
    std::fs::copy(probes.join(exe), game.join(exe)).unwrap();
    let stdout = std::fs::File::create(root.join("stdout")).unwrap();
    let config = Config {
        dist: &dist,
        prefix: &root.join("session"),
        drive: None,
        locale: 0x409,
        fonts: "",
        exe: &game.join(exe),
        width: 640,
        height: 480,
        jit: true,
        debug: "",
        stdout: Some(std::os::fd::AsRawFd::as_raw_fd(&stdout)),
    };
    let wake = adv_sync::WakeEvent::new().unwrap();
    let mut session = Session::start(&config, wake, |_| Ok(())).unwrap();
    setup(&mut session, &game);
    let stop = session.run(Instant::now() + Duration::from_secs(60));
    drop(session);
    drop(stdout);
    let text = std::fs::read_to_string(root.join("stdout")).unwrap();
    std::fs::remove_dir_all(&root).unwrap();
    Some((stop, text))
}

/// Asserts that every `check` printed `<check><ok>` on a line of its own.
fn assert_checks(text: &str, ok: &str, checks: &[&str]) {
    for check in checks {
        assert!(
            text.lines().any(|line| line == format!("{check}{ok}")),
            "{check}: {text}"
        );
    }
}

// spec: NT-PROC-1, NT-PROC-2, NT-PROC-3
#[test]
fn l_process() {
    let Some((stop, text)) = probe("l_process", "nt_l_process.exe", |session, game| {
        // The hive operations require enabled backup and restore privileges.
        session
            .machine
            .visit_context(None, |env| {
                use adv_nt::{protocol, server, status};
                let mut open = protocol::OpenTokenRequest {
                    handle: u32::MAX,
                    access: 0x20,
                    ..Default::default()
                };
                let (token, _) = server::request(env, &mut open, &[], &mut []).unwrap();
                status::check(token.header.error).unwrap();
                let privileges: Vec<u8> = [17u32, 0, 2, 18, 0, 2]
                    .iter()
                    .flat_map(|value| value.to_le_bytes())
                    .collect();
                let mut adjust = protocol::AdjustTokenPrivilegesRequest {
                    handle: token.token,
                    ..Default::default()
                };
                let (reply, _) =
                    server::request(env, &mut adjust, &[&privileges], &mut []).unwrap();
                status::check(reply.header.error).unwrap();
                server::close(env, token.token).unwrap();
                let attributes = adv_nt::object::Attributes {
                    root: 0,
                    attributes: 0,
                    name: None,
                    security: Vec::new(),
                };
                let mut allocate = protocol::AllocateReserveObjectRequest {
                    r#type: 1,
                    ..Default::default()
                };
                let (reserve, _) =
                    server::request(env, &mut allocate, &[&attributes.wire()], &mut []).unwrap();
                status::check(reserve.header.error).unwrap();
                std::fs::write(game.join("nt_l_reserve.bin"), reserve.handle.to_le_bytes())
                    .unwrap();
                // The fixture can resume this sole process while its guest is suspended.
                assert!(matches!(
                    adv_nt::process::suspend(env, &[u32::MAX]),
                    adv_machine::Outcome::Return(0)
                ));
                assert!(
                    env.sys
                        .contexts
                        .iter()
                        .filter(|context| !context.is_done())
                        .all(|context| context.suspend_count == 1)
                );
                assert!(matches!(
                    adv_nt::process::resume(env, &[u32::MAX]),
                    adv_machine::Outcome::Return(0)
                ));
                assert!(
                    env.sys
                        .contexts
                        .iter()
                        .all(|context| context.suspend_count == 0)
                );
            })
            .unwrap();
    }) else {
        return;
    };
    assert!(matches!(stop, Stop::Exited(37)), "{stop:?}: {text}");
    let checks = [
        "registry_create",
        "open_transacted",
        "open_transacted_ex",
        "registry_rename",
        "registry_save",
        "registry_load",
        "registry_unload",
        "registry_load2",
        "registry_load_ex",
        "event_clear",
        "alert_resume",
        "queue_apc_ex",
        "continue_ex",
        "continue_ex_alert",
        "processor_number",
        "process_suspend_access",
        "process_suspend_invalid",
        "process_resume",
        "process_next",
        "completion_query",
        "completion_ex_reserve",
        "completion_ex_post",
        "job_membership_before",
        "job_set_limits",
        "job_completion",
        "job_assign",
        "job_notification",
        "job_accounting",
        "job_process_ids",
        "job_query_limits",
        "job_terminate_empty",
        "job_terminate_self",
    ];
    let expected: Vec<_> = checks.iter().map(|name| format!("{name}=1")).collect();
    assert_eq!(text.lines().collect::<Vec<_>>(), expected);
}

// spec: NT-FILE-1, NT-FILE-2, NT-FILE-3, WINE-THR-1
#[test]
fn j_file() {
    let Some((stop, text)) = probe("j_file", "nt_j_file.exe", |_, _| {}) else {
        return;
    };
    assert!(matches!(stop, Stop::Exited(0)), "{stop:?}: {text}");
    assert_checks(
        &text,
        "=ok",
        &[
            "flush_file",
            "flush_ex",
            "flush_access",
            "lock",
            "lock_conflict",
            "lock_wait",
            "unlock",
            "lock_woken",
            "delete",
            "mailslot",
            "notify_filter",
            "notify",
            "cancel",
            "cancel_ex",
            "cancel_ex_result",
            "cancel_sync",
            "gather",
            "scatter",
            "device_invalid",
            "device_disk",
            "device_server",
            "flush_pipe",
            "pipe_read",
        ],
    );
}

// spec: NT-SEC-1, NT-SEC-2, NT-SEC-3, WINE-SYS-11, WINE-SES-1
#[test]
fn k_security() {
    let Some((stop, text)) = probe("k_security", "nt_security.exe", |_, _| {}) else {
        return;
    };
    assert!(matches!(stop, Stop::Exited(0)), "{stop:?}: {text}");
    let expected = [
        "open_process",
        "open_process_ex",
        "fixed_user",
        "duplicate_token",
        "open_thread_empty",
        "set_thread_token",
        "open_thread",
        "open_thread_ex",
        "revert",
        "lookup_privilege",
        "adjust_privileges",
        "privilege_disabled",
        "restore_privileges",
        "privilege_enabled",
        "filter_token",
        "filtered_privilege",
        "default_dacl_init",
        "set_token_dacl",
        "token_dacl_roundtrip",
        "set_token_short",
        "world_sid",
        "security_init",
        "access_allowed",
        "access_denied",
        "access_short",
        "create_event",
        "set_security",
        "query_security_short",
        "query_security",
        "duplicate_handle",
        "compare_objects",
        "make_permanent",
        "permanent_lifetime",
        "make_temporary",
        "temporary_lifetime",
        "logical_drives",
        "dos_device_directory",
        "dos_device_create",
        "dos_device_list",
        "dos_device_write",
        "dos_device_attributes",
        "dos_device_missing_path",
        "dos_device_read",
        "dos_device_volume",
        "dos_device_rename",
        "dos_device_delete",
        "dos_device_remove",
        "dos_device_removed",
        "dos_device_cleanup",
        "token_id",
        "create_token",
        "created_user",
        "invalid_primary_group",
        "reserve_object",
        "reserve_invalid",
    ];
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(lines, expected.map(|name| format!("{name} ok")), "{text}");
}

// spec: NT-SYS-1, NT-SYS-2, NT-SYS-3, NT-SYS-4
#[test]
fn m_system() {
    let Some((stop, text)) = probe("m_system", "nt_m_system.exe", |_, _| {}) else {
        return;
    };
    assert!(matches!(stop, Stop::Exited(0)), "{stop:?}: {text}");
    assert_checks(
        &text,
        ": ok",
        &[
            "create-section-ex",
            "map-view-ex-shared",
            "map-view-ex-alignment",
            "prefetch-memory",
            "shared-virtual-lock",
            "shared-virtual-unlock",
            "unmap-view-ex",
            "map-view-ex-placeholder",
            "unmap-preserve-placeholder",
            "flush-view",
            "flush-file-data",
            "virtual-lock",
            "virtual-unlock",
            "flush-write-buffers",
            "execution-state-set",
            "execution-state-previous",
            "set-default-locale",
            "system-locale-isolation",
            "set-system-locale",
            "user-locale-isolation",
            "license-size",
            "license-registry-data",
            "wow64-allocate-validation",
            "wow64-allocate",
            "wow64-query-process",
            "wow64-read-empty",
            "wow64-write-empty",
            "wow64-read-access",
            "wow64-write-access",
            "debug-attach-self",
            "debug-kill-on-exit",
            "debug-continue-invalid",
            "debug-detach-unattached",
            "debug-wait-timeout",
        ],
    );
}

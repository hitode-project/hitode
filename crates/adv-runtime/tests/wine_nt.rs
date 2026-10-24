//! NT call probes through Wine's Windows DLLs, in headless sessions.
#![cfg(any(target_vendor = "apple", target_os = "android"))]

use std::fs::File;
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use adv_runtime::wine::session::{Config, Session, Stop};

// spec: WINE-NT-201, WINE-NT-202, WINE-NT-203
#[test]
fn security_tokens_and_objects() {
    let (Some(dist), Some(probes)) = (
        std::env::var_os("ADV_WINE_DIST").map(PathBuf::from),
        std::env::var_os("ADV_WINE_PROBES").map(PathBuf::from),
    ) else {
        eprintln!("skipped: ADV_WINE_DIST or ADV_WINE_PROBES is not set");
        return;
    };
    // Each server owns process globals; isolate this package from other probes.
    if std::env::var_os("ADV_NT_SECURITY_CHILD").is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "security_tokens_and_objects", "--nocapture"])
            .env("ADV_NT_SECURITY_CHILD", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let root = std::env::temp_dir().join(format!("adv-wine-nt-security-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let exe = probes.join("nt_security.exe");
    let prefix = root.join("prefix");
    let stdout = File::create(root.join("stdout")).unwrap();
    let config = Config {
        dist: &dist,
        prefix: &prefix,
        drive: None,
        locale: 0x409,
        fonts: "",
        exe: &exe,
        width: 640,
        height: 480,
        jit: true,
        debug: "",
        stdout: Some(stdout.as_raw_fd()),
    };
    let wake = adv_sync::WakeEvent::new().unwrap();
    let mut session = Session::start(&config, wake, |_| Ok(())).unwrap();
    let stop = session.run(Instant::now() + Duration::from_secs(60));
    drop(session);
    drop(stdout);
    let text = std::fs::read_to_string(root.join("stdout")).unwrap();
    std::fs::remove_dir_all(&root).unwrap();
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
        "dos_device_create",
        "dos_device_remove",
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

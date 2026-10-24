//! NT call packages through Wine's Win32 DLLs in headless sessions.
#![cfg(any(target_vendor = "apple", target_os = "android"))]

// spec: WINE-NT-301, WINE-NT-302, WINE-NT-303
#[test]
fn l_process() {
    // Each package gets its own process: the server keeps global state.
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "l_process_probe", "--ignored", "--nocapture"])
        .status()
        .unwrap();
    assert!(status.success());
}

// spec: WINE-NT-301, WINE-NT-302, WINE-NT-303
#[test]
#[ignore = "run by l_process in an isolated process"]
fn l_process_probe() {
    use std::fs::File;
    use std::io::Read;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use adv_runtime::wine::session::{Config, Session, Stop};

    let (Some(dist), Some(probes)) = (
        std::env::var_os("ADV_WINE_DIST").map(PathBuf::from),
        std::env::var_os("ADV_WINE_PROBES").map(PathBuf::from),
    ) else {
        eprintln!("skipped: ADV_WINE_DIST or ADV_WINE_PROBES is not set");
        return;
    };
    let root = std::env::temp_dir().join(format!("adv-nt-l-process-{}", std::process::id()));
    let game = root.join("game");
    std::fs::create_dir_all(&game).unwrap();
    let exe = game.join("nt_l_process.exe");
    std::fs::copy(probes.join("nt_l_process.exe"), &exe).unwrap();
    let mut fds = [0; 2];
    // SAFETY: fds has room for both descriptors.
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    // SAFETY: these new descriptors have no other owners.
    let (mut output, stdout) = unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) };
    let prefix = root.join("session");
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
            let (reply, _) = server::request(env, &mut adjust, &[&privileges], &mut []).unwrap();
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
            std::fs::write(game.join("nt_l_reserve.bin"), reserve.handle.to_le_bytes()).unwrap();
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
    drop(stdout);
    let stop = session.run(Instant::now() + Duration::from_secs(60));
    drop(session);
    // SAFETY: output owns the read descriptor.
    assert_eq!(
        unsafe { libc::fcntl(output.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) },
        0
    );
    let mut text = String::new();
    let _ = output.read_to_string(&mut text);
    std::fs::remove_dir_all(&root).unwrap();
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

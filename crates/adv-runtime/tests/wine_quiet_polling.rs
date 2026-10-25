//! Quiet Wine message/clock polling and real notification paths, without a host window.
#![cfg(any(target_vendor = "apple", target_os = "android"))]
use adv_cpu::backend::GuestExt;
use adv_machine::{Outcome, RunResult, SliceResult};
use adv_runtime::wine::session::{Config, Session, Stop};
use std::{
    fs::File,
    io::Read,
    os::fd::{AsRawFd, FromRawFd},
    path::PathBuf,
    time::{Duration, Instant},
};

fn until(session: &mut Session, output: &mut File, text: &mut String, expected: &str) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !text.contains(expected) {
        let stop = session.run(Instant::now() + Duration::from_millis(10));
        let _ = output.read_to_string(text);
        assert!(matches!(stop, Stop::Paused), "{stop:?}: {text}");
        assert!(Instant::now() < deadline, "missing {expected}: {text}");
    }
}

// spec: PROC-CLOCK-7, PROC-CLOCK-3, PROC-CLOCK-5, PROC-CLOCK-6
#[test]
fn wine_peek_and_time_loop_idles_and_notifications_wake_it() {
    let (Some(dist), Some(probes)) = (
        std::env::var_os("ADV_WINE_DIST").map(PathBuf::from),
        std::env::var_os("ADV_WINE_PROBES").map(PathBuf::from),
    ) else {
        eprintln!("skipped: ADV_WINE_DIST or ADV_WINE_PROBES is not set");
        return;
    };
    let root = std::env::temp_dir().join(format!("adv-quiet-poll-{}", std::process::id()));
    let mut fds = [0; 2];
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    let (mut output, stdout) = unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) };
    assert_eq!(
        unsafe { libc::fcntl(output.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) },
        0
    );
    let config = Config {
        dist: &dist,
        prefix: &root,
        drive: None,
        locale: 0x409,
        fonts: "",
        exe: &probes.join("quiet_poll.exe"),
        width: 640,
        height: 480,
        jit: true,
        debug: "",
        stdout: Some(stdout.as_raw_fd()),
    };
    let wake = adv_sync::WakeEvent::new().unwrap();
    let mut session = Session::start(&config, wake.clone(), |_| Ok(())).unwrap();
    drop(stdout);
    let mut text = String::new();
    until(&mut session, &mut output, &mut text, "deadline=");
    let ready = text
        .lines()
        .find(|line| line.starts_with("ready="))
        .unwrap();
    let fields: Vec<_> = ready
        .split_whitespace()
        .map(|field| u32::from_str_radix(field.split_once('=').unwrap().1, 16).unwrap())
        .collect();
    let elapsed: u32 = text
        .lines()
        .find_map(|line| line.strip_prefix("deadline="))
        .unwrap()
        .parse()
        .unwrap();
    assert!(elapsed >= 100);
    assert!(session.control_metrics().idle_waits > 0);
    let before = session.machine.cpu.read_u32(fields[4]).unwrap();
    wake.take_interrupt();
    let result = session.machine.run_slice(Duration::from_millis(100));
    assert!(
        matches!(result, SliceResult::Complete(RunResult::Idle { .. })),
        "{result:?}"
    );
    let after = session.machine.cpu.read_u32(fields[4]).unwrap();
    assert!(
        after - before < 10,
        "quiet round took {} peeks",
        after - before
    );
    for source in 1..=6 {
        match source {
            1 => session.input(0x100, 0x41, 0x001e0001).unwrap(),
            2 => adv_drv::user::post_message(&mut session.machine.ext, [fields[0], 0x8000, 2, 0]),
            3 | 5 => {
                session
                    .machine
                    .visit_context(None, |env| {
                        let handle = fields[if source == 3 { 1 } else { 3 }];
                        assert!(matches!(
                            adv_nt::sync::set_event(env, &[handle, 0]),
                            Outcome::Return(0)
                        ));
                    })
                    .unwrap();
            }
            4 => {
                session
                    .machine
                    .visit_context(None, |env| {
                        adv_nt::sync::set_audio_event(env, fields[2]).unwrap()
                    })
                    .unwrap();
            }
            6 => adv_drv::user::post_message(&mut session.machine.ext, [fields[0], 0x8001, 0, 0]),
            _ => unreachable!(),
        }
        let started = Instant::now();
        wake.signal();
        if source < 6 {
            until(
                &mut session,
                &mut output,
                &mut text,
                &format!("observed={source}"),
            );
            assert!(
                started.elapsed() < Duration::from_secs(1),
                "notification {source}: {text}"
            );
        }
    }
    let stop = session.run(Instant::now() + Duration::from_secs(10));
    let _ = output.read_to_string(&mut text);
    assert!(matches!(stop, Stop::Exited(0)), "{stop:?}: {text}");
    assert!(text.contains("observed=6"));
    drop(session);
    std::fs::remove_dir_all(root).unwrap();
}

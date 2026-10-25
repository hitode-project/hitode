//! The Win32 conversion exports reach native bodies in a normal Wine process.
#![cfg(any(target_vendor = "apple", target_os = "android"))]
use adv_runtime::wine::session::{Config, Session, Stop};
use std::{
    fs::File,
    os::fd::AsRawFd,
    path::PathBuf,
    time::{Duration, Instant},
};

fn probe(jit: bool) -> Option<String> {
    let (Some(dist), Some(probes)) = (
        std::env::var_os("ADV_WINE_DIST").map(PathBuf::from),
        std::env::var_os("ADV_WINE_PROBES").map(PathBuf::from),
    ) else {
        return None;
    };
    let root = std::env::temp_dir().join(format!("adv-pe-bodies-{}", std::process::id()));
    let game = root.join("game");
    std::fs::create_dir_all(&game).unwrap();
    std::fs::copy(probes.join("pe_bodies.exe"), game.join("pe_bodies.exe")).unwrap();
    let stdout = File::create(root.join("stdout")).unwrap();
    let config = Config {
        dist: &dist,
        prefix: &root.join("prefix"),
        drive: None,
        locale: 0x409,
        fonts: "",
        exe: &game.join("pe_bodies.exe"),
        width: 64,
        height: 32,
        jit,
        debug: "",
        stdout: Some(stdout.as_raw_fd()),
    };
    let mut session = Session::start(&config, adv_sync::WakeEvent::new().unwrap(), |m| {
        m.sys.stats.services_enabled = true;
        Ok(())
    })
    .unwrap();
    let stop = session.run(Instant::now() + Duration::from_secs(60));
    let mut csv = Vec::new();
    session
        .machine
        .sys
        .stats
        .write_host_csv(&session.machine.cpu, &session.machine.sys.thunks, &mut csv)
        .unwrap();
    let csv = String::from_utf8(csv).unwrap();
    if jit {
        for name in ["MultiByteToWideChar", "WideCharToMultiByte"] {
            let row = csv
                .lines()
                .find(|l| l.contains(&format!("\"kernelbase.dll!{name}\"")))
                .unwrap_or_else(|| panic!("no body calls for {name}: {csv}"));
            let leaves: u64 = row.split(',').nth(5).unwrap().parse().unwrap();
            assert!(leaves > 100, "{row}");
        }
    }
    drop(session);
    drop(stdout);
    let output = std::fs::read_to_string(root.join("stdout")).unwrap();
    assert!(matches!(stop, Stop::Exited(0)), "{stop:?}: {output}");
    assert!(output.starts_with("conversion="), "{output}");
    std::fs::remove_dir_all(root).unwrap();
    Some(output)
}

// spec: NAT-BODY-4
#[test]
#[ignore = "interpreter subprocess of code_page_exports_reach_bodies"]
fn code_page_probe_interpreter() {
    if let Some(output) = probe(false) {
        println!("probe-output={}", output.trim());
    }
}

// spec: NAT-BODY-4
#[test]
fn code_page_exports_reach_bodies() {
    if std::env::var_os("ADV_WINE_DIST").is_none() || std::env::var_os("ADV_WINE_PROBES").is_none()
    {
        return;
    }
    let reference = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "code_page_probe_interpreter",
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
    let expected = text
        .lines()
        .find_map(|l| l.strip_prefix("probe-output="))
        .unwrap();
    let actual = probe(true).unwrap();
    assert_eq!(actual.trim(), expected);
}

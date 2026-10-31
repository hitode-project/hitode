//! Win32 conversion results agree in translated and interpreted Wine processes.
#![cfg(any(target_vendor = "apple", target_os = "android"))]
use adv_runtime::wine::session::{Config, Session, Stop};
use std::{
    fs::File,
    os::fd::AsRawFd,
    time::{Duration, Instant},
};

#[allow(dead_code)]
mod wine {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../adv-nt/tests/common/wine.rs"
    ));
}

fn probe(jit: bool) -> Option<String> {
    let wine::Wine { dist, probes } = wine::wine()?;
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
    drop(session);
    drop(stdout);
    let output = std::fs::read_to_string(root.join("stdout")).unwrap();
    assert!(matches!(stop, Stop::Exited(0)), "{stop:?}: {output}");
    assert!(output.starts_with("conversion="), "{output}");
    std::fs::remove_dir_all(root).unwrap();
    Some(output)
}

#[test]
#[ignore = "interpreter subprocess of code_page_results_match_wine"]
fn code_page_probe_interpreter() {
    if let Some(output) = probe(false) {
        println!("probe-output={}", output.trim());
    }
}

#[test]
fn code_page_results_match_wine() {
    if wine::wine().is_none() {
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

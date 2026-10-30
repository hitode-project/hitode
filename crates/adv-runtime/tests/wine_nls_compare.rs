//! Wine's comparison code is the oracle, with direct bodies disabled by tracing.
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
fn probe(reference: bool) -> Option<String> {
    let wine::Wine { dist, probes } = wine::wine()?;
    let root = std::env::temp_dir().join(format!("adv-nls-compare-{}", std::process::id()));
    let game = root.join("game");
    std::fs::create_dir_all(&game).unwrap();
    std::fs::copy(probes.join("nls_compare.exe"), game.join("nls_compare.exe")).unwrap();
    let stdout = File::create(root.join("stdout")).unwrap();
    let config = Config {
        dist: &dist,
        prefix: &root.join("prefix"),
        drive: None,
        locale: 0x411,
        fonts: "",
        exe: &game.join("nls_compare.exe"),
        width: 64,
        height: 32,
        jit: true,
        debug: "",
        stdout: Some(stdout.as_raw_fd()),
    };
    let mut session = Session::start(&config, adv_sync::WakeEvent::new().unwrap(), |machine| {
        if reference {
            machine.sys.trace = adv_machine::Trace::to_writer(Box::new(std::io::sink()));
        }
        Ok(())
    })
    .unwrap();
    let stop = session.run(Instant::now() + Duration::from_secs(180));
    if !reference {
        let images = session.machine.ext.get::<adv_nt::identity::Images>();
        let kb = images
            .0
            .values()
            .find(|image| image.name == "kernelbase.dll")
            .unwrap();
        let table = kb.table().unwrap();
        for name in [
            "CompareStringA",
            "CompareStringW",
            "CompareStringEx",
            "CompareStringOrdinal",
            "lstrcmpA",
            "lstrcmpW",
            "lstrcmpiA",
            "lstrcmpiW",
        ] {
            let entry = kb.base + table.named(name).unwrap().rva;
            assert!(
                session.machine.sys.direct_bodies.contains_key(&entry),
                "{name} is not bound"
            );
        }
    }
    drop(session);
    drop(stdout);
    let output = std::fs::read_to_string(root.join("stdout")).unwrap();
    assert!(
        matches!(stop, Stop::Exited(0)),
        "{stop:?}: {}",
        output.lines().last().unwrap_or("")
    );
    assert!(output.contains("done "), "incomplete probe: {output}");
    std::fs::remove_dir_all(root).unwrap();
    Some(output)
}
#[test]
#[ignore = "oracle subprocess of comparisons_match_wine"]
fn comparison_oracle() {
    if let Some(output) = probe(true) {
        print!("{output}");
    }
}
// spec: NAT-BODY-20
#[test]
fn comparisons_match_wine() {
    if wine::wine().is_none() {
        return;
    }
    let reference = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "comparison_oracle", "--ignored", "--nocapture"])
        .output()
        .unwrap();
    assert!(
        reference.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&reference.stdout),
        String::from_utf8_lossy(&reference.stderr)
    );
    let expected = String::from_utf8(reference.stdout).unwrap();
    let expected: Vec<_> = expected
        .lines()
        .filter(|line| {
            line.starts_with(|ch: char| ch.is_ascii_digit()) || line.starts_with("done ")
        })
        .collect();
    eprintln!("oracle {}", expected.last().unwrap());
    let actual = probe(false).unwrap();
    let actual: Vec<_> = actual.lines().collect();
    assert_eq!(actual.len(), expected.len());
    for (actual, expected) in actual.into_iter().zip(expected) {
        assert_eq!(actual, expected);
    }
}

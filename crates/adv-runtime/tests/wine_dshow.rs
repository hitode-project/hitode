//! The SDK empty-graph fixture uses Wine's COM activation and our DirectShow front.
#![cfg(any(target_vendor = "apple", target_os = "android"))]
use adv_runtime::wine::session::{Config, Session, Stop};
use std::path::PathBuf;
use std::time::{Duration, Instant};

// spec: MEDIA-GRAPH-1
#[test]
fn empty_graph_preserves_identity_state_enumeration_and_event_contract() {
    let (Some(dist), Some(probes)) = (
        std::env::var_os("ADV_WINE_DIST"),
        std::env::var_os("ADV_WINE_PROBES"),
    ) else {
        eprintln!("skipped: ADV_WINE_DIST or ADV_WINE_PROBES is not set");
        return;
    };
    let root = std::env::temp_dir().join(format!("adv-dshow-empty-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let exe = root.join("dshow_empty_graph.exe");
    std::fs::copy(PathBuf::from(probes).join("dshow_empty_graph.exe"), &exe).unwrap();
    let dist = PathBuf::from(dist);
    let config = Config {
        dist: &dist,
        prefix: &root.join("prefix"),
        drive: None,
        locale: 0x409,
        fonts: "",
        exe: &exe,
        width: 640,
        height: 480,
        jit: true,
        debug: "",
        stdout: None,
    };
    let wake = adv_sync::WakeEvent::new().unwrap();
    let mut session = Session::start(&config, wake, |_| Ok(())).unwrap();
    let until = Instant::now() + Duration::from_secs(60);
    let stop = loop {
        let stop = session.run_ui(until);
        if !matches!(stop, Stop::Paused) || Instant::now() >= until {
            break stop;
        }
    };
    drop(session);
    assert!(matches!(stop, Stop::Exited(0)), "{stop:?}");
    let bytes = std::fs::read(root.join("dshow-empty-graph.bin")).unwrap();
    std::fs::remove_dir_all(root).unwrap();
    assert_eq!(bytes.len(), 50 * 4);
    let words: Vec<_> = bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|word| u32::from_le_bytes(*word))
        .collect();
    let mut expected = vec![0, 0, 0];
    for _ in 0..8 {
        expected.extend([0, 0, 1]); // Every graph face returns the canonical IUnknown.
    }
    expected.extend([
        0, 0, 0, // Stop and GetState: stopped.
        0, 1, 0, 0, 0x80070057, 0, 1, 0, // Empty enumerator, Reset, exhausted Skip and Clone.
        0x80040216, 1, // Missing filter clears the output.
        0, 0, 1, // Notification flags round-trip.
        0, 1, 258, 0x80004004, // Unsignaled event handle and empty event queue.
        0x80004002, 1, 0x44534801, // Missing interface and completion marker.
    ]);
    assert_eq!(words, expected);
}

//! Text records (WINE-UNIX-8) from a guest that draws known lines through the
//! GDI text entry points, measures without drawing and fetches a glyph.
#![cfg(any(target_vendor = "apple", target_os = "android"))]

use adv_runtime::wine::session::{Config, Host, SessionOptions, Stop};
use adv_runtime::wine::text_records::{Code, DrawnLine, TextRecord};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

#[allow(dead_code)]
mod wine {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../adv-nt/tests/common/wine.rs"
    ));
}

fn text(line: &DrawnLine) -> String {
    line.glyphs
        .iter()
        .map(|glyph| match glyph.code {
            Code::Unicode(c) => char::from_u32(c).unwrap(),
            code => panic!("{code:?} in a character line"),
        })
        .collect()
}

// spec: WINE-UNIX-8
#[test]
fn drawn_lines_and_produced_glyphs_are_recorded_once_each() {
    let Some(wine::Wine { dist, probes }) = wine::wine() else {
        return;
    };
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/text-records")
        .join(std::process::id().to_string());
    if root.exists() {
        std::fs::remove_dir_all(&root).unwrap();
    }
    let game = root.join("game");
    std::fs::create_dir_all(&game).unwrap();
    std::fs::copy(
        probes.join("text-records.exe"),
        game.join("text-records.exe"),
    )
    .unwrap();
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../adv-device/tests/fixtures");
    let font_path = format!(r"\??\unix{}", fixtures.to_string_lossy().replace('/', "\\"));
    let output = root.join("stdout");
    let stdout = std::fs::File::create(&output).unwrap();
    let config = Config {
        dist: &dist,
        prefix: &root.join("prefix"),
        drive: None,
        locale: 0x411,
        fonts: &font_path,
        exe: Some(&game.join("text-records.exe")),
        width: 640,
        height: 480,
        jit: true,
        debug: "",
        stdout: Some(std::os::fd::AsRawFd::as_raw_fd(&stdout)),
    };
    let mut options = SessionOptions::default();
    options
        .fonts
        .families
        .insert("MS Gothic".into(), "ADV Japanese CFF Fixture".into());
    let mut host = Host::default();
    let mut session = host
        .start(
            &config,
            adv_sync::WakeEvent::new().unwrap(),
            &options,
            |_| Ok(()),
        )
        .unwrap();
    assert_eq!(session.take_text_records(), Default::default());
    session.set_text_records(true);
    let stop = session.run(Instant::now() + Duration::from_secs(40));
    let batch = session.take_text_records();
    drop(session);
    drop(stdout);
    assert!(
        matches!(stop, Stop::Exited(0)),
        "{stop:?}: {}",
        std::fs::read_to_string(output).unwrap()
    );
    let words: Vec<u32> = std::fs::read(game.join("text-records.bin"))
        .unwrap()
        .chunks(4)
        .map(|word| u32::from_le_bytes(word.try_into().unwrap()))
        .collect();
    let [buffer, bytes, first, second] = words[..] else {
        panic!("{words:?}");
    };

    assert_eq!(batch.dropped, 0);
    let sequences: Vec<_> = batch
        .records
        .iter()
        .map(|record| match record {
            TextRecord::Line(line) => line.sequence,
            TextRecord::Glyph(glyph) => glyph.sequence,
        })
        .collect();
    assert!(
        sequences.windows(2).all(|pair| pair[0] < pair[1]),
        "{sequences:?}"
    );
    let lines: Vec<_> = batch
        .records
        .iter()
        .filter_map(|record| match record {
            TextRecord::Line(line) => Some(line),
            TextRecord::Glyph(_) => None,
        })
        .collect();
    // Measurement draws nothing; each call or wrapped line draws once.
    assert_eq!(lines.len(), 5, "{lines:#?}");
    let characters: Vec<_> = lines[..4].iter().map(|line| text(line)).collect();
    assert_eq!(characters, ["\u{3042}A", "\u{3042}B", "C", "D"]);
    let indices: Vec<_> = lines[4].glyphs.iter().map(|glyph| glyph.code).collect();
    assert_eq!(indices, [Code::Index(first), Code::Index(second)]);
    let dc = lines[0].dc;
    for line in &lines {
        assert_eq!(line.dc, dc);
        assert!(line.surface.is_some());
        assert_ne!(line.thread, 0);
        // The session's family alias already replaced the requested face.
        assert_eq!(
            (line.face.as_str(), line.height),
            ("ADV Japanese CFF Fixture", -16)
        );
        assert!(
            line.glyphs
                .windows(2)
                .all(|pair| pair[0].origin[0] < pair[1].origin[0]),
            "{line:?}"
        );
    }
    assert_eq!(lines[0].glyphs[0].origin[0], 2);
    assert!(lines[1].glyphs[0].origin[1] > lines[0].glyphs[0].origin[1]);
    assert!(lines[3].glyphs[0].origin[1] > lines[2].glyphs[0].origin[1]);
    assert_eq!(lines[4].glyphs[0].origin[0], 80);

    let glyphs: Vec<_> = batch
        .records
        .iter()
        .filter_map(|record| match record {
            TextRecord::Glyph(glyph) => Some(glyph),
            TextRecord::Line(_) => None,
        })
        .collect();
    assert_eq!(glyphs.len(), 1, "{glyphs:#?}");
    assert_eq!(
        (
            glyphs[0].code,
            glyphs[0].buffer,
            glyphs[0].len,
            glyphs[0].dc
        ),
        (Code::Unicode(0x3042), buffer, bytes, dc)
    );
}

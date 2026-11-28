//! Text records (WINE-UNIX-8) from a guest that draws known lines through the
//! GDI text entry points, measures without drawing, fetches a glyph, and moves
//! the drawn text to a window and other DIBs.
#![cfg(any(target_vendor = "apple", target_os = "android"))]

use adv_runtime::wine::session::{Config, Host, SessionOptions, Stop};
use adv_runtime::wine::text_records::{Code, CopiedPixels, CopyKind, DrawnLine, TextRecord};
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
fn drawn_text_is_recorded_where_it_is_drawn_and_wherever_it_is_copied() {
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

    // The guest keeps its window until the host has taken what it needs.
    let report = game.join("text-records.bin");
    let deadline = Instant::now() + Duration::from_secs(40);
    let words: Vec<u32> = loop {
        let stop = session.run(Instant::now() + Duration::from_millis(50));
        let bytes = std::fs::read(&report).unwrap_or_default();
        if bytes.len() == 24 {
            break bytes
                .chunks(4)
                .map(|word| u32::from_le_bytes(word.try_into().unwrap()))
                .collect();
        }
        assert!(
            matches!(stop, Stop::Paused) && Instant::now() < deadline,
            "{stop:?}: {}",
            std::fs::read_to_string(&output).unwrap()
        );
    };
    let [buffer, bytes, first, second, hwnd, converted] = words[..] else {
        panic!("{words:?}");
    };
    let batch = session.take_text_records();
    let windows = session.window_planes();
    drop(session);

    assert_eq!(batch.dropped, 0);
    let sequences: Vec<_> = batch.records.iter().map(TextRecord::sequence).collect();
    assert!(
        sequences.windows(2).all(|pair| pair[0] < pair[1]),
        "{sequences:?}"
    );
    let thread = match &batch.records[0] {
        TextRecord::Line(line) => line.thread,
        other => panic!("{other:?}"),
    };
    assert_ne!(thread, 0);

    let lines: Vec<_> = batch
        .records
        .iter()
        .filter_map(|record| match record {
            TextRecord::Line(line) => Some(line),
            _ => None,
        })
        .collect();
    // Measurement draws nothing; each call or wrapped line draws once.
    assert_eq!(lines.len(), 6, "{lines:#?}");
    let characters: Vec<_> = lines[..4].iter().map(|line| text(line)).collect();
    assert_eq!(characters, ["\u{3042}A", "\u{3042}B", "C", "D"]);
    let indices: Vec<_> = lines[4].glyphs.iter().map(|glyph| glyph.code).collect();
    assert_eq!(indices, [Code::Index(first), Code::Index(second)]);
    assert_eq!(text(lines[5]), "E");
    let dib = lines[0].plane.unwrap();
    for line in &lines {
        assert_eq!(line.thread, thread);
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
    for line in &lines[..5] {
        assert_eq!((line.dc, line.plane), (lines[0].dc, Some(dib)));
    }
    assert_eq!((dib.stride, dib.bpp, dib.size), (640, 32, [160, 96]));
    assert_eq!(lines[0].glyphs[0].origin[0], 2);
    assert!(lines[1].glyphs[0].origin[1] > lines[0].glyphs[0].origin[1]);
    assert!(lines[3].glyphs[0].origin[1] > lines[2].glyphs[0].origin[1]);
    assert_eq!(lines[4].glyphs[0].origin[0], 80);

    let glyphs: Vec<_> = batch
        .records
        .iter()
        .filter_map(|record| match record {
            TextRecord::Glyph(glyph) => Some(glyph),
            _ => None,
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
        (Code::Unicode(0x3042), buffer, bytes, lines[0].dc)
    );

    // The window's surface holds the copied text and the line drawn on it.
    let window = windows
        .iter()
        .find(|plane| plane.hwnd == hwnd)
        .unwrap_or_else(|| panic!("{windows:#?}"));
    assert_eq!(window.client, Some([0, 0, 200, 120]));
    assert_eq!(lines[5].plane.unwrap().bits, window.bits);

    // Only copies out of planes that hold text are kept: not the plain blit.
    let copies: Vec<&CopiedPixels> = batch
        .records
        .iter()
        .filter_map(|record| match record {
            TextRecord::Copy(copy) => Some(copy),
            _ => None,
        })
        .collect();
    let moved: Vec<_> = copies
        .iter()
        .map(|copy| (copy.kind, copy.source.bits, copy.rect, copy.offset))
        .collect();
    assert_eq!(
        moved,
        [
            (CopyKind::Copy, dib.bits, [10, 4, 170, 100], [-10, -4]),
            (CopyKind::Blend, dib.bits, [0, 0, 64, 32], [2, 2]),
            (CopyKind::Convert, dib.bits, [0, 0, 160, 96], [0, 0]),
        ],
        "{copies:#?}"
    );
    assert_eq!(
        (copies[0].destination.bits, copies[0].destination.stride),
        (window.bits, window.stride as i32)
    );
    assert_ne!(copies[1].destination.bits, dib.bits);
    assert_eq!(
        (copies[2].destination.bits, copies[2].destination.bpp),
        (converted, 24)
    );
    assert!(copies.iter().all(|copy| copy.thread == thread));
}

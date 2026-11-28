//! Text records (WINE-UNIX-8) from a guest that draws known lines through the
//! GDI text entry points, measures without drawing, fetches a glyph, and moves
//! the drawn text to a window and other DIBs.
#![cfg(any(target_vendor = "apple", target_os = "android"))]

use adv_runtime::wine::session::{Config, Host, SessionOptions, Stop};
use adv_runtime::wine::text_records::{
    Code, DrawnLine, Map, MoveKind, MovedPixels, Store, TextRecord,
};
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

/// Run the test `child` alone in a new test process, as Wine keeps one
/// server per process.
fn in_child(child: &str) {
    if wine::wine().is_none() {
        return;
    }
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", child, "--nocapture"])
        .status()
        .unwrap();
    assert!(status.success(), "{child}");
}

// spec: WINE-UNIX-8
#[test]
fn drawn_text_is_recorded_where_it_is_drawn_and_wherever_it_is_copied() {
    in_child("drawn_text_is_recorded_where_it_is_drawn_and_wherever_it_is_copied_child");
}

#[test]
#[ignore = "subprocess entry: Wine owns process-global server state"]
fn drawn_text_is_recorded_where_it_is_drawn_and_wherever_it_is_copied_child() {
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
    let cff = fixtures.join("font-resource-japanese-cff.otf");
    std::fs::copy(cff, game.join("cff.otf")).unwrap();
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
        if bytes.len() == 28 {
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
    let [buffer, bytes, first, second, hwnd, converted, dwrite] = words[..] else {
        panic!("{words:?}");
    };
    let batch = session.take_text_records();
    let windows = session.window_planes();
    // Glyph indices turn back into characters through the recorded fonts.
    let mut characters = |font, code| match code {
        Code::Index(glyph) => session.glyph_characters(font, glyph),
        other => panic!("{other:?}"),
    };
    let indexed: Vec<_> = batch
        .records
        .iter()
        .filter_map(|record| match record {
            TextRecord::Line(line) if matches!(line.glyphs[0].code, Code::Index(_)) => Some(
                line.glyphs
                    .iter()
                    .map(|glyph| characters(line.font, glyph.code))
                    .collect::<Vec<_>>(),
            ),
            TextRecord::Glyph(glyph) if glyph.dc.is_none() => {
                Some(vec![characters(glyph.font, glyph.code)])
            }
            _ => None,
        })
        .collect();
    drop(session);
    assert_eq!(indexed[0], [[0x3042], [0x20]]);
    assert!(
        indexed[1..].iter().all(|glyphs| glyphs == &[[0x3042]]),
        "{indexed:?}"
    );
    assert!(indexed.len() > 1, "{indexed:?}");

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
    // GetGlyphOutline's glyph, then the one DirectWrite rasterized.
    let (outline, directwrite) = glyphs.split_first().unwrap();
    assert_eq!(
        (outline.code, outline.buffer, outline.len, outline.dc),
        (Code::Unicode(0x3042), buffer, bytes, Some(lines[0].dc))
    );
    assert!(!directwrite.is_empty(), "{glyphs:#?}");
    for glyph in directwrite {
        assert_eq!((glyph.code, glyph.dc), (Code::Index(dwrite), None));
        assert_ne!(glyph.len, 0);
    }

    // The window's surface holds the copied text and the line drawn on it.
    let window = windows
        .iter()
        .find(|plane| plane.hwnd == hwnd)
        .unwrap_or_else(|| panic!("{windows:#?}"));
    assert_eq!(window.client, Some([0, 0, 200, 120]));
    assert_eq!(lines[5].plane.unwrap().bits, window.bits);

    // Only moves out of stores that hold text are kept: not the plain blit.
    let moves: Vec<&MovedPixels> = batch
        .records
        .iter()
        .filter_map(|record| match record {
            TextRecord::Move(moved) => Some(moved),
            _ => None,
        })
        .collect();
    let memory = |store: Store| match store {
        Store::Memory(plane) => plane,
        other => panic!("{other:?}"),
    };
    let summary: Vec<_> = moves
        .iter()
        .map(|moved| (moved.kind, memory(moved.source).bits, moved.rect, moved.map))
        .collect();
    assert_eq!(
        summary,
        [
            (
                MoveKind::Copy,
                dib.bits,
                [10, 4, 170, 100],
                Some(Map::offset(-10, -4))
            ),
            (
                MoveKind::Blend,
                dib.bits,
                [0, 0, 64, 32],
                Some(Map::offset(2, 2))
            ),
            (
                MoveKind::Convert,
                dib.bits,
                [0, 0, 160, 96],
                Some(Map::offset(0, 0))
            ),
        ],
        "{moves:#?}"
    );
    let [window_copy, blended, converted_copy] = [0, 1, 2].map(|n| memory(moves[n].destination));
    assert_eq!(
        (window_copy.bits, window_copy.stride),
        (window.bits, window.stride as i32)
    );
    assert_ne!(blended.bits, dib.bits);
    assert_eq!((converted_copy.bits, converted_copy.bpp), (converted, 24));
    assert!(moves.iter().all(|moved| moved.thread == thread));
}

// spec: WINE-UNIX-8
#[cfg(feature = "gpu")]
#[test]
fn text_drawn_on_a_d3d9_surface_is_recorded_up_to_the_window() {
    in_child("text_drawn_on_a_d3d9_surface_is_recorded_up_to_the_window_child");
}

#[cfg(feature = "gpu")]
#[test]
#[ignore = "subprocess entry: Wine owns process-global server state"]
fn text_drawn_on_a_d3d9_surface_is_recorded_up_to_the_window_child() {
    use adv_runtime::wine::session::Session;
    let Some(wine::Wine { dist, probes }) = wine::wine() else {
        return;
    };
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/text-records-d3d9")
        .join(std::process::id().to_string());
    if root.exists() {
        std::fs::remove_dir_all(&root).unwrap();
    }
    let game = root.join("game");
    std::fs::create_dir_all(&game).unwrap();
    std::fs::copy(probes.join("d3d9.exe"), game.join("d3d9.exe")).unwrap();
    let output = root.join("stdout");
    let stdout = std::fs::File::create(&output).unwrap();
    let config = Config {
        dist: &dist,
        prefix: &root.join("prefix"),
        drive: None,
        locale: 0x409,
        fonts: "",
        exe: Some(&game.join("d3d9.exe")),
        width: 640,
        height: 480,
        jit: true,
        debug: "",
        stdout: Some(std::os::fd::AsRawFd::as_raw_fd(&stdout)),
    };
    let wake = adv_sync::WakeEvent::new().unwrap();
    let gpu_wake = wake.clone();
    let gpu = adv_device::gpu::test_support::request(false).unwrap();
    let mut session = Session::start(&config, wake, move |machine| {
        machine.ext.insert(gpu.waker());
        Session::attach_gpu(machine, gpu, gpu_wake)
    })
    .unwrap();
    session.set_text_records(true);
    let stop = session.run(Instant::now() + Duration::from_secs(60));
    let batch = session.take_text_records();
    drop(session);
    drop(stdout);
    assert!(
        matches!(stop, Stop::Exited(0)),
        "{stop:?}: {}",
        std::fs::read_to_string(output).unwrap()
    );

    assert_eq!(batch.dropped, 0);
    let lines: Vec<_> = batch
        .records
        .iter()
        .filter_map(|record| match record {
            TextRecord::Line(line) => Some(line),
            _ => None,
        })
        .collect();
    // "Hi" on the system-memory surface, then on the back buffer, then the
    // static window's own caption as the window repaints.
    assert_eq!(
        lines.iter().map(|line| text(line)).collect::<Vec<_>>(),
        ["Hi", "Hi", "probe"]
    );
    let moves: Vec<_> = batch
        .records
        .iter()
        .filter_map(|record| match record {
            TextRecord::Move(moved) => Some(moved),
            _ => None,
        })
        .collect();
    let back = lines[1].plane.unwrap();
    let Some(upload) = moves.iter().find(|moved| moved.kind == MoveKind::Upload) else {
        panic!("{moves:#?}");
    };
    // The back buffer's DC pixels go up to the back buffer resource whole.
    let Store::Memory(read) = upload.source else {
        panic!("{upload:?}");
    };
    assert_eq!((read.bits, read.size), (back.bits, [64, 32]), "{upload:?}");
    assert_eq!(
        (upload.rect, upload.map),
        ([0, 0, 64, 32], Some(Map::offset(0, 0)))
    );
    assert!(matches!(upload.destination, Store::Resource(_)));
    // Each present shows that resource across the 64 × 32 window.
    let presents: Vec<_> = moves
        .iter()
        .filter(|moved| moved.kind == MoveKind::Present)
        .collect();
    assert!(!presents.is_empty(), "{moves:#?}");
    for present in &presents {
        assert_eq!(present.source, upload.destination);
        assert!(matches!(present.destination, Store::Window(_)));
        assert_eq!(
            (present.rect, present.map),
            ([0, 0, 64, 32], Some(Map::offset(0, 0)))
        );
    }
    // Nothing moved out of the system-memory surface, which no copy reads.
    let [start, end] = lines[0].plane.unwrap().span();
    assert!(moves.iter().all(|moved| match moved.source {
        Store::Memory(plane) => plane.span()[1] <= start || plane.span()[0] >= end,
        _ => true,
    }));
}

// spec: WINE-UNIX-8
#[cfg(feature = "gpu")]
#[test]
fn a_d3d9_draw_that_samples_a_text_texture_is_recorded_with_its_map() {
    in_child("a_d3d9_draw_that_samples_a_text_texture_is_recorded_with_its_map_child");
}

#[cfg(feature = "gpu")]
#[test]
#[ignore = "subprocess entry: Wine owns process-global server state"]
fn a_d3d9_draw_that_samples_a_text_texture_is_recorded_with_its_map_child() {
    use adv_runtime::wine::session::Session;
    let Some(wine::Wine { dist, probes }) = wine::wine() else {
        return;
    };
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/text-records-d3d9-draw")
        .join(std::process::id().to_string());
    if root.exists() {
        std::fs::remove_dir_all(&root).unwrap();
    }
    let game = root.join("game");
    std::fs::create_dir_all(&game).unwrap();
    let exe = game.join("text-records-d3d9.exe");
    std::fs::copy(probes.join("text-records-d3d9.exe"), &exe).unwrap();
    let config = Config {
        dist: &dist,
        prefix: &root.join("prefix"),
        drive: None,
        locale: 0x409,
        fonts: "",
        exe: Some(&exe),
        width: 640,
        height: 480,
        jit: true,
        debug: "",
        stdout: None,
    };
    let wake = adv_sync::WakeEvent::new().unwrap();
    let gpu_wake = wake.clone();
    let gpu = adv_device::gpu::test_support::request(false).unwrap();
    let mut session = Session::start(&config, wake, move |machine| {
        machine.ext.insert(gpu.waker());
        Session::attach_gpu(machine, gpu, gpu_wake)
    })
    .unwrap();
    session.set_text_records(true);
    let stop = session.run(Instant::now() + Duration::from_secs(60));
    let batch = session.take_text_records();
    drop(session);
    assert!(matches!(stop, Stop::Exited(0)), "{stop:?}");

    let moves: Vec<_> = batch
        .records
        .iter()
        .filter_map(|record| match record {
            TextRecord::Move(moved) => Some(moved),
            _ => None,
        })
        .collect();
    let kinds: Vec<_> = moves.iter().map(|moved| moved.kind).collect();
    assert_eq!(
        kinds,
        [MoveKind::Upload, MoveKind::Draw, MoveKind::Present],
        "{moves:#?}"
    );
    let [upload, draw, present] = [moves[0], moves[1], moves[2]];
    // The texture's DC pixels reach the texture, the quad samples it 1:1 at
    // (8, 4) on the back buffer, and the back buffer reaches the window.
    assert_eq!(draw.source, upload.destination);
    // Draw bounds are the damage planner's, which may pad a pixel.
    let [l, t, r, b] = draw.rect;
    assert!(
        (7..=8).contains(&l)
            && (3..=4).contains(&t)
            && (72..=73).contains(&r)
            && (36..=37).contains(&b),
        "{draw:?}"
    );
    assert_eq!(draw.map, Some(Map::offset(-8, -4)), "{draw:?}");
    assert_eq!(present.source, draw.destination);
    assert!(matches!(present.destination, Store::Window(_)));
    assert_eq!(present.rect, [0, 0, 128, 64]);
}

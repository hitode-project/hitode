use super::*;
use adv_cpu::{
    Cpu as JitCpu,
    backend::{Guest, Prot, Reg},
};
use adv_machine::{Clock, Machine};
mod outline_metrics;
mod placement;
mod spacing;
mod widths;

fn fixture() -> (Machine<JitCpu>, u32, u32) {
    let mut registry = Registry::default();
    register(&mut registry);
    let cpu = JitCpu::new(adv_machine::layout::PARK_ADDR, false).unwrap();
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    machine
        .spawn("font fixture", adv_machine::layout::PARK_ADDR, &[], 65536)
        .unwrap();
    let data = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, 4096, Prot::RW)
        .unwrap();
    let base = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, 0x400000, Prot::RW)
        .unwrap();
    let mut text = Text::with_font_directories(&[]);
    text.add_resource(include_bytes!(
        "../../../../adv-device/tests/fixtures/font-resource.ttf"
    ))
    .unwrap();
    install(&mut machine, text, 0x411);
    adv_nt::identity::note_unix(
        &mut Env {
            context: machine.sys.current,
            cpu: &mut machine.cpu,
            sys: &mut machine.sys,
            ext: &mut machine.ext,
            thunk: 0,
        },
        std::path::Path::new("lib/wine/i386-unix/win32u.so"),
        base,
        &[0; 52],
        &[],
    );
    let shared = base + 0x200000;
    let dc = 0x1000;
    let font = 0x2000;
    let dc_pointer = base + 0x380000;
    let font_pointer = base + 0x381000;
    let attr = base + 0x382000;
    // The fixture's win32u has no identity table: its data symbols sit
    // 0x100 apart below the shared handle table.
    let synthetic = layout::Layout::resolve(|_, name| {
        let index = layout::Layout::NAMES.iter().position(|n| *n == name)?;
        Some(0x1f0000 + 0x100 * index as u32)
    })
    .unwrap();
    assert_eq!(*layout::LAYOUT.get_or_init(|| synthetic), synthetic);
    machine
        .cpu
        .write_u32(base + layout::layout().gdi_shared, shared)
        .unwrap();
    for (handle, pointer, kind) in [(dc, dc_pointer, 1), (font, font_pointer, 10)] {
        let entry = shared + handle * layout::handle::SIZE;
        machine.cpu.write_u32(entry, pointer).unwrap();
        machine
            .cpu
            .write(entry + layout::handle::Type, &[kind])
            .unwrap();
    }
    machine
        .cpu
        .write_u32(dc_pointer + layout::dc::attr, attr)
        .unwrap();
    machine
        .cpu
        .write_u32(dc_pointer + layout::dc::hFont, font)
        .unwrap();
    let mut logical = [0; 92];
    logical[..4].copy_from_slice(&(-24i32).to_le_bytes());
    logical[16..20].copy_from_slice(&400i32.to_le_bytes());
    logical[28..38].copy_from_slice(&[65, 0, 114, 0, 105, 0, 97, 0, 108, 0]);
    machine
        .cpu
        .write(font_pointer + layout::font::logfont, &logical)
        .unwrap();
    (machine, data, dc)
}
fn invoke(machine: &mut Machine<JitCpu>, data: u32, name: &str, args: &[u32]) -> u32 {
    let address = machine.resolve_import("gdi32.dll", name).unwrap();
    let index = (address - adv_machine::layout::THUNK_BASE) / adv_machine::layout::THUNK_STRIDE;
    machine.cpu.set_reg(Reg::Esp, data + 2048);
    for (i, &arg) in args.iter().enumerate() {
        machine
            .cpu
            .write_u32(data + 2052 + i as u32 * 4, arg)
            .unwrap();
    }
    let handler = machine.sys.thunks.get(index).f.handler;
    match handler(&mut Env {
        context: machine.sys.current,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: index,
    }) {
        Outcome::Return(value) => value,
        _ => panic!("font query did not return"),
    }
}

// spec: WIN32-GDI-12
#[test]
fn empty_glyph_reports_a_black_box_without_a_bitmap() {
    let (mut machine, data, dc) = fixture();
    let matrix = data + 64;
    let buffer = data + 128;
    machine.cpu.write_u32(matrix, 1 << 16).unwrap();
    machine.cpu.write_u32(matrix + 12, 1 << 16).unwrap();
    for name in ["GetGlyphOutlineA", "GetGlyphOutlineW"] {
        let space = u32::from(b' ');
        assert_eq!(
            invoke(
                &mut machine,
                data,
                name,
                &[dc, space, 6, data, 0, 0, matrix]
            ),
            0
        );
        assert_eq!(machine.cpu.read_u32(data).unwrap(), 1);
        assert_eq!(machine.cpu.read_u32(data + 4).unwrap(), 1);

        machine.cpu.write(data, &[0xab; 20]).unwrap();
        machine.cpu.write(buffer, &[0xcd; 4]).unwrap();
        assert_eq!(
            invoke(
                &mut machine,
                data,
                name,
                &[dc, space, 6, data, 4, buffer, matrix],
            ),
            GDI_ERROR
        );
        let mut metrics = [0; 20];
        machine.cpu.read(data, &mut metrics).unwrap();
        assert_eq!(metrics, [0xab; 20]);
        assert_eq!(machine.cpu.read_u32(buffer).unwrap(), 0xcdcd_cdcd);
    }
}

// spec: WINE-UNIX-8
#[test]
fn produced_glyph_bytes_are_recorded_only_while_records_are_on() {
    use super::super::text_records::{self, Code, ProducedGlyph, TextRecord};
    let (mut machine, data, dc) = fixture();
    let matrix = data + 64;
    let buffer = data + 256;
    machine.cpu.write_u32(matrix, 1 << 16).unwrap();
    machine.cpu.write_u32(matrix + 12, 1 << 16).unwrap();
    let gray = |machine: &mut Machine<JitCpu>, name| {
        invoke(
            machine,
            data,
            name,
            &[dc, 65, 6, data, 1024, buffer, matrix],
        )
    };
    assert!(gray(&mut machine, "GetGlyphOutlineW") > 0);
    assert!(text_records::records(&machine.ext).is_none());

    text_records::set(&mut machine.sys, &mut machine.ext, true);
    let written = gray(&mut machine, "GetGlyphOutlineW");
    // Metrics alone and a size query hand the guest no glyph bytes.
    invoke(
        &mut machine,
        data,
        "GetGlyphOutlineW",
        &[dc, 66, 0, data, 0, 0, matrix],
    );
    invoke(
        &mut machine,
        data,
        "GetGlyphOutlineW",
        &[dc, 66, 6, data, 0, 0, matrix],
    );
    invoke(
        &mut machine,
        data,
        "GetGlyphOutlineA",
        &[dc, 67, 6, data, 1024, buffer, matrix],
    );
    let batch = text_records::take(&mut machine.ext);
    assert_eq!(batch.dropped, 0);
    let glyphs: Vec<_> = batch
        .records
        .iter()
        .map(|record| match record {
            TextRecord::Glyph(glyph) => glyph.clone(),
            other => panic!("only glyphs were produced: {other:?}"),
        })
        .collect();
    assert_eq!(glyphs.len(), 2);
    let ProducedGlyph {
        sequence,
        dc: recorded,
        code,
        buffer: at,
        len,
        ref face,
        height,
        ..
    } = glyphs[0];
    assert_eq!(
        (sequence, recorded, code, at, len),
        (0, dc, Code::Unicode(65), buffer, written)
    );
    assert_eq!((face.as_str(), height), ("Arial", -24));
    assert_eq!((glyphs[1].sequence, glyphs[1].code), (1, Code::Unicode(67)));
}

// spec: WIN32-GDI-10
#[test]
fn text_extent_rejects_short_readonly_and_overflowing_ranges_without_partial_size() {
    let (mut machine, data, dc) = fixture();
    let output = data + 1024;
    machine.cpu.write(data + 512, b"AV\0x").unwrap();
    for (pointer, count) in [
        (u32::MAX - 1, 4),
        (data + 512, u32::MAX),
        (data + 512, 2 * 1024 * 1024),
    ] {
        machine.cpu.write(output, &[0xcc; 8]).unwrap();
        for name in ["GetTextExtentPoint32A", "GetTextExtentPoint32W"] {
            assert_eq!(
                invoke(&mut machine, data, name, &[dc, pointer, count, output]),
                0
            );
            let mut actual = [0; 8];
            machine.cpu.read(output, &mut actual).unwrap();
            assert_eq!(actual, [0xcc; 8]);
        }
    }
    let read_only = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, 4096, Prot::RW)
        .unwrap();
    machine.cpu.write(read_only, &[0xcd; 8]).unwrap();
    machine.cpu.protect(read_only, 4096, Prot::READ).unwrap();
    for name in ["GetTextExtentPoint32A", "GetTextExtentPoint32W"] {
        assert_eq!(
            invoke(&mut machine, data, name, &[dc, data + 512, 2, read_only]),
            0
        );
        let mut actual = [0; 8];
        machine.cpu.read(read_only, &mut actual).unwrap();
        assert_eq!(actual, [0xcd; 8]);
    }
    assert_eq!(
        machine.ext.get::<Fonts>().text.stats().fonts,
        0,
        "invalid outputs fail before native font matching"
    );
}

/// Selects `charset` into the fixture's font, then runs `f` on its request.
fn with_charset<R>(
    machine: &mut Machine<JitCpu>,
    dc: u32,
    charset: u8,
    f: impl FnOnce(&Env<'_>, &FontRequest) -> R,
) -> R {
    let env = Env {
        context: machine.sys.current,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: 0,
    };
    let font = env
        .cpu
        .read_u32(dc_pointer(&env, dc).unwrap() + layout::dc::hFont)
        .unwrap();
    let pointer = object(&env, font, 10).unwrap();
    env.cpu
        .write(pointer + layout::font::logfont + 23, &[charset])
        .unwrap();
    let request = font_request(&env, dc);
    f(&env, &request)
}

// spec: WIN32-GDI-10
#[test]
fn ansi_text_decodes_through_the_code_page_windows_uses() {
    let (mut machine, _, dc) = fixture();
    // A Macintosh or default font decodes through the ANSI code page (932
    // here), as Windows realizes neither as Macintosh.
    for charset in [0, 1, 77] {
        let units = with_charset(&mut machine, dc, charset, |env, request| {
            ansi_text(env, request, b"\x82\xa0").unwrap()
        });
        assert_eq!(units, [0x3042], "charset {charset}");
        // A lone lead byte shows its Latin-1 glyph.
        let units = with_charset(&mut machine, dc, charset, |env, request| {
            ansi_text(env, request, b"\x82").unwrap()
        });
        assert_eq!(units, [0x201a], "charset {charset}");
    }
    // Glyph outlines read only the packed low word of the character.
    for character in [0x82a0, 0x15ce_82a0, 0xffff_82a0] {
        let decoded = with_charset(&mut machine, dc, 0, |env, request| {
            ansi_character(env, request, character)
        });
        assert_eq!(decoded, Character::Unicode(0x3042), "{character:#x}");
    }
    // A single byte that is a character of the font's code page stays one.
    let units = with_charset(&mut machine, dc, 128, |env, request| {
        ansi_text(env, request, b"\xbb").unwrap()
    });
    assert_eq!(units, [0xff7b]);
    // A Big5 font decodes through code page 950.
    let units = with_charset(&mut machine, dc, 136, |env, request| {
        ansi_text(env, request, b"\xac\x4f").unwrap()
    });
    assert_eq!(units, [0x662f]);
}

// spec: WINE-SES-5
#[test]
fn traditional_chinese_sessions_use_big5_as_the_ansi_code_page() {
    let (mut machine, _, _) = fixture();
    let locale = crate::wine::session::locale(950);
    assert_eq!(locale, 0x404);
    install(&mut machine, Text::with_font_directories(&[]), locale);
    assert_eq!(machine.ext.get::<Fonts>().codec.acp, 950);
}

// spec: WIN32-GDI-10
#[test]
fn default_and_macintosh_requests_realize_a_charset_the_face_declares() {
    let (mut machine, data, dc) = fixture();
    // The Latin fixture lacks the Shift_JIS repertoire, so both realize ANSI.
    for charset in [1, 77] {
        with_charset(&mut machine, dc, charset, |_, _| ());
        assert_eq!(invoke(&mut machine, data, "GetTextCharset", &[dc]), 0);
    }
    with_charset(&mut machine, dc, 128, |_, _| ());
    assert_eq!(invoke(&mut machine, data, "GetTextCharset", &[dc]), 128);
}

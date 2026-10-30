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
    machine
        .cpu
        .write_u32(base + layout::GDI_SHARED, shared)
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

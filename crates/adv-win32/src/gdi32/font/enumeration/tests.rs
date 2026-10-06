use super::*;
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt},
};
use adv_machine::{Clock, Machine, Resume};

fn fixture() -> (Machine<JitCpu>, u32) {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    let cpu = JitCpu::new(adv_machine::layout::PARK_ADDR, false).unwrap();
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    crate::Win32::install(
        &mut machine,
        crate::Win32Config::new(
            ".",
            "font-enumeration.exe",
            "target/font-enumeration-overlay",
        ),
    )
    .unwrap();
    machine
        .spawn(
            "font enumeration",
            adv_machine::layout::PARK_ADDR,
            &[],
            65536,
        )
        .unwrap();
    let stack = machine.sys.current_context().stack_hi - 2048;
    machine.cpu.set_reg(Reg::Esp, stack);
    let thunk = machine
        .resolve_import("gdi32.dll", "CreateCompatibleDC")
        .unwrap();
    let index = (thunk - adv_machine::layout::THUNK_BASE) / adv_machine::layout::THUNK_STRIDE;
    machine.cpu.write_u32(stack + 4, 0).unwrap();
    let handler = machine.sys.thunks.get(index).f.handler;
    let Outcome::Return(dc) = handler(&mut Env {
        context: machine.sys.current,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: index,
    }) else {
        panic!("dc creation");
    };
    (machine, dc)
}
fn env(machine: &mut Machine<JitCpu>) -> Env<'_> {
    Env {
        context: machine.sys.current,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: 0,
    }
}

#[test]
#[cfg(windows)]
fn extended_fonts_match_native_charset_script_and_signature_in_a_and_w_callbacks() {
    use core::ffi::c_void;
    struct NativeFont {
        logical: [u8; 348],
        metrics: [u8; 100],
        kind: u32,
    }
    #[link(name = "gdi32")]
    unsafe extern "system" {
        fn CreateCompatibleDC(dc: *mut c_void) -> *mut c_void;
        fn DeleteDC(dc: *mut c_void) -> i32;
        fn EnumFontFamiliesExW(
            dc: *mut c_void,
            query: *const u8,
            callback: unsafe extern "system" fn(*const u8, *const u8, u32, isize) -> i32,
            parameter: isize,
            flags: u32,
        ) -> i32;
    }
    unsafe extern "system" fn collect(
        font: *const u8,
        metrics: *const u8,
        kind: u32,
        parameter: isize,
    ) -> i32 {
        let records = unsafe { &mut *(parameter as *mut Vec<NativeFont>) };
        let mut record = NativeFont {
            logical: [0; 348],
            metrics: [0; 100],
            kind,
        };
        // TrueType promises the full extended metrics; raster callbacks only
        // promise TEXTMETRIC. ENUMLOGFONTEX is supplied by this API for both.
        unsafe {
            core::ptr::copy_nonoverlapping(font, record.logical.as_mut_ptr(), 348);
            core::ptr::copy_nonoverlapping(
                metrics,
                record.metrics.as_mut_ptr(),
                if kind & 4 != 0 { 100 } else { 60 },
            );
        }
        records.push(record);
        1
    }
    let native = |charset: u8, face: Option<&str>| {
        let mut query = [0; 92];
        query[23] = charset;
        if let Some(face) = face {
            for (dst, unit) in query[28..].chunks_exact_mut(2).zip(face.encode_utf16()) {
                dst.copy_from_slice(&unit.to_le_bytes());
            }
        }
        let mut records = Vec::<NativeFont>::new();
        unsafe {
            let dc = CreateCompatibleDC(core::ptr::null_mut());
            assert!(!dc.is_null());
            assert_ne!(
                EnumFontFamiliesExW(
                    dc,
                    query.as_ptr(),
                    collect,
                    &mut records as *mut _ as isize,
                    0
                ),
                0
            );
            assert_ne!(DeleteDC(dc), 0);
        }
        records
    };
    let string = |bytes: &[u8]| {
        String::from_utf16_lossy(
            &bytes
                .chunks_exact(2)
                .map(|b| u16::from_le_bytes([b[0], b[1]]))
                .take_while(|&u| u != 0)
                .collect::<Vec<_>>(),
        )
    };
    let (mut machine, dc) = fixture();
    let pointer = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, 4096, Prot::RW)
        .unwrap();
    let japanese = native(128, None);
    assert!(!japanese.is_empty()); // This test host has an installed Japanese font.
    let face = string(&japanese[0].logical[28..92]);
    for (charset, filter) in [(0, None), (128, None), (1, Some(face.as_str()))] {
        let expected = native(charset, filter);
        assert!(!expected.is_empty());
        for wide in [false, true] {
            let mut logical = vec![0; if wide { 92 } else { 60 }];
            logical[23] = charset;
            if let Some(face) = filter {
                if wide {
                    for (destination, unit) in
                        logical[28..].chunks_exact_mut(2).zip(face.encode_utf16())
                    {
                        destination.copy_from_slice(&unit.to_le_bytes());
                    }
                } else {
                    let encoded = machine.ext.get::<Win32>().codec.encode(face);
                    if encoded.contains(&b'?') {
                        continue;
                    } // Host-only face unavailable in guest ACP.
                    let length = encoded.len().min(31);
                    logical[28..28 + length].copy_from_slice(&encoded[..length]);
                }
            }
            machine.cpu.write(pointer, &logical).unwrap();
            let mut outcome = if wide {
                families_ex_w(
                    &mut env(&mut machine),
                    Handle(dc),
                    GuestPtr::new(pointer),
                    GuestPtr::new(adv_machine::layout::PARK_ADDR),
                    0xfeedface,
                    0,
                )
            } else {
                families_ex_a(
                    &mut env(&mut machine),
                    Handle(dc),
                    GuestPtr::new(pointer),
                    GuestPtr::new(adv_machine::layout::PARK_ADDR),
                    0xfeedface,
                    0,
                )
            };
            let mut count = 0;
            loop {
                outcome = match outcome {
                    Outcome::Call { args, then, .. } => {
                        let record = &expected[count];
                        assert_eq!(args[2], record.kind);
                        assert_eq!(args[3], 0xfeedface);
                        let mut output = [0; 348];
                        machine.cpu.read(args[0], &mut output).unwrap();
                        let mut expected_logical = record.logical[..28].to_vec();
                        // Neutral device requests canonicalize logical BOOLs;
                        // native raster fonts sometimes use 0xff for true.
                        for flag in &mut expected_logical[20..23] {
                            *flag = u8::from(*flag != 0);
                        }
                        assert_eq!(&output[..28], &expected_logical);
                        let codec = machine.ext.get::<Win32>().codec;
                        for (offset, length) in [(28, 32), (92, 64), (220, 32), (284, 32)] {
                            let expected_text =
                                string(&record.logical[offset..offset + length * 2]);
                            let actual_offset = 28 + (offset - 28) / if wide { 1 } else { 2 };
                            let mut expected_bytes = vec![0; length * if wide { 2 } else { 1 }];
                            put_string(codec, &mut expected_bytes, &expected_text, wide);
                            assert_eq!(
                                &output[actual_offset..actual_offset + expected_bytes.len()],
                                &expected_bytes
                            );
                        }
                        if record.kind & 4 != 0 {
                            let mut signature = [0; 24];
                            machine
                                .cpu
                                .read(args[1] + if wide { 76 } else { 72 }, &mut signature)
                                .unwrap();
                            assert_eq!(&signature, &record.metrics[76..100]);
                        }
                        count += 1;
                        then(&mut env(&mut machine), Resume::Returned(7))
                    }
                    Outcome::Return(value) => {
                        assert_eq!(value, 7);
                        break;
                    }
                    _ => panic!("extended font callback"),
                };
            }
            assert_eq!(count, expected.len());
        }
    }
}

#[test]
fn extended_font_logfont_rejects_unreadable_and_wrapping_input() {
    let (mut machine, dc) = fixture();
    for pointer in [0, 0xdeadbeef, u32::MAX - 24] {
        assert!(matches!(
            families_ex_a(
                &mut env(&mut machine),
                Handle(dc),
                GuestPtr::new(pointer),
                GuestPtr::new(adv_machine::layout::PARK_ADDR),
                0,
                0
            ),
            Outcome::Return(0)
        ));
        assert!(matches!(
            families_ex_w(
                &mut env(&mut machine),
                Handle(dc),
                GuestPtr::new(pointer),
                GuestPtr::new(adv_machine::layout::PARK_ADDR),
                0,
                0
            ),
            Outcome::Return(0)
        ));
    }
}

#[test]
#[cfg(windows)]
fn callback_layout_parameter_stop_and_final_result_follow_guest_returns() {
    let (mut machine, dc) = fixture();
    let mut outcome = families_a(
        &mut env(&mut machine),
        Handle(dc),
        AnsiStr(GuestPtr::new(0)),
        GuestPtr::new(adv_machine::layout::PARK_ADDR),
        0x51c0ffee,
    );
    let expected = machine
        .ext
        .get::<Fonts>()
        .text
        .enumerate(None, FontEnumeration::Families)
        .unwrap();
    assert!(expected.len() > 1);
    let original_stack = machine.cpu.reg(Reg::Esp);
    let mut count = 0;
    loop {
        outcome = match outcome {
            Outcome::Call {
                target,
                args,
                stack,
                then,
                ..
            } => {
                assert_eq!(target, adv_machine::layout::PARK_ADDR);
                assert_eq!(args[3], 0x51c0ffee);
                assert_eq!(stack, Some(args[0]));
                assert!(args[0] + SCRATCH <= original_stack);
                assert_eq!(args[1], args[0] + METRICS_OFFSET);
                let mut logical = [0; 60];
                machine.cpu.read(args[0], &mut logical).unwrap();
                let mut metrics = [0; 56];
                machine.cpu.read(args[1], &mut metrics).unwrap();
                assert_eq!(logical[23], encode_encoding(expected[count].font.encoding));
                assert_eq!(
                    metrics[52],
                    encode_encoding(expected[count].metrics.encoding)
                );
                let face = logical[28..].split(|&value| value == 0).next().unwrap();
                let codec = machine.ext.get::<Win32>().codec;
                assert!(codec.encode(&expected[count].font.face).starts_with(face));
                count += 1;
                then(
                    &mut env(&mut machine),
                    Resume::Returned(if count == 2 { 0 } else { 7 }),
                )
            }
            Outcome::Return(value) => {
                assert_eq!(value, 0);
                break;
            }
            _ => panic!("unexpected enumeration outcome"),
        };
    }
    assert_eq!(count, 2, "zero callback stops before third font");
    let fonts = expected.into_iter().take(1).collect();
    let scratch = original_stack - SCRATCH - 16;
    let outcome = next(
        &mut env(&mut machine),
        Enumeration {
            fonts,
            callback: adv_machine::layout::PARK_ADDR,
            param: 0,
            scratch,
            wide: true,
            mode: FontEnumeration::Faces,
        },
        1,
    );
    let Outcome::Call { then, .. } = outcome else {
        panic!("single callback");
    };
    assert!(matches!(
        then(&mut env(&mut machine), Resume::Returned(0x1234)),
        Outcome::Return(0x1234)
    ));
}

#[test]
#[cfg(windows)]
fn full_actual_enumeration_keeps_japanese_filter_and_wide_metrics() {
    let (mut machine, dc) = fixture();
    let expected = machine
        .ext
        .get::<Fonts>()
        .text
        .enumerate(None, FontEnumeration::Families)
        .unwrap();
    let expected_japanese = expected
        .iter()
        .filter(|font| font.font.encoding == Encoding::Japanese && !font.font.face.starts_with('@'))
        .count();
    let mut outcome = families_w(
        &mut env(&mut machine),
        Handle(dc),
        WideStr(GuestPtr::new(0)),
        GuestPtr::new(adv_machine::layout::PARK_ADDR),
        0,
    );
    let mut count = 0;
    let mut japanese = 0;
    loop {
        outcome = match outcome {
            Outcome::Call { args, then, .. } => {
                let mut logical = [0; 92];
                machine.cpu.read(args[0], &mut logical).unwrap();
                let mut metrics = [0; 60];
                machine.cpu.read(args[1], &mut metrics).unwrap();
                assert_eq!(
                    metrics[56],
                    encode_encoding(expected[count].metrics.encoding)
                );
                let face = logical[28..]
                    .chunks_exact(2)
                    .map(|v| u16::from_le_bytes([v[0], v[1]]))
                    .take_while(|&v| v != 0)
                    .collect::<Vec<_>>();
                if logical[23] == 128 && face.first() != Some(&(b'@' as u16)) {
                    japanese += 1;
                }
                count += 1;
                then(&mut env(&mut machine), Resume::Returned(9))
            }
            Outcome::Return(value) => {
                assert_eq!(value, 9);
                break;
            }
            _ => panic!("unexpected font callback"),
        };
    }
    assert_eq!(count, expected.len());
    assert_eq!(japanese, expected_japanese);
}

#[test]
fn invalid_dc_callback_and_short_stack_reject_without_guest_callbacks() {
    let (mut machine, dc) = fixture();
    assert!(matches!(
        families_a(
            &mut env(&mut machine),
            Handle(0xdead),
            AnsiStr(GuestPtr::new(0)),
            GuestPtr::new(adv_machine::layout::PARK_ADDR),
            0
        ),
        Outcome::Return(0)
    ));
    assert!(matches!(
        fonts_a(
            &mut env(&mut machine),
            Handle(dc),
            AnsiStr(GuestPtr::new(0)),
            GuestPtr::new(0),
            0
        ),
        Outcome::Return(0)
    ));
    let stack_lo = machine.sys.current_context().stack_lo;
    machine.cpu.set_reg(Reg::Esp, stack_lo + 16);
    assert!(matches!(
        families_a(
            &mut env(&mut machine),
            Handle(dc),
            AnsiStr(GuestPtr::new(0)),
            GuestPtr::new(adv_machine::layout::PARK_ADDR),
            0
        ),
        Outcome::Return(0)
    ));
}

#[test]
fn encoded_strings_keep_guest_codepage_and_complete_dbcs_characters() {
    for (codepage, expected) in [
        (932, &b"\x93\xfa\x96\x7b\x8c\xea"[..]),
        (936, &b"\xc8\xd5\xb1\xbe\xd5\x5a"[..]),
    ] {
        let mut bytes = [0; 8];
        put_string(adv_os::Codec::new(codepage), &mut bytes, "日本語文", false);
        assert_eq!(&bytes[..6], expected);
        assert_eq!(&bytes[6..], &[0, 0]);
    }
    let mut wide = [0; 6];
    put_string(adv_os::Codec::new(932), &mut wide, "😀a", true);
    assert_eq!(&wide[..4], &[0x3d, 0xd8, 0x00, 0xde]);
    assert_eq!(&wide[4..], &[0, 0]);
}

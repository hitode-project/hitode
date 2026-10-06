//! Real guest DrawText A/W calls paint the shared CPU surface, not a descriptor.
use super::*;
use adv_abi::Conv;
use adv_cpu::{
    JitCpu,
    backend::{Backend, Guest, GuestExt, Prot, asm::Asm},
};
use adv_device::surface::{MemoryOperations, MemoryView, PixelFormat, Snapshot, SurfaceStore};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
use std::borrow::Cow;

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const RECT: u32 = 0x420000;
const BAD: u32 = 0x430000;
const TEXT_W: u32 = DATA;
const TEXT_A: u32 = DATA + 128;
const LITERAL: u32 = DATA + 256;
const PREFIX: u32 = DATA + 384;
const EMPTY: u32 = DATA + 512;
const METRICS: u32 = DATA + 768;
const LOGFONT: u32 = DATA + 896;
const CALC_RECT: u32 = DATA + 1024;
const OUTPUTS: u32 = DATA + 2048;
const WIDTH: u32 = 128;
const HEIGHT: u32 = 48;
const BACKGROUND: [u8; 4] = [0x77, 0x55, 0x33, 0];

struct Observations {
    pixels: u32,
    snapshots: Vec<Vec<u8>>,
}
fn observe(env: &mut Env<'_>) -> Outcome {
    let pixels = env.ext.get::<Observations>().pixels;
    let mut bytes = vec![0; (WIDTH * HEIGHT * 4) as usize];
    env.cpu.read(pixels, &mut bytes).unwrap();
    env.ext.get_mut::<Observations>().snapshots.push(bytes);
    let baseline: Vec<_> = (0..WIDTH * HEIGHT).flat_map(|_| BACKGROUND).collect();
    env.cpu.write(pixels, &baseline).unwrap();
    Outcome::Return(0)
}
fn protect_output(env: &mut Env<'_>) -> Outcome {
    let pixels = env.ext.get::<Observations>().pixels;
    env.cpu
        .protect(pixels, WIDTH * HEIGHT * 4, Prot::READ)
        .unwrap();
    Outcome::Return(0)
}
fn memory(cpu: &mut dyn Guest) -> MemoryView<'_, dyn Guest + '_> {
    MemoryView::new(
        cpu,
        MemoryOperations {
            read: |cpu, address, length| {
                let mut bytes = vec![0; length];
                cpu.read(address, &mut bytes).map_err(|e| e.to_string())?;
                Ok(Cow::Owned(bytes))
            },
            write: |cpu, address, bytes| cpu.write(address, bytes).map_err(|e| e.to_string()),
            writable: |cpu, address, length| {
                adv_abi::guest::check_range(cpu, address, length as u32, Prot::WRITE)
                    .map_err(|e| e.to_string())
            },
            copy: |cpu, source, dest, length| {
                let mut bytes = vec![0; length];
                cpu.read(source, &mut bytes).map_err(|e| e.to_string())?;
                cpu.write(dest, &bytes).map_err(|e| e.to_string())
            },
        },
    )
}
fn wide<B: Backend>(machine: &mut Machine<B>, address: u32, text: &str) {
    let bytes: Vec<_> = text
        .encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect();
    machine.cpu.write(address, &bytes).unwrap();
}
fn call(code: &mut Asm, entry: u32, args: &[u32], out: u32) {
    for &arg in args.iter().rev() {
        code.push_imm(arg);
    }
    code.call(entry).mov_mem_eax(out);
}

fn guest<B: Backend>(cpu: B) {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    registry.add("fixture.dll", "observe", Conv::Cdecl { argc: 0 }, observe);
    registry.add(
        "fixture.dll",
        "protect",
        Conv::Cdecl { argc: 0 },
        protect_output,
    );
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/drawtext-singleline-tests");
    let mut config = crate::Win32Config::new(&root, "fixture.exe", &root);
    config.os.codepage = 936;
    crate::Win32::install(&mut machine, config).unwrap();
    for (address, prot) in [
        (CODE, Prot::RX),
        (DATA, Prot::RW),
        (RECT, Prot::RW),
        (BAD, Prot::WRITE),
    ] {
        machine.cpu.map(address, 4096, prot).unwrap();
    }
    let arena = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, 128 * 1024, Prot::RW)
        .unwrap();
    let mut store =
        SurfaceStore::new(WIDTH, HEIGHT, arena, 128 * 1024, Snapshot::default()).unwrap();
    let target = store
        .create(
            WIDTH,
            HEIGHT,
            false,
            PixelFormat::Bgra32,
            &mut memory(&mut machine.cpu),
        )
        .unwrap();
    let pixels = store.get(target).unwrap().pixels;
    let dc = store.new_id();
    let mut contexts = adv_os::gdi::DeviceContexts::new(machine.ext.get::<Win32>().gdi.clone());
    assert!(contexts.create(&mut machine.sys.objects, dc, target));
    machine.ext.insert(store);
    machine.ext.insert(Observations {
        pixels,
        snapshots: Vec::new(),
    });
    super::super::super::raster::front::set_provider(
        &mut machine.ext,
        super::super::super::raster::front::DrawingProvider {
            memory,
            completed: |_, _| None,
        },
    );
    machine
        .cpu
        .write(
            pixels,
            &(0..WIDTH * HEIGHT)
                .flat_map(|_| BACKGROUND)
                .collect::<Vec<_>>(),
        )
        .unwrap();
    wide(&mut machine, TEXT_W, "中A&&B");
    wide(&mut machine, LITERAL, "中A&B");
    wide(&mut machine, PREFIX, "A&B");
    wide(&mut machine, EMPTY, "");
    let mut ansi = machine.ext.get::<Win32>().codec.encode("中A&&B");
    let ansi_count = ansi.len() as u32;
    ansi.push(0);
    machine.cpu.write(TEXT_A, &ansi).unwrap();
    let rectangle = [4i32, 5, 24, 20]
        .into_iter()
        .flat_map(i32::to_le_bytes)
        .collect::<Vec<_>>();
    machine.cpu.write(RECT, &rectangle).unwrap();
    machine.cpu.protect(RECT, 4096, Prot::READ).unwrap();
    machine.cpu.write(CALC_RECT, &rectangle).unwrap();
    let mut logical = [0; 92];
    logical[..4].copy_from_slice(&(-16i32).to_le_bytes());
    logical[16..20].copy_from_slice(&400i32.to_le_bytes());
    logical[23] = 128; // Selected font charset must not decode USER's ANSI bytes.
    logical[26] = 3;
    for (at, unit) in "Arial".encode_utf16().enumerate() {
        logical[28 + at * 2..30 + at * 2].copy_from_slice(&unit.to_le_bytes());
    }
    machine.cpu.write(LOGFONT, &logical).unwrap();

    let w = machine.resolve_import("user32.dll", "DrawTextW").unwrap();
    let a = machine.resolve_import("user32.dll", "DrawTextA").unwrap();
    let font = machine
        .resolve_import("gdi32.dll", "CreateFontIndirectW")
        .unwrap();
    let select = machine.resolve_import("gdi32.dll", "SelectObject").unwrap();
    let metrics = machine
        .resolve_import("gdi32.dll", "GetTextMetricsA")
        .unwrap();
    let text_color = machine.resolve_import("gdi32.dll", "SetTextColor").unwrap();
    let bk = machine.resolve_import("gdi32.dll", "SetBkMode").unwrap();
    let viewport = machine
        .resolve_import("gdi32.dll", "SetViewportOrgEx")
        .unwrap();
    let clip = machine
        .resolve_import("gdi32.dll", "IntersectClipRect")
        .unwrap();
    let ext_text = machine.resolve_import("gdi32.dll", "ExtTextOutW").unwrap();
    let observer = machine.resolve_import("fixture.dll", "observe").unwrap();
    let protect = machine.resolve_import("fixture.dll", "protect").unwrap();
    let error = machine
        .resolve_import("kernel32.dll", "GetLastError")
        .unwrap();
    let exit = machine
        .resolve_import("kernel32.dll", "ExitProcess")
        .unwrap();
    let mut code = Asm::new(CODE);
    code.push_imm(LOGFONT)
        .call(font)
        .push_eax()
        .push_imm(dc)
        .call(select);
    call(&mut code, metrics, &[dc, METRICS], OUTPUTS + 160);
    call(&mut code, text_color, &[dc, 0xffffff], OUTPUTS + 164);
    call(&mut code, bk, &[dc, 1], OUTPUTS + 168);
    call(&mut code, viewport, &[dc, 3, 4, 0], OUTPUTS + 172);
    call(&mut code, clip, &[dc, 5, 6, 25, 22], OUTPUTS + 176);
    call(&mut code, w, &[dc, TEXT_W, 5, CALC_RECT, 0x420], OUTPUTS);
    call(&mut code, w, &[dc, TEXT_W, 5, RECT, 0x20], OUTPUTS + 4);
    code.call(observer);
    call(
        &mut code,
        a,
        &[dc, TEXT_A, ansi_count, RECT, 0x20],
        OUTPUTS + 8,
    );
    code.call(observer);
    call(
        &mut code,
        ext_text,
        &[dc, 4, 5, 4, RECT, LITERAL, 4, 0],
        OUTPUTS + 12,
    );
    code.call(observer);
    for (index, args) in [
        [dc, TEXT_W, 5, RECT, 0x420], // CALCRECT still requires writable RECT.
        [dc, BAD, 1, RECT, 0x20],
        [dc, TEXT_W, 5, BAD, 0x20],
        [dc, TEXT_W, u32::MAX - 1, RECT, 0x20],
        [dc, TEXT_W, 5, RECT, 0x30], // Paragraph layout remains qualified unsupported.
        [dc, PREFIX, 3, RECT, 0x20], // No silent removal of mnemonic underlining.
        [0xdeadbeef, TEXT_W, 5, RECT, 0x20],
    ]
    .into_iter()
    .enumerate()
    {
        let out = OUTPUTS + 32 + index as u32 * 8;
        call(&mut code, w, &args, out);
        code.call(error).mov_mem_eax(out + 4).call(observer);
    }
    call(&mut code, w, &[dc, TEXT_W, 0, RECT, 0x20], OUTPUTS + 112);
    code.call(observer);
    call(
        &mut code,
        w,
        &[dc, EMPTY, u32::MAX, RECT, 0x20],
        OUTPUTS + 116,
    );
    code.call(observer);
    code.call(protect);
    call(&mut code, w, &[dc, TEXT_W, 5, RECT, 0x20], OUTPUTS + 120);
    code.call(error)
        .mov_mem_eax(OUTPUTS + 124)
        .push_imm(0)
        .call(exit)
        .ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine
        .spawn("DrawText CPU pixels", CODE, &[], 65536)
        .unwrap();
    let result = machine.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    let height = machine.cpu.read_u32(METRICS).unwrap();
    assert!(height > 0);
    for offset in [0, 4, 8, 116] {
        assert_eq!(machine.cpu.read_u32(OUTPUTS + offset).unwrap(), height);
    }
    assert_eq!(machine.cpu.read_u32(OUTPUTS + 12).unwrap(), 1);
    let snapshots = &machine.ext.get::<Observations>().snapshots;
    assert_eq!(
        snapshots[0], snapshots[1],
        "ACP936 A matches Unicode W despite font charset128"
    );
    assert_eq!(
        snapshots[0], snapshots[2],
        "shared ExtTextOut keeps its literal semantics"
    );
    let changed: Vec<_> = snapshots[0]
        .as_chunks::<4>()
        .0
        .iter()
        .enumerate()
        .filter(|(_, pixel)| **pixel != BACKGROUND)
        .map(|(index, _)| (index as u32 % WIDTH, index as u32 / WIDTH))
        .collect();
    assert!(!changed.is_empty(), "real glyph coverage");
    assert!(
        changed
            .iter()
            .all(|&(x, y)| (8..27).contains(&x) && (10..24).contains(&y)),
        "RECT and DC clip translated by viewport"
    );
    for bytes in &snapshots[3..] {
        assert!(
            bytes
                .as_chunks::<4>()
                .0
                .iter()
                .all(|&pixel| pixel == BACKGROUND),
            "rejected/empty calls don't paint"
        );
    }
    for (index, last_error) in [87, 87, 87, 87, 120, 120, 6].into_iter().enumerate() {
        let out = OUTPUTS + 32 + index as u32 * 8;
        assert_eq!(machine.cpu.read_u32(out).unwrap(), 0);
        assert_eq!(machine.cpu.read_u32(out + 4).unwrap(), last_error);
    }
    assert_eq!(machine.cpu.read_u32(OUTPUTS + 112).unwrap(), 1);
    assert_eq!(machine.cpu.read_u32(OUTPUTS + 120).unwrap(), 0);
    assert_eq!(machine.cpu.read_u32(OUTPUTS + 124).unwrap(), 87);
    let mut actual = [0; 16];
    machine.cpu.read(RECT, &mut actual).unwrap();
    assert_eq!(actual.as_slice(), rectangle);
    let diagnostics = machine.ext.get::<adv_abi::ApiDiagnostics>();
    assert_eq!(diagnostics.stub_calls, 0);
    assert!(
        diagnostics
            .warnings
            .keys()
            .all(|(api, _)| api == "user32.dll!DrawTextW")
    );
    assert_eq!(diagnostics.warnings.values().sum::<u64>(), 2);
}

#[test]
fn singleline_guest_pixels_acp_clipping_returns_and_guards() {
    guest(JitCpu::new(PARK_ADDR, false).unwrap());
    guest(JitCpu::new(PARK_ADDR, true).unwrap());
    guest(UnicornCpu::new(PARK_ADDR).unwrap());
}

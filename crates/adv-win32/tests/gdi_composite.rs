//! Authored DIB pixels exercise the shared MSIMG32/GDI32 path on every CPU.
use adv_abi::{ApiDiagnostics, Registry};
use adv_cpu::{
    JitCpu,
    backend::{Backend, Guest, GuestExt, Prot, asm::Asm},
};
use adv_device::surface::{MemoryOperations, MemoryView, Snapshot, SurfaceStore};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
use adv_win32::{
    Win32, Win32Config,
    gdi32::raster::front::{DrawingProvider, set_provider},
};
use std::borrow::Cow;

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
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
                cpu.copy_memory(source, dest, length)
                    .map_err(|e| e.to_string())
            },
        },
    )
}
fn call<B: Backend>(
    m: &mut Machine<B>,
    code: &mut Asm,
    dll: &str,
    name: &str,
    args: &[u32],
    out: u32,
) {
    for &arg in args.iter().rev() {
        if arg == 0xffff_0000 || arg == 0xffff_0004 {
            code.mov_eax_mem(DATA + 128 + (arg & 4)).push_eax();
        } else {
            code.push_imm(arg);
        }
    }
    code.call(m.resolve_import(dll, name).unwrap())
        .mov_mem_eax(out);
}
fn fixture<B: Backend>(cpu: B, alias: bool) {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    let mut m = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/gdi-composite-tests");
    Win32::install(&mut m, Win32Config::new(&root, "fixture.exe", &root)).unwrap();
    m.cpu.map(CODE, 4096, Prot::RX).unwrap();
    m.cpu.map(DATA, 4096, Prot::RW).unwrap();
    let arena = m.sys.vm.alloc(&mut m.cpu, 65536, Prot::RW).unwrap();
    m.ext
        .insert(SurfaceStore::new(4, 1, arena, 65536, Snapshot::default()).unwrap());
    set_provider(
        &mut m.ext,
        DrawingProvider {
            memory,
            completed: |_, _| None,
        },
    );
    let mut info = [0u8; 40];
    for (offset, value) in [(0, 40u32), (4, 4), (8, u32::MAX - 3), (12, 32 << 16 | 1)] {
        info[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    m.cpu.write(DATA, &info).unwrap();
    let mut code = Asm::new(CODE);
    for (index, slot) in [(0, 0xffff_0000), (1, 0xffff_0004)] {
        call(
            &mut m,
            &mut code,
            "gdi32.dll",
            "CreateCompatibleDC",
            &[0],
            DATA + 128 + index * 4,
        );
        call(
            &mut m,
            &mut code,
            "gdi32.dll",
            "CreateDIBSection",
            &[slot, DATA, 0, DATA + 136 + index * 4, 0, 0],
            DATA + 144 + index * 4,
        );
        // Select the created bitmap without replacing its handle record.
        code.mov_eax_mem(DATA + 144 + index * 4).push_eax();
        code.mov_eax_mem(DATA + 128 + index * 4).push_eax();
        code.call(m.resolve_import("gdi32.dll", "SelectObject").unwrap());
    }
    code.ret();
    m.cpu.write(CODE, &code.bytes).unwrap();
    m.spawn("create DIBs", CODE, &[], 65536).unwrap();
    assert!(matches!(m.run_virtual(1_000_000), RunResult::Exited(_)));
    let src = m.cpu.read_u32(DATA + 136).unwrap();
    let dst = m.cpu.read_u32(DATA + 140).unwrap();
    let original = [0, 0, 80, 128, 0, 40, 0, 255, 20, 0, 0, 64, 0, 0, 255, 0];
    m.cpu.write(src, &original).unwrap();
    m.cpu.write(dst, &[100; 16]).unwrap();
    let mut code = Asm::new(CODE + 512);
    let (dll, alpha, transparent) = if alias {
        ("gdi32.dll", "GdiAlphaBlend", "GdiTransparentBlt")
    } else {
        ("msimg32.dll", "AlphaBlend", "TransparentBlt")
    };
    call(
        &mut m,
        &mut code,
        dll,
        alpha,
        &[0xffff_0004, 0, 0, 4, 1, 0xffff_0000, 0, 0, 4, 1, 0x01ff0000],
        DATA + 200,
    );
    // Both rectangles overlap in the same selected DIB: native rejects the call.
    call(
        &mut m,
        &mut code,
        dll,
        alpha,
        &[0xffff_0004, 0, 0, 4, 1, 0xffff_0004, 0, 0, 4, 1, 0x01ff0000],
        DATA + 204,
    );
    // Invalid BlendFlags and negative dimensions cannot modify the target.
    call(
        &mut m,
        &mut code,
        dll,
        alpha,
        &[0xffff_0004, 0, 0, 4, 1, 0xffff_0000, 0, 0, 4, 1, 0x01ff0100],
        DATA + 208,
    );
    // Exclude the first pixel through DC clipping; copy the high byte of others.
    call(
        &mut m,
        &mut code,
        "gdi32.dll",
        "IntersectClipRect",
        &[0xffff_0004, 1, 0, 4, 1],
        DATA + 212,
    );
    call(
        &mut m,
        &mut code,
        dll,
        transparent,
        &[0xffff_0004, 0, 0, 4, 1, 0xffff_0000, 0, 0, 4, 1, 0x002800],
        DATA + 216,
    );
    call(
        &mut m,
        &mut code,
        "gdi32.dll",
        "SelectClipRgn",
        &[0xffff_0004, 0],
        DATA + 228,
    );
    call(
        &mut m,
        &mut code,
        dll,
        alpha,
        &[0xffff_0004, 0, 1, 4, 1, 0xffff_0000, 0, 0, 4, 1, 0x00800000],
        DATA + 220,
    );
    call(
        &mut m,
        &mut code,
        "kernel32.dll",
        "GetLastError",
        &[],
        DATA + 224,
    );
    code.ret();
    m.cpu.write(CODE + 512, &code.bytes).unwrap();
    m.spawn("composite DIBs", CODE + 512, &[], 65536).unwrap();
    assert!(matches!(m.run_virtual(1_000_000), RunResult::Exited(_)));
    for (i, expected) in [1, 0, 0, 3, 1].into_iter().enumerate() {
        assert_eq!(
            m.cpu.read_u32(DATA + 200 + i as u32 * 4).unwrap(),
            expected,
            "call {i}"
        );
    }
    let mut result = [0; 16];
    m.cpu.read(dst, &mut result).unwrap();
    assert_eq!(
        result,
        [50, 50, 130, 178, 0, 40, 0, 255, 20, 0, 0, 64, 0, 0, 255, 0]
    );
    assert_eq!(m.cpu.read_u32(DATA + 220).unwrap(), 1);
    assert_eq!(m.cpu.read_u32(DATA + 224).unwrap(), 87);
    let mut constant = [0; 16];
    m.cpu.read(dst + 16, &mut constant).unwrap();
    assert_eq!(
        constant,
        [0, 0, 40, 64, 0, 20, 0, 128, 10, 0, 0, 32, 0, 0, 128, 0]
    );
    assert_eq!(m.ext.get::<SurfaceStore>().stats.presentations, 0);
    let mut source = [0; 16];
    m.cpu.read(src, &mut source).unwrap();
    assert_eq!(source, original);
    for mode in 0..3 {
        m.cpu.write(dst, &[0x77; 64]).unwrap();
        let positions: &[(u32, u32, [u16; 4])] = if mode < 2 {
            &[(0, 0, [0, 0, 0, 0xffff]), (4, 4, [0xff00, 0, 0, 0xffff])]
        } else {
            &[
                (0, 0, [0, 0, 0, 0xffff]),
                (4, 0, [0xff00, 0, 0, 0xffff]),
                (0, 4, [0, 0xff00, 0, 0xffff]),
            ]
        };
        for (i, &(x, y, colors)) in positions.iter().enumerate() {
            let bytes: Vec<u8> = x
                .to_le_bytes()
                .into_iter()
                .chain(y.to_le_bytes())
                .chain(colors.into_iter().flat_map(u16::to_le_bytes))
                .collect();
            m.cpu.write(DATA + 256 + i as u32 * 16, &bytes).unwrap();
        }
        for i in 0..3 {
            m.cpu.write_u32(DATA + 320 + i * 4, i).unwrap();
        }
        let entry = CODE + 1024 + mode * 256;
        let mut code = Asm::new(entry);
        call(
            &mut m,
            &mut code,
            "gdi32.dll",
            "SelectClipRgn",
            &[0xffff_0004, 0],
            DATA + 396,
        );
        let name = if alias {
            "GdiGradientFill"
        } else {
            "GradientFill"
        };
        call(
            &mut m,
            &mut code,
            dll,
            name,
            &[
                0xffff_0004,
                DATA + 256,
                positions.len() as u32,
                DATA + 320,
                1,
                mode,
            ],
            DATA + 400,
        );
        // Validate all mesh entries before the first pixel: index 99 is invalid.
        m.cpu.write_u32(DATA + 336, 99).unwrap();
        m.cpu.write_u32(DATA + 340, 1).unwrap();
        m.cpu.write_u32(DATA + 344, 2).unwrap();
        call(
            &mut m,
            &mut code,
            dll,
            name,
            &[
                0xffff_0004,
                DATA + 256,
                positions.len() as u32,
                DATA + 336,
                1,
                mode,
            ],
            DATA + 404,
        );
        code.ret();
        m.cpu.write(entry, &code.bytes).unwrap();
        m.spawn("gradient DIB", entry, &[], 65536).unwrap();
        assert!(matches!(m.run_virtual(1_000_000), RunResult::Exited(_)));
        assert_eq!(m.cpu.read_u32(DATA + 400).unwrap(), 1);
        assert_eq!(m.cpu.read_u32(DATA + 404).unwrap(), 0);
        let mut pixels = [0; 64];
        m.cpu.read(dst, &mut pixels).unwrap();
        let mut expected = [0x77; 64];
        for y in 0..4 {
            for x in 0..4 {
                let pixel = if mode == 2 {
                    if x + y >= 4 {
                        continue;
                    }
                    [0, (y * 255 / 4) as u8, (x * 255 / 4) as u8, 0]
                } else {
                    [0, 0, ((if mode == 0 { x } else { y }) * 255 / 4) as u8, 0]
                };
                expected[(y * 4 + x) * 4..(y * 4 + x + 1) * 4].copy_from_slice(&pixel);
            }
        }
        assert_eq!(pixels, expected, "gradient mode {mode}");
    }
    assert!(
        m.ext
            .try_get::<ApiDiagnostics>()
            .is_none_or(|d| d.warnings.is_empty())
    );
    m.cpu.write_u32(DATA + 12, 16 << 16 | 1).unwrap();
    m.cpu.write_u32(DATA + 276, 4).unwrap(); // Nonempty rectangle needs dithering.
    let mut code = Asm::new(CODE + 2400);
    call(
        &mut m,
        &mut code,
        "gdi32.dll",
        "CreateDIBSection",
        &[0xffff_0000, DATA, 0, DATA + 352, 0, 0],
        DATA + 356,
    );
    code.mov_eax_mem(DATA + 356).push_eax();
    code.mov_eax_mem(DATA + 128).push_eax();
    code.call(m.resolve_import("gdi32.dll", "SelectObject").unwrap());
    let name = if alias {
        "GdiGradientFill"
    } else {
        "GradientFill"
    };
    call(
        &mut m,
        &mut code,
        dll,
        name,
        &[0xffff_0000, DATA + 256, 3, DATA + 320, 1, 0],
        DATA + 400,
    );
    call(
        &mut m,
        &mut code,
        "kernel32.dll",
        "GetLastError",
        &[],
        DATA + 404,
    );
    code.ret();
    m.cpu.write(CODE + 2400, &code.bytes).unwrap();
    m.spawn("unsupported gradient dithering", CODE + 2400, &[], 65536)
        .unwrap();
    assert!(matches!(m.run_virtual(1_000_000), RunResult::Exited(_)));
    assert_eq!(m.cpu.read_u32(DATA + 400).unwrap(), 0);
    assert_eq!(m.cpu.read_u32(DATA + 404).unwrap(), 50);
    let bits = m.cpu.read_u32(DATA + 352).unwrap();
    let mut pixels = [0; 32];
    m.cpu.read(bits, &mut pixels).unwrap();
    assert_eq!(pixels, [0; 32]);
    assert!(
        m.ext
            .get::<ApiDiagnostics>()
            .warnings
            .keys()
            .any(|(name, reason)| name
                == if alias {
                    "gdi32.dll!GdiGradientFill"
                } else {
                    "msimg32.dll!GradientFill"
                }
                && reason.contains("dithering"))
    );
}
#[test]
// spec: GFX-BLIT-1 GFX-BLIT-2
fn alpha_and_color_key_transfers_share_pixels_and_aliases() {
    for alias in [false, true] {
        fixture(JitCpu::new(PARK_ADDR, false).unwrap(), alias);
        fixture(JitCpu::new(PARK_ADDR, true).unwrap(), alias);
        fixture(UnicornCpu::new(PARK_ADDR).unwrap(), alias);
    }
}

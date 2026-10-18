use super::*;
use adv_cpu::{
    JitCpu,
    backend::{Guest, Reg},
};
use adv_device::surface::{MemoryOperations, MemoryView, PixelFormat, Snapshot};
use adv_machine::{Clock, Machine, layout::PARK_ADDR};
use std::borrow::Cow;

const DATA: u32 = 0x410000;
const INFO: u32 = DATA + 256;
const OUTPUT: u32 = DATA + 512;
const PIXELS: [u8; 24] = [
    0, 0, 255, 11, 0, 255, 0, 22, 255, 0, 0, 33, 255, 255, 255, 44, 0, 0, 0, 55, 128, 128, 128, 66,
];

fn env(m: &mut Machine<JitCpu>) -> Env<'_> {
    Env {
        context: m.sys.current,
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk: 0,
    }
}
fn memory(cpu: &mut dyn Guest) -> MemoryView<'_, dyn Guest + '_> {
    MemoryView::new(
        cpu,
        MemoryOperations {
            read: |cpu, addr, length| {
                let mut b = vec![0; length];
                cpu.read(addr, &mut b).map_err(|e| e.to_string())?;
                Ok(Cow::Owned(b))
            },
            write: |cpu, addr, bytes| cpu.write(addr, bytes).map_err(|e| e.to_string()),
            writable: |cpu, addr, length| {
                adv_abi::guest::check_range(cpu, addr, length as u32, Prot::WRITE)
                    .map_err(|e| e.to_string())
            },
            copy: |cpu, src, dst, length| {
                cpu.copy_memory(src, dst, length).map_err(|e| e.to_string())
            },
        },
    )
}
fn fixture(dib: bool, bottom_up: bool) -> (Machine<JitCpu>, u32, u32) {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    assert_eq!(
        registry.get("gdi32.dll", "GetDIBits").unwrap().conv,
        adv_abi::Conv::Stdcall { argc: 7 }
    );
    let mut m = Machine::new(
        JitCpu::new(PARK_ADDR, false).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/bitmap-readback-tests");
    crate::Win32::install(
        &mut m,
        crate::Win32Config::new(&root, "authored.exe", &root),
    )
    .unwrap();
    m.spawn("authored", PARK_ADDR, &[], 65536).unwrap();
    m.cpu.map(DATA, 8192, Prot::RW).unwrap();
    m.cpu.set_reg(Reg::Esp, DATA);
    let dc = super::super::window_dc(&mut env(&mut m), 0);
    let bitmap = if dib {
        m.cpu.map(0x500000, 4096, Prot::RW).unwrap();
        let mut store = SurfaceStore::new(2, 3, 0x500000, 4096, Snapshot::default()).unwrap();
        let bitmap = store
            .create(
                2,
                3,
                bottom_up,
                PixelFormat::Bgra32,
                &mut memory(&mut m.cpu),
            )
            .unwrap();
        let s = store.get(bitmap).unwrap();
        let raw: Vec<_> = if bottom_up {
            PIXELS
                .as_chunks::<8>()
                .0
                .iter()
                .rev()
                .flatten()
                .copied()
                .collect()
        } else {
            PIXELS.to_vec()
        };
        m.cpu.write(s.pixels, &raw).unwrap();
        assert!(m.ext.get_mut::<Win32>().gdi.add_at(
            &mut m.sys.objects,
            bitmap,
            GdiObj::Bitmap {
                width: 2,
                height: if bottom_up { 3 } else { -3 },
                bpp: 32,
                bits: s.pixels,
                stride: 8,
                owned: false,
                mapping_view: None,
                dib_colors: 0
            }
        ));
        m.ext.insert(store);
        bitmap
    } else {
        let (bitmap, bits) = super::super::new_bitmap(&mut env(&mut m), 2, 3, 32).unwrap();
        m.cpu.write(bits, &PIXELS).unwrap();
        bitmap
    };
    (m, dc, bitmap)
}
fn header(bpp: u16, height: i32) -> [u32; 13] {
    [
        40,
        2,
        height as u32,
        1 | (u32::from(bpp) << 16),
        0,
        0,
        123,
        456,
        0,
        0,
        0,
        0,
        0,
    ]
}
fn prepare(m: &mut Machine<JitCpu>, h: [u32; 13]) {
    m.cpu
        .write(
            INFO,
            &h.into_iter().flat_map(u32::to_le_bytes).collect::<Vec<_>>(),
        )
        .unwrap();
    m.cpu.write(OUTPUT, &[0xa5; 64]).unwrap();
}
fn invoke(
    m: &mut Machine<JitCpu>,
    dc: u32,
    bitmap: u32,
    start: u32,
    count: u32,
    output: u32,
) -> u32 {
    invoke_with_usage(m, dc, bitmap, start, count, output, 0)
}
fn invoke_with_usage(
    m: &mut Machine<JitCpu>,
    dc: u32,
    bitmap: u32,
    start: u32,
    count: u32,
    output: u32,
    usage: u32,
) -> u32 {
    for (i, a) in [dc, bitmap, start, count, output, INFO, usage]
        .into_iter()
        .enumerate()
    {
        m.cpu.write_u32(DATA + 4 + i as u32 * 4, a).unwrap();
    }
    let address = m.resolve_import("gdi32.dll", "GetDIBits").unwrap();
    let index = (address - adv_machine::layout::THUNK_BASE) / adv_machine::layout::THUNK_STRIDE;
    let handler = m.sys.thunks.get(index).f.handler;
    let Outcome::Return(result) = handler(&mut env(m)) else {
        panic!("readback must return")
    };
    result
}
fn bytes(m: &Machine<JitCpu>, addr: u32, length: usize) -> Vec<u8> {
    let mut b = vec![0; length];
    m.cpu.read(addr, &mut b).unwrap();
    b
}

#[test]
fn bitmap_readback_uses_live_common_bytes_and_validates_outputs_atomically() {
    for dib in [false, true] {
        for bottom_up in [false, true] {
            let (mut m, dc, bitmap) = fixture(dib, bottom_up);
            prepare(&mut m, header(32, -3));
            assert_eq!(invoke(&mut m, dc, bitmap, 0, 3, OUTPUT), 3);
            assert_eq!(bytes(&m, OUTPUT, 24), PIXELS);
            // A later guest write is visible, rather than a creation-time snapshot.
            let bits = match m.ext.get::<Win32>().gdi.get(&m.sys.objects, bitmap) {
                Some(GdiObj::Bitmap { bits, .. }) => *bits,
                _ => panic!("bitmap"),
            };
            m.cpu.write_u32(bits, 0x11223344).unwrap();
            prepare(&mut m, header(32, if dib && bottom_up { 3 } else { -3 }));
            assert_eq!(invoke(&mut m, dc, bitmap, 0, 3, OUTPUT), 3);
            assert_eq!(m.cpu.read_u32(OUTPUT).unwrap(), 0x11223344);
            let original = header(32, 3);
            prepare(&mut m, original);
            assert_eq!(invoke(&mut m, dc, bitmap, 0, 3, DATA + 8192 - 16), 0);
            assert_eq!(
                bytes(&m, INFO, 52),
                original
                    .into_iter()
                    .flat_map(u32::to_le_bytes)
                    .collect::<Vec<_>>()
            );
            assert_eq!(bytes(&m, OUTPUT, 64), [0xa5; 64]);
            prepare(&mut m, header(8, 3));
            assert_eq!(invoke(&mut m, dc, bitmap, 0, 3, OUTPUT), 0);
            let old = super::super::select_object_value(&mut env(&mut m), dc, bitmap);
            let Outcome::Return(old) = old else {
                panic!("select")
            };
            prepare(&mut m, header(32, 3));
            assert_eq!(invoke(&mut m, dc, bitmap, 0, 3, OUTPUT), 0);
            super::super::select_object_value(&mut env(&mut m), dc, old);
            assert_eq!(invoke(&mut m, 0xdeadbeef, bitmap, 0, 3, OUTPUT), 0);
        }
    }
}

#[test]
#[cfg(windows)]
fn dib_readback_matches_native_orientation_ranges_padding_and_queries() {
    use windows_sys::Win32::Graphics::Gdi as gdi;
    let (mut m, dc, bitmap) = fixture(true, false);
    let initial = header(32, -3);
    let mut pointer = core::ptr::null_mut();
    // SAFETY: aligned BITMAPINFO storage and an owned, unselected native DIB;
    // native calls receive counted output buffers large enough for every case.
    let native_dc = unsafe { gdi::CreateCompatibleDC(core::ptr::null_mut()) };
    let native = unsafe {
        gdi::CreateDIBSection(
            native_dc,
            initial.as_ptr().cast(),
            0,
            &mut pointer,
            core::ptr::null_mut(),
            0,
        )
    };
    assert!(!native.is_null() && !pointer.is_null());
    unsafe {
        core::ptr::copy_nonoverlapping(PIXELS.as_ptr(), pointer.cast(), PIXELS.len());
    }
    for bpp in [24, 32] {
        for height in [3, -3] {
            for (start, count) in [(0, 3), (0, 1), (1, 1), (2, 4), (3, 1), (0, 0)] {
                let mut info = header(bpp, height);
                let mut output = [0xa5; 64];
                let expected = unsafe {
                    gdi::GetDIBits(
                        native_dc,
                        native,
                        start,
                        count,
                        output.as_mut_ptr().cast(),
                        info.as_mut_ptr().cast(),
                        0,
                    )
                };
                prepare(&mut m, header(bpp, height));
                assert_eq!(
                    invoke(&mut m, dc, bitmap, start, count, OUTPUT),
                    expected as u32
                );
                assert_eq!(
                    bytes(&m, OUTPUT, 64),
                    output,
                    "bpp={bpp} height={height} start={start} count={count}"
                );
                assert_eq!(
                    bytes(&m, INFO, 52),
                    info.into_iter()
                        .flat_map(u32::to_le_bytes)
                        .collect::<Vec<_>>()
                );
            }
        }
    }
    let mut info = header(0, -3);
    let expected = unsafe {
        gdi::GetDIBits(
            native_dc,
            native,
            0,
            0,
            core::ptr::null_mut(),
            info.as_mut_ptr().cast(),
            0,
        )
    };
    prepare(&mut m, header(0, -3));
    assert_eq!(invoke(&mut m, dc, bitmap, 0, 0, 0), expected as u32);
    assert_eq!(
        bytes(&m, INFO, 52),
        info.into_iter()
            .flat_map(u32::to_le_bytes)
            .collect::<Vec<_>>()
    );
    assert_eq!(unsafe { gdi::DeleteObject(native) }, 1);
    assert_eq!(unsafe { gdi::DeleteDC(native_dc) }, 1);
}

#[test]
fn true_colour_readback_ignores_palette_flag_and_rejects_unknown_usage_without_output_writes() {
    for dib in [false, true] {
        let (mut m, dc, bitmap) = fixture(dib, false);
        for bpp in [24, 32] {
            prepare(&mut m, header(bpp, -3));
            assert_eq!(invoke_with_usage(&mut m, dc, bitmap, 0, 3, OUTPUT, 0), 3);
            let expected = bytes(&m, OUTPUT, 24);
            let expected_header = bytes(&m, INFO, 40);
            prepare(&mut m, header(bpp, -3));
            assert_eq!(invoke_with_usage(&mut m, dc, bitmap, 0, 3, OUTPUT, 1), 3);
            assert_eq!(bytes(&m, OUTPUT, 24), expected);
            assert_eq!(bytes(&m, INFO, 40), expected_header);
            prepare(&mut m, header(bpp, -3));
            let before = bytes(&m, INFO, 40);
            assert_eq!(invoke_with_usage(&mut m, dc, bitmap, 0, 3, OUTPUT, 2), 0);
            assert_eq!(bytes(&m, INFO, 40), before);
            assert_eq!(bytes(&m, OUTPUT, 24), [0xa5; 24]);
            assert_eq!(crate::util::last_error(&env(&mut m)), 87);
        }
    }
}

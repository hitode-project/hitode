use super::super::tests::{invoke, memory};
use super::*;
use crate::kernel32::file_mapping::{create_section, map_section};
use adv_cpu::{
    JitCpu,
    backend::{Backend, Reg},
};
use adv_device::surface::Snapshot;
use adv_machine::{Clock, Machine, ObjectLimits, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;

fn env<B: Backend>(m: &mut Machine<B>) -> Env<'_> {
    Env {
        context: m.sys.current,
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk: 0,
    }
}
fn value(outcome: Outcome) -> u32 {
    let Outcome::Return(value) = outcome else {
        panic!("synchronous mapping")
    };
    value
}
fn fixture<B: Backend>(cpu: B) -> (Machine<B>, u32) {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    let mut m = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/dib-mapping-tests");
    Win32::install(
        &mut m,
        crate::Win32Config::new(&root, "authored.exe", &root),
    )
    .unwrap();
    m.spawn("authored", PARK_ADDR, &[], 65536).unwrap();
    let data = m.sys.vm.alloc(&mut m.cpu, 4096, Prot::RW).unwrap();
    m.cpu.set_reg(Reg::Esp, data);
    let arena = m.sys.vm.alloc(&mut m.cpu, 4096, Prot::RW).unwrap();
    m.ext
        .insert(SurfaceStore::new(3, 2, arena, 4096, Snapshot::default()).unwrap());
    set_provider(
        &mut m.ext,
        DrawingProvider {
            memory,
            completed: |_, _| None,
        },
    );
    (m, data)
}
fn header<B: Backend>(m: &mut Machine<B>, address: u32, bpp: u16, bottom_up: bool) {
    let mut bytes = [0; 40];
    bytes[..4].copy_from_slice(&40u32.to_le_bytes());
    bytes[4..8].copy_from_slice(&3u32.to_le_bytes());
    bytes[8..12].copy_from_slice(&(if bottom_up { 2i32 } else { -2i32 }).to_le_bytes());
    bytes[12..14].copy_from_slice(&1u16.to_le_bytes());
    bytes[14..16].copy_from_slice(&bpp.to_le_bytes());
    m.cpu.write(address, &bytes).unwrap();
}

fn shared<B: Backend>(cpu: B) {
    let (mut m, data) = fixture(cpu);
    for offset in [40u32, 0x10028] {
        for bottom_up in [false, true] {
            let section = value(create_section(
                &mut env(&mut m),
                u32::MAX,
                0,
                4,
                0x20000,
                None,
            ));
            assert_ne!(section, 0);
            let view = value(map_section(&mut env(&mut m), section, 2, 0, 0, 0));
            header(&mut m, data + 128, 24, bottom_up);
            let initial: Vec<u8> = (1..=24).collect();
            m.cpu.write(view + offset, &initial).unwrap();
            let free_before = m.ext.get::<SurfaceStore>().arena_usage().free;
            let bitmap = invoke(
                &mut m,
                data,
                create_dib_ABI,
                &[0, data + 128, 0, data + 192, section, offset],
            );
            assert_ne!(bitmap, 0);
            let pixels = m.cpu.read_u32(data + 192).unwrap();
            assert_ne!(pixels, view + offset); // DIB owns a distinct view.
            let mut actual = [0; 24];
            m.cpu.read(pixels, &mut actual).unwrap();
            assert_eq!(actual, initial.as_slice()); // Existing section data was not zeroed.
            m.cpu.write(pixels + 1, &[0xf1]).unwrap();
            assert_eq!(m.cpu.read_u8(view + offset + 1).unwrap(), 0xf1);
            m.cpu.write(view + offset + 2, &[0xf2]).unwrap();
            assert_eq!(m.cpu.read_u8(pixels + 2).unwrap(), 0xf2);
            assert_eq!(m.ext.get::<SurfaceStore>().arena_usage().free, free_before);
            let dc = invoke(&mut m, data, create_dc_ABI, &[0]);
            assert_eq!(
                invoke(&mut m, data, select_object_ABI, &[dc, bitmap]),
                STOCK_BITMAP
            );
            assert_eq!(invoke(&mut m, data, delete_object_ABI, &[bitmap]), 0);
            assert!(m.cpu.accessible(pixels, 24, Prot::RW));
            assert_eq!(value(unmap_section(&mut env(&mut m), view)), 1);
            assert!(m.sys.objects.close(section).is_some());
            assert_eq!(
                invoke(
                    &mut m,
                    data,
                    bit_blt_ABI,
                    &[WINDOW_DC, 0, 0, 3, 2, dc, 0, 0, 0x00cc0020]
                ),
                1
            );
            let primary = m.ext.get::<SurfaceStore>().primary_id().unwrap();
            let source = m.ext.get::<SurfaceStore>().get(bitmap).unwrap();
            let destination = m.ext.get::<SurfaceStore>().get(primary).unwrap();
            for y in 0..2 {
                let src = source.row(0, y);
                let dst = destination.row(0, y);
                for x in 0..3 {
                    let mut color = [0; 3];
                    m.cpu.read(src + x * 3, &mut color).unwrap();
                    let mut pixel = [0; 4];
                    m.cpu.read(dst + x * 4, &mut pixel).unwrap();
                    // The display's reserved byte is not part of GDI RGB.
                    assert_eq!(&pixel[..3], &color);
                }
            }
            assert_eq!(
                invoke(&mut m, data, select_object_ABI, &[dc, STOCK_BITMAP]),
                bitmap
            );
            let arena_free = m.ext.get::<SurfaceStore>().arena_usage().free;
            assert_eq!(invoke(&mut m, data, delete_object_ABI, &[bitmap]), 1);
            assert!(!m.cpu.accessible(pixels, 24, Prot::READ));
            assert_eq!(m.ext.get::<SurfaceStore>().arena_usage().free, arena_free);
            assert_eq!(invoke(&mut m, data, delete_dc_ABI, &[dc]), 1);
        }
    }
}

#[test]
fn mapped_dib_aliases_sections_and_outlives_source_views_and_handles_on_three_backends() {
    shared(JitCpu::new(PARK_ADDR, false).unwrap());
    shared(JitCpu::new(PARK_ADDR, true).unwrap());
    shared(UnicornCpu::new(PARK_ADDR).unwrap());
}

#[test]
fn mapped_dib_rejects_alignment_range_rights_and_retires_failed_publication() {
    let (mut m, data) = fixture(JitCpu::new(PARK_ADDR, false).unwrap());
    header(&mut m, data + 128, 24, false);
    let rw = value(create_section(&mut env(&mut m), u32::MAX, 0, 4, 4096, None));
    let ro = value(create_section(&mut env(&mut m), u32::MAX, 0, 2, 4096, None));
    for (section, offset) in [
        (rw, 1),
        (rw, 4092),
        (rw, u32::MAX - 3),
        (ro, 40),
        (0xdeadbeef, 40),
    ] {
        m.cpu.write_u32(data + 192, 0xdeadbeef).unwrap();
        assert_eq!(
            invoke(
                &mut m,
                data,
                create_dib_ABI,
                &[0, data + 128, 0, data + 192, section, offset]
            ),
            0
        );
        assert_eq!(m.cpu.read_u32(data + 192).unwrap(), 0);
        assert_eq!(m.ext.get::<SurfaceStore>().live_surfaces(), 0);
    }
    // Refuse the GDI object's publication, then verify no orphan view or surface.
    m.sys.objects = Objects::with_limits(ObjectLimits {
        objects: 1,
        ..ObjectLimits::default()
    });
    let rw = value(create_section(&mut env(&mut m), u32::MAX, 0, 4, 4096, None));
    let candidate = m.sys.vm.reserve(4096).unwrap();
    m.sys.vm.free(&mut m.cpu, candidate).unwrap();
    assert_eq!(
        invoke(
            &mut m,
            data,
            create_dib_ABI,
            &[0, data + 128, 0, data + 192, rw, 40]
        ),
        0
    );
    assert_eq!(m.ext.get::<SurfaceStore>().live_surfaces(), 0);
    assert!(m.sys.vm.region(candidate).is_none());
    assert!(crate::kernel32::file_mapping::view_extent(&env(&mut m), candidate).is_none());
}

#[test]
fn write_copy_dib_uses_the_common_private_view_rules() {
    let (mut m, data) = fixture(JitCpu::new(PARK_ADDR, true).unwrap());
    header(&mut m, data + 128, 24, false);
    let section = value(create_section(&mut env(&mut m), u32::MAX, 0, 8, 4096, None));
    let view = value(map_section(&mut env(&mut m), section, 1, 0, 0, 0));
    let bitmap = invoke(
        &mut m,
        data,
        create_dib_ABI,
        &[0, data + 128, 0, data + 192, section, 40],
    );
    assert_ne!(bitmap, 0);
    let pixels = m.cpu.read_u32(data + 192).unwrap();
    m.cpu.write(pixels, &[0x31]).unwrap();
    assert_eq!(m.cpu.read_u8(view + 40).unwrap(), 0);
    m.cpu.write(view + 41, &[0x42]).unwrap();
    assert_eq!(m.cpu.read_u8(pixels + 1).unwrap(), 0);
    assert_eq!(value(unmap_section(&mut env(&mut m), view)), 1);
    assert!(m.sys.objects.close(section).is_some());
    assert_eq!(m.cpu.read_u8(pixels).unwrap(), 0x31);
    assert_eq!(invoke(&mut m, data, delete_object_ABI, &[bitmap]), 1);
}

#[cfg(windows)]
#[test]
fn native_mapped_dib_preserves_shared_bytes_and_survives_view_and_handle_close() {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateFileMappingW(
            file: usize,
            attributes: *const u8,
            protection: u32,
            high: u32,
            low: u32,
            name: *const u16,
        ) -> usize;
        fn MapViewOfFile(section: usize, access: u32, high: u32, low: u32, bytes: usize)
        -> *mut u8;
        fn UnmapViewOfFile(view: *const u8) -> i32;
        fn CloseHandle(handle: usize) -> i32;
    }
    #[link(name = "gdi32")]
    unsafe extern "system" {
        fn CreateDIBSection(
            dc: usize,
            info: *const u8,
            usage: u32,
            pixels: *mut *mut u8,
            section: usize,
            offset: u32,
        ) -> usize;
        fn DeleteObject(bitmap: *mut core::ffi::c_void) -> i32;
    }
    unsafe {
        for offset in [40usize, 0x10028] {
            let section = CreateFileMappingW(
                usize::MAX,
                std::ptr::null(),
                4,
                0,
                0x20000,
                std::ptr::null(),
            );
            assert_ne!(section, 0);
            let view = MapViewOfFile(section, 2, 0, 0, 0);
            assert!(!view.is_null());
            let mut info = [0u8; 40];
            info[..4].copy_from_slice(&40u32.to_le_bytes());
            info[4..8].copy_from_slice(&3u32.to_le_bytes());
            info[8..12].copy_from_slice(&(-2i32).to_le_bytes());
            info[12..14].copy_from_slice(&1u16.to_le_bytes());
            info[14..16].copy_from_slice(&24u16.to_le_bytes());
            for i in 0..24 {
                *view.add(offset + i) = i as u8 + 1;
            }
            let mut pixels = std::ptr::null_mut();
            let bitmap = CreateDIBSection(0, info.as_ptr(), 0, &mut pixels, section, offset as u32);
            assert_ne!(bitmap, 0);
            assert_eq!(
                std::slice::from_raw_parts(pixels, 24),
                &(1u8..=24).collect::<Vec<_>>()
            );
            *pixels.add(1) = 0xf1;
            assert_eq!(*view.add(offset + 1), 0xf1);
            *view.add(offset + 2) = 0xf2;
            assert_eq!(*pixels.add(2), 0xf2);
            assert_eq!(UnmapViewOfFile(view), 1);
            assert_eq!(CloseHandle(section), 1);
            assert_eq!(*pixels.add(1), 0xf1);
            assert_eq!(*pixels.add(2), 0xf2);
            assert_eq!(DeleteObject(bitmap as *mut core::ffi::c_void), 1);
        }
    }
}

use super::*;
use adv_cpu::{JitCpu, backend::GuestExt};
use adv_device::surface::{MemoryOperations, Snapshot};
use adv_machine::{Clock, layout};
use std::borrow::Cow;

fn memory(cpu: &mut dyn Guest) -> MemoryView<'_, dyn Guest + '_> {
    MemoryView::new(
        cpu,
        MemoryOperations {
            read: |cpu, address, length| {
                let mut data = vec![0; length];
                cpu.read(address, &mut data).map_err(|e| e.to_string())?;
                Ok(Cow::Owned(data))
            },
            write: |cpu, address, data| cpu.write(address, data).map_err(|e| e.to_string()),
            writable: |cpu, address, length| {
                adv_abi::guest::check_range(cpu, address, length as u32, Prot::WRITE)
                    .map_err(|e| e.to_string())
            },
            copy: |cpu, source, destination, length| {
                cpu.copy_memory(source, destination, length)
                    .map_err(|e| e.to_string())
            },
        },
    )
}
fn invoke(m: &mut Machine<JitCpu>, data: u32, api: adv_abi::Api, args: &[u32]) -> u32 {
    for (i, &arg) in args.iter().enumerate() {
        m.cpu.write_u32(data + 4 * (i as u32 + 1), arg).unwrap();
    }
    match (api.handler)(&mut Env {
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk: 0,
    }) {
        Outcome::Return(value) => value,
        _ => panic!("GDI resource request must return a Win32 value"),
    }
}
#[test]
fn dib_growth_uses_shared_vm_and_releases_pixels_with_standard_gdi_lifetime() {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    let mut m = Machine::new(
        JitCpu::new(layout::PARK_ADDR, false).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/dib-storage-tests");
    Win32::install(
        &mut m,
        crate::Win32Config::new(&root, "authored.exe", &root),
    )
    .unwrap();
    m.spawn("authored", layout::PARK_ADDR, &[], 65536).unwrap();
    let data = m.sys.vm.alloc(&mut m.cpu, 4096, Prot::RW).unwrap();
    m.cpu.set_reg(adv_cpu::backend::Reg::Esp, data);
    let arena = m.sys.vm.alloc(&mut m.cpu, 64, Prot::RW).unwrap();
    m.ext
        .insert(SurfaceStore::new(8, 2, arena, 64, Snapshot::default()).unwrap());
    set_provider(
        &mut m.ext,
        DrawingProvider {
            memory,
            completed: |_, _| None,
        },
    );
    let info = data + 128;
    let bits = data + 192;
    for (offset, value) in [(0, 40), (4, 8), (8, 2), (12, 0x0020_0001)] {
        m.cpu.write_u32(info + offset, value).unwrap();
    }
    let first = invoke(&mut m, data, create_dib_ABI, &[0, info, 0, bits, 0, 0]);
    let second = invoke(&mut m, data, create_dib_ABI, &[0, info, 0, bits, 0, 0]);
    assert_ne!(first, 0);
    assert_ne!(second, 0);
    let capacity = m.ext.get::<SurfaceStore>().arena_usage().capacity;
    assert_eq!(capacity, 64 + 16 * 1024 * 1024);
    let dc = invoke(&mut m, data, create_dc_ABI, &[0]);
    assert_eq!(
        invoke(&mut m, data, select_object_ABI, &[dc, first]),
        STOCK_BITMAP
    );
    assert_eq!(
        invoke(&mut m, data, select_object_ABI, &[dc, STOCK_BITMAP]),
        first
    );
    assert_eq!(invoke(&mut m, data, delete_object_ABI, &[first]), 1);
    assert_eq!(invoke(&mut m, data, delete_dc_ABI, &[dc]), 1);
    assert_eq!(invoke(&mut m, data, delete_object_ABI, &[second]), 1);
    assert_eq!(m.ext.get::<SurfaceStore>().arena_usage().pixel_bytes, 0);
    for _ in 0..16 {
        let bitmap = invoke(&mut m, data, create_dib_ABI, &[0, info, 0, bits, 0, 0]);
        assert_ne!(bitmap, 0);
        assert_eq!(invoke(&mut m, data, delete_object_ABI, &[bitmap]), 1);
    }
    assert_eq!(m.ext.get::<SurfaceStore>().arena_usage().capacity, capacity);
    // Deny any new region through the shared address-space owner, while the
    // existing guest buffers stay readable. The API must return OOM, not fail Machine.
    m.sys.vm = adv_machine::vm::Vm::new();
    assert!(
        m.sys
            .vm
            .reserve_at(layout::DYN_START, layout::DYN_END - layout::DYN_START)
    );
    m.cpu.write_u32(info + 4, 4096).unwrap();
    m.cpu.write_u32(info + 8, 2048).unwrap();
    m.cpu.write_u32(bits, 0xdead_beef).unwrap();
    assert_eq!(
        invoke(&mut m, data, create_dib_ABI, &[0, info, 0, bits, 0, 0]),
        0
    );
    assert_eq!(
        crate::util::last_error(&Env {
            cpu: &mut m.cpu,
            sys: &mut m.sys,
            ext: &mut m.ext,
            thunk: 0
        }),
        8
    );
    assert_eq!(m.cpu.read_u32(bits).unwrap(), 0);
    assert_eq!(
        (
            m.ext.get::<SurfaceStore>().arena_usage().capacity,
            m.ext.get::<SurfaceStore>().live_surfaces()
        ),
        (capacity, 0)
    );
}

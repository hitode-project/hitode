use super::*;
use adv_cpu::{JitCpu, backend::GuestExt};
use adv_device::surface::{MemoryOperations, Snapshot};
use adv_machine::{Clock, layout};
use std::borrow::Cow;

pub(super) fn memory(cpu: &mut dyn Guest) -> MemoryView<'_, dyn Guest + '_> {
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
pub(super) fn invoke<B: Backend>(
    m: &mut Machine<B>,
    data: u32,
    api: adv_abi::Api,
    args: &[u32],
) -> u32 {
    for (i, &arg) in args.iter().enumerate() {
        m.cpu.write_u32(data + 4 * (i as u32 + 1), arg).unwrap();
    }
    match (api.handler)(&mut Env {
        context: m.sys.current,
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk: 0,
    }) {
        Outcome::Return(value) => value,
        _ => panic!("GDI resource request must return a Win32 value"),
    }
}

// spec: DEV-FRAME-8, PROC-MACH-7
#[test]
fn win32_message_readback_and_wait_boundaries_publish_only_the_writer_frame() {
    use adv_device::surface::frame_mailbox;
    let mut registry = Registry::default();
    crate::register(&mut registry);
    crate::compat::register(&mut registry);
    let mut m = Machine::new(
        JitCpu::new(layout::PARK_ADDR, false).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    let mut registry = Registry::default();
    crate::register(&mut registry);
    crate::compat::register(&mut registry);
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/scratch/codex/frame-boundary-tests");
    let mut config = crate::Win32Config::new(&root, "authored.exe", &root);
    config.screen = (2, 2);
    Win32::install(&mut m, config).unwrap();
    m.spawn("writer", layout::PARK_ADDR, &[], 65536).unwrap();
    let data = m.sys.vm.alloc(&mut m.cpu, 4096, Prot::RW).unwrap();
    m.cpu.set_reg(adv_cpu::backend::Reg::Esp, data);
    let arena = m.sys.vm.alloc(&mut m.cpu, 4096, Prot::RW).unwrap();
    m.ext
        .insert(SurfaceStore::new(2, 2, arena, 4096, Snapshot::default()).unwrap());
    set_provider(
        &mut m.ext,
        DrawingProvider {
            memory,
            completed: |env, serial| {
                env.sys.frame = serial as u32;
                env.sys.complete_frame();
                None
            },
        },
    );
    let (tx, mut rx) = frame_mailbox();
    m.ext
        .get_mut::<SurfaceStore>()
        .set_frame_mailbox(tx, adv_sync::WakeEvent::new().unwrap());
    let dc = invoke(&mut m, data, get_dc_ABI, &[0]);
    let primary = m.ext.get::<SurfaceStore>().primary_id().unwrap();
    let queue = |m: &mut Machine<JitCpu>| {
        let store = m.ext.get_mut::<SurfaceStore>();
        store
            .fill_raw(
                primary,
                store[primary].rect(),
                0x123456,
                &mut memory(&mut m.cpu),
            )
            .unwrap();
        store
            .present_from(0, primary, &memory(&mut m.cpu), store[primary].rect())
            .unwrap();
    };
    for (name, args) in [
        ("PeekMessageA", vec![data + 128, 0, 0, 0, 0]),
        ("PeekMessageW", vec![data + 128, 0, 0, 0, 0]),
        ("GetMessageA", vec![data + 128, 0, 0, 0]),
        ("GetMessageW", vec![data + 128, 0, 0, 0]),
        ("MsgWaitForMultipleObjects", vec![0, 0, 0, 0, 0]),
        ("GetPixel", vec![dc, 0, 0]),
        ("BitBlt", vec![dc, 0, 0, 1, 1, dc, 0, 0, 0xcc0020]),
    ] {
        queue(&mut m);
        assert!(!rx.update());
        for (i, arg) in args.iter().enumerate() {
            m.cpu.write_u32(data + 4 + i as u32 * 4, *arg).unwrap();
        }
        let module = if matches!(name, "GetPixel" | "BitBlt") {
            "gdi32.dll"
        } else {
            "user32.dll"
        };
        let handler = registry.get(module, name).unwrap().handler;
        let entry = m.resolve_import(module, name).unwrap();
        let thunk = (entry - layout::THUNK_BASE) / layout::THUNK_STRIDE;
        handler(&mut Env {
            context: 0,
            cpu: &mut m.cpu,
            sys: &mut m.sys,
            ext: &mut m.ext,
            thunk,
        });
        assert!(rx.update(), "{name} must flush the pending frame");
    }

    let before = m.ext.get::<SurfaceStore>().stats.presentations;
    m.ext
        .get_mut::<SurfaceStore>()
        .present_from(
            0,
            primary,
            &memory(&mut m.cpu),
            Rect {
                x: 0,
                y: 0,
                w: 1,
                h: 1,
            },
        )
        .unwrap();
    let hook = m.sys.context_wait.unwrap();
    assert!(
        hook(&mut Env {
            context: 1,
            cpu: &mut m.cpu,
            sys: &mut m.sys,
            ext: &mut m.ext,
            thunk: 0
        })
        .is_none()
    );
    assert!(!rx.update());
    assert!(
        hook(&mut Env {
            context: 0,
            cpu: &mut m.cpu,
            sys: &mut m.sys,
            ext: &mut m.ext,
            thunk: 0
        })
        .is_none()
    );
    assert!(rx.update());
    assert_eq!(m.ext.get::<SurfaceStore>().stats.presentations, before + 1);
    assert_eq!(
        m.ext.get::<SurfaceStore>().stats.software_flushes,
        [1, 5, 0, 2]
    );
    assert_eq!(m.sys.completed_frames, 8);
    queue(&mut m);
    assert!(!rx.update());
    m.cpu.write_u32(data + 4, 0).unwrap();
    (registry.get("kernel32.dll", "ExitProcess").unwrap().handler)(&mut Env {
        context: 0,
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk: 0,
    });
    assert!(rx.update());
    assert_eq!(
        m.ext.get::<SurfaceStore>().stats.software_flushes,
        [1, 5, 0, 3]
    );
    assert_eq!(m.sys.completed_frames, 9);
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
            context: m.sys.current,
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

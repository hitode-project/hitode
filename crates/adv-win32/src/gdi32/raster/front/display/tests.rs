use super::*;
use adv_cpu::{
    JitCpu,
    backend::{GuestExt, Reg},
};
use adv_device::surface::{MemoryOperations, Snapshot};
use adv_machine::{Clock, layout};
use std::borrow::Cow;

fn memory(cpu: &mut dyn Guest) -> MemoryView<'_, dyn Guest + '_> {
    MemoryView::new(
        cpu,
        MemoryOperations {
            read: |cpu, address, length| {
                let mut data = vec![0; length];
                cpu.read(address, &mut data)
                    .map_err(|error| error.to_string())?;
                Ok(Cow::Owned(data))
            },
            write: |cpu, address, data| cpu.write(address, data).map_err(|error| error.to_string()),
            writable: |cpu, address, length| {
                adv_abi::guest::check_range(cpu, address, length as u32, Prot::WRITE)
                    .map_err(|error| error.to_string())
            },
            copy: |cpu, source, destination, length| {
                cpu.copy_memory(source, destination, length)
                    .map_err(|error| error.to_string())
            },
        },
    )
}
fn fixture() -> (Machine<JitCpu>, u32) {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    for name in ["CreateDCA", "CreateDCW"] {
        assert_eq!(
            registry.get("gdi32.dll", name).unwrap().conv,
            adv_abi::Conv::Stdcall { argc: 4 }
        );
        assert_eq!(
            registry.level("gdi32.dll", name),
            Some(adv_abi::ImplementationLevel::Implement)
        );
    }
    let mut machine = Machine::new(
        JitCpu::new(layout::PARK_ADDR, false).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/display-dc-tests");
    Win32::install(
        &mut machine,
        crate::Win32Config::new(&root, "authored.exe", &root),
    )
    .unwrap();
    machine.ext.get_mut::<Win32>().user.screen = (8, 2);
    machine
        .spawn("authored", layout::PARK_ADDR, &[], 65536)
        .unwrap();
    let data = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, 8192, Prot::RW)
        .unwrap();
    machine.cpu.set_reg(Reg::Esp, data);
    let arena = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, 4096, Prot::RW)
        .unwrap();
    machine
        .ext
        .insert(SurfaceStore::new(8, 2, arena, 4096, Snapshot::default()).unwrap());
    set_provider(
        &mut machine.ext,
        DrawingProvider {
            memory,
            completed: |_, _| None,
        },
    );
    machine.cpu.write(data + 256, b"dIsPlAy\0").unwrap();
    machine
        .cpu
        .write(
            data + 288,
            &"DISPLAY\0"
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>(),
        )
        .unwrap();
    machine
        .cpu
        .write(
            data + 320,
            &"\\\\.\\DISPLAY1\0"
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>(),
        )
        .unwrap();
    (machine, data)
}
fn invoke(machine: &mut Machine<JitCpu>, data: u32, module: &str, name: &str, args: &[u32]) -> u32 {
    let handler = (machine.sys.thunks.resolver() as &dyn std::any::Any)
        .downcast_ref::<Registry>()
        .unwrap()
        .get(module, name)
        .unwrap()
        .handler;
    machine.cpu.write_u32(data, 0x12345678).unwrap();
    for (index, value) in args.iter().enumerate() {
        machine
            .cpu
            .write_u32(data + 4 + index as u32 * 4, *value)
            .unwrap();
    }
    match handler(&mut Env {
        context: machine.sys.current,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: 0,
    }) {
        Outcome::Return(value) => value,
        _ => panic!("{name} did not return a Win32 value"),
    }
}
fn error(machine: &mut Machine<JitCpu>) -> u32 {
    crate::util::last_error(&Env {
        context: machine.sys.current,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: 0,
    })
}

#[test]
fn ansi_and_unicode_display_dcs_share_pixels_and_fonts_but_have_independent_owned_lifetimes() {
    let (mut machine, data) = fixture();
    let baseline = machine.sys.objects.object_count();
    invoke(
        &mut machine,
        data,
        "kernel32.dll",
        "SetLastError",
        &[0x76543210],
    );
    let a = invoke(
        &mut machine,
        data,
        "gdi32.dll",
        "CreateDCA",
        &[data + 256, 0, 0xdead0000, 0],
    );
    let primary_before = machine.ext.get::<SurfaceStore>().primary_id().unwrap();
    let pixel = machine
        .ext
        .get::<SurfaceStore>()
        .get(primary_before)
        .unwrap()
        .pixels;
    machine.cpu.write_u32(pixel, 0x01020304).unwrap();
    machine.ext.get_mut::<Win32>().user.screen = (1280, 720);
    let w = invoke(
        &mut machine,
        data,
        "gdi32.dll",
        "CreateDCW",
        &[data + 288, data + 320, 0xdead0000, 0],
    );
    let named = invoke(
        &mut machine,
        data,
        "gdi32.dll",
        "CreateDCW",
        &[0, data + 320, 0, 0],
    );
    let canvas = machine
        .ext
        .get::<SurfaceStore>()
        .get(primary_before)
        .unwrap();
    assert_eq!((canvas.width, canvas.height), (8, 2));
    assert_eq!(machine.cpu.read_u32(pixel).unwrap(), 0x01020304);
    machine.ext.get_mut::<Win32>().user.screen = (8, 2);
    assert_ne!(a, 0);
    assert_ne!(w, 0);
    assert_ne!(named, 0);
    assert_ne!(a, w);
    assert_ne!(named, w);
    assert_eq!(error(&mut machine), 0x76543210);
    let primary = machine.ext.get::<SurfaceStore>().primary_id().unwrap();
    for dc in [a, w, named] {
        assert_ne!(dc, WINDOW_DC);
        assert_eq!(
            machine.ext.get::<State>().dcs.get(&machine.sys.objects, dc),
            Some(primary)
        );
        assert!(matches!(
            machine.ext.get::<Win32>().gdi.get(&machine.sys.objects, dc),
            Some(adv_os::gdi::GdiObj::Dc {
                raster: true,
                hwnd: 0,
                ..
            })
        ));
        assert_eq!(
            invoke(&mut machine, data, "gdi32.dll", "GetDeviceCaps", &[dc, 8]),
            8
        );
        assert_eq!(
            invoke(&mut machine, data, "gdi32.dll", "GetDeviceCaps", &[dc, 12]),
            32
        );
        assert_eq!(
            invoke(&mut machine, data, "gdi32.dll", "GetMapMode", &[dc]),
            1
        );
    }
    assert_eq!(
        invoke(
            &mut machine,
            data,
            "gdi32.dll",
            "SetTextColor",
            &[a, 0x123456]
        ),
        0
    );
    assert_eq!(
        invoke(
            &mut machine,
            data,
            "gdi32.dll",
            "SetTextColor",
            &[w, 0x654321]
        ),
        0
    );
    assert!(matches!(
        machine.ext.get::<Win32>().gdi.get(&machine.sys.objects, a),
        Some(adv_os::gdi::GdiObj::Dc {
            text_color: 0x123456,
            ..
        })
    ));
    let text = data + 512;
    machine
        .cpu
        .write(
            text,
            &"漢字"
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>(),
        )
        .unwrap();
    assert_eq!(
        invoke(
            &mut machine,
            data,
            "gdi32.dll",
            "GetTextExtentPoint32W",
            &[w, text, 2, data + 544]
        ),
        1
    );
    assert!(machine.cpu.read_u32(data + 544).unwrap() > 0);
    assert!(machine.cpu.read_u32(data + 548).unwrap() > 0);
    // A display DC cannot be returned as a borrowed window DC or retired as a bitmap.
    assert_eq!(
        invoke(&mut machine, data, "user32.dll", "ReleaseDC", &[0, a]),
        0
    );
    assert_eq!(
        invoke(&mut machine, data, "gdi32.dll", "DeleteObject", &[a]),
        0
    );
    assert_eq!(
        invoke(
            &mut machine,
            data,
            "gdi32.dll",
            "SelectObject",
            &[a, STOCK_BITMAP]
        ),
        0
    );
    assert_eq!(
        machine.ext.get::<State>().dcs.get(&machine.sys.objects, a),
        Some(primary)
    );
    for dc in [a, w, named] {
        assert_eq!(
            invoke(&mut machine, data, "gdi32.dll", "DeleteDC", &[dc]),
            1
        );
        assert_eq!(
            invoke(&mut machine, data, "gdi32.dll", "DeleteDC", &[dc]),
            0
        );
        assert!(
            !machine
                .ext
                .get::<State>()
                .dcs
                .contains(&machine.sys.objects, dc)
        );
    }
    assert_eq!(machine.sys.objects.object_count(), baseline);
    assert_eq!(
        machine.ext.get::<SurfaceStore>().primary_id(),
        Some(primary)
    );
    assert_eq!(machine.ext.get::<SurfaceStore>().live_surfaces(), 1);
    let borrowed = invoke(&mut machine, data, "user32.dll", "GetDC", &[0]);
    assert_ne!(borrowed, 0);
    assert_eq!(
        invoke(&mut machine, data, "gdi32.dll", "DeleteDC", &[borrowed]),
        0
    );
    assert_eq!(
        invoke(
            &mut machine,
            data,
            "user32.dll",
            "ReleaseDC",
            &[0, borrowed]
        ),
        1
    );
    assert_eq!(machine.sys.objects.object_count(), baseline);
}

#[test]
fn invalid_strings_and_unsupported_devices_do_not_publish_dcs_or_mutate_guest_input() {
    let (mut machine, data) = fixture();
    let baseline = machine.sys.objects.object_count();
    for (name, args, expected) in [
        ("CreateDCA", [0, 0, 0, 0], 87),
        ("CreateDCW", [0xdead0000, 0, 0, 0], 87),
        ("CreateDCA", [data + 256, 0xdead0000, 0, 0], 87),
        ("CreateDCW", [data + 288, 0, 0, data + 640], 50),
    ] {
        assert_eq!(invoke(&mut machine, data, "gdi32.dll", name, &args), 0);
        assert_eq!(error(&mut machine), expected);
        assert_eq!(machine.sys.objects.object_count(), baseline);
    }
    let printer = data + 640;
    let bytes = "プリンター\0"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    machine.cpu.write(printer, &bytes).unwrap();
    assert_eq!(
        invoke(
            &mut machine,
            data,
            "gdi32.dll",
            "CreateDCW",
            &[0, printer, 0, 0]
        ),
        0
    );
    assert_eq!(error(&mut machine), 50);
    let mut after = vec![0; bytes.len()];
    machine.cpu.read(printer, &mut after).unwrap();
    assert_eq!(after, bytes);
    // Even a mapped UTF-16 buffer must be readable and terminated.
    machine
        .cpu
        .write(data + 4094, &(b'D' as u16).to_le_bytes())
        .unwrap();
    machine.cpu.protect(data + 4096, 4096, Prot::WRITE).unwrap();
    assert_eq!(
        invoke(
            &mut machine,
            data,
            "gdi32.dll",
            "CreateDCW",
            &[data + 4094, 0, 0, 0]
        ),
        0
    );
    assert_eq!(error(&mut machine), 87);
    let diagnostics = machine.ext.get::<adv_abi::ApiDiagnostics>();
    assert_eq!(diagnostics.warnings.len(), 2);
    assert_eq!(diagnostics.stub_calls, 0);
    assert_eq!(machine.sys.objects.object_count(), baseline);
    assert!(machine.ext.get::<SurfaceStore>().primary_id().is_none());
    // A missing raster backend is a real failure, not a placeholder DC.
    machine.ext.get_mut::<State>().provider = None;
    assert_eq!(
        invoke(
            &mut machine,
            data,
            "gdi32.dll",
            "CreateDCW",
            &[data + 288, 0, 0, 0]
        ),
        0
    );
    assert_eq!(error(&mut machine), 50);
    assert_eq!(machine.sys.objects.object_count(), baseline);
}

#[test]
fn information_contexts_share_device_queries_and_retire_without_a_raster_canvas() {
    let (mut machine, data) = fixture();
    let baseline = machine.sys.objects.object_count();
    let contexts = [
        invoke(
            &mut machine,
            data,
            "gdi32.dll",
            "CreateICA",
            &[data + 256, 0, 0xdead0000, 0],
        ),
        invoke(
            &mut machine,
            data,
            "gdi32.dll",
            "CreateICW",
            &[data + 288, data + 320, 0, 0],
        ),
    ];
    assert_ne!(contexts[0], 0);
    assert_ne!(contexts[1], 0);
    assert_ne!(contexts[0], contexts[1]);
    assert!(machine.ext.get::<SurfaceStore>().primary_id().is_none());
    for dc in contexts {
        assert_ne!(
            invoke(
                &mut machine,
                data,
                "gdi32.dll",
                "GetCurrentObject",
                &[dc, 6]
            ),
            0
        );
        assert_eq!(
            invoke(&mut machine, data, "gdi32.dll", "GetDeviceCaps", &[dc, 8]),
            8
        );
        assert_eq!(
            invoke(&mut machine, data, "gdi32.dll", "GetDeviceCaps", &[dc, 12]),
            32
        );
        assert_eq!(
            invoke(&mut machine, data, "gdi32.dll", "GetDeviceCaps", &[dc, 90]),
            96
        );
        assert!(
            !machine
                .ext
                .get::<State>()
                .dcs
                .contains(&machine.sys.objects, dc)
        );
    }
    // Selecting and querying fonts uses the same logical object as other DCs.
    machine.cpu.write(data + 1024, &[0; 60]).unwrap();
    machine.cpu.write_u32(data + 1024, 18).unwrap();
    machine.cpu.write_u32(data + 1040, 400).unwrap();
    machine.cpu.write(data + 1052, b"Arial\0").unwrap();
    let font = invoke(
        &mut machine,
        data,
        "gdi32.dll",
        "CreateFontIndirectA",
        &[data + 1024],
    );
    assert_ne!(font, 0);
    assert_ne!(
        invoke(
            &mut machine,
            data,
            "gdi32.dll",
            "SelectObject",
            &[contexts[0], font]
        ),
        0
    );
    assert_eq!(
        invoke(
            &mut machine,
            data,
            "gdi32.dll",
            "GetCurrentObject",
            &[contexts[0], 6]
        ),
        font
    );
    machine.cpu.write(data + 2048, b"Hello").unwrap();
    assert_eq!(
        invoke(
            &mut machine,
            data,
            "gdi32.dll",
            "GetTextExtentPoint32A",
            &[contexts[0], data + 2048, 5, data + 2304]
        ),
        1
    );
    assert!(machine.cpu.read_u32(data + 2304).unwrap() > 0);
    assert_eq!(
        invoke(&mut machine, data, "gdi32.dll", "DeleteObject", &[font]),
        0
    );
    for dc in contexts {
        assert_eq!(
            invoke(&mut machine, data, "gdi32.dll", "DeleteDC", &[dc]),
            1
        );
        assert_eq!(
            invoke(&mut machine, data, "gdi32.dll", "DeleteDC", &[dc]),
            0
        );
        assert_eq!(
            invoke(
                &mut machine,
                data,
                "gdi32.dll",
                "GetCurrentObject",
                &[dc, 6]
            ),
            0
        );
        assert_eq!(
            invoke(&mut machine, data, "gdi32.dll", "GetDeviceCaps", &[dc, 8]),
            0
        );
    }
    assert_eq!(
        invoke(&mut machine, data, "gdi32.dll", "DeleteObject", &[font]),
        1
    );
    // An information context does not require a raster frontend at all.
    machine.ext.get_mut::<State>().provider = None;
    let dc = invoke(
        &mut machine,
        data,
        "gdi32.dll",
        "CreateICW",
        &[0, data + 320, 0, 0],
    );
    assert_ne!(dc, 0);
    assert_eq!(
        invoke(&mut machine, data, "gdi32.dll", "DeleteDC", &[dc]),
        1
    );
    assert_eq!(machine.sys.objects.object_count(), baseline);
    assert!(machine.ext.get::<SurfaceStore>().primary_id().is_none());
}

use super::*;
use crate::Win32;
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Reg},
};
use adv_machine::{Clock, Machine, layout::PARK_ADDR};

fn env(machine: &mut Machine<JitCpu>) -> Env<'_> {
    Env {
        context: machine.sys.current,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: 0,
    }
}

fn fixture() -> (Machine<JitCpu>, u32, u32) {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    let mut machine = Machine::new(
        JitCpu::new(PARK_ADDR, false).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/scratch/brush-origin-tests");
    Win32::install(
        &mut machine,
        crate::Win32Config::new(&root, "authored.exe", &root),
    )
    .unwrap();
    machine.spawn("brush", PARK_ADDR, &[], 65536).unwrap();
    let data = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, 4096, Prot::RW)
        .unwrap();
    let dc = super::super::window_dc(&mut env(&mut machine), 7);
    assert_ne!(dc, 0);
    (machine, data, dc)
}

fn call(machine: &mut Machine<JitCpu>, data: u32, name: &str, args: &[u32]) -> u32 {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    let api = registry.get("gdi32.dll", name).unwrap();
    assert_eq!(
        api.conv,
        adv_abi::Conv::Stdcall {
            argc: args.len() as u8
        }
    );
    assert!(registry.dynamic_available("gdi32.dll", name));
    for (i, &arg) in args.iter().enumerate() {
        machine.cpu.write_u32(data + 4 + 4 * i as u32, arg).unwrap();
    }
    let old_esp = machine.cpu.reg(Reg::Esp);
    machine.cpu.set_reg(Reg::Esp, data);
    let result = (api.handler)(&mut env(machine));
    machine.cpu.set_reg(Reg::Esp, old_esp);
    match result {
        Outcome::Return(value) => value,
        _ => panic!("brush state must return synchronously"),
    }
}

fn point(machine: &Machine<JitCpu>, address: u32) -> [i32; 2] {
    [
        machine.cpu.read_u32(address).unwrap() as i32,
        machine.cpu.read_u32(address + 4).unwrap() as i32,
    ]
}

fn origin(machine: &mut Machine<JitCpu>, data: u32, dc: u32) -> [i32; 2] {
    assert_eq!(call(machine, data, "GetBrushOrgEx", &[dc, data + 128]), 1);
    point(machine, data + 128)
}

#[test]
fn brush_origin_is_shared_dc_state_with_signed_device_coordinates_and_saved_levels() {
    let (mut machine, data, dc) = fixture();
    let other = super::super::window_dc(&mut env(&mut machine), 8);
    assert_eq!(origin(&mut machine, data, dc), [0, 0]);
    for next in [[11_i32, -7], [10000, -10000], [i32::MAX, i32::MIN]] {
        let old = origin(&mut machine, data, dc);
        machine.cpu.write(data + 124, &[0xa5; 16]).unwrap();
        crate::util::set_last_error(&mut env(&mut machine), 0x12345678);
        assert_eq!(
            call(
                &mut machine,
                data,
                "SetBrushOrgEx",
                &[dc, next[0] as u32, next[1] as u32, data + 128]
            ),
            1
        );
        assert_eq!(point(&machine, data + 128), old);
        assert_eq!(machine.cpu.read_u32(data + 124).unwrap(), 0xa5a5a5a5);
        assert_eq!(machine.cpu.read_u32(data + 136).unwrap(), 0xa5a5a5a5);
        assert_eq!(crate::util::last_error(&env(&mut machine)), 0x12345678);
        assert_eq!(origin(&mut machine, data, dc), next);
        assert_eq!(origin(&mut machine, data, other), [0, 0]);
    }
    assert_eq!(call(&mut machine, data, "SaveDC", &[dc]), 1);
    assert_eq!(
        call(
            &mut machine,
            data,
            "SetBrushOrgEx",
            &[dc, (-2_i32) as u32, 5, 0]
        ),
        1
    );
    assert_eq!(
        call(
            &mut machine,
            data,
            "SetWindowOrgEx",
            &[dc, 10, (-9_i32) as u32, 0]
        ),
        1
    );
    assert_eq!(
        call(
            &mut machine,
            data,
            "SetViewportOrgEx",
            &[dc, (-3_i32) as u32, 4, 0]
        ),
        1
    );
    let gdi = machine.ext.get::<Win32>().gdi.clone();
    // Mapping modes beyond MM_TEXT are not exposed by the current setter.
    // Brush coordinates still belong to device space for any retained mode.
    let Some(GdiObj::Dc { modes, .. }) = gdi.get_mut(&mut machine.sys.objects, dc) else {
        panic!("DC owner")
    };
    modes.map = 2;
    let mut contexts = adv_os::gdi::DeviceContexts::new(gdi.clone());
    assert_eq!(
        contexts.set_stretch_mode(&mut machine.sys.objects, dc, 4),
        Some(1)
    );
    assert_eq!(origin(&mut machine, data, dc), [-2, 5]);
    assert_eq!(call(&mut machine, data, "SaveDC", &[dc]), 2);
    assert_eq!(call(&mut machine, data, "SetBrushOrgEx", &[dc, 7, 8, 0]), 1);
    assert_eq!(call(&mut machine, data, "RestoreDC", &[dc, u32::MAX]), 1);
    assert_eq!(origin(&mut machine, data, dc), [-2, 5]);
    assert_eq!(call(&mut machine, data, "RestoreDC", &[dc, 1]), 1);
    assert_eq!(origin(&mut machine, data, dc), [i32::MAX, i32::MIN]);
    assert!(
        matches!(gdi.get(&machine.sys.objects, dc), Some(GdiObj::Dc { modes, .. })
        if modes.map == 1 && modes.stretch == 1 && modes.window_origin == [0, 0]
            && modes.viewport_origin == [0, 0])
    );
    // External raster/DC providers use the same payload, and repeated registration
    // preserves brush alignment without requiring CPU pixels or a drawing provider.
    assert!(machine.ext.get_mut::<Win32>().gdi.register_external_dc(
        &mut machine.sys.objects,
        adv_os::gdi::WINDOW_DC,
        0
    ));
    assert_eq!(
        call(
            &mut machine,
            data,
            "SetBrushOrgEx",
            &[adv_os::gdi::WINDOW_DC, 0, 0, 0]
        ),
        1
    );
    assert_eq!(
        call(
            &mut machine,
            data,
            "SetBrushOrgEx",
            &[adv_os::gdi::WINDOW_DC, 21, 22, 0]
        ),
        1
    );
    assert!(machine.ext.get_mut::<Win32>().gdi.register_external_dc(
        &mut machine.sys.objects,
        adv_os::gdi::WINDOW_DC,
        9
    ));
    assert_eq!(origin(&mut machine, data, adv_os::gdi::WINDOW_DC), [21, 22]);
    assert!(gdi.remove_external_dc(&mut machine.sys.objects, dc));
    assert_eq!(
        call(&mut machine, data, "GetBrushOrgEx", &[dc, data + 128]),
        0
    );
    assert_eq!(crate::util::last_error(&env(&mut machine)), 87);
}

#[test]
fn invalid_brush_origin_outputs_fail_without_partial_writes_or_state_changes() {
    let (mut machine, data, dc) = fixture();
    assert_eq!(
        call(
            &mut machine,
            data,
            "SetBrushOrgEx",
            &[dc, 11, (-7_i32) as u32, 0]
        ),
        1
    );
    let pages = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, 8192, Prot::RW)
        .unwrap();
    machine.cpu.write_u32(pages + 4092, 0xdeadbeef).unwrap();
    machine.cpu.protect(pages + 4096, 4096, Prot(0)).unwrap();
    machine.cpu.protect(pages, 4096, Prot::READ).unwrap();
    let count = machine.sys.objects.object_count();
    for output in [1, u32::MAX - 3, pages, pages + 4092] {
        assert_eq!(
            call(&mut machine, data, "SetBrushOrgEx", &[dc, 31, 32, output]),
            0
        );
        assert_eq!(crate::util::last_error(&env(&mut machine)), 87);
        assert_eq!(origin(&mut machine, data, dc), [11, -7]);
        assert_eq!(call(&mut machine, data, "GetBrushOrgEx", &[dc, output]), 0);
        assert_eq!(crate::util::last_error(&env(&mut machine)), 87);
    }
    machine.cpu.protect(pages, 4096, Prot::RW).unwrap();
    assert_eq!(
        call(
            &mut machine,
            data,
            "SetBrushOrgEx",
            &[dc, 31, 32, pages + 4092]
        ),
        0
    );
    assert_eq!(machine.cpu.read_u32(pages + 4092).unwrap(), 0xdeadbeef);
    assert_eq!(origin(&mut machine, data, dc), [11, -7]);
    assert_eq!(call(&mut machine, data, "GetBrushOrgEx", &[dc, 0]), 0);
    assert_eq!(crate::util::last_error(&env(&mut machine)), 87);
    for invalid in [0, 0x1234, adv_os::gdi::stock_object(0)] {
        machine.cpu.write(data + 128, &[0xa5; 8]).unwrap();
        assert_eq!(
            call(&mut machine, data, "GetBrushOrgEx", &[invalid, data + 128]),
            0
        );
        assert_eq!(crate::util::last_error(&env(&mut machine)), 87);
        assert_eq!(
            call(
                &mut machine,
                data,
                "SetBrushOrgEx",
                &[invalid, 31, 32, data + 128]
            ),
            0
        );
        assert_eq!(crate::util::last_error(&env(&mut machine)), 6);
        assert_eq!(point(&machine, data + 128), [0xa5a5a5a5_u32 as i32; 2]);
    }
    assert_eq!(machine.sys.objects.object_count(), count);
}

#[cfg(windows)]
#[test]
fn brush_origin_matches_native_default_signed_coords_mapping_halftone_and_restore() {
    use windows_sys::Win32::{
        Foundation::{GetLastError, POINT as NativePoint, SetLastError},
        Graphics::Gdi as gdi,
    };
    struct NativeDc(gdi::HDC);
    impl Drop for NativeDc {
        fn drop(&mut self) {
            // SAFETY: this test owns the non-null DC until it is retired here.
            unsafe {
                gdi::DeleteDC(self.0);
            }
        }
    }
    let (mut machine, data, dc) = fixture();
    // SAFETY: every native call uses this owned DC and valid stack POINTs,
    // except documented null/invalid-handle cases with valid output pointers.
    unsafe {
        let native = NativeDc(gdi::CreateCompatibleDC(std::ptr::null_mut()));
        assert!(!native.0.is_null());
        let mut expected = NativePoint::default();
        assert_eq!(gdi::GetBrushOrgEx(native.0, &mut expected), 1);
        assert_eq!(origin(&mut machine, data, dc), [expected.x, expected.y]);
        for [x, y] in [[11_i32, -7], [10000, -10000], [i32::MAX, i32::MIN]] {
            assert_eq!(
                call(
                    &mut machine,
                    data,
                    "SetBrushOrgEx",
                    &[dc, x as u32, y as u32, data + 128]
                ),
                gdi::SetBrushOrgEx(native.0, x, y, &mut expected) as u32
            );
            assert_eq!(point(&machine, data + 128), [expected.x, expected.y]);
            assert_eq!(gdi::GetBrushOrgEx(native.0, &mut expected), 1);
            assert_eq!(origin(&mut machine, data, dc), [expected.x, expected.y]);
        }
        assert_eq!(
            call(&mut machine, data, "SaveDC", &[dc]),
            gdi::SaveDC(native.0) as u32
        );
        assert_eq!(
            call(&mut machine, data, "SetBrushOrgEx", &[dc, 0, 0, 0]),
            gdi::SetBrushOrgEx(native.0, 0, 0, std::ptr::null_mut()) as u32
        );
        assert_ne!(
            gdi::SetWindowOrgEx(native.0, 10, -9, std::ptr::null_mut()),
            0
        );
        assert_ne!(
            gdi::SetViewportOrgEx(native.0, -3, 4, std::ptr::null_mut()),
            0
        );
        assert_ne!(gdi::SetStretchBltMode(native.0, gdi::HALFTONE), 0);
        assert_eq!(gdi::GetBrushOrgEx(native.0, &mut expected), 1);
        assert_eq!(origin(&mut machine, data, dc), [expected.x, expected.y]);
        assert_eq!(
            call(&mut machine, data, "RestoreDC", &[dc, u32::MAX]),
            gdi::RestoreDC(native.0, -1) as u32
        );
        assert_eq!(gdi::GetBrushOrgEx(native.0, &mut expected), 1);
        assert_eq!(origin(&mut machine, data, dc), [expected.x, expected.y]);
        for name in ["GetBrushOrgEx", "SetBrushOrgEx"] {
            expected = NativePoint { x: 77, y: 88 };
            SetLastError(0x12345678);
            let native_result = if name == "GetBrushOrgEx" {
                gdi::GetBrushOrgEx(std::ptr::null_mut(), &mut expected)
            } else {
                gdi::SetBrushOrgEx(std::ptr::null_mut(), 1, 2, &mut expected)
            };
            let native_error = GetLastError();
            let args: &[u32] = if name == "GetBrushOrgEx" {
                &[0, data + 128]
            } else {
                &[0, 1, 2, data + 128]
            };
            assert_eq!(call(&mut machine, data, name, args), native_result as u32);
            assert_eq!(crate::util::last_error(&env(&mut machine)), native_error);
            assert_eq!([expected.x, expected.y], [77, 88]);
        }
        SetLastError(0x12345678);
        assert_eq!(gdi::GetBrushOrgEx(native.0, std::ptr::null_mut()), 0);
        let native_error = GetLastError();
        assert_eq!(call(&mut machine, data, "GetBrushOrgEx", &[dc, 0]), 0);
        assert_eq!(crate::util::last_error(&env(&mut machine)), native_error);
    }
}

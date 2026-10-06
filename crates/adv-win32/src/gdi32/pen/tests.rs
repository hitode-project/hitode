use super::*;
use adv_cpu::{
    JitCpu,
    backend::{Guest, Reg},
};
use adv_machine::{Clock, Machine, layout::PARK_ADDR};

const DATA: u32 = 0x410000;

fn fixture() -> Machine<JitCpu> {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    assert_eq!(
        registry.get("gdi32.dll", "CreatePenIndirect").unwrap().conv,
        adv_abi::Conv::Stdcall { argc: 1 }
    );
    let mut m = Machine::new(
        JitCpu::new(PARK_ADDR, false).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/logical-pen-tests");
    crate::Win32::install(
        &mut m,
        crate::Win32Config::new(&root, "authored.exe", &root),
    )
    .unwrap();
    m.spawn("authored", PARK_ADDR, &[], 65536).unwrap();
    m.cpu.map(DATA, 4096, Prot::RW).unwrap();
    m.cpu.set_reg(Reg::Esp, DATA);
    m
}
fn env(m: &mut Machine<JitCpu>) -> Env<'_> {
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
        panic!("expected immediate GDI return")
    };
    value
}
fn bytes(m: &Machine<JitCpu>, address: u32, length: usize) -> Vec<u8> {
    let mut result = vec![0; length];
    m.cpu.read(address, &mut result).unwrap();
    result
}
fn invoke(m: &mut Machine<JitCpu>, style: u32, width: i32, y: i32, color: u32) -> u32 {
    for (index, word) in [style, width as u32, y as u32, color]
        .into_iter()
        .enumerate()
    {
        m.cpu
            .write_u32(DATA + 128 + index as u32 * 4, word)
            .unwrap();
    }
    m.cpu.write_u32(DATA + 4, DATA + 128).unwrap();
    let address = m.resolve_import("gdi32.dll", "CreatePenIndirect").unwrap();
    let index = (address - adv_machine::layout::THUNK_BASE) / adv_machine::layout::THUNK_STRIDE;
    let handler = m.sys.thunks.get(index).f.handler;
    value(handler(&mut env(m)))
}

#[test]
fn indirect_pen_preserves_logical_description_and_common_selection_lifetime() {
    let mut m = fixture();
    let baseline = m.sys.objects.object_count();
    let dc = super::super::window_dc(&mut env(&mut m), 0);
    let pen = invoke(&mut m, 3, -4, 123, 0x12abcdef);
    assert_ne!(pen, 0);
    let direct = value(super::super::create_pen(
        &mut env(&mut m),
        3,
        -4,
        0x12abcdef,
    ));
    assert_eq!(describe(&env(&mut m), pen), describe(&env(&mut m), direct));
    assert_eq!(
        value(super::super::get_object_a(
            &mut env(&mut m),
            Handle(pen),
            0,
            GuestPtr::new(0)
        )),
        16
    );
    m.cpu.write(DATA + 256, &[0xa5; 20]).unwrap();
    assert_eq!(
        value(super::super::get_object_a(
            &mut env(&mut m),
            Handle(pen),
            15,
            GuestPtr::new(DATA + 256)
        )),
        0
    );
    assert_eq!(bytes(&m, DATA + 256, 20), [0xa5; 20]);
    assert_eq!(
        value(super::super::get_object_a(
            &mut env(&mut m),
            Handle(pen),
            20,
            GuestPtr::new(DATA + 256)
        )),
        16
    );
    let expected: Vec<_> = [3u32, 4, 0, 0x12abcdef]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect();
    assert_eq!(bytes(&m, DATA + 256, 16), expected);
    assert_eq!(bytes(&m, DATA + 272, 4), [0xa5; 4]);
    let old = value(super::super::select_object_value(&mut env(&mut m), dc, pen));
    assert_eq!(old, stock_object(7));
    assert_eq!(
        value(super::super::delete_object_value(&mut env(&mut m), pen)),
        0
    );
    assert_eq!(
        value(super::super::select_object_value(&mut env(&mut m), dc, old)),
        pen
    );
    for h in [pen, direct] {
        assert_eq!(
            value(super::super::delete_object_value(&mut env(&mut m), h)),
            1
        );
    }
    assert!(describe(&env(&mut m), pen).is_none());
    assert!(super::super::release_dc(&mut env(&mut m), dc));
    assert_eq!(m.sys.objects.object_count(), baseline);
    for pointer in [0, 0xdeadbeef, DATA + 4096 - 12] {
        assert_eq!(
            value(create_indirect(&mut env(&mut m), GuestPtr::new(pointer))),
            0
        );
        assert_eq!(m.sys.objects.object_count(), baseline);
    }
    assert_eq!(invoke(&mut m, 0, i32::MIN, 0, 0), 0);
    assert_eq!(crate::util::last_error(&env(&mut m)), 87);
    assert_eq!(invoke(&mut m, 5, 4, 999, 0xffffff), stock_object(8));
    assert_eq!(m.sys.objects.object_count(), baseline);
}

#[test]
#[cfg(windows)]
fn logical_pen_description_matches_native_normalization() {
    use core::ffi::c_void;
    #[link(name = "gdi32")]
    unsafe extern "system" {
        fn CreatePenIndirect(input: *const u32) -> *mut c_void;
        fn GetObjectW(object: *mut c_void, count: i32, output: *mut u8) -> i32;
        fn DeleteObject(object: *mut c_void) -> i32;
    }
    let mut m = fixture();
    for (style, width) in [
        (0, 0),
        (0, -3),
        (1, 3),
        (5, 4),
        (5, i32::MIN),
        (6, 3),
        (7, 1),
        (u32::MAX, 1),
    ] {
        let raw = [style, width as u32, 99, 0x12abcdef];
        // SAFETY: LOGPEN consists of four 32-bit words. Both buffers remain live
        // through each native call and the pen is released before the next case.
        let native = unsafe { CreatePenIndirect(raw.as_ptr()) };
        assert!(!native.is_null());
        let mut bytes = [0; 16];
        assert_eq!(unsafe { GetObjectW(native, 16, bytes.as_mut_ptr()) }, 16);
        let actual = invoke(&mut m, style, width, 99, 0x12abcdef);
        assert_eq!(describe(&env(&mut m), actual), Some(bytes));
        assert_eq!(unsafe { DeleteObject(native) }, 1);
        assert_eq!(
            value(super::super::delete_object_value(&mut env(&mut m), actual)),
            1
        );
    }
}

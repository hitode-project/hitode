//! Authored pointer values exercise the documented process lifetime contract.
use super::*;
use adv_cpu::JitCpu;
use adv_machine::{Clock, Machine, layout::PARK_ADDR};

fn machine(cookie: u32) -> Machine<JitCpu> {
    let mut machine = Machine::new(
        JitCpu::new(PARK_ADDR, false).unwrap(),
        Registry::default(),
        Clock::virtual_time(),
    )
    .unwrap();
    machine.ext.insert(Cookie(cookie));
    machine
}
fn env(machine: &mut Machine<JitCpu>) -> Env<'_> {
    Env {
        context: 0,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: 0,
    }
}

#[test]
fn process_cookies_roundtrip_all_pointer_bits_and_encoded_null_without_dereferencing() {
    let mut first = machine(0x124578a5);
    let mut second = machine(0x124578c5);
    for pointer in [0, 1, 0x400000, 0xdeadbeef, u32::MAX] {
        let a = encode(&mut env(&mut first), pointer);
        let b = encode(&mut env(&mut second), pointer);
        assert_eq!(decode(&mut env(&mut first), a), pointer);
        assert_eq!(decode(&mut env(&mut second), b), pointer);
        assert_eq!(
            encode(&mut env(&mut first), pointer),
            a,
            "one process keeps its cookie"
        );
        assert_ne!(a, b, "distinct process state owns distinct encodings");
    }
    assert_ne!(encode(&mut env(&mut first), 0), 0);
    assert_eq!(encode(&mut env(&mut first), 0x124578a5), 0);
    assert_eq!(decode(&mut env(&mut first), 0), 0x124578a5);
}

#[cfg(windows)]
#[test]
fn native_kernel_and_rtl_share_roundtrip_and_encoded_null_and_preserve_last_error() {
    use std::ffi::c_void;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        #[link_name = "EncodePointer"]
        fn native_encode(pointer: *const c_void) -> *const c_void;
        #[link_name = "DecodePointer"]
        fn native_decode(pointer: *const c_void) -> *const c_void;
        #[link_name = "SetLastError"]
        fn native_set_error(error: u32);
        #[link_name = "GetLastError"]
        fn native_error() -> u32;
    }
    #[link(name = "ntdll")]
    unsafe extern "system" {
        #[link_name = "RtlEncodePointer"]
        fn rtl_encode(pointer: *const c_void) -> *const c_void;
        #[link_name = "RtlDecodePointer"]
        fn rtl_decode(pointer: *const c_void) -> *const c_void;
    }
    unsafe {
        native_set_error(0x12345678);
        for value in [0usize, 1, 0x400000, 0xdeadbeef, u32::MAX as usize] {
            let pointer = value as *const c_void;
            let encoded = native_encode(pointer);
            assert_eq!(encoded, rtl_encode(pointer));
            assert_eq!(native_decode(encoded), pointer);
            assert_eq!(rtl_decode(encoded), pointer);
        }
        assert_eq!(native_error(), 0x12345678);
    }
}

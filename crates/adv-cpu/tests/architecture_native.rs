//! Silent host instruction evidence for the writable architectural bit groups.
#![cfg(all(windows, target_arch = "x86_64"))]
use adv_cpu::ArchitecturalState;
use iced_x86::code_asm::*;

unsafe extern "system" {
    fn VirtualAlloc(
        at: *mut std::ffi::c_void,
        len: usize,
        kind: u32,
        prot: u32,
    ) -> *mut std::ffi::c_void;
    fn VirtualProtect(at: *mut std::ffi::c_void, len: usize, prot: u32, old: *mut u32) -> i32;
    fn VirtualFree(at: *mut std::ffi::c_void, len: usize, kind: u32) -> i32;
    fn FlushInstructionCache(
        process: *mut std::ffi::c_void,
        at: *const std::ffi::c_void,
        len: usize,
    ) -> i32;
}
struct Probe(*mut std::ffi::c_void);
impl Probe {
    fn new(f: impl FnOnce(&mut CodeAssembler)) -> Self {
        let mut a = CodeAssembler::new(64).unwrap();
        f(&mut a);
        let code = a.assemble(0).unwrap();
        // SAFETY: own a fresh ordinary code allocation, copy only while writable,
        // and retain it until every synchronous call returns.
        let at = unsafe { VirtualAlloc(std::ptr::null_mut(), code.len(), 0x3000, 4) };
        assert!(!at.is_null());
        let probe = Self(at);
        unsafe {
            std::ptr::copy_nonoverlapping(code.as_ptr(), at.cast(), code.len());
        }
        let mut old = 0;
        assert_ne!(unsafe { VirtualProtect(at, code.len(), 0x20, &mut old) }, 0);
        assert_ne!(
            unsafe { FlushInstructionCache(-1isize as *mut _, at, code.len()) },
            0
        );
        probe
    }
    fn call(&self, value: u32) -> u32 {
        // SAFETY: authored leaf has this Windows x64 ABI and restores control
        // words/flags before return. It never calls Rust with altered DF/CW.
        let function: unsafe extern "C" fn(u32) -> u32 = unsafe { std::mem::transmute(self.0) };
        unsafe { function(value) }
    }
}
impl Drop for Probe {
    fn drop(&mut self) {
        assert_ne!(unsafe { VirtualFree(self.0, 0, 0x8000) }, 0);
    }
}

#[test]
fn native_fldcw_observes_supported_controls_and_fixed_reserved_bits() {
    let probe = Probe::new(|a| {
        a.sub(rsp, 8).unwrap();
        a.fnstcw(word_ptr(rsp)).unwrap();
        a.mov(word_ptr(rsp + 2), cx).unwrap();
        a.fldcw(word_ptr(rsp + 2)).unwrap();
        a.fnstcw(word_ptr(rsp + 4)).unwrap();
        a.fldcw(word_ptr(rsp)).unwrap();
        a.movzx(eax, word_ptr(rsp + 4)).unwrap();
        a.add(rsp, 8).unwrap();
        a.ret().unwrap();
    });
    for input in [0x007f, 0x027f, 0x077f, 0x0f7f, 0x127f, 0xff3f, 0xffff] {
        // All exception masks remain set, so FNSTCW/restoration cannot expose
        // an ambient pending exception. No floating computation is performed.
        let expected = input & u32::from(ArchitecturalState::WRITABLE_X87_CONTROL) | 0x40;
        assert_eq!(probe.call(input), expected, "CW input {input:#x}");
    }
}
#[test]
fn native_popfq_preserves_arithmetic_direction_and_id_requested_bits() {
    const SAFE: u32 = 0x0020_0cd5; // TF/IF excluded from a running host probe.
    let probe = Probe::new(|a| {
        a.pushfq().unwrap();
        a.pop(r11).unwrap();
        a.mov(r8, r11).unwrap();
        a.and(r8, !(SAFE as i32)).unwrap();
        a.and(ecx, SAFE as i32).unwrap();
        a.or(r8, rcx).unwrap();
        a.push(r8).unwrap();
        a.popfq().unwrap();
        a.pushfq().unwrap();
        a.pop(rax).unwrap();
        a.push(r11).unwrap();
        a.popfq().unwrap();
        a.ret().unwrap();
    });
    assert_eq!(SAFE & !ArchitecturalState::WRITABLE_EFLAGS, 0);
    for flags in [0, SAFE, 0x401, 0x200044, 0x890] {
        assert_eq!(probe.call(flags) & SAFE, flags & SAFE);
    }
}

//! NT VM exports found through the guest PE table share kernel32's VM owner.
use adv_abi::{Conv, Registry};
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_os::Process;
use adv_win32::{Win32, Win32Config};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const READONLY: u32 = 0x420000;
const FIXED: u32 = 0x20000000;
const BASE: u32 = DATA;
const SIZE: u32 = DATA + 4;
const OWNER: u32 = DATA + 8;
const ALLOCATED_SIZE: u32 = DATA + 12;
const PROTECTED_SIZE: u32 = DATA + 16;
const FREED_SIZE: u32 = DATA + 20;
const PRESERVED: u32 = DATA + 24;
const ZEROED: u32 = DATA + 28;
const FIXED_BASE: u32 = DATA + 32;
const FIXED_SIZE: u32 = DATA + 36;
const INFO: u32 = DATA + 0x100;
const RESERVED_INFO: u32 = DATA + 0x120;
const RESULTS: u32 = DATA + 0x800;
const LAST_ERROR: u32 = 0x31415926;

struct Empty;
impl adv_os::FileSource for Empty {
    fn read(&self, _: &str) -> Option<Vec<u8>> {
        None
    }
}

// Read the actual guest-visible export directory, as manual loader lookups do.
fn export(machine: &Machine<JitCpu>, image: u32, wanted: &str) -> u32 {
    let pe = image + machine.cpu.read_u32(image + 0x3c).unwrap();
    let directory = image + machine.cpu.read_u32(pe + 0x78).unwrap();
    let count = machine.cpu.read_u32(directory + 24).unwrap();
    let functions = image + machine.cpu.read_u32(directory + 28).unwrap();
    let names = image + machine.cpu.read_u32(directory + 32).unwrap();
    let ordinals = image + machine.cpu.read_u32(directory + 36).unwrap();
    for index in 0..count {
        let name = image + machine.cpu.read_u32(names + index * 4).unwrap();
        let mut bytes = vec![0; wanted.len() + 1];
        machine.cpu.read(name, &mut bytes).unwrap();
        if bytes[..wanted.len()] == *wanted.as_bytes() && bytes[wanted.len()] == 0 {
            let mut ordinal = [0; 2];
            machine
                .cpu
                .read(ordinals + index * 2, &mut ordinal)
                .unwrap();
            return image
                + machine
                    .cpu
                    .read_u32(functions + u32::from(u16::from_le_bytes(ordinal)) * 4)
                    .unwrap();
        }
    }
    panic!("missing guest-visible ntdll export: {wanted}");
}

enum Arg {
    Value(u32),
    Saved(u32),
}
impl From<u32> for Arg {
    fn from(value: u32) -> Self {
        Self::Value(value)
    }
}
use Arg::{Saved as S, Value as V};

struct Fixture {
    machine: Machine<JitCpu>,
    code: Asm,
    image: u32,
    expected: Vec<(String, Arg)>,
}
impl Fixture {
    fn new(jit: bool) -> Self {
        let mut registry = Registry::default();
        adv_win32::register(&mut registry);
        for (name, argc) in [
            ("NtAllocateVirtualMemory", 6),
            ("ZwAllocateVirtualMemory", 6),
            ("NtFreeVirtualMemory", 4),
            ("ZwFreeVirtualMemory", 4),
        ] {
            assert_eq!(
                registry.get("ntdll.dll", name).unwrap().conv,
                Conv::Stdcall { argc }
            );
            assert!(registry.dynamic_available("ntdll.dll", name));
        }
        let mut machine = Machine::new(
            JitCpu::new(PARK_ADDR, jit).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        Win32::install(
            &mut machine,
            Win32Config::new(
                "/nonexistent/nt-vm",
                "fixture.exe",
                "/nonexistent/nt-vm-data",
            ),
        )
        .unwrap();
        let id = machine.load(&Empty, "ntdll.dll").unwrap();
        let image = adv_os::modules(&machine.ext).get(id).base;
        for address in [CODE, DATA, READONLY] {
            assert!(machine.sys.vm.reserve_at(address, 4096));
        }
        machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
        machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
        machine.cpu.map(READONLY, 4096, Prot::READ).unwrap();
        machine.cpu.write_u32(READONLY, FIXED).unwrap();
        let mut code = Asm::new(CODE);
        code.push_imm(LAST_ERROR).call(
            machine
                .resolve_import("kernel32.dll", "SetLastError")
                .unwrap(),
        );
        Self {
            machine,
            code,
            image,
            expected: Vec::new(),
        }
    }
    fn request(&mut self, base: u32, size: u32) {
        self.code.mov_mem_imm(BASE, base).mov_mem_imm(SIZE, size);
    }
    fn save_esp(&mut self, destination: u32) {
        self.code.bytes.extend_from_slice(&[0x89, 0x25]);
        self.code
            .bytes
            .extend_from_slice(&destination.to_le_bytes());
    }
    fn call(&mut self, dll: &str, name: &str, args: &[Arg], expected: impl Into<Arg>) {
        let row = RESULTS + self.expected.len() as u32 * 16;
        self.save_esp(row + 8);
        for arg in args.iter().rev() {
            match arg {
                V(value) => {
                    self.code.push_imm(*value);
                }
                S(address) => {
                    self.code.mov_eax_mem(*address).push_eax();
                }
            }
        }
        let entry = if dll == "ntdll.dll" {
            export(&self.machine, self.image, name)
        } else {
            self.machine.resolve_import(dll, name).unwrap()
        };
        // Exercise the synthetic export entry via an indirect guest call.
        self.code.bytes.push(0xb8); // mov eax, entry
        self.code.bytes.extend_from_slice(&entry.to_le_bytes());
        self.code.bytes.extend_from_slice(&[0xff, 0xd0]); // call eax
        self.code.mov_mem_eax(row);
        self.save_esp(row + 12);
        self.code
            .call(
                self.machine
                    .resolve_import("kernel32.dll", "GetLastError")
                    .unwrap(),
            )
            .mov_mem_eax(row + 4);
        self.expected.push((name.into(), expected.into()));
    }
    fn allocate(&mut self, name: &str, flags: u32, protection: u32, expected: u32) {
        self.call(
            "ntdll.dll",
            name,
            &[V(u32::MAX), V(BASE), V(0), V(SIZE), V(flags), V(protection)],
            expected,
        );
    }
    fn free(&mut self, name: &str, flags: u32, expected: u32) {
        self.call(
            "ntdll.dll",
            name,
            &[V(u32::MAX), V(BASE), V(SIZE), V(flags)],
            expected,
        );
    }
    fn run(mut self) -> Machine<JitCpu> {
        self.code.bytes.extend_from_slice(&[0x31, 0xc0, 0xc3]);
        assert!(self.code.bytes.len() <= 4096);
        self.machine.cpu.write(CODE, &self.code.bytes).unwrap();
        self.machine
            .spawn("NT virtual memory", CODE, &[], 65536)
            .unwrap();
        let result = self.machine.run_virtual(1_000_000);
        assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
        for (index, (name, expected)) in self.expected.iter().enumerate() {
            let row = RESULTS + index as u32 * 16;
            let expected = match expected {
                V(value) => *value,
                S(pointer) => self.machine.cpu.read_u32(*pointer).unwrap(),
            };
            assert_eq!(
                self.machine.cpu.read_u32(row).unwrap(),
                expected,
                "{name} #{index}"
            );
            assert_eq!(
                self.machine.cpu.read_u32(row + 4).unwrap(),
                LAST_ERROR,
                "LastError: {name} #{index}"
            );
            assert_eq!(
                self.machine.cpu.read_u32(row + 8).unwrap(),
                self.machine.cpu.read_u32(row + 12).unwrap(),
                "stack cleanup: {name} #{index}"
            );
        }
        self.machine
    }
}

#[test]
fn nt_vm_manual_exports_round_outputs_preserve_contents_and_share_kernel32_ownership() {
    for jit in [false, true] {
        let mut f = Fixture::new(jit);
        // MEM_COMMIT at an unspecified base also creates a reservation.
        f.request(0, 4097);
        f.allocate("NtAllocateVirtualMemory", 0x1000, 0x40, 0);
        f.code.mov_eax_mem(BASE).mov_mem_eax(OWNER);
        f.code
            .bytes
            .extend_from_slice(&[0xc7, 0x00, 0x78, 0x56, 0x34, 0x12]);
        f.code.mov_eax_mem(SIZE).mov_mem_eax(ALLOCATED_SIZE);
        f.call(
            "kernel32.dll",
            "VirtualQuery",
            &[S(OWNER), V(INFO), V(28)],
            28,
        );
        f.code
            .mov_eax_mem(OWNER)
            .add_eax_imm(4095)
            .mov_mem_eax(BASE)
            .mov_mem_imm(SIZE, 2);
        f.allocate("ZwAllocateVirtualMemory", 0x1000, 2, 0);
        f.code.mov_eax_mem(SIZE).mov_mem_eax(PROTECTED_SIZE);
        f.code
            .mov_eax_mem(OWNER)
            .mov_eax_ptr_eax()
            .mov_mem_eax(PRESERVED);
        f.code
            .mov_eax_mem(OWNER)
            .add_eax_imm(4095)
            .mov_mem_eax(BASE)
            .mov_mem_imm(SIZE, 2);
        f.free("NtFreeVirtualMemory", 0x4000, 0);
        f.call(
            "kernel32.dll",
            "VirtualQuery",
            &[S(OWNER), V(RESERVED_INFO), V(28)],
            28,
        );
        // kernel32 can recommit the NT reservation; decommitted data is zeroed.
        f.call(
            "kernel32.dll",
            "VirtualAlloc",
            &[S(OWNER), V(8192), V(0x1000), V(4)],
            S(OWNER),
        );
        f.code
            .mov_eax_mem(OWNER)
            .mov_eax_ptr_eax()
            .mov_mem_eax(ZEROED);
        f.code
            .mov_eax_mem(OWNER)
            .add_eax_imm(1)
            .mov_mem_eax(BASE)
            .mov_mem_imm(SIZE, 0);
        f.free("ZwFreeVirtualMemory", 0x8000, 0);
        f.code.mov_eax_mem(SIZE).mov_mem_eax(FREED_SIZE);
        f.request(FIXED + 0x1003, 1);
        f.allocate("NtAllocateVirtualMemory", 0x2000, 4, 0);
        f.code
            .mov_eax_mem(BASE)
            .mov_mem_eax(FIXED_BASE)
            .mov_eax_mem(SIZE)
            .mov_mem_eax(FIXED_SIZE);
        f.call(
            "kernel32.dll",
            "VirtualAlloc",
            &[V(FIXED + 4099), V(1), V(0x1000), V(4)],
            FIXED + 4096,
        );
        f.request(FIXED + 3, 1);
        f.allocate("ZwAllocateVirtualMemory", 0x1000, 4, 0);
        f.call(
            "kernel32.dll",
            "VirtualFree",
            &[V(FIXED), V(0), V(0x8000)],
            1,
        );
        let m = f.run();
        let word = |address| m.cpu.read_u32(address).unwrap();
        assert_eq!(word(ALLOCATED_SIZE), 8192);
        assert_eq!(word(PROTECTED_SIZE), 8192);
        assert_eq!(word(FREED_SIZE), 8192);
        assert_eq!(word(PRESERVED), 0x12345678);
        assert_eq!(word(ZEROED), 0);
        assert_eq!((word(FIXED_BASE), word(FIXED_SIZE)), (FIXED, 8192));
        assert_eq!((word(BASE), word(SIZE)), (FIXED, 4096));
        assert_eq!(
            (word(INFO + 12), word(INFO + 16), word(INFO + 20)),
            (8192, 0x1000, 0x40)
        );
        assert_eq!(
            (word(RESERVED_INFO + 12), word(RESERVED_INFO + 16)),
            (8192, 0x2000)
        );
        assert_eq!(m.sys.vm.containing(word(OWNER)), None);
        assert_eq!(m.sys.vm.containing(FIXED), None);
    }
}

#[test]
fn nt_vm_failures_leave_outputs_last_error_and_reservations_intact() {
    for jit in [false, true] {
        let mut f = Fixture::new(jit);
        f.request(FIXED + 3, 7);
        f.call(
            "ntdll.dll",
            "NtAllocateVirtualMemory",
            &[V(0), V(BASE), V(0), V(SIZE), V(0x3000), V(4)],
            0xc0000008,
        );
        f.call(
            "ntdll.dll",
            "NtAllocateVirtualMemory",
            &[V(u32::MAX), V(0), V(0), V(SIZE), V(0x3000), V(4)],
            0xc0000005,
        );
        f.call(
            "ntdll.dll",
            "NtAllocateVirtualMemory",
            &[V(u32::MAX), V(READONLY), V(0), V(SIZE), V(0x3000), V(4)],
            0xc0000005,
        );
        f.allocate("NtAllocateVirtualMemory", 0x3000, 8, 0xc0000045);
        f.free("NtFreeVirtualMemory", 0x8000, 0xc00000a0);
        f.free("NtFreeVirtualMemory", 0, 0xc00000f2);
        // Save the untouched input values before later legitimate requests.
        f.code
            .mov_eax_mem(BASE)
            .mov_mem_eax(FIXED_BASE)
            .mov_eax_mem(SIZE)
            .mov_mem_eax(FIXED_SIZE);
        f.request(0, 0);
        f.allocate("NtAllocateVirtualMemory", 0x3000, 4, 0xc000000d);
        f.request(CODE, 4096);
        f.allocate("NtAllocateVirtualMemory", 0x3000, 4, 0xc0000018);
        f.request(FIXED, 8192);
        f.allocate("NtAllocateVirtualMemory", 0x3000, 4, 0);
        f.request(FIXED + 4097, 0);
        f.free("NtFreeVirtualMemory", 0x8000, 0xc000009f);
        f.request(FIXED + 4096, 8192);
        f.free("NtFreeVirtualMemory", 0x4000, 0xc000001a);
        let m = f.run();
        assert_eq!(m.cpu.read_u32(FIXED_BASE).unwrap(), FIXED + 3);
        assert_eq!(m.cpu.read_u32(FIXED_SIZE).unwrap(), 7);
        assert_eq!(m.cpu.read_u32(BASE).unwrap(), FIXED + 4096);
        assert_eq!(m.cpu.read_u32(SIZE).unwrap(), 8192);
        assert_eq!(m.sys.vm.containing(FIXED), Some((FIXED, 8192)));
        assert!(m.cpu.accessible(FIXED, 8192, Prot::RW));
    }
}

#[test]
fn nt_protect_rejects_private_copy_pages_and_publishes_noaccess_old_protection() {
    const OLD: u32 = DATA + 0x30;
    const SNAPSHOTS: u32 = DATA + 0x40;
    const SENTINEL: u32 = 0x12345678;
    for jit in [false, true] {
        let mut f = Fixture::new(jit);
        f.request(FIXED, 8192);
        f.allocate("NtAllocateVirtualMemory", 0x3000, 4, 0);
        for (index, protection) in [8, 0x80].into_iter().enumerate() {
            f.request(FIXED + 123, 0x321);
            f.code.mov_mem_imm(OLD, SENTINEL);
            f.call(
                "ntdll.dll",
                "NtProtectVirtualMemory",
                &[V(u32::MAX), V(BASE), V(SIZE), V(protection), V(OLD)],
                0xc00000f2,
            );
            let snapshot = SNAPSHOTS + index as u32 * 16;
            f.code
                .mov_eax_mem(BASE)
                .mov_mem_eax(snapshot)
                .mov_eax_mem(SIZE)
                .mov_mem_eax(snapshot + 4)
                .mov_eax_mem(OLD)
                .mov_mem_eax(snapshot + 8);
            f.call(
                "kernel32.dll",
                "VirtualQuery",
                &[V(FIXED), V(INFO + index as u32 * 32), V(28)],
                28,
            );
        }
        let m = f.run();
        for index in 0..2 {
            let snapshot = SNAPSHOTS + index * 16;
            assert_eq!(m.cpu.read_u32(snapshot).unwrap(), FIXED + 123);
            assert_eq!(m.cpu.read_u32(snapshot + 4).unwrap(), 0x321);
            // Native initializes this output even though the request fails.
            assert_eq!(m.cpu.read_u32(snapshot + 8).unwrap(), 1);
            let info = INFO + index * 32;
            assert_eq!(m.cpu.read_u32(info + 16).unwrap(), 0x1000);
            assert_eq!(m.cpu.read_u32(info + 20).unwrap(), 4);
            assert_eq!(m.cpu.read_u32(info + 24).unwrap(), 0x20000);
        }
        assert_eq!(m.sys.vm.containing(FIXED), Some((FIXED, 8192)));
        assert!(m.cpu.accessible(FIXED, 8192, Prot::RW));
    }
}

#[cfg(windows)]
#[test]
fn native_nt_vm_confirms_commit_and_free_output_rounding() {
    use std::ffi::c_void;
    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn NtAllocateVirtualMemory(
            process: *mut c_void,
            base: *mut *mut c_void,
            zero: usize,
            size: *mut usize,
            flags: u32,
            protection: u32,
        ) -> i32;
        fn NtFreeVirtualMemory(
            process: *mut c_void,
            base: *mut *mut c_void,
            size: *mut usize,
            flags: u32,
        ) -> i32;
    }
    struct Reservation(*mut c_void);
    impl Drop for Reservation {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: This test owns the reservation, including panic cleanup.
                unsafe {
                    NtFreeVirtualMemory((-1isize) as _, &mut self.0, &mut 0, 0x8000);
                }
            }
        }
    }
    // SAFETY: Native calls only receive live, initialized output cells. The
    // reservation is owned here, never dereferenced, and released on every exit.
    unsafe {
        let process = (-1isize) as *mut c_void;
        let mut region = Reservation(std::ptr::null_mut());
        let mut size = 4097;
        assert_eq!(
            NtAllocateVirtualMemory(process, &mut region.0, 0, &mut size, 0x1000, 0x40),
            0
        );
        assert_eq!(size, 8192);
        let mut base = (region.0 as usize + 4095) as *mut c_void;
        size = 2;
        assert_eq!(
            NtFreeVirtualMemory(process, &mut base, &mut size, 0x4000),
            0
        );
        assert_eq!((base, size), (region.0, 8192));
        base = (region.0 as usize + 1) as *mut c_void;
        size = 0;
        assert_eq!(
            NtFreeVirtualMemory(process, &mut base, &mut size, 0x8000),
            0
        );
        assert_eq!((base, size), (region.0, 8192));
        region.0 = std::ptr::null_mut();
        base = std::ptr::null_mut();
        size = 1;
        assert_eq!(
            NtAllocateVirtualMemory(process, &mut base, 0, &mut size, 0x3000, 8) as u32,
            0xc0000045
        );
        assert!(base.is_null());
        assert_eq!(size, 1);
    }
}

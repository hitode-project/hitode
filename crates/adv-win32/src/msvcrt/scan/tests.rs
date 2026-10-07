use super::*;
use adv_abi::Conv;
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Reg, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
use std::cell::{Cell, RefCell};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const TAIL: u32 = 0x440000;

fn machine<B: Backend>(cpu: B) -> Machine<B> {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    super::super::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    for module in MODULES {
        let api = registry.get(module, "memchr").unwrap();
        assert_eq!(api.conv, Conv::Cdecl { argc: 3 });
        assert_eq!(
            adv_abi::signature::lookup(module, "memchr")
                .unwrap()
                .parameters
                .len(),
            3
        );
    }
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    let directory =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/crt-memchr-tests");
    crate::Win32::install(
        &mut machine,
        crate::Win32Config::new(&directory, "fixture.exe", &directory),
    )
    .unwrap();
    machine.cpu.map(CODE, PAGE_BYTES, Prot::RX).unwrap();
    machine.cpu.map(DATA, PAGE_BYTES, Prot::RW).unwrap();
    machine
}

fn guest_calls<B: Backend>(cpu: B) {
    let mut machine = machine(cpu);
    let source = DATA + 1024;
    machine
        .cpu
        .write(source, &[0, 0xff, 0x41, 0, 0xff])
        .unwrap();
    let set_error = machine
        .resolve_import("kernel32.dll", "SetLastError")
        .unwrap();
    let get_error = machine
        .resolve_import("kernel32.dll", "GetLastError")
        .unwrap();
    let errno = machine.resolve_import("msvcr90.dll", "_errno").unwrap();
    let mut code = Asm::new(CODE);
    code.push_imm(0x12345678).call(set_error);
    code.call(errno).mov_mem_eax(DATA + 512);
    code.bytes.extend_from_slice(&[0xc7, 0x00]); // mov dword ptr [eax],79
    code.bytes.extend_from_slice(&79u32.to_le_bytes());
    let mut golden = Vec::new();
    for module in MODULES {
        let entry = machine.resolve_import(module, "memchr").unwrap();
        for (buffer, value, count, result) in [
            (source, 0xffu32, 5, source + 1),  // first repeated high byte
            (source, u32::MAX, 5, source + 1), // negative int -> unsigned char
            (source, 0x100, 5, source),        // low byte is NUL; not string scanning
            (source, 0x141, 5, source + 2),
            (source, 0x41, 2, 0), // count excludes the next matching byte
            (source, 0x5a, 5, 0),
            (0, 0xff, 0, 0),
            (0xdeadffff, 0xff, 0, 0),
        ] {
            code.push_imm(count)
                .push_imm(value)
                .push_imm(buffer)
                .call(entry)
                .add_esp(12)
                .mov_mem_eax(DATA + golden.len() as u32 * 4);
            golden.push(result);
        }
    }
    code.mov_eax_mem(DATA + 512)
        .mov_eax_ptr_eax()
        .mov_mem_eax(DATA + 516);
    code.call(get_error).mov_mem_eax(DATA + 520);
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine
        .spawn("five CRT memchr cdecl fronts", CODE, &[], 65536)
        .unwrap();
    let result = machine.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    for (index, value) in golden.into_iter().enumerate() {
        assert_eq!(
            machine.cpu.read_u32(DATA + index as u32 * 4).unwrap(),
            value
        );
    }
    assert_eq!(machine.cpu.read_u32(DATA + 516).unwrap(), 79);
    assert_eq!(machine.cpu.read_u32(DATA + 520).unwrap(), 0x12345678);
}
#[test]
fn memchr_five_typed_cdecl_fronts_scan_unsigned_bytes_and_zero_count_in_interpreter_and_jit() {
    guest_calls(UnicornCpu::new(PARK_ADDR).unwrap());
    guest_calls(JitCpu::new(PARK_ADDR, false).unwrap());
}

fn real_page_boundaries<B: Backend>(cpu: B) {
    let mut machine = machine(cpu);
    machine.cpu.map(TAIL, PAGE_BYTES, Prot::RW).unwrap();
    let last = TAIL + PAGE_BYTES - 1;
    machine.cpu.write(last, &[0x7b]).unwrap();
    // An oversized declared count must not prevalidate the unmapped tail.
    assert_eq!(find(&machine.cpu, last, 0x7b, u32::MAX), Ok(last));
    assert_eq!(find(&machine.cpu, last, 0x5a, 1), Ok(0));
    assert_eq!(
        find(&machine.cpu, last, 0x5a, 2),
        Err(MemFault {
            addr: TAIL + PAGE_BYTES,
            len: 1
        })
    );
    machine
        .cpu
        .map(TAIL + PAGE_BYTES, PAGE_BYTES, Prot::RW)
        .unwrap();
    machine.cpu.write(TAIL + PAGE_BYTES, &[0x5a]).unwrap();
    machine
        .cpu
        .protect(TAIL + PAGE_BYTES, PAGE_BYTES, Prot::WRITE)
        .unwrap();
    // Host reads may bypass guest permissions; the scan must still enforce them.
    assert_eq!(find(&machine.cpu, last, 0x7b, 2), Ok(last));
    assert_eq!(
        find(&machine.cpu, last, 0x5a, 2),
        Err(MemFault {
            addr: TAIL + PAGE_BYTES,
            len: 1
        })
    );
    machine
        .cpu
        .protect(TAIL + PAGE_BYTES, PAGE_BYTES, Prot::READ)
        .unwrap();
    assert_eq!(find(&machine.cpu, last, 0x5a, 2), Ok(TAIL + PAGE_BYTES));
    assert_eq!(
        find(&machine.cpu, TAIL + PAGE_BYTES, 0x5a, 1),
        Ok(TAIL + PAGE_BYTES)
    );
    assert_eq!(
        find(&machine.cpu, 0, 0, 1),
        Err(MemFault { addr: 0, len: 1 })
    );
}

/// A narrow observable memory owner for the top of the32-bit address space and
/// sub-page readable prefixes that a real host cannot reliably allocate. Reads
/// intentionally ignore guest protection, as some backend host reads do.
struct ProbeGuest {
    base: u32,
    bytes: Vec<u8>,
    readable: bool,
    reads: RefCell<Vec<(u32, usize)>>,
    checks: Cell<usize>,
}
impl ProbeGuest {
    fn new(base: u32, bytes: &[u8]) -> Self {
        Self {
            base,
            bytes: bytes.to_vec(),
            readable: true,
            reads: RefCell::default(),
            checks: Cell::default(),
        }
    }
    fn covers(&self, address: u32, length: u32) -> bool {
        address >= self.base
            && u64::from(address) + u64::from(length)
                <= u64::from(self.base) + self.bytes.len() as u64
    }
}
impl Guest for ProbeGuest {
    fn reg(&self, _: Reg) -> u32 {
        unreachable!()
    }
    fn set_reg(&mut self, _: Reg, _: u32) {
        unreachable!()
    }
    fn accessible(&self, address: u32, length: u32, prot: Prot) -> bool {
        self.checks.set(self.checks.get() + 1);
        self.readable && prot == Prot::READ && self.covers(address, length)
    }
    fn read(&self, address: u32, out: &mut [u8]) -> Result<(), MemFault> {
        self.reads.borrow_mut().push((address, out.len()));
        if !self.covers(address, out.len() as u32) {
            return Err(MemFault {
                addr: address,
                len: out.len() as u32,
            });
        }
        let offset = (address - self.base) as usize;
        out.copy_from_slice(&self.bytes[offset..offset + out.len()]);
        Ok(())
    }
    fn write(&mut self, _: u32, _: &[u8]) -> Result<(), MemFault> {
        unreachable!()
    }
    fn map(&mut self, _: u32, _: u32, _: Prot) -> Result<(), MemFault> {
        unreachable!()
    }
    fn protect(&mut self, _: u32, _: u32, _: Prot) -> Result<(), MemFault> {
        unreachable!()
    }
    fn unmap(&mut self, _: u32, _: u32) -> Result<(), MemFault> {
        unreachable!()
    }
}

#[test]
fn memchr_early_hits_do_not_touch_unreadable_tails_and_addresses_never_wrap() {
    real_page_boundaries(UnicornCpu::new(PARK_ADDR).unwrap());
    real_page_boundaries(JitCpu::new(PARK_ADDR, false).unwrap());
    let zero = ProbeGuest::new(u32::MAX, &[]);
    assert_eq!(find(&zero, 0, 0, 0), Ok(0));
    assert_eq!(find(&zero, u32::MAX, 0, 0), Ok(0));
    assert_eq!(zero.checks.get(), 0);
    assert!(zero.reads.borrow().is_empty());
    let partial = ProbeGuest::new(0x1008, &[0x41]);
    assert_eq!(find(&partial, 0x1008, 0x41, 4096), Ok(0x1008));
    assert_eq!(&*partial.reads.borrow(), &[(0x1008, 1)]);
    let top = ProbeGuest::new(u32::MAX - 1, &[0x41, 0x7b]);
    assert_eq!(find(&top, u32::MAX - 1, 0x7b, u32::MAX), Ok(u32::MAX));
    assert_eq!(&*top.reads.borrow(), &[(u32::MAX - 1, 2)]);
    top.reads.borrow_mut().clear();
    assert_eq!(
        find(&top, u32::MAX - 1, 0x5a, 3),
        Err(MemFault {
            addr: u32::MAX - 1,
            len: 3
        })
    );
    assert_eq!(&*top.reads.borrow(), &[(u32::MAX - 1, 2)]);
    let mut protected = ProbeGuest::new(0x1008, &[0x41]);
    protected.readable = false;
    assert_eq!(
        find(&protected, 0x1008, 0x41, 1),
        Err(MemFault {
            addr: 0x1008,
            len: 1
        })
    );
    assert!(protected.reads.borrow().is_empty());
}

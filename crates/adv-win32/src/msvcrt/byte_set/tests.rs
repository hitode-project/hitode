use super::*;
use adv_cpu::{
    JitCpu, Region,
    backend::{Backend, GuestExt, Reg, asm::Asm},
};
use adv_machine::{Clock, Conv, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
use std::{cell::RefCell, collections::BTreeMap};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const TAIL: u32 = 0x440000;

trait TestCpu: Backend {
    fn region(&mut self, _: u32, _: usize) {}
}
impl TestCpu for UnicornCpu {}
impl TestCpu for JitCpu {
    fn region(&mut self, start: u32, size: usize) {
        self.executor_mut()
            .add_region(Region::new(start, size))
            .unwrap();
    }
}

fn guest<B: TestCpu>(cpu: B) {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    super::super::register(&mut registry);
    for module in MODULES {
        for name in ["strpbrk", "strcspn", "strspn"] {
            assert_eq!(
                registry.get(module, name).unwrap().conv,
                Conv::Cdecl { argc: 2 }
            );
            assert!(registry.dynamic_available(module, name));
        }
    }
    assert!(registry.signature_conflicts().is_empty());
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/crt-byte-set-tests");
    crate::Win32::install(
        &mut machine,
        crate::Win32Config::new(&root, "fixture.exe", &root),
    )
    .unwrap();
    machine.cpu.map(CODE, 8192, Prot::RX).unwrap();
    machine.cpu.map(DATA, 8192, Prot::RW).unwrap();
    machine.cpu.map(TAIL, 8192, Prot::RW).unwrap();
    machine
        .cpu
        .protect(TAIL + PAGE_BYTES, PAGE_BYTES, Prot(0))
        .unwrap();
    machine.ext.get_or_default::<adv_abi::ApiDiagnostics>();

    let mut input = DATA + 0x1500;
    let mut store = |bytes: &[u8]| {
        let address = input;
        machine.cpu.write(address, bytes).unwrap();
        input += bytes.len() as u32;
        address
    };
    let high = store(&[0xff, 0x80, b'A', 0xff, 0]);
    let high_set = store(&[0x80, 0xff, 0]);
    let empty = store(&[0]);
    let letter = store(b"A\0");
    let aaa = store(b"aaa\0");
    let repeated = store(b"abac\0");
    let ba = store(b"ba\0");
    let embedded = store(&[b'A', 0, b'z', 0]);
    let stopped_set = store(&[b'z', 0, b'A', 0]);
    let ff = store(&[0xff, 0]);
    let last = TAIL + PAGE_BYTES - 1;
    machine.cpu.write(last - 2, &[b'A', 0xff, 0]).unwrap();
    // The same seven valid VC90 native cases, including strings ending exactly
    // at the readable page boundary; no locale/UTF-8 interpretation occurs.
    let cases = [
        (high, high_set, [high, 0, 2]),
        (empty, letter, [0, 0, 0]),
        (aaa, empty, [0, 3, 0]),
        (repeated, ba, [repeated, 0, 3]),
        (embedded, stopped_set, [0, 1, 0]),
        (last - 2, ff, [last - 1, 1, 0]),
        (last, letter, [0, 0, 0]),
    ];
    let set_error = machine
        .resolve_import("kernel32.dll", "SetLastError")
        .unwrap();
    let get_error = machine
        .resolve_import("kernel32.dll", "GetLastError")
        .unwrap();
    let mut code = Asm::new(CODE);
    let mut expected = Vec::new();
    let mut output = DATA;
    let mut capture = |code: &mut Asm, value| {
        code.mov_mem_eax(output);
        expected.push((output, value));
        output += 4;
    };
    code.bytes.extend_from_slice(&[0x89, 0xe0]); // mov eax,esp
    code.mov_mem_eax(DATA + 0x1200);
    for (index, module) in MODULES.into_iter().enumerate() {
        let errno = machine.resolve_import(module, "_errno").unwrap();
        let cell = DATA + 0x1100 + index as u32 * 4;
        code.call(errno).mov_mem_eax(cell);
        code.bytes.extend_from_slice(&[0xc7, 0x00]);
        code.bytes.extend_from_slice(&79_u32.to_le_bytes());
        code.bytes.extend_from_slice(&[0xc7, 0x40, 4]);
        code.bytes.extend_from_slice(&91_u32.to_le_bytes());
        code.push_imm(0x12345678).call(set_error);
        for (api_index, name) in ["strpbrk", "strcspn", "strspn"].into_iter().enumerate() {
            let entry = machine.resolve_import(module, name).unwrap();
            for (source, control, results) in cases {
                code.push_imm(control)
                    .push_imm(source)
                    .call(entry)
                    .add_esp(8);
                capture(&mut code, results[api_index]);
                code.mov_eax_mem(cell).mov_eax_ptr_eax();
                capture(&mut code, 79);
                code.mov_eax_mem(cell).add_eax_imm(4).mov_eax_ptr_eax();
                capture(&mut code, 91);
                code.call(get_error);
                capture(&mut code, 0x12345678);
            }
        }
    }
    code.bytes.extend_from_slice(&[0x89, 0xe0]);
    code.mov_mem_eax(DATA + 0x1204);
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    assert!(code.bytes.len() < 8192);
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine.cpu.region(CODE, code.bytes.len());
    machine
        .spawn("CRT byte-set guest", CODE, &[], 65536)
        .unwrap();
    let result = machine.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    for (address, value) in expected {
        assert_eq!(
            machine.cpu.read_u32(address).unwrap(),
            value,
            "at {address:#x}"
        );
    }
    assert_eq!(
        machine.cpu.read_u32(DATA + 0x1200).unwrap(),
        machine.cpu.read_u32(DATA + 0x1204).unwrap()
    );

    // Defensive early-stop profile: a result in the readable source prefix does
    // not require a later page. A scan that actually needs that byte must fail.
    machine.cpu.write(last, &[0xff]).unwrap();
    assert_eq!(search(&machine.cpu, last, ff, Search::Pointer), Ok(last));
    assert_eq!(search(&machine.cpu, last, ff, Search::RejectSpan), Ok(0));
    assert_eq!(
        search(&machine.cpu, last, letter, Search::AcceptSpan),
        Ok(0)
    );
    assert_eq!(
        search(&machine.cpu, last, ff, Search::AcceptSpan),
        Err(ScanError::Memory(MemFault {
            addr: TAIL + PAGE_BYTES,
            len: 1
        }))
    );
    assert_eq!(
        search(&machine.cpu, high, last, Search::Pointer),
        Err(ScanError::Memory(MemFault {
            addr: TAIL + PAGE_BYTES,
            len: 1
        }))
    );
    let index = machine
        .sys
        .thunks
        .iter()
        .find(|(_, t)| t.module == "msvcr80.dll" && t.name == "strpbrk")
        .unwrap()
        .0;
    let mut env = Env {
        context: 0,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: index,
    };
    for (source, control) in [(0, high_set), (high, 0), (TAIL + PAGE_BYTES, high_set)] {
        assert!(matches!(
            strpbrk(
                &mut env,
                AnsiStr(adv_abi::GuestPtr::new(source)),
                AnsiStr(adv_abi::GuestPtr::new(control))
            ),
            Outcome::Fail(_)
        ));
    }
    for index in 0..3 {
        let cell = env.cpu.read_u32(DATA + 0x1100 + index * 4).unwrap();
        assert_eq!(env.cpu.read_u32(cell).unwrap(), 79);
        assert_eq!(env.cpu.read_u32(cell + 4).unwrap(), 91);
    }
    assert_eq!(crate::util::last_error(&env), 0x12345678);
    assert_eq!(env.ext.get::<adv_abi::ApiDiagnostics>().stub_calls, 0);
}

/// Observable sub-page memory, including the final two x86 addresses. Host reads
/// deliberately ignore permissions, so guards are independently tested.
struct ProbeGuest {
    bytes: BTreeMap<u32, u8>,
    reads: RefCell<Vec<(u32, usize)>>,
}
impl Guest for ProbeGuest {
    fn reg(&self, _: Reg) -> u32 {
        unreachable!()
    }
    fn set_reg(&mut self, _: Reg, _: u32) {
        unreachable!()
    }
    fn accessible(&self, address: u32, length: u32, prot: Prot) -> bool {
        prot == Prot::READ
            && (0..length).all(|offset| {
                address
                    .checked_add(offset)
                    .is_some_and(|a| self.bytes.contains_key(&a))
            })
    }
    fn read(&self, address: u32, out: &mut [u8]) -> Result<(), MemFault> {
        self.reads.borrow_mut().push((address, out.len()));
        let length = out.len() as u32;
        for (offset, value) in out.iter_mut().enumerate() {
            *value = *address
                .checked_add(offset as u32)
                .and_then(|a| self.bytes.get(&a))
                .ok_or(MemFault {
                    addr: address,
                    len: length,
                })?;
        }
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
fn byte_set_nine_cdecl_fronts_preserve_errors_unsigned_nul_and_checked_early_stop() {
    guest(UnicornCpu::new(PARK_ADDR).unwrap());
    guest(JitCpu::new(PARK_ADDR, false).unwrap());
    guest(JitCpu::new(PARK_ADDR, true).unwrap());
    let cpu = ProbeGuest {
        bytes: [(u32::MAX - 1, b'A'), (u32::MAX, 0xff)].into(),
        reads: RefCell::default(),
    };
    assert_eq!(
        scan_bytes(&cpu, u32::MAX - 1, 3, |byte, addr, _| (byte == 0xff)
            .then_some(addr)),
        Ok(u32::MAX)
    );
    assert_eq!(&*cpu.reads.borrow(), &[(u32::MAX - 1, 2)]);
    cpu.reads.borrow_mut().clear();
    assert_eq!(
        scan_bytes(&cpu, u32::MAX - 1, 3, |_, _, _| None),
        Err(ScanError::Memory(MemFault {
            addr: u32::MAX - 1,
            len: 3
        }))
    );
    assert_eq!(&*cpu.reads.borrow(), &[(u32::MAX - 1, 2)]);
    assert_eq!(
        scan_bytes(&cpu, u32::MAX - 1, 1, |_, _, _| None),
        Err(ScanError::Limit)
    );
}

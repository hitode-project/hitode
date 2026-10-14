//! An authored second COM front allocates through adv-os; Automation exports
//! must consume those same owned BSTRs without a second string registry.
use std::cell::Cell;

use adv_abi::{ApiDiagnostics, Registry};
use adv_cpu::{
    JitCpu,
    backend::{Backend, Guest, GuestExt, MemFault, Prot, Reg, asm::Asm},
};
use adv_machine::{Clock, Conv, Env, Machine, Outcome, RunResult, layout::PARK_ADDR};
use adv_os::{
    Heaps,
    com::{MAX_BSTR_BYTES, TaskMemory, alloc_bstr, copy_bstr, free_bstr, owns_bstr},
};
use adv_unicorn::UnicornCpu;
use adv_win32::{Win32, Win32Config};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const READ_ONLY: u32 = 0x420000;
const UNMAPPED: u32 = 0x600000;
const POINTER: u32 = DATA + 0x100;
const OLD: u32 = DATA + 0x104;
const NEW: u32 = DATA + 0x108;
const VARIANT: u32 = DATA + 0x200;
const STACK_BEFORE: u32 = DATA + 0x300;
const STACK_AFTER: u32 = DATA + 0x304;
const TEXT: &[u16] = &[0x63, 0, 0x3042];

struct Snapshot {
    pointer: u32,
    owned: bool,
    heap_size: Option<u32>,
    memory: Option<Vec<u8>>,
}
#[derive(Default)]
struct Snapshots(Vec<Snapshot>);

fn front_alloc(env: &mut Env<'_>) -> Outcome {
    let pointer = match env.arg(0) {
        0 => alloc_bstr(env, TEXT),
        1 => alloc_bstr(env, &[]),
        _ => alloc_bstr(env, &[0x3042; 2050]),
    };
    Outcome::Return(pointer)
}
fn front_free(env: &mut Env<'_>) -> Outcome {
    let pointer = env.arg(0);
    Outcome::Return(u32::from(free_bstr(env, pointer)))
}
fn snapshot(env: &mut Env<'_>) -> Outcome {
    let pointer = env.arg(0);
    let owned = owns_bstr(env, pointer);
    let base = pointer.checked_sub(4);
    let heap_size = base.and_then(|base| env.ext.get_mut::<Heaps>().size(env.cpu, 0, base));
    let memory = if owned {
        let base = base.unwrap();
        let length = env.cpu.read_u32(base).unwrap();
        assert!(length <= 5000);
        let mut bytes = vec![0; length as usize + 6];
        env.cpu.read(base, &mut bytes).unwrap();
        Some(bytes)
    } else {
        None
    };
    env.ext.get_or_default::<Snapshots>().0.push(Snapshot {
        pointer,
        owned,
        heap_size,
        memory,
    });
    Outcome::Return(0)
}
fn machine<B: Backend>(cpu: B) -> Machine<B> {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    for (name, handler) in [
        ("Alloc", front_alloc as adv_machine::Handler),
        ("Free", front_free),
        ("Snapshot", snapshot),
    ] {
        registry.add(
            "automation-fixture.dll",
            name,
            Conv::Stdcall { argc: 1 },
            handler,
        );
    }
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    Win32::install(
        &mut machine,
        Win32Config::new(
            "/nonexistent/automation",
            "fixture.exe",
            "/nonexistent/automation-data",
        ),
    )
    .unwrap();
    machine.cpu.map(CODE, 16384, Prot::RX).unwrap();
    machine.cpu.map(DATA, 16384, Prot::RW).unwrap();
    machine.cpu.map(READ_ONLY, 4096, Prot::READ).unwrap();
    machine.cpu.write(READ_ONLY, &[0xcd; 16]).unwrap();
    machine.cpu.write(DATA + 0x1000, b"ABC").unwrap();
    machine
}
fn call<B: Backend>(
    machine: &mut Machine<B>,
    code: &mut Asm,
    dll: &str,
    name: &str,
    args: &[u32],
    slot: u32,
) {
    for &arg in args.iter().rev() {
        code.push_imm(arg);
    }
    code.call(machine.resolve_import(dll, name).unwrap())
        .mov_mem_eax(DATA + slot * 4);
}
fn call_pointer<B: Backend>(
    machine: &mut Machine<B>,
    code: &mut Asm,
    dll: &str,
    name: &str,
    pointer_slot: u32,
    slot: u32,
) {
    code.mov_eax_mem(pointer_slot).push_eax();
    code.call(machine.resolve_import(dll, name).unwrap())
        .mov_mem_eax(DATA + slot * 4);
}
fn observe<B: Backend>(machine: &mut Machine<B>, code: &mut Asm, pointer_slot: u32) {
    call_pointer(
        machine,
        code,
        "automation-fixture.dll",
        "Snapshot",
        pointer_slot,
        63,
    );
}
fn run<B: Backend>(machine: &mut Machine<B>, mut code: Asm) -> RunResult {
    code.bytes.extend_from_slice(&[0x89, 0xe0]); // mov eax, esp
    code.mov_mem_eax(STACK_AFTER);
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine
        .spawn("shared Automation strings", CODE, &[], 65536)
        .unwrap();
    machine.run_virtual(1_000_000)
}
fn live(snapshot: &Snapshot, text: &[u16]) {
    assert_ne!(snapshot.pointer, 0);
    assert!(snapshot.owned);
    let mut expected = ((text.len() * 2) as u32).to_le_bytes().to_vec();
    expected.extend(text.iter().flat_map(|unit| unit.to_le_bytes()));
    expected.extend([0, 0]);
    assert_eq!(snapshot.heap_size, Some(expected.len() as u32));
    assert_eq!(snapshot.memory.as_deref(), Some(expected.as_slice()));
}
fn retired(snapshot: &Snapshot) {
    assert_ne!(snapshot.pointer, 0);
    assert!(!snapshot.owned);
    assert_eq!(snapshot.heap_size, None);
}
fn shared<B: Backend>(cpu: B) {
    let mut machine = machine(cpu);
    let mut code = Asm::new(CODE);
    code.bytes.extend_from_slice(&[0x89, 0xe0]);
    code.mov_mem_eax(STACK_BEFORE);
    call(
        &mut machine,
        &mut code,
        "kernel32.dll",
        "SetLastError",
        &[0x12345678],
        62,
    );
    // COM-front BSTR can be measured and freed by oleaut32.
    call(
        &mut machine,
        &mut code,
        "automation-fixture.dll",
        "Alloc",
        &[0],
        61,
    );
    code.mov_mem_eax(POINTER);
    observe(&mut machine, &mut code, POINTER);
    call_pointer(
        &mut machine,
        &mut code,
        "oleaut32.dll",
        "SysStringLen",
        POINTER,
        0,
    );
    call_pointer(
        &mut machine,
        &mut code,
        "oleaut32.dll",
        "SysStringByteLen",
        POINTER,
        1,
    );
    call_pointer(
        &mut machine,
        &mut code,
        "oleaut32.dll",
        "SysFreeString",
        POINTER,
        60,
    );
    observe(&mut machine, &mut code, POINTER);
    // VariantClear consumes a BSTR from that same frontend.
    call(
        &mut machine,
        &mut code,
        "automation-fixture.dll",
        "Alloc",
        &[0],
        61,
    );
    code.mov_mem_eax(POINTER).mov_mem_eax(VARIANT + 8);
    code.mov_mem_imm(VARIANT, 8)
        .mov_mem_imm(VARIANT + 4, 0x77777777)
        .mov_mem_imm(VARIANT + 12, 0x77777777);
    call(
        &mut machine,
        &mut code,
        "oleaut32.dll",
        "VariantClear",
        &[VARIANT],
        2,
    );
    observe(&mut machine, &mut code, POINTER);
    // Reallocation copies an old BSTR alias before retiring that shared owner.
    call(
        &mut machine,
        &mut code,
        "automation-fixture.dll",
        "Alloc",
        &[0],
        61,
    );
    code.mov_mem_eax(POINTER).mov_mem_eax(OLD);
    code.push_imm(2).add_eax_imm(2).push_eax().push_imm(POINTER);
    code.call(
        machine
            .resolve_import("oleaut32.dll", "SysReAllocStringLen")
            .unwrap(),
    )
    .mov_mem_eax(DATA + 3 * 4);
    observe(&mut machine, &mut code, OLD);
    observe(&mut machine, &mut code, POINTER);
    code.mov_eax_mem(POINTER).mov_mem_eax(NEW);
    // Oversize, inaccessible source, and unwritable output preserve the old BSTR.
    call(
        &mut machine,
        &mut code,
        "oleaut32.dll",
        "SysReAllocStringLen",
        &[POINTER, 0, u32::MAX],
        4,
    );
    call(
        &mut machine,
        &mut code,
        "oleaut32.dll",
        "SysReAllocStringLen",
        &[POINTER, UNMAPPED, 2],
        5,
    );
    call(
        &mut machine,
        &mut code,
        "oleaut32.dll",
        "SysReAllocStringLen",
        &[READ_ONLY, 0, 1],
        6,
    );
    observe(&mut machine, &mut code, POINTER);
    call(
        &mut machine,
        &mut code,
        "oleaut32.dll",
        "SysReAllocString",
        &[POINTER, 0],
        7,
    );
    observe(&mut machine, &mut code, NEW);
    // Odd-byte BSTRs from the guest also belong to the common frontend owner.
    call(
        &mut machine,
        &mut code,
        "oleaut32.dll",
        "SysAllocStringByteLen",
        &[DATA + 0x1000, 3],
        61,
    );
    code.mov_mem_eax(OLD);
    observe(&mut machine, &mut code, OLD);
    call_pointer(
        &mut machine,
        &mut code,
        "oleaut32.dll",
        "SysStringLen",
        OLD,
        8,
    );
    call_pointer(
        &mut machine,
        &mut code,
        "oleaut32.dll",
        "SysStringByteLen",
        OLD,
        9,
    );
    call_pointer(
        &mut machine,
        &mut code,
        "automation-fixture.dll",
        "Free",
        OLD,
        10,
    );
    observe(&mut machine, &mut code, OLD);
    // Empty and multi-chunk host strings use the same complete layout.
    for mode in [1, 2] {
        call(
            &mut machine,
            &mut code,
            "automation-fixture.dll",
            "Alloc",
            &[mode],
            61,
        );
        code.mov_mem_eax(OLD);
        observe(&mut machine, &mut code, OLD);
        call_pointer(
            &mut machine,
            &mut code,
            "oleaut32.dll",
            "SysFreeString",
            OLD,
            60,
        );
        observe(&mut machine, &mut code, OLD);
    }
    call(
        &mut machine,
        &mut code,
        "oleaut32.dll",
        "SysAllocStringByteLen",
        &[0, MAX_BSTR_BYTES + 1],
        11,
    );
    call(
        &mut machine,
        &mut code,
        "oleaut32.dll",
        "SysAllocStringLen",
        &[0, u32::MAX],
        12,
    );
    call(
        &mut machine,
        &mut code,
        "oleaut32.dll",
        "SysAllocStringByteLen",
        &[UNMAPPED, 1],
        13,
    );
    call(
        &mut machine,
        &mut code,
        "automation-fixture.dll",
        "Free",
        &[0],
        14,
    );
    call(
        &mut machine,
        &mut code,
        "kernel32.dll",
        "GetLastError",
        &[],
        15,
    );
    let result = run(&mut machine, code);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    for (slot, value) in [3, 6, 0, 1, 0, 0, 0, 1, 1, 3, 1, 0, 0, 0, 1, 0x12345678]
        .into_iter()
        .enumerate()
    {
        assert_eq!(
            machine.cpu.read_u32(DATA + slot as u32 * 4).unwrap(),
            value,
            "slot {slot}"
        );
    }
    assert_eq!(machine.cpu.read_u32(POINTER).unwrap(), 0);
    assert_eq!(
        machine.cpu.read_u32(STACK_BEFORE).unwrap(),
        machine.cpu.read_u32(STACK_AFTER).unwrap()
    );
    let mut variant = [0; 16];
    machine.cpu.read(VARIANT, &mut variant).unwrap();
    // Native VariantClear retires ownership and replaces only vt, retaining
    // the reserved and union bytes rather than zeroing the entire VARIANT.
    assert_eq!(&variant[..4], &[0; 4]);
    assert_eq!(&variant[4..8], &0x77777777u32.to_le_bytes());
    assert_eq!(&variant[12..16], &0x77777777u32.to_le_bytes());
    let mut readonly = [0; 16];
    machine.cpu.read(READ_ONLY, &mut readonly).unwrap();
    assert_eq!(readonly, [0xcd; 16]);
    let snapshots = &machine.ext.get::<Snapshots>().0;
    assert_eq!(snapshots.len(), 13);
    live(&snapshots[0], TEXT);
    retired(&snapshots[1]);
    retired(&snapshots[2]);
    retired(&snapshots[3]);
    live(&snapshots[4], &[0, 0x3042]);
    live(&snapshots[5], &[0, 0x3042]);
    retired(&snapshots[6]);
    assert_ne!(snapshots[3].pointer, snapshots[4].pointer);
    assert_eq!(snapshots[4].pointer, snapshots[5].pointer);
    assert!(snapshots[7].owned);
    assert_eq!(snapshots[7].heap_size, Some(9));
    assert_eq!(
        snapshots[7].memory.as_deref(),
        Some([3, 0, 0, 0, b'A', b'B', b'C', 0, 0].as_slice())
    );
    retired(&snapshots[8]);
    live(&snapshots[9], &[]);
    retired(&snapshots[10]);
    live(&snapshots[11], &[0x3042; 2050]);
    retired(&snapshots[12]);
    assert_eq!(
        machine
            .ext
            .try_get::<ApiDiagnostics>()
            .map_or(0, |diag| diag.stub_calls),
        0
    );
}

#[test]
fn common_bstr_owner_supports_sys_free_variant_clear_and_transactional_reallocation() {
    for jit in [false, true] {
        let mut cpu = JitCpu::new(PARK_ADDR, jit).unwrap();
        cpu.set_async_compilation(false);
        shared(cpu);
    }
    shared(UnicornCpu::new(PARK_ADDR).unwrap());
}

#[test]
fn foreign_and_already_retired_bstrs_keep_sys_free_string_failure_semantics() {
    for duplicate in [false, true] {
        let mut machine = machine(JitCpu::new(PARK_ADDR, false).unwrap());
        let mut code = Asm::new(CODE);
        if duplicate {
            call(
                &mut machine,
                &mut code,
                "automation-fixture.dll",
                "Alloc",
                &[0],
                0,
            );
            code.mov_mem_eax(POINTER);
            call_pointer(
                &mut machine,
                &mut code,
                "oleaut32.dll",
                "SysFreeString",
                POINTER,
                1,
            );
        } else {
            call(
                &mut machine,
                &mut code,
                "ole32.dll",
                "CoTaskMemAlloc",
                &[16],
                0,
            );
            code.mov_mem_eax(OLD).add_eax_imm(4).mov_mem_eax(POINTER);
        }
        call_pointer(
            &mut machine,
            &mut code,
            "oleaut32.dll",
            "SysFreeString",
            POINTER,
            2,
        );
        let RunResult::Failed(error) = run(&mut machine, code) else {
            panic!("invalid BSTR must fail");
        };
        assert!(
            error.message.contains("SysFreeString: invalid BSTR"),
            "{}",
            error.message
        );
        let pointer = machine.cpu.read_u32(POINTER).unwrap();
        assert_eq!(
            machine
                .ext
                .get_mut::<Heaps>()
                .size(&machine.cpu, 0, pointer - 4),
            if duplicate { None } else { Some(16) }
        );
        if !duplicate {
            assert!(
                machine
                    .ext
                    .get::<TaskMemory>()
                    .allocations
                    .contains(&(pointer - 4))
            );
        }
    }
}

/// Inject string source/payload faults without intercepting heap metadata access.
/// Retain the actual CPU mappings and common heap allocation bookkeeping.
struct AllocationFault<'a> {
    inner: &'a mut dyn Guest,
    reads: Cell<usize>,
    writes: usize,
    fail_read: Option<usize>,
    fail_write: Option<usize>,
    base: Option<u32>,
    allocation: std::ops::Range<u32>,
}
impl Guest for AllocationFault<'_> {
    fn reg(&self, register: Reg) -> u32 {
        self.inner.reg(register)
    }
    fn set_reg(&mut self, register: Reg, value: u32) {
        self.inner.set_reg(register, value);
    }
    fn read(&self, address: u32, bytes: &mut [u8]) -> Result<(), MemFault> {
        if address != DATA + 0x1000 {
            return self.inner.read(address, bytes);
        }
        self.reads.set(self.reads.get() + 1);
        if self.fail_read == Some(self.reads.get()) {
            return Err(MemFault {
                addr: address,
                len: bytes.len() as u32,
            });
        }
        self.inner.read(address, bytes)
    }
    fn write(&mut self, address: u32, bytes: &[u8]) -> Result<(), MemFault> {
        if !self.allocation.contains(&address) {
            return self.inner.write(address, bytes);
        }
        self.base.get_or_insert(address);
        self.writes += 1;
        if self.fail_write == Some(self.writes) {
            return Err(MemFault {
                addr: address,
                len: bytes.len() as u32,
            });
        }
        self.inner.write(address, bytes)
    }
    fn accessible(&self, address: u32, bytes: u32, protection: Prot) -> bool {
        self.inner.accessible(address, bytes, protection)
    }
    fn map(&mut self, address: u32, bytes: u32, protection: Prot) -> Result<(), MemFault> {
        self.inner.map(address, bytes, protection)
    }
    fn protect(&mut self, address: u32, bytes: u32, protection: Prot) -> Result<(), MemFault> {
        self.inner.protect(address, bytes, protection)
    }
    fn unmap(&mut self, address: u32, bytes: u32) -> Result<(), MemFault> {
        self.inner.unmap(address, bytes)
    }
}

#[test]
fn allocation_faults_retire_partial_strings_before_publishing_shared_ownership() {
    for fault in 0..=4 {
        let mut machine = machine(JitCpu::new(PARK_ADDR, false).unwrap());
        let size = if fault == 0 { 9 } else { 4106 };
        let heaps = machine.ext.get_mut::<Heaps>();
        let reserved = heaps
            .alloc(&mut machine.cpu, &mut machine.sys.vm, 0, size, false)
            .unwrap();
        assert!(heaps.free(&mut machine.cpu, &mut machine.sys.vm, 0, reserved));
        let base = {
            let mut cpu = AllocationFault {
                inner: &mut machine.cpu,
                reads: Cell::new(0),
                writes: 0,
                fail_read: (fault == 0).then_some(1),
                fail_write: (fault != 0).then_some(fault),
                base: None,
                allocation: reserved..reserved + size,
            };
            {
                let mut env = Env {
                    cpu: &mut cpu,
                    sys: &mut machine.sys,
                    ext: &mut machine.ext,
                    context: 0,
                    thunk: 0,
                };
                let pointer = if fault == 0 {
                    copy_bstr(&mut env, DATA + 0x1000, 3)
                } else {
                    // Header, first chunk, second chunk, and terminator writes.
                    alloc_bstr(&mut env, &[0x3042; 2050])
                };
                assert_eq!(pointer, 0, "fault {fault}");
            }
            cpu.base.unwrap()
        };
        assert_eq!(
            machine.ext.get_mut::<Heaps>().size(&machine.cpu, 0, base),
            None
        );
        let mut env = Env {
            cpu: &mut machine.cpu,
            sys: &mut machine.sys,
            ext: &mut machine.ext,
            context: 0,
            thunk: 0,
        };
        assert!(!owns_bstr(&env, base + 4));
        // Retry within the original allocation class.
        let text = if fault == 0 {
            TEXT.to_vec()
        } else {
            vec![0x3042; 2050]
        };
        let pointer = alloc_bstr(&mut env, &text);
        assert_eq!(pointer, base + 4, "failed allocation is reusable");
        assert!(owns_bstr(&env, pointer));
        assert!(free_bstr(&mut env, pointer));
        assert!(!owns_bstr(&env, pointer));
    }
}

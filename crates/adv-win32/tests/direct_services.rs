//! Authored native x86 INT 2E calls use existing NT owners and control flow.
//! Native counterpart: local owned NtContinue/NtRaiseException/APC fixture;
//! Win7 SP1 numeric metadata is independently pinned in the service directory.
use adv_abi::Registry;
use adv_cpu::{
    JitCpu, Region,
    backend::{Backend, GuestExt, Prot, Reg, asm::Asm},
};
use adv_machine::{Clock, Conv, Machine, Outcome, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
use std::path::Path;
#[path = "../src/kernel32/exception/context.rs"]
mod win_context;

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const ENTRY: u32 = 0x420000;
const STATUS_INVALID_SYSTEM_SERVICE: u32 = 0xc000001c;
const STATUS_ACCESS_VIOLATION: u32 = 0xc0000005;
const LAST_ERROR: u32 = 0x13572468;
const SERVICES: [(u32, &str, u8); 8] = [
    (50, "NtClose", 1),
    (64, "NtCreateEvent", 5),
    (323, "NtSetEvent", 2),
    (300, "NtResetEvent", 2),
    (391, "NtWaitForSingleObject", 3),
    (60, "NtContinue", 2),
    (271, "NtRaiseException", 3),
    (372, "NtTestAlert", 0),
];
trait LabCpu: Backend {
    fn region(&mut self, start: u32, len: usize);
}
impl LabCpu for JitCpu {
    fn region(&mut self, start: u32, len: usize) {
        self.executor_mut()
            .add_region(Region::new(start, len))
            .unwrap();
    }
}
impl LabCpu for UnicornCpu {
    fn region(&mut self, _: u32, _: usize) {}
}

struct Lab<B: LabCpu> {
    machine: Machine<B>,
    code: Asm,
}
fn imm(code: &mut Asm, op: &[u8], v: u32) {
    code.bytes.extend_from_slice(op);
    code.bytes.extend_from_slice(&v.to_le_bytes());
}
fn entry(service: u32, argc: u8) -> Vec<u8> {
    let mut code = Asm::new(0);
    imm(&mut code, &[0xb8], service);
    code.bytes
        .extend_from_slice(&[0x8d, 0x54, 0x24, 0x04, 0xcd, 0x2e]);
    code.ret_n(u16::from(argc) * 4);
    code.bytes
}
impl<B: LabCpu> Lab<B> {
    fn new(cpu: B, dir: &Path) -> Self {
        let mut registry = Registry::default();
        adv_win32::register(&mut registry);
        registry.add("fixture", "context", Conv::Stdcall { argc: 0 }, |env| {
            let mut state = env.checkpoint().unwrap().state;
            state.eip = CODE + 0x600;
            state.regs[4] += 4;
            state.regs[0] = 0x24681357;
            env.cpu
                .write(DATA + 0x400, &win_context::encode(&state))
                .unwrap();
            Outcome::Return(0)
        });
        let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
        machine.set_execution_budget(Some(std::time::Duration::from_secs(20)));
        adv_win32::Win32::install(
            &mut machine,
            adv_win32::Win32Config::new(dir, "owned.exe", dir.join("overlay")),
        )
        .unwrap();
        machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
        machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
        machine.cpu.map(ENTRY, 4096, Prot::RX).unwrap();
        // The authored entry directly implements the native x86 service ABI.
        // It needs no shared data mapping or indirect gateway pointer.
        for (i, &(id, _, argc)) in SERVICES.iter().enumerate() {
            let bytes = entry(id, argc);
            let address = ENTRY + i as u32 * 32;
            machine.cpu.write(address, &bytes).unwrap();
            machine.cpu.region(address, bytes.len());
        }
        Self {
            machine,
            code: Asm::new(CODE),
        }
    }
    fn call(&mut self, module: &str, name: &str, args: &[u32], out: u32) {
        let target = self.machine.resolve_import(module, name).unwrap();
        self.call_at(target, args, out);
    }
    fn call_at(&mut self, target: u32, args: &[u32], out: u32) {
        for &arg in args.iter().rev() {
            self.code.push_imm(arg);
        }
        self.code.call(target).mov_mem_eax(DATA + out);
    }
    fn service(&mut self, index: usize, args: &[u32], out: u32) {
        self.call_at(ENTRY + index as u32 * 32, args, out);
    }
    fn saved_regs(&mut self, offset: u32) {
        for (op, i) in [
            (0x1d, 0),
            (0x0d, 1),
            (0x15, 2),
            (0x2d, 3),
            (0x35, 4),
            (0x3d, 5),
            (0x25, 6),
        ] {
            imm(&mut self.code, &[0x89, op], DATA + offset + i * 4);
        }
        self.code.bytes.extend([0x9c, 0x8f, 0x05]);
        self.code.bytes.extend((DATA + offset + 28).to_le_bytes());
    }
    fn raw(&mut self, id: u32, ptr: u32, out: u32) {
        imm(&mut self.code, &[0xb8], id);
        imm(&mut self.code, &[0xba], ptr);
        self.code.trap().mov_mem_eax(DATA + out);
    }
    fn run(&mut self) {
        let result = self.run_result();
        assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
        assert!(self.machine.sys.first_cpu_fault.is_none());
    }
    fn run_result(&mut self) -> RunResult {
        self.code.bytes.extend([0x31, 0xc0]);
        self.code.ret();
        self.machine.cpu.write(CODE, &self.code.bytes).unwrap();
        self.machine.cpu.region(CODE, self.code.bytes.len());
        self.machine
            .spawn("owned NT service main", CODE, &[], 65536)
            .unwrap();
        let result = self.machine.run_virtual(1_000_000);
        if !matches!(result, RunResult::Exited(0)) {
            eprintln!(
                "diagnostic eip={:08x} esp={:08x} eax={:08x} status={:08x}",
                self.machine.cpu.reg(Reg::Eip),
                self.machine.cpu.reg(Reg::Esp),
                self.machine.cpu.reg(Reg::Eax),
                self.rd(0x10)
            );
            let sp = self.machine.cpu.reg(Reg::Esp);
            for i in 0..8 {
                eprintln!("stack {i}: {:08x?}", self.machine.cpu.read_u32(sp + i * 4));
            }
        }
        result
    }
    fn rd(&self, off: u32) -> u32 {
        self.machine.cpu.read_u32(DATA + off).unwrap()
    }
    fn eq(&mut self, off: u32, expected: u32) {
        assert_eq!(self.rd(off), expected, "output + {off:x}");
    }
    fn raw_checks(&mut self) {
        self.call("kernel32.dll", "SetLastError", &[LAST_ERROR], 0x200);
        for (opcode, value) in [
            (0xbb, 0x11223344),
            (0xb9, 0x55667788),
            (0xbd, 0x12345678),
            (0xbe, 0x31415926),
            (0xbf, 0x27182818),
        ] {
            imm(&mut self.code, &[opcode], value);
        }
        imm(&mut self.code, &[0xba], DATA + 0x80);
        self.saved_regs(0x100);
        self.raw(323, DATA + 0x80, 0x20);
        self.saved_regs(0x140);
        self.call("kernel32.dll", "GetLastError", &[], 0x24);
        self.raw(0xfff, 0xffff0000, 0x28);
        self.raw(323, 0xffff0000, 0x2c);
        self.raw(323, DATA + 4092, 0x30);
    }
}

fn exercise<B: LabCpu>(factory: impl Fn() -> B, dir: &Path) {
    let mut lab = Lab::new(factory(), dir);
    lab.service(1, &[DATA, 0x1f0003, 0, 0, 0], 0x10);
    // Raw EDX arguments are separate from both the caller and bridge stacks.
    lab.code.mov_eax_mem(DATA).mov_mem_eax(DATA + 0x80);
    lab.code.mov_mem_imm(DATA + 0x84, DATA + 4);
    lab.raw_checks();
    lab.code.push_imm(0).mov_eax_mem(DATA).push_eax();
    let wait = lab
        .machine
        .resolve_import("kernel32.dll", "WaitForSingleObject")
        .unwrap();
    lab.code.call(wait).mov_mem_eax(DATA + 0x34);
    lab.code.push_imm(DATA + 8).mov_eax_mem(DATA).push_eax();
    lab.code.call(ENTRY + 3 * 32).mov_mem_eax(DATA + 0x38);
    // Legacy entry -> gateway -> service owner -> correct ret 12 cleanup.
    lab.code
        .push_imm(DATA + 0x90)
        .push_imm(0)
        .mov_eax_mem(DATA)
        .push_eax();
    lab.code.call(ENTRY + 4 * 32).mov_mem_eax(DATA + 0x3c);
    // Copy the owned service entry; dispatch must not depend on entry PC.
    let copied = entry(323, 2);
    lab.machine.cpu.write(ENTRY + 0x400, &copied).unwrap();
    lab.machine.cpu.region(ENTRY + 0x400, copied.len());
    lab.code.push_imm(DATA + 12).mov_eax_mem(DATA).push_eax();
    lab.code.call(ENTRY + 0x400).mov_mem_eax(DATA + 0x40);
    lab.code.mov_eax_mem(DATA).push_eax();
    lab.code.call(ENTRY).mov_mem_eax(DATA + 0x44);
    lab.raw(323, DATA + 0x80, 0x48);
    lab.run();
    for (off, value) in [
        (0x10, 0),
        (0x20, 0),
        (4, 0),
        (0x24, LAST_ERROR),
        (0x28, STATUS_INVALID_SYSTEM_SERVICE),
        (0x2c, STATUS_ACCESS_VIOLATION),
        (0x30, STATUS_ACCESS_VIOLATION),
        (0x34, 0),
        (0x38, 0),
        (8, 1),
        (0x3c, 0x102),
        (0x40, 0),
        (12, 0),
        (0x44, 0),
        (0x48, 0xc0000008),
    ] {
        lab.eq(off, value);
    }
    for i in 0..8 {
        lab.eq(0x140 + i * 4, lab.rd(0x100 + i * 4));
    }
    let mut blocking = Lab::new(factory(), dir);
    blocking.call("kernel32.dll", "CreateEventW", &[0, 1, 0, 0], 0);
    let mut worker = Asm::new(CODE + 0x800);
    let sleep = blocking
        .machine
        .resolve_import("kernel32.dll", "Sleep")
        .unwrap();
    let set = blocking
        .machine
        .resolve_import("kernel32.dll", "SetEvent")
        .unwrap();
    worker
        .push_imm(10)
        .call(sleep)
        .mov_eax_mem(DATA)
        .push_eax()
        .call(set);
    worker.mov_mem_eax(DATA + 8).bytes.extend([0x31, 0xc0]);
    worker.ret_n(4);
    blocking
        .machine
        .cpu
        .write(CODE + 0x800, &worker.bytes)
        .unwrap();
    blocking
        .machine
        .cpu
        .region(CODE + 0x800, worker.bytes.len());
    blocking.call(
        "kernel32.dll",
        "CreateThread",
        &[0, 0, CODE + 0x800, 0, 0, 0],
        4,
    );
    blocking
        .code
        .push_imm(0)
        .push_imm(0)
        .mov_eax_mem(DATA)
        .push_eax();
    blocking.code.call(ENTRY + 4 * 32).mov_mem_eax(DATA + 12);
    blocking.run();
    blocking.eq(8, 1);
    blocking.eq(12, 0);
    assert_ne!(blocking.rd(4), 0);
    let mut nonlocal = Lab::new(factory(), dir);
    let mut resumed = Asm::new(CODE + 0x600);
    resumed.mov_mem_eax(DATA + 0x20).mov_mem_imm(DATA + 0x24, 1);
    resumed.bytes.extend([0x31, 0xc0]);
    resumed.ret();
    nonlocal
        .machine
        .cpu
        .write(CODE + 0x600, &resumed.bytes)
        .unwrap();
    nonlocal
        .machine
        .cpu
        .region(CODE + 0x600, resumed.bytes.len());
    nonlocal.call("fixture", "context", &[], 0x10);
    nonlocal.service(5, &[DATA + 0x400, 0], 0x14);
    nonlocal.code.mov_mem_imm(DATA + 0x28, 1);
    let nonlocal_result = nonlocal.run_result();
    assert!(
        matches!(nonlocal_result, RunResult::Exited(0)),
        "{nonlocal_result:?}"
    );
    nonlocal.eq(0x20, 0x24681357);
    nonlocal.eq(0x24, 1);
    nonlocal.eq(0x28, 0);
    let mut raising = Lab::new(factory(), dir);
    raising
        .machine
        .cpu
        .write(CODE + 0x600, &resumed.bytes)
        .unwrap();
    raising
        .machine
        .cpu
        .region(CODE + 0x600, resumed.bytes.len());
    let mut handler = Asm::new(CODE + 0x700);
    handler.mov_mem_imm(DATA + 0x30, 1);
    imm(&mut handler, &[0xb8], u32::MAX);
    handler.ret_n(4);
    raising
        .machine
        .cpu
        .write(CODE + 0x700, &handler.bytes)
        .unwrap();
    raising
        .machine
        .cpu
        .region(CODE + 0x700, handler.bytes.len());
    raising.call(
        "kernel32.dll",
        "AddVectoredExceptionHandler",
        &[1, CODE + 0x700],
        0x34,
    );
    raising.call("fixture", "context", &[], 0x10);
    raising.code.mov_mem_imm(DATA + 0x800, 0xe0425151);
    raising.code.mov_mem_imm(DATA + 0x80c, CODE + 0x600);
    raising.service(6, &[DATA + 0x800, DATA + 0x400, 1], 0x14);
    raising.code.mov_mem_imm(DATA + 0x28, 1);
    raising.run();
    for (off, value) in [(0x20, 0x24681357), (0x24, 1), (0x28, 0), (0x30, 1)] {
        raising.eq(off, value);
    }
    let mut apc = Lab::new(factory(), dir);
    let mut routine = Asm::new(CODE + 0x800);
    for i in 0..3 {
        routine
            .mov_eax_esp(4 + i * 4)
            .mov_mem_eax(DATA + 0x100 + u32::from(i) * 4);
    }
    let sleep = apc.machine.resolve_import("kernel32.dll", "Sleep").unwrap();
    routine
        .push_imm(1)
        .call(sleep)
        .mov_mem_imm(DATA + 0x10c, 1)
        .ret_n(12);
    apc.machine.cpu.write(CODE + 0x800, &routine.bytes).unwrap();
    apc.machine.cpu.region(CODE + 0x800, routine.bytes.len());
    apc.call(
        "ntdll.dll",
        "NtQueueApcThread",
        &[u32::MAX - 1, CODE + 0x800, 3, 5, 7],
        0x10,
    );
    apc.service(7, &[], 0x14);
    apc.run();
    for (off, value) in [
        (0x10, 0),
        (0x14, 0),
        (0x100, 3),
        (0x104, 5),
        (0x108, 7),
        (0x10c, 1),
    ] {
        apc.eq(off, value);
    }
}

#[test]
fn native_services_share_owners_and_preserve_nonlocal_control_flow() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/scratch/nt-service-tests");
    exercise(|| JitCpu::new(PARK_ADDR, false).unwrap(), &dir);
    exercise(|| JitCpu::new(PARK_ADDR, true).unwrap(), &dir);
    exercise(|| UnicornCpu::new(PARK_ADDR).unwrap(), &dir);
}

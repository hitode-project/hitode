//! Top-level filters run through guest callbacks and may repair a frame's CONTEXT.
use adv_abi::Registry;
use adv_cpu::{
    JitCpu, Region,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
use adv_win32::{Win32, Win32Config};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const FILTER: u32 = CODE + 0x200;
const FRAME: u32 = CODE + 0x300;

trait TestCpu: Backend {
    fn prepare(&mut self) {}
}
impl TestCpu for UnicornCpu {}
impl TestCpu for JitCpu {
    fn prepare(&mut self) {
        self.executor_mut()
            .add_region(Region::new(CODE, 4096))
            .unwrap();
    }
}

fn machine<B: TestCpu>(cpu: B) -> Machine<B> {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/unhandled-filter-tests");
    Win32::install(&mut machine, Win32Config::new(&root, "fixture.exe", &root)).unwrap();
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    machine
}
fn jit() -> JitCpu {
    JitCpu::new(PARK_ADDR, true).unwrap()
}
fn api<B: TestCpu>(machine: &mut Machine<B>, name: &str) -> u32 {
    machine.resolve_import("kernel32.dll", name).unwrap()
}
fn run<B: TestCpu>(machine: &mut Machine<B>, code: Asm) {
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine.cpu.prepare();
    machine.spawn("unhandled filter", CODE, &[], 65536).unwrap();
    let result = machine.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
}

fn dispositions<B: TestCpu>(cpu: B) {
    let mut machine = machine(cpu);
    let set = api(&mut machine, "SetUnhandledExceptionFilter");
    let dispatch = api(&mut machine, "UnhandledExceptionFilter");
    let mut filter = Asm::new(FILTER);
    filter.mov_eax_esp(4).mov_mem_eax(DATA);
    filter.mov_eax_mem(DATA + 4).ret_n(4);
    machine.cpu.write(FILTER, &filter.bytes).unwrap();
    let mut code = Asm::new(CODE);
    code.push_imm(FILTER).call(set).mov_mem_eax(DATA + 8);
    code.push_imm(FILTER).call(set).mov_mem_eax(DATA + 12);
    for (index, disposition) in [u32::MAX, 0, 1].into_iter().enumerate() {
        code.mov_mem_imm(DATA + 4, disposition);
        code.push_imm(DATA + 0x100).call(dispatch);
        code.mov_mem_eax(DATA + 16 + index as u32 * 4);
    }
    code.push_imm(0).call(set).mov_mem_eax(DATA + 28);
    code.push_imm(DATA + 0x100)
        .call(dispatch)
        .mov_mem_eax(DATA + 32);
    code.bytes.extend([0x31, 0xc0]);
    code.ret();
    run(&mut machine, code);
    let word = |offset| machine.cpu.read_u32(DATA + offset).unwrap();
    assert_eq!(word(0), DATA + 0x100);
    assert_eq!(word(8), 0);
    assert_eq!(word(12), FILTER);
    assert_eq!([word(16), word(20), word(24)], [u32::MAX, 0, 1]);
    assert_eq!(word(28), FILTER);
    assert_eq!(word(32), 0);
}

#[test]
fn registered_filter_receives_original_pointer_and_propagates_disposition() {
    dispositions(UnicornCpu::new(PARK_ADDR).unwrap());
    dispositions(jit());
}

fn resume_via_frame<B: TestCpu>(cpu: B) {
    let mut machine = machine(cpu);
    let set = api(&mut machine, "SetUnhandledExceptionFilter");
    let dispatch = api(&mut machine, "UnhandledExceptionFilter");
    let sleep = api(&mut machine, "Sleep");
    let mut filter = Asm::new(FILTER);
    filter.mov_eax_esp(4).mov_mem_eax(DATA + 4);
    filter.bytes.extend([0x8b, 0x48, 4]); // ECX = ContextRecord.
    filter.bytes.extend([0xc7, 0x81, 0xb8, 0, 0, 0]); // CONTEXT.Eip = Sleep.
    filter.bytes.extend(sleep.to_le_bytes());
    filter.bytes.extend([0xb8, 0xff, 0xff, 0xff, 0xff]);
    filter.ret_n(4);
    machine.cpu.write(FILTER, &filter.bytes).unwrap();

    // A CRT-style frame filter constructs EXCEPTION_POINTERS and delegates to
    // UnhandledExceptionFilter. Its -1 maps to ExceptionContinueExecution (0).
    let mut frame = Asm::new(FRAME);
    frame.bytes.extend([0x83, 0xec, 8]);
    frame.mov_eax_esp(12); // ExceptionRecord, before our eight scratch bytes.
    frame.bytes.extend([0x89, 0x04, 0x24]);
    frame.mov_eax_esp(20); // ContextRecord.
    frame.bytes.extend([0x89, 0x44, 0x24, 4, 0x89, 0xe0]);
    frame.push_eax().call(dispatch).add_esp(8);
    frame.bytes.push(0x40); // INC EAX: -1 -> 0.
    frame.ret();
    machine.cpu.write(FRAME, &frame.bytes).unwrap();

    let mut code = Asm::new(CODE);
    code.push_imm(FILTER).call(set);
    code.mov_eax_fs(0).mov_mem_eax(DATA);
    code.push_imm(FRAME);
    code.bytes.extend([0x64, 0xff, 0x35, 0, 0, 0, 0]); // PUSH FS:[0].
    code.bytes.extend([0x64, 0x89, 0x25, 0, 0, 0, 0]); // MOV FS:[0], ESP.
    code.push_imm(100);
    let resume_slot = code.bytes.len() + 1;
    code.push_imm(0);
    code.bytes.extend([0x31, 0xc0]);
    let fault = code.here();
    code.bytes.extend([0x8b, 0x18]); // Real guest null read.
    let resume = code.here();
    code.bytes[resume_slot..resume_slot + 4].copy_from_slice(&resume.to_le_bytes());
    code.mov_mem_imm(DATA + 8, 1);
    code.mov_eax_mem(DATA);
    code.bytes.extend([0x64, 0xa3, 0, 0, 0, 0]); // Restore FS:[0].
    code.add_esp(8);
    code.bytes.extend([0x31, 0xc0]);
    code.ret();
    run(&mut machine, code);
    assert_eq!(machine.cpu.read_u32(DATA + 8).unwrap(), 1);
    assert_ne!(machine.cpu.read_u32(DATA + 4).unwrap(), 0);
    assert_eq!(
        machine.sys.first_cpu_fault.as_ref().unwrap().fault.pc,
        fault
    );
    assert!(machine.sys.clock.now_us() >= 100_000);
    assert_eq!(machine.ext.get::<Win32>().unhandled_filter, FILTER);
}

#[test]
fn frame_filter_can_resume_a_fault_through_edited_context_and_original_stack() {
    resume_via_frame(UnicornCpu::new(PARK_ADDR).unwrap());
    resume_via_frame(jit());
}

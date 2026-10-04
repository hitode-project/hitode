//! Win32 priority ABI/state tests. Only authored guest snippets run; no windows,
//! hardware devices or game executables are opened.
use adv_abi::Registry;
use adv_cpu::backend::asm::Asm;
use adv_cpu::backend::{Guest, GuestExt, Prot};
use adv_machine::layout::PARK_ADDR;
use adv_machine::{Clock, Conv, Machine, Object, Outcome, RunResult};
use adv_unicorn::UnicornCpu;

const CODE: u32 = 0x0040_0000;
const DATA: u32 = 0x0041_0000;
const CURRENT_THREAD: u32 = 0xffff_fffe;
const ERROR_RETURN: u32 = 0x7fff_ffff;
const SENTINEL: u32 = 0x4242;

struct Test {
    machine: Machine<UnicornCpu>,
    code: Asm,
}
impl Test {
    fn new() -> Self {
        let mut registry = Registry::default();
        adv_win32::register(&mut registry);
        registry.add("test", "done", Conv::Stdcall { argc: 0 }, |_| {
            Outcome::ExitProcess(0)
        });
        let mut machine = Machine::new(
            UnicornCpu::new(PARK_ADDR).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
        machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
        machine.spawn("main", CODE, &[], 65536).unwrap();
        Self {
            machine,
            code: Asm::new(CODE),
        }
    }
    fn call(&mut self, name: &str, args: &[u32], output: u32) {
        for value in args.iter().rev() {
            self.code.push_imm(*value);
        }
        let function = self.machine.resolve_import("kernel32.dll", name).unwrap();
        self.code.call(function).mov_mem_eax(output);
    }
    fn sentinel(&mut self) {
        self.call("SetLastError", &[SENTINEL], DATA + 0xffc);
    }
    fn run(&mut self) {
        let done = self.machine.resolve_import("test", "done").unwrap();
        self.code.call(done);
        self.machine.cpu.write(CODE, &self.code.bytes).unwrap();
        let result = self.machine.run_virtual(1_000_000);
        assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    }
    fn word(&self, offset: u32) -> u32 {
        self.machine.cpu.read_u32(DATA + offset).unwrap()
    }
}

#[test]
fn priority_values_pseudohandle_aliases_and_exited_threads_share_context_state() {
    let mut t = Test::new();
    let worker = t.machine.spawn("worker", CODE + 0x800, &[], 65536).unwrap();
    let handle = t.machine.sys.context(worker).thread_handle;
    let alias = t.machine.sys.objects.duplicate(handle).unwrap();
    t.machine.sys.terminate_other_contexts(42);
    assert!(t.machine.sys.context(worker).is_done());
    assert_eq!(t.machine.sys.relative_priority(worker), Some(0));
    assert_eq!(t.machine.sys.relative_priority(0), Some(0));
    t.sentinel();
    t.call("GetThreadPriority", &[CURRENT_THREAD], DATA);
    t.call("GetThreadPriority", &[handle], DATA + 4);
    for (i, priority) in [-15i32, -2, -1, 0, 1, 2, 15].into_iter().enumerate() {
        let at = DATA + 0x20 + i as u32 * 20;
        t.call("SetThreadPriority", &[CURRENT_THREAD, priority as u32], at);
        t.call("GetThreadPriority", &[CURRENT_THREAD], at + 4);
        t.call("SetThreadPriority", &[handle, priority as u32], at + 8);
        t.call("GetThreadPriority", &[alias], at + 12);
        t.call("GetLastError", &[], at + 16);
    }
    t.run();
    assert_eq!(t.word(0), 0);
    assert_eq!(t.word(4), 0);
    for (i, priority) in [-15i32, -2, -1, 0, 1, 2, 15].into_iter().enumerate() {
        let at = 0x20 + i as u32 * 20;
        assert_eq!(t.word(at), 1);
        assert_eq!(t.word(at + 4), priority as u32);
        assert_eq!(t.word(at + 8), 1);
        assert_eq!(t.word(at + 12), priority as u32);
        assert_eq!(
            t.word(at + 16),
            SENTINEL,
            "successful Set/Get preserve LastError"
        );
    }
    assert!(
        t.machine.sys.context(worker).is_done(),
        "setting priority cannot revive a thread"
    );
    assert_eq!(t.machine.sys.relative_priority(worker), Some(15));
}

#[test]
fn priority_rejects_invalid_handles_and_values_without_changing_state() {
    let mut t = Test::new();
    let event = t.machine.sys.objects.insert(Object::Event {
        manual_reset: true,
        signaled: false,
    });
    let worker = t.machine.spawn("worker", CODE + 0x800, &[], 65536).unwrap();
    let closed = t.machine.sys.context(worker).thread_handle;
    t.machine.sys.objects.close(closed).unwrap();
    t.machine.sys.terminate_other_contexts(0);
    t.call("SetThreadPriority", &[CURRENT_THREAD, 1], DATA + 0xffc);
    let handles = [0, u32::MAX, 0x1234_5678, event, closed];
    for (i, handle) in handles.into_iter().enumerate() {
        let at = DATA + i as u32 * 16;
        t.sentinel();
        t.call("SetThreadPriority", &[handle, 1], at);
        t.call("GetLastError", &[], at + 4);
        t.call("GetThreadPriority", &[handle], at + 8);
        t.call("GetLastError", &[], at + 12);
    }
    let values = [-16i32, -3, 3, 16, 255, i32::MIN, i32::MAX];
    for (i, value) in values.into_iter().enumerate() {
        let at = DATA + 0x100 + i as u32 * 12;
        t.sentinel();
        t.call("SetThreadPriority", &[CURRENT_THREAD, value as u32], at);
        t.call("GetLastError", &[], at + 4);
        t.call("GetThreadPriority", &[CURRENT_THREAD], at + 8);
    }
    t.run();
    for i in 0..handles.len() {
        let at = i as u32 * 16;
        assert_eq!(
            [t.word(at), t.word(at + 4), t.word(at + 8), t.word(at + 12)],
            [0, 6, ERROR_RETURN, 6]
        );
    }
    for i in 0..values.len() {
        let at = 0x100 + i as u32 * 12;
        assert_eq!([t.word(at), t.word(at + 4), t.word(at + 8)], [0, 87, 1]);
    }
    assert_eq!(t.machine.sys.relative_priority(worker), Some(0));
}

#[test]
fn background_resource_modes_fail_visibly_without_changing_relative_priority() {
    let mut t = Test::new();
    t.call("SetThreadPriority", &[CURRENT_THREAD, 1], DATA + 0xffc);
    for (i, mode) in [0x10000, 0x20000, 0x10000, 0x20000].into_iter().enumerate() {
        let at = DATA + i as u32 * 12;
        t.sentinel();
        t.call("SetThreadPriority", &[CURRENT_THREAD, mode], at);
        t.call("GetLastError", &[], at + 4);
        t.call("GetThreadPriority", &[CURRENT_THREAD], at + 8);
    }
    t.run();
    for i in 0..4 {
        let at = i * 12;
        assert_eq!([t.word(at), t.word(at + 4), t.word(at + 8)], [0, 50, 1]);
    }
    let diagnostics = t.machine.ext.get::<adv_abi::ApiDiagnostics>();
    assert_eq!(diagnostics.warnings.len(), 1);
    assert_eq!(diagnostics.warnings.values().copied().sum::<u64>(), 4);
}

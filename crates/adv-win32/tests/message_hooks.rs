//! Real guest hooks: chain continuations, MSG mutation, reentry and ownership.
use adv_abi::{Conv, ImplementationLevel, Registry};
use adv_cpu::{
    JitCpu,
    backend::{Backend, Guest, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
use adv_win32::{Win32, Win32Config};

const CODE: u32 = 0x400000;
const HOOK_A: u32 = CODE + 0x1000;
const HOOK_B: u32 = CODE + 0x1800;
const DATA: u32 = 0x410000;
const HANDLE_A: u32 = DATA;
const HANDLE_B: u32 = DATA + 4;
const MODE: u32 = DATA + 8;
const TID: u32 = DATA + 12;
const LOG_NEXT: u32 = DATA + 16;
const SELF_UNHOOK: u32 = DATA + 20;
const RESULT: u32 = DATA + 0x100;
const MESSAGE: u32 = DATA + 0x200;
const NESTED_MESSAGE: u32 = DATA + 0x240;
const LOG: u32 = DATA + 0x800;
const APP: u32 = 0x8000;

#[derive(Clone, Copy)]
enum Arg {
    I(u32),
    M(u32),
}

struct Fixture<B: Backend> {
    m: Machine<B>,
}
impl<B: Backend> Fixture<B> {
    fn new(cpu: B) -> Self {
        let mut registry = Registry::default();
        adv_win32::register(&mut registry);
        let mut m = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
        Win32::install(
            &mut m,
            Win32Config::new(
                "/nonexistent/hooks",
                "fixture.exe",
                "/nonexistent/hooks-data",
            ),
        )
        .unwrap();
        m.cpu.map(CODE, 0x3000, Prot::RX).unwrap();
        m.cpu.map(DATA, 0x1000, Prot::RW).unwrap();
        m.cpu.write_u32(LOG_NEXT, LOG).unwrap();
        Self { m }
    }
    fn api(&mut self, name: &str) -> u32 {
        self.m
            .resolve_import(
                if matches!(
                    name,
                    "GetCurrentThreadId"
                        | "GetLastError"
                        | "SetLastError"
                        | "Sleep"
                        | "ExitProcess"
                        | "ExitThread"
                ) {
                    "kernel32.dll"
                } else {
                    "user32.dll"
                },
                name,
            )
            .unwrap()
    }
    fn call(&mut self, a: &mut Asm, name: &str, args: &[Arg]) {
        for arg in args.iter().rev() {
            match arg {
                Arg::I(value) => {
                    a.push_imm(*value);
                }
                Arg::M(address) => {
                    a.mov_eax_mem(*address).push_eax();
                }
            }
        }
        a.call(self.api(name));
    }
    fn store(&mut self, a: &mut Asm, name: &str, args: &[Arg], output: u32) {
        self.call(a, name, args);
        a.mov_mem_eax(output);
    }
    fn install(&mut self, a: &mut Asm, wide: bool, callback: u32, output: u32) {
        self.store(
            a,
            if wide {
                "SetWindowsHookExW"
            } else {
                "SetWindowsHookExA"
            },
            &[Arg::I(3), Arg::I(callback), Arg::I(0), Arg::M(TID)],
            output,
        );
    }
    fn post(&mut self, a: &mut Asm, message: u32) {
        self.call(
            a,
            "PostThreadMessageA",
            &[Arg::M(TID), Arg::I(message), Arg::I(100), Arg::I(200)],
        );
    }
    fn retrieve(&mut self, a: &mut Asm, get: bool, remove: bool, out: u32, result: u32) {
        let mut args = vec![Arg::I(out), Arg::I(u32::MAX), Arg::I(0), Arg::I(0)];
        if !get {
            args.push(Arg::I(u32::from(remove)));
        }
        self.store(
            a,
            if get { "GetMessageA" } else { "PeekMessageA" },
            &args,
            result,
        );
        a.mov_eax_mem(out + 4).mov_mem_eax(result + 4);
        a.mov_eax_mem(out + 8).mov_mem_eax(result + 8);
    }
    fn callbacks(&mut self) {
        for (address, id, increment) in [(HOOK_A, 1u32, 10u8), (HOOK_B, 2, 1)] {
            let mut a = Asm::new(address);
            a.bytes.extend_from_slice(&[0x56, 0x89, 0xe6]); // push esi; mov esi,esp
            a.bytes.extend_from_slice(&[0x8b, 0x0d]);
            a.bytes.extend_from_slice(&LOG_NEXT.to_le_bytes());
            a.bytes.extend_from_slice(&[0xc7, 0x01]);
            a.bytes.extend_from_slice(&id.to_le_bytes());
            for (offset, source) in [(4u8, 8u8), (8, 12), (12, 16)] {
                a.bytes.extend_from_slice(&[0x8b, 0x46, source]);
                if source == 16 {
                    a.bytes.extend_from_slice(&[0x8b, 0x40, 8]);
                }
                a.bytes.extend_from_slice(&[0x89, 0x41, offset]);
            }
            a.bytes.extend_from_slice(&[0x83, 0xc1, 16, 0x89, 0x0d]);
            a.bytes.extend_from_slice(&LOG_NEXT.to_le_bytes());
            a.bytes
                .extend_from_slice(&[0x8b, 0x46, 16, 0x83, 0x40, 8, increment]);
            if id == 1 {
                for (mode, message) in [(2, 18u32), (3, APP + 3)] {
                    a.cmp_mem_imm8(MODE, mode);
                    let skip = a.here() + 6 + 10;
                    a.jnz(skip);
                    a.bytes.extend_from_slice(&[0x8b, 0x46, 16, 0xc7, 0x40, 4]);
                    a.bytes.extend_from_slice(&message.to_le_bytes());
                }
            } else {
                for (mode, handle) in [(1, HANDLE_B), (4, HANDLE_A)] {
                    a.cmp_mem_imm8(MODE, mode);
                    let branch = a.bytes.len();
                    a.jnz(0);
                    self.store(
                        &mut a,
                        "UnhookWindowsHookEx",
                        &[Arg::M(handle)],
                        SELF_UNHOOK,
                    );
                    self.call(&mut a, "Sleep", &[Arg::I(0)]);
                    let end = a.here();
                    patch(&mut a, address, branch, end);
                }
                a.cmp_mem_imm8(MODE, 5);
                let branch = a.bytes.len();
                a.jnz(0);
                a.mov_mem_imm(MODE, 0);
                self.retrieve(&mut a, false, true, NESTED_MESSAGE, RESULT + 0x80);
                let end = a.here();
                patch(&mut a, address, branch, end);
            }
            for offset in [16, 12, 8] {
                a.bytes.extend_from_slice(&[0xff, 0x76, offset]);
            }
            a.push_imm(0xdeadbeef).call(self.api("CallNextHookEx"));
            if id == 2 {
                a.add_eax_imm(77);
            }
            a.bytes.extend_from_slice(&[0x5e]);
            a.ret_n(12);
            self.m.cpu.write(address, &a.bytes).unwrap();
        }
    }
    fn run(&mut self, mut a: Asm, exit: u32) {
        self.call(&mut a, "ExitProcess", &[Arg::I(exit)]);
        a.bytes.extend_from_slice(&[0x31, 0xc0]);
        a.ret();
        assert!(a.bytes.len() < 0x1000);
        self.m.cpu.write(CODE, &a.bytes).unwrap();
        self.m.spawn("thread hooks", CODE, &[], 65536).unwrap();
        let objects = self.m.sys.objects.object_count();
        let result = self.m.run_virtual(1_000_000);
        assert!(
            matches!(result, RunResult::Exited(value) if value == exit),
            "{result:?}"
        );
        assert_eq!(
            self.m.sys.objects.object_count(),
            objects,
            "all hook aliases and in-flight references are reclaimed"
        );
    }
    fn word(&self, p: u32) -> u32 {
        self.m.cpu.read_u32(p).unwrap()
    }
    fn log(&self) -> Vec<[u32; 4]> {
        (LOG..self.word(LOG_NEXT))
            .step_by(16)
            .map(|p| std::array::from_fn(|i| self.word(p + i as u32 * 4)))
            .collect()
    }
}

fn patch(a: &mut Asm, base: u32, offset: usize, target: u32) {
    a.bytes[offset + 2..offset + 6]
        .copy_from_slice(&target.wrapping_sub(base + offset as u32 + 6).to_le_bytes());
}

fn chain_and_reentry<B: Backend>(cpu: B) {
    let mut t = Fixture::new(cpu);
    t.callbacks();
    let mut a = Asm::new(CODE);
    t.store(&mut a, "GetCurrentThreadId", &[], TID);
    t.install(&mut a, false, HOOK_A, HANDLE_A);
    t.install(&mut a, true, HOOK_B, HANDLE_B);
    t.post(&mut a, APP + 1);
    t.retrieve(&mut a, false, false, MESSAGE, RESULT);
    a.mov_mem_imm(MODE, 1);
    t.retrieve(&mut a, false, true, MESSAGE, RESULT + 16);
    a.mov_mem_imm(MODE, 2);
    t.post(&mut a, APP + 2);
    t.retrieve(&mut a, true, true, MESSAGE, RESULT + 32);
    a.mov_mem_imm(MODE, 3);
    t.call(&mut a, "PostQuitMessage", &[Arg::I(100)]);
    t.retrieve(&mut a, true, true, MESSAGE, RESULT + 48);
    a.mov_mem_imm(MODE, 5);
    t.install(&mut a, true, HOOK_B, HANDLE_B);
    t.post(&mut a, APP + 5);
    t.post(&mut a, APP + 6);
    t.retrieve(&mut a, false, true, MESSAGE, RESULT + 64);
    a.mov_mem_imm(MODE, 4);
    t.post(&mut a, APP + 4);
    t.retrieve(&mut a, false, true, MESSAGE, RESULT + 96);
    t.store(
        &mut a,
        "UnhookWindowsHookEx",
        &[Arg::M(HANDLE_A)],
        RESULT + 112,
    );
    t.store(&mut a, "GetLastError", &[], RESULT + 116);
    t.run(a, 0);
    assert_ne!(t.word(HANDLE_A), 0);
    assert_ne!(t.word(HANDLE_B), 0);
    for (offset, expected) in [
        (0, [1, APP + 1, 111]),
        (16, [1, APP + 1, 111]),
        (32, [0, 18, 110]),
        (48, [1, APP + 3, 110]),
        (64, [1, APP + 5, 111]),
        (0x80, [1, APP + 6, 111]),
        (96, [1, APP + 4, 101]),
    ] {
        assert_eq!(
            std::array::from_fn::<_, 3, _>(|i| t.word(RESULT + offset + i as u32 * 4)),
            expected
        );
    }
    assert_eq!(t.word(SELF_UNHOOK), 1);
    assert_eq!(t.word(RESULT + 112), 0);
    assert_eq!(t.word(RESULT + 116), 1404);
    assert_eq!(
        t.log(),
        vec![
            [2, 0, 0, 100],
            [1, 0, 0, 101],
            [2, 0, 1, 100],
            [1, 0, 1, 101],
            [1, 0, 1, 100],
            [1, 0, 1, 100],
            [2, 0, 1, 100],
            [2, 0, 1, 100],
            [1, 0, 1, 101],
            [1, 0, 1, 101],
            [2, 0, 1, 100],
        ]
    );
}

#[test]
fn hook_chain_mutation_self_unhook_and_reentrant_retrieval_match_native() {
    for jit in [false, true] {
        let mut cpu = JitCpu::new(PARK_ADDR, jit).unwrap();
        cpu.set_async_compilation(false);
        chain_and_reentry(cpu);
    }
    chain_and_reentry(UnicornCpu::new(PARK_ADDR).unwrap());
}

#[test]
fn exiting_inside_a_hook_reclaims_its_active_owner() {
    let mut t = Fixture::new(UnicornCpu::new(PARK_ADDR).unwrap());
    let mut callback = Asm::new(HOOK_A);
    t.call(&mut callback, "ExitProcess", &[Arg::I(99)]);
    callback.ret_n(12);
    t.m.cpu.write(HOOK_A, &callback.bytes).unwrap();
    let mut a = Asm::new(CODE);
    t.store(&mut a, "GetCurrentThreadId", &[], TID);
    t.install(&mut a, false, HOOK_A, HANDLE_A);
    t.post(&mut a, APP + 1);
    t.retrieve(&mut a, false, true, MESSAGE, RESULT);
    t.run(a, 99);
}

#[test]
fn hook_exports_have_sdk_x86_conventions() {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    for (name, argc) in [
        ("SetWindowsHookExA", 4),
        ("SetWindowsHookExW", 4),
        ("CallNextHookEx", 4),
        ("UnhookWindowsHookEx", 1),
    ] {
        assert_eq!(
            registry.get("user32.dll", name).unwrap().conv,
            Conv::Stdcall { argc }
        );
        assert_eq!(
            registry.level("user32.dll", name),
            Some(ImplementationLevel::State)
        );
        assert!(registry.dynamic_available("user32.dll", name));
    }
    assert!(registry.signature_conflicts().is_empty());
}

#[test]
fn invalid_installations_and_stale_handles_fail_without_changing_queues() {
    let mut t = Fixture::new(UnicornCpu::new(PARK_ADDR).unwrap());
    t.callbacks();
    let mut a = Asm::new(CODE);
    t.store(&mut a, "GetCurrentThreadId", &[], TID);
    for (index, (kind, callback, module, tid, error)) in [
        (99, HOOK_A, 0, Arg::M(TID), 1426),
        (3, 0, 0, Arg::M(TID), 1427),
        (3, 0x600000, 0, Arg::M(TID), 1427),
        (3, HOOK_A, 0, Arg::I(u32::MAX), 87),
        (3, HOOK_A, 0, Arg::I(0), 1428),
        (2, HOOK_A, 0, Arg::M(TID), 50),
        (3, HOOK_A, 0x400000, Arg::M(TID), 50),
    ]
    .into_iter()
    .enumerate()
    {
        t.store(
            &mut a,
            "SetWindowsHookExA",
            &[Arg::I(kind), Arg::I(callback), Arg::I(module), tid],
            RESULT + index as u32 * 8,
        );
        t.store(&mut a, "GetLastError", &[], RESULT + index as u32 * 8 + 4);
        t.m.cpu
            .write_u32(DATA + 0x500 + index as u32 * 4, error)
            .unwrap();
    }
    t.store(&mut a, "UnhookWindowsHookEx", &[Arg::I(0)], RESULT + 64);
    t.store(&mut a, "GetLastError", &[], RESULT + 68);
    t.store(
        &mut a,
        "CallNextHookEx",
        &[Arg::I(0xdeadbeef), Arg::I(u32::MAX), Arg::I(0), Arg::I(0)],
        RESULT + 72,
    );
    t.run(a, 0);
    for index in 0..7 {
        assert_eq!(t.word(RESULT + index * 8), 0);
        assert_eq!(
            t.word(RESULT + index * 8 + 4),
            t.word(DATA + 0x500 + index * 4)
        );
    }
    assert_eq!(t.word(RESULT + 64), 0);
    assert_eq!(t.word(RESULT + 68), 1404);
    assert_eq!(t.word(RESULT + 72), 0);
    assert!(t.log().is_empty());
}

#[test]
fn a_hook_for_another_guest_thread_runs_there_and_is_removed_on_thread_exit() {
    let mut t = Fixture::new(UnicornCpu::new(PARK_ADDR).unwrap());
    t.callbacks();
    let worker_address = CODE + 0x2800;
    let mut worker = Asm::new(worker_address);
    // A remote post requires the target to have created its USER queue.
    t.retrieve(&mut worker, false, false, MESSAGE, RESULT);
    t.call(&mut worker, "Sleep", &[Arg::I(1)]);
    t.retrieve(&mut worker, false, true, MESSAGE, RESULT);
    t.call(&mut worker, "ExitThread", &[Arg::I(7)]);
    worker.ret();
    t.m.cpu.write(worker_address, &worker.bytes).unwrap();
    let worker_context =
        t.m.spawn("hook target", worker_address, &[], 65536)
            .unwrap();
    t.m.cpu
        .write_u32(TID, t.m.sys.contexts[worker_context as usize].tid)
        .unwrap();
    let mut a = Asm::new(CODE);
    t.install(&mut a, false, HOOK_A, HANDLE_A);
    t.post(&mut a, APP + 1);
    t.call(&mut a, "Sleep", &[Arg::I(5)]);
    t.store(
        &mut a,
        "UnhookWindowsHookEx",
        &[Arg::M(HANDLE_A)],
        RESULT + 16,
    );
    t.store(&mut a, "GetLastError", &[], RESULT + 20);
    t.run(a, 0);
    assert_eq!(t.word(RESULT), 1);
    assert_eq!(t.word(RESULT + 8), 110);
    assert_eq!(t.word(RESULT + 16), 0);
    assert_eq!(t.word(RESULT + 20), 1404);
    assert_eq!(t.log(), vec![[1, 0, 1, 100]]);
    assert!(t.m.sys.contexts[worker_context as usize].is_done());
}

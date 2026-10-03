//! Real guest thunk calls validate protected output buffers before host writes.
use pal_cpu::asm::Asm;
use pal_cpu::unicorn::UnicornCpu;
use pal_cpu::{Guest, GuestExt, Prot};
use pal_machine::layout::{PAGE, PARK_ADDR};
use pal_machine::{Clock, Machine, Registry, RunResult};
use pal_win32::{Win32, Win32Config};

const CODE: u32 = 0x0040_0000;
const DATA: u32 = 0x0041_0000;
const BAD: u32 = 0x0042_0000;
const SPLIT: u32 = 0x0044_0000;

enum Arg {
    Value(u32),
    Stored(u32),
}

struct Test {
    m: Machine<UnicornCpu>,
}

impl Test {
    fn new() -> Self {
        let mut registry = Registry::default();
        pal_win32::register(&mut registry);
        let mut m = Machine::new(UnicornCpu::new(PARK_ADDR).unwrap(), registry, Clock::virtual_time()).unwrap();
        let directory = std::env::temp_dir().join("pal-memory-toolhelp-tests");
        let mut cfg = Win32Config::new(&directory, "fixture.exe", &directory);
        cfg.write_game = false;
        Win32::install(&mut m, cfg).unwrap();
        m.cpu.map(CODE, PAGE, Prot::RX).unwrap();
        m.cpu.map(DATA, PAGE, Prot::RW).unwrap();
        Self { m }
    }

    fn call(&mut self, a: &mut Asm, name: &str, args: &[Arg], save: u32) {
        for arg in args.iter().rev() {
            match arg {
                Arg::Value(value) => {
                    a.push_imm(*value);
                }
                Arg::Stored(address) => {
                    a.mov_eax_mem(*address).push_eax();
                }
            }
        }
        let thunk = self.m.resolve_import("kernel32.dll", name).unwrap();
        a.call(thunk).mov_mem_eax(save);
    }

    fn run(&mut self, mut a: Asm, worker: bool) {
        let exit = self.m.resolve_import("kernel32.dll", "ExitProcess").unwrap();
        a.push_imm(0).call(exit);
        self.m.cpu.write(CODE, &a.bytes).unwrap();
        self.m.spawn("main", CODE, &[], 0x1_0000).unwrap();
        if worker {
            self.m.cpu.write(CODE + 0x400, &[0xeb, 0xfe]).unwrap();
            // The snapshot must include a second live guest context. Main finishes
            // synchronously, before this context needs to execute its entry code.
            self.m.spawn("worker", CODE + 0x400, &[], 0x1_0000).unwrap();
        }
        let result = self.m.run_virtual(1_000_000);
        assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    }

    fn word(&self, address: u32) -> u32 {
        self.m.cpu.read_u32(address).unwrap()
    }

    fn protected_output(&mut self, prot: Prot, cross_page: bool) -> (u32, Vec<u8>) {
        self.m.cpu.map(BAD, PAGE, prot).unwrap();
        let (out, len) = if cross_page { (BAD + PAGE - 16, 16) } else { (BAD, 28) };
        let mut sentinel = vec![0xa5; len];
        sentinel[..4].copy_from_slice(&28u32.to_le_bytes());
        self.m.cpu.write(out, &sentinel).unwrap();
        (out, sentinel)
    }

    fn assert_unchanged(&self, out: u32, sentinel: &[u8]) {
        let mut actual = vec![0; sentinel.len()];
        self.m.cpu.read(out, &mut actual).unwrap();
        assert_eq!(actual, sentinel, "a failed output must not write even its mapped prefix");
    }
}

#[test]
fn virtual_query_rejects_readonly_exec_and_unmapped_tail_outputs_atomically() {
    use Arg::Value as V;
    for (prot, cross_page) in [(Prot::READ, false), (Prot::EXEC, false), (Prot::RW, true)] {
        let mut t = Test::new();
        let (out, sentinel) = t.protected_output(prot, cross_page);
        let mut a = Asm::new(CODE);
        t.call(&mut a, "VirtualQuery", &[V(CODE), V(out), V(28)], DATA);
        t.call(&mut a, "GetLastError", &[], DATA + 4);
        t.call(&mut a, "VirtualQuery", &[V(CODE), V(DATA + 0x100), V(28)], DATA + 8);
        t.run(a, false);
        assert_eq!([t.word(DATA), t.word(DATA + 4), t.word(DATA + 8)], [0, 998, 28]);
        assert_eq!(t.word(DATA + 0x100), CODE);
        t.assert_unchanged(out, &sentinel);
    }
}

#[test]
fn virtual_query_accepts_output_with_write_permission_without_read_permission() {
    use Arg::Value as V;
    let mut t = Test::new();
    t.m.cpu.map(BAD, PAGE, Prot::WRITE).unwrap();
    let mut a = Asm::new(CODE);
    t.call(&mut a, "VirtualQuery", &[V(CODE), V(BAD), V(28)], DATA);
    t.run(a, false);
    assert_eq!(t.word(DATA), 28);
    assert_eq!(t.word(BAD), CODE);
}

#[test]
fn thread_entries_reject_invalid_outputs_without_writes_or_consuming_records() {
    use Arg::{Stored as S, Value as V};
    for (prot, cross_page) in [(Prot::READ, false), (Prot::EXEC, false), (Prot::RW, true)] {
        let mut t = Test::new();
        let (out, sentinel) = t.protected_output(prot, cross_page);
        t.m.cpu.write_u32(DATA + 0x100, 28).unwrap();
        t.m.cpu.write_u32(DATA + 0x200, 28).unwrap();
        let mut a = Asm::new(CODE);
        t.call(&mut a, "CreateToolhelp32Snapshot", &[V(4), V(0)], DATA);
        t.call(&mut a, "Thread32First", &[S(DATA), V(out)], DATA + 4);
        t.call(&mut a, "GetLastError", &[], DATA + 8);
        t.call(&mut a, "Thread32First", &[S(DATA), V(DATA + 0x100)], DATA + 12);
        t.call(&mut a, "Thread32Next", &[S(DATA), V(out)], DATA + 16);
        t.call(&mut a, "GetLastError", &[], DATA + 20);
        t.call(&mut a, "Thread32Next", &[S(DATA), V(DATA + 0x200)], DATA + 24);
        t.call(&mut a, "Thread32Next", &[S(DATA), V(DATA + 0x200)], DATA + 28);
        t.call(&mut a, "GetLastError", &[], DATA + 32);
        t.run(a, true);
        assert_eq!([t.word(DATA + 4), t.word(DATA + 8), t.word(DATA + 12)], [0, 998, 1]);
        assert_eq!([t.word(DATA + 16), t.word(DATA + 20), t.word(DATA + 24)], [0, 998, 1]);
        assert_eq!([t.word(DATA + 28), t.word(DATA + 32)], [0, 18]);
        assert_eq!(t.word(DATA + 0x100 + 8), t.m.sys.context(0).tid);
        assert_eq!(t.word(DATA + 0x200 + 8), t.m.sys.context(1).tid);
        let handle = t.word(DATA);
        assert_eq!(t.m.ext.get::<Win32>().thread_snapshots[&handle].1, 2);
        t.assert_unchanged(out, &sentinel);
    }
}

#[test]
fn thread_entries_only_require_dwsize_to_be_readable() {
    use Arg::{Stored as S, Value as V};
    let mut t = Test::new();
    t.m.cpu.map(SPLIT, PAGE, Prot::RW).unwrap();
    t.m.cpu.map(SPLIT + PAGE, PAGE, Prot::WRITE).unwrap();
    let out = SPLIT + PAGE - 4;
    t.m.cpu.write_u32(out, 28).unwrap();
    t.m.cpu.write_u32(DATA + 0x100, 28).unwrap();
    let mut a = Asm::new(CODE);
    t.call(&mut a, "CreateToolhelp32Snapshot", &[V(4), V(0)], DATA);
    t.call(&mut a, "Thread32First", &[S(DATA), V(out)], DATA + 4);
    t.call(&mut a, "Thread32Next", &[S(DATA), V(out)], DATA + 8);
    t.run(a, true);
    assert_eq!([t.word(DATA + 4), t.word(DATA + 8)], [1, 1]);
    assert_eq!(t.word(out + 8), t.m.sys.context(1).tid);
}

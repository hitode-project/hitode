use pal_cpu::asm::Asm;
use pal_cpu::unicorn::UnicornCpu;
use pal_cpu::{Guest, GuestExt, Prot};
use pal_machine::{Clock, Machine, Registry, RunResult};
use pal_win32::{Win32, Win32Config};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const LOG: u32 = DATA + 2048;
enum Arg {
    Value(u32),
    Stored(u32),
}
use Arg::{Stored as S, Value as V};
struct Test {
    machine: Machine<UnicornCpu>,
    code: Asm,
    next: u32,
}
impl Test {
    fn new() -> Self {
        let mut registry = Registry::default();
        pal_win32::register(&mut registry);
        let mut machine =
            Machine::new(UnicornCpu::new(pal_machine::layout::PARK_ADDR).unwrap(), registry, Clock::virtual_time())
                .unwrap();
        Win32::install(&mut machine, Win32Config::new(".", "atom-fixture.exe", "target/atom-fixture")).unwrap();
        machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
        machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
        Self { machine, code: Asm::new(CODE), next: LOG }
    }
    fn call(&mut self, name: &str, args: &[Arg]) -> u32 {
        for arg in args.iter().rev() {
            match arg {
                V(v) => {
                    self.code.push_imm(*v);
                }
                S(p) => {
                    self.code.mov_eax_mem(*p).push_eax();
                }
            }
        }
        let thunk = self.machine.resolve_import("kernel32.dll", name).unwrap();
        let out = self.next;
        self.next += 4;
        self.code.call(thunk).mov_mem_eax(out);
        out
    }
    fn run(&mut self) {
        self.call("ExitProcess", &[V(0)]);
        self.machine.cpu.write(CODE, &self.code.bytes).unwrap();
        self.machine.spawn("main", CODE, &[], 0x10000).unwrap();
        assert!(matches!(self.machine.run_virtual(1_000_000), RunResult::Exited(0)));
    }
    fn value(&self, address: u32) -> u32 {
        self.machine.cpu.read_u32(address).unwrap()
    }
    fn bytes(&self, address: u32, len: usize) -> Vec<u8> {
        let mut bytes = vec![0; len];
        self.machine.cpu.read(address, &mut bytes).unwrap();
        bytes
    }
}

#[test]
fn global_atoms_preserve_first_spelling_and_release_last_reference() {
    let mut t = Test::new();
    t.machine.cpu.write(DATA, b"AbC\0abc\0#12\0#0\0\0").unwrap();
    let atom = t.call("GlobalAddAtomA", &[V(DATA)]);
    let same = t.call("GlobalAddAtomA", &[V(DATA + 4)]);
    let found = t.call("GlobalFindAtomA", &[V(DATA + 4)]);
    let named = t.call("GlobalGetAtomNameA", &[S(atom), V(DATA + 100), V(8)]);
    t.call("GlobalDeleteAtom", &[S(atom)]);
    let retained = t.call("GlobalFindAtomA", &[V(DATA)]);
    t.call("GlobalDeleteAtom", &[S(atom)]);
    let removed = t.call("GlobalFindAtomA", &[V(DATA)]);
    let error = t.call("GetLastError", &[]);
    let integer = t.call("GlobalAddAtomA", &[V(DATA + 8)]);
    let integer_name = t.call("GlobalGetAtomNameW", &[S(integer), V(DATA + 120), V(8)]);
    let bad_integer = t.call("GlobalAddAtomA", &[V(DATA + 12)]);
    let bad_error = t.call("GetLastError", &[]);
    let empty = t.call("GlobalAddAtomA", &[V(DATA + 15)]);
    let empty_error = t.call("GetLastError", &[]);
    t.run();
    assert!((0xc000..=0xffff).contains(&t.value(atom)));
    for out in [same, found, retained] {
        assert_eq!(t.value(out), t.value(atom));
    }
    assert_eq!(t.value(named), 3);
    assert_eq!(t.bytes(DATA + 100, 4), b"AbC\0");
    assert_eq!((t.value(removed), t.value(error)), (0, 2));
    assert_eq!((t.value(integer), t.value(integer_name)), (12, 3));
    assert_eq!(
        t.bytes(DATA + 120, 8),
        b"#\0".iter().chain(b"1\0".iter()).chain(b"2\0\0\0".iter()).copied().collect::<Vec<_>>()
    );
    assert_eq!((t.value(bad_integer), t.value(bad_error)), (0, 87));
    assert_eq!((t.value(empty), t.value(empty_error)), (0, 123));
}

#[test]
fn ansi_and_wide_truncation_follow_their_distinct_native_contracts() {
    let mut t = Test::new();
    t.machine.cpu.write(DATA, b"AbC\0").unwrap();
    t.machine.cpu.write(DATA + 100, &[0x21; 32]).unwrap();
    let atom = t.call("GlobalAddAtomA", &[V(DATA)]);
    let a = t.call("GlobalGetAtomNameA", &[S(atom), V(DATA + 100), V(2)]);
    let ae = t.call("GetLastError", &[]);
    let w = t.call("GlobalGetAtomNameW", &[S(atom), V(DATA + 110), V(2)]);
    t.run();
    assert_eq!((t.value(a), t.value(ae)), (0, 234));
    assert_eq!(t.bytes(DATA + 100, 3), b"A\0!");
    assert_eq!(t.value(w), 2);
    assert_eq!(t.bytes(DATA + 110, 6), [b'A', 0, b'b', 0, 0x21, 0x21]);
}

#[test]
fn integer_atoms_are_not_reference_counted_and_excess_lengths_fail() {
    let mut t = Test::new();
    t.machine.cpu.write(DATA, &[b'a'; 256]).unwrap();
    let a = t.call("GlobalAddAtomA", &[V(123)]);
    t.call("GlobalDeleteAtom", &[V(123)]);
    let b = t.call("GlobalFindAtomW", &[V(123)]);
    let long = t.call("GlobalAddAtomA", &[V(DATA)]);
    let err = t.call("GetLastError", &[]);
    t.run();
    assert_eq!((t.value(a), t.value(b)), (123, 123));
    assert_eq!((t.value(long), t.value(err)), (0, 87));
}

//! Authored guest class queries share registered/stock class ownership and layout.
use adv_abi::{Conv, Registry};
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
use adv_win32::{Win32, Win32Config};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const READONLY: u32 = DATA + 0x2000;

#[derive(Clone, Copy)]
enum Arg {
    Imm(u32),
    Mem(u32),
}
use Arg::{Imm as I, Mem as M};

struct Fixture<B: Backend> {
    machine: Machine<B>,
    code: Asm,
    next: u32,
}
impl<B: Backend> Fixture<B> {
    fn new(cpu: B) -> Self {
        let mut registry = Registry::default();
        adv_win32::register(&mut registry);
        for api in ["GetClassInfoW", "GetClassInfoExA", "GetClassInfoExW"] {
            assert_eq!(
                registry.get("user32.dll", api).unwrap().conv,
                Conv::Stdcall { argc: 3 }
            );
            assert!(registry.dynamic_available("user32.dll", api));
        }
        assert!(registry.signature_conflicts().is_empty());
        let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
        machine.ext.get_or_default::<adv_abi::ApiDiagnostics>();
        let root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/class-query-tests");
        let mut config = Win32Config::new(&root, "fixture.exe", &root);
        config.os.codepage = 936;
        Win32::install(&mut machine, config).unwrap();
        machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
        machine.cpu.map(DATA, 8192, Prot::RW).unwrap();
        machine.cpu.map(READONLY, 4096, Prot::READ).unwrap();
        machine.cpu.write(READONLY, &[0xcc; 4096]).unwrap();
        Self {
            machine,
            code: Asm::new(CODE),
            next: DATA + 0x100,
        }
    }
    fn alloc(&mut self, size: u32) -> u32 {
        let p = self.next;
        self.next += size.next_multiple_of(4);
        assert!(self.next < DATA + 0x1800);
        p
    }
    fn bytes(&mut self, bytes: &[u8]) -> u32 {
        let p = self.alloc(bytes.len() as u32);
        self.machine.cpu.write(p, bytes).unwrap();
        p
    }
    fn text(&mut self, text: &str, wide: bool) -> u32 {
        let b = if wide {
            utf16(text)
        } else {
            let mut b = self.machine.ext.get::<Win32>().codec.encode(text);
            b.push(0);
            b
        };
        self.bytes(&b)
    }
    fn function(&mut self, name: &str) -> u32 {
        let (module, name) = name.split_once('!').unwrap_or(("user32.dll", name));
        self.machine.resolve_import(module, name).unwrap()
    }
    fn call(&mut self, name: &str, args: &[Arg]) -> u32 {
        for arg in args.iter().rev() {
            match *arg {
                I(v) => {
                    self.code.push_imm(v);
                }
                M(p) => {
                    self.code.mov_eax_mem(p).push_eax();
                }
            }
        }
        let f = self.function(name);
        let p = self.alloc(4);
        self.code.call(f).mov_mem_eax(p);
        p
    }
    fn class(&mut self, name: &str, wide: bool, menu: u32) -> (u32, u32) {
        let name = self.text(name, wide);
        let proc = self.function(if wide {
            "DefWindowProcW"
        } else {
            "DefWindowProcA"
        });
        let fields = [8, proc, 4, 8, CODE, 0, 0, 12, menu, name];
        let wc = self.bytes(
            &fields
                .into_iter()
                .flat_map(u32::to_le_bytes)
                .collect::<Vec<_>>(),
        );
        (
            self.call(
                if wide {
                    "RegisterClassW"
                } else {
                    "RegisterClassA"
                },
                &[I(wc)],
            ),
            proc,
        )
    }
    fn out(&mut self) -> u32 {
        self.bytes(&[0xcc; 48])
    }
    fn query(&mut self, wide: bool, inst: u32, name: Arg, out: u32) -> u32 {
        self.call(
            if wide {
                "GetClassInfoW"
            } else {
                "GetClassInfoA"
            },
            &[I(inst), name, I(out)],
        )
    }
    fn class_ex(&mut self, name: &str, wide: bool, instance: u32, menu: u32) -> (u32, u32) {
        let name = self.text(name, wide);
        let proc = self.function(if wide {
            "DefWindowProcW"
        } else {
            "DefWindowProcA"
        });
        let fields = [
            48, 8, proc, 4, 8, instance, 1001, 1004, 12, menu, name, 1002,
        ];
        let wc = self.bytes(
            &fields
                .into_iter()
                .flat_map(u32::to_le_bytes)
                .collect::<Vec<_>>(),
        );
        (
            self.call(
                if wide {
                    "RegisterClassExW"
                } else {
                    "RegisterClassExA"
                },
                &[I(wc)],
            ),
            proc,
        )
    }
    fn out_ex(&mut self, size: u32) -> u32 {
        let out = self.bytes(&[0xcc; 56]);
        self.machine.cpu.write_u32(out, size).unwrap();
        out
    }
    fn query_ex(&mut self, wide: bool, inst: u32, name: Arg, out: u32) -> u32 {
        self.call(
            if wide {
                "GetClassInfoExW"
            } else {
                "GetClassInfoExA"
            },
            &[I(inst), name, I(out)],
        )
    }
    fn error(&mut self) -> u32 {
        self.call("kernel32.dll!GetLastError", &[])
    }
    fn run(&mut self) {
        self.code.bytes.extend_from_slice(&[0x31, 0xc0]);
        self.code.ret();
        self.machine.cpu.write(CODE, &self.code.bytes).unwrap();
        self.machine
            .spawn("class queries guest", CODE, &[], 65536)
            .unwrap();
        let result = self.machine.run_virtual(1_000_000);
        assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    }
    fn value(&self, p: u32) -> u32 {
        self.machine.cpu.read_u32(p).unwrap()
    }
    fn read(&self, p: u32, len: usize) -> Vec<u8> {
        let mut bytes = vec![0; len];
        self.machine.cpu.read(p, &mut bytes).unwrap();
        bytes
    }
}
fn utf16(s: &str) -> Vec<u8> {
    s.encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect()
}

fn queries<B: Backend>(cpu: B) {
    let mut t = Fixture::new(cpu);
    let menu_w = t.text("Menu星", true);
    let menu_a = t.text("Menu中", false);
    let (atom_w, proc_w) = t.class("类Wide", true, menu_w);
    let (atom_a, proc_a) = t.class("类Ansi", false, menu_a);
    let wide_name = t.text("类Wide", true);
    let ansi_name = t.text("类Ansi", false);
    // Native retains registration text even when the original guest buffers change.
    t.code.mov_mem_imm(menu_w, u32::from(b'X'));
    t.code.mov_mem_imm(menu_a, u32::from(b'X'));
    t.call("kernel32.dll!SetLastError", &[I(0x12345678)]);
    let w = t.out();
    let wr = t.query(true, CODE, I(wide_name), w);
    let we = t.error();
    let a = t.out();
    let ar = t.query(false, CODE, I(ansi_name), a);
    let ae = t.error();
    let atom_out = t.out();
    let atom_result = t.query(true, CODE, M(atom_w), atom_out);
    let system_name = t.text("Button", true);
    let stock = t.out();
    let sr = t.query(true, 0, I(system_name), stock);
    let integer_menu = 77;
    let (integer_atom, _) = t.class("IntegerMenu", true, integer_menu);
    let integer = t.out();
    let ir = t.query(true, CODE, M(integer_atom), integer);
    let (retired_atom, _) = t.class("RetiredMenu", true, menu_w);
    let retired = t.out();
    t.query(true, CODE, M(retired_atom), retired);
    let retire_name = t.text("RetiredMenu", false);
    let removed = t.call("UnregisterClassA", &[I(retire_name), I(CODE)]);
    t.run();
    assert_eq!(
        (t.value(wr), t.value(ar), t.value(atom_result)),
        (t.value(atom_w), t.value(atom_a), t.value(atom_w))
    );
    assert_eq!((t.value(we), t.value(ae)), (0x12345678, 0x12345678));
    for (out, proc, name) in [(w, proc_w, wide_name), (a, proc_a, ansi_name)] {
        let fields: [u32; 10] = std::array::from_fn(|i| t.value(out + 4 * i as u32));
        assert_eq!(&fields[..8], &[8, proc, 4, 8, CODE, 0, 0, 12]);
        assert_eq!(fields[9], name);
        assert_eq!(t.read(out + 40, 8), [0xcc; 8]);
        assert_ne!(fields[8], if out == w { menu_w } else { menu_a });
        assert!(
            t.machine
                .ext
                .get::<adv_os::Heaps>()
                .owner(fields[8])
                .is_some()
        );
    }
    assert_eq!(
        t.read(t.value(w + 32), utf16("Menu星").len()),
        utf16("Menu星")
    );
    let mut ansi = t.machine.ext.get::<Win32>().codec.encode("Menu中");
    ansi.push(0);
    assert_eq!(t.read(t.value(a + 32), ansi.len()), ansi);
    assert_eq!(
        (t.value(atom_out + 32), t.value(atom_out + 36)),
        (t.value(w + 32), t.value(atom_w))
    );
    assert_ne!(t.value(sr), 0);
    assert_eq!((t.value(stock + 16), t.value(stock + 36)), (0, system_name));
    assert_eq!(t.value(ir), t.value(integer_atom));
    assert_eq!(t.value(integer + 32), integer_menu);
    assert_eq!(t.value(removed), 1);
    assert_eq!(
        t.machine
            .ext
            .get::<adv_os::Heaps>()
            .owner(t.value(retired + 32)),
        None
    );
    assert_eq!(t.machine.ext.get::<adv_abi::ApiDiagnostics>().stub_calls, 0);
}

#[test]
// spec: WIN32-USER-10
fn class_queries_preserve_atoms_layout_menu_snapshot_lifetime_and_stock_source() {
    queries(UnicornCpu::new(PARK_ADDR).unwrap());
    queries(JitCpu::new(PARK_ADDR, false).unwrap());
}

fn failures<B: Backend>(cpu: B) {
    let mut t = Fixture::new(cpu);
    let (aw, _) = t.class("WideOnly", true, 0);
    let (aa, _) = t.class("AnsiOnly", false, 0);
    let wide = t.text("WideOnly", true);
    let ansi = t.text("AnsiOnly", false);
    let missing = t.text("AbsentClass", true);
    let mut cases = Vec::new();
    for (name, instance, error) in [
        (I(missing), CODE, 1411),
        (I(wide), CODE + 1, 1411),
        (I(0x700000), CODE, 998),
        (M(aa), CODE, 50),
    ] {
        let out = t.out();
        let result = t.query(true, instance, name, out);
        let last = t.error();
        cases.push((out, result, last, error));
    }
    let a_cross = t.out();
    let ac = t.query(false, CODE, M(aw), a_cross);
    let ace = t.error();
    cases.push((a_cross, ac, ace, 50));
    let ro = t.query(true, CODE, I(wide), READONLY);
    let roe = t.error();
    let end = DATA + 8192 - 20;
    t.machine.cpu.write(end, &[0xcc; 20]).unwrap();
    let partial = t.query(false, CODE, I(ansi), end);
    let partial_error = t.error();
    let null = t.query(true, CODE, I(wide), 0);
    let null_error = t.error();
    t.run();
    for (out, result, last, error) in cases {
        assert_eq!((t.value(result), t.value(last)), (0, error));
        assert_eq!(t.read(out, 48), [0xcc; 48]);
    }
    assert_eq!((t.value(ro), t.value(roe)), (0, 998));
    assert_eq!(t.read(READONLY, 48), [0xcc; 48]);
    assert_eq!((t.value(partial), t.value(partial_error)), (0, 998));
    assert_eq!(t.read(end, 20), [0xcc; 20]);
    assert_eq!((t.value(null), t.value(null_error)), (0, 998));
    let d = t.machine.ext.get::<adv_abi::ApiDiagnostics>();
    assert_eq!(d.stub_calls, 0);
    for api in ["user32.dll!GetClassInfoA", "user32.dll!GetClassInfoW"] {
        assert!(d.warnings.iter().any(|((name, reason), count)| name == api
            && reason.contains("procedure thunk")
            && *count == 1));
    }
}

#[test]
fn class_query_failures_preserve_outputs_and_reject_unavailable_cross_encoding_thunks() {
    failures(UnicornCpu::new(PARK_ADDR).unwrap());
    failures(JitCpu::new(PARK_ADDR, false).unwrap());
}

fn extended_queries<B: Backend>(cpu: B) {
    let mut t = Fixture::new(cpu);
    let ansi_name = t.text("SharedOwnerClass", false);
    let missing = t.out_ex(48);
    let mr = t.query_ex(false, CODE, I(ansi_name), missing);
    let me = t.error();
    let menu = t.text("Menu星", true);
    let (aa, ap) = t.class_ex("SharedOwnerClass", false, CODE, 77);
    let (other_atom, _) = t.class_ex("SharedOwnerClass", false, CODE + 0x1000, 88);
    let (wa, wp) = t.class_ex("类Extended", true, CODE, menu);
    let wide_name = t.text("类Extended", true);
    t.code.mov_mem_imm(menu, u32::from(b'X'));
    t.call("kernel32.dll!SetLastError", &[I(0x12345678)]);
    let a = t.out_ex(48);
    let ar = t.query_ex(false, CODE, I(ansi_name), a);
    let ae = t.error();
    let w = t.out_ex(48);
    let wr = t.query_ex(true, CODE, I(wide_name), w);
    let we = t.error();
    let atom = t.out_ex(0);
    let atom_result = t.query_ex(false, CODE, M(aa), atom);
    let other = t.out_ex(1);
    let other_result = t.query_ex(false, CODE + 0x1000, I(ansi_name), other);
    let classic = t.out();
    t.query(true, CODE, M(wa), classic);
    let system_name = t.text("Button", true);
    let stock = t.out_ex(48);
    let sr = t.query_ex(true, 0, I(system_name), stock);
    let (icon_atom, _) = t.class_ex("ClassIconState", false, CODE, 0);
    let window = t.call(
        "CreateWindowExA",
        &[
            I(0),
            M(icon_atom),
            I(0),
            I(0),
            I(0),
            I(0),
            I(40),
            I(40),
            I(0),
            I(0),
            I(CODE),
            I(0),
        ],
    );
    let small = t.call("GetClassLongA", &[M(window), I((-34i32) as u32)]);
    let old_small = t.call("SetClassLongA", &[M(window), I((-34i32) as u32), I(1003)]);
    let large = t.call("GetClassLongA", &[M(window), I((-14i32) as u32)]);
    let changed = t.out_ex(48);
    t.query_ex(false, CODE, M(icon_atom), changed);
    t.run();
    assert_eq!((t.value(mr), t.value(me)), (0, 1411));
    assert_eq!(t.value(missing), 48);
    assert_eq!(t.read(missing + 4, 52), [0xcc; 52]);
    assert_eq!((t.value(ar), t.value(wr)), (t.value(aa), t.value(wa)));
    assert_eq!((t.value(ae), t.value(we)), (0x12345678, 0x12345678));
    for (out, proc, name) in [(a, ap, ansi_name), (w, wp, wide_name)] {
        let fields: [u32; 12] = std::array::from_fn(|i| t.value(out + 4 * i as u32));
        assert_eq!(&fields[..9], &[48, 8, proc, 4, 8, CODE, 1001, 1004, 12]);
        assert_eq!(&fields[10..], &[name, 1002]);
        assert_eq!(t.read(out + 48, 8), [0xcc; 8]);
    }
    assert_eq!(t.value(a + 36), 77);
    assert_eq!(
        t.read(t.value(w + 36), utf16("Menu星").len()),
        utf16("Menu星")
    );
    assert_eq!(t.value(classic + 32), t.value(w + 36));
    assert_eq!(t.value(atom_result), t.value(aa));
    assert_eq!((t.value(atom), t.value(atom + 40)), (0, t.value(aa)));
    assert_eq!(t.value(other_result), t.value(other_atom));
    assert_ne!(t.value(aa), t.value(other_atom));
    assert_eq!(
        (t.value(other), t.value(other + 20), t.value(other + 36)),
        (1, CODE + 0x1000, 88)
    );
    assert_ne!(t.value(sr), 0);
    assert_eq!(
        (
            t.value(stock + 20),
            t.value(stock + 40),
            t.value(stock + 44)
        ),
        (0, system_name, 0)
    );
    assert_ne!(t.value(window), 0);
    assert_eq!(
        (t.value(small), t.value(old_small), t.value(large)),
        (1002, 1002, 1001)
    );
    assert_eq!(t.value(changed + 44), 1003);
    assert_eq!(t.machine.ext.get::<adv_abi::ApiDiagnostics>().stub_calls, 0);
}

#[test]
fn extended_class_queries_allow_registration_and_preserve_owner_layout_and_small_icons() {
    extended_queries(UnicornCpu::new(PARK_ADDR).unwrap());
    extended_queries(JitCpu::new(PARK_ADDR, false).unwrap());
}

fn extended_failures<B: Backend>(cpu: B) {
    let mut t = Fixture::new(cpu);
    let (aw, _) = t.class_ex("WideExtendedOnly", true, CODE, 0);
    let (aa, _) = t.class_ex("AnsiExtendedOnly", false, CODE, 0);
    let missing = t.text("AbsentClass", true);
    let mut cases = Vec::new();
    for (wide, name, instance, error) in [
        (true, I(missing), CODE, 1411),
        (true, M(aw), CODE + 1, 1411),
        (true, I(0x700000), CODE, 998),
        (true, M(aa), CODE, 50),
        (false, M(aw), CODE, 50),
    ] {
        let out = t.out_ex(48);
        let result = t.query_ex(wide, instance, name, out);
        let last = t.error();
        cases.push((out, result, last, error));
    }
    let ro = t.query_ex(true, CODE, M(aw), READONLY);
    let roe = t.error();
    // Forty bytes of WNDCLASS would fit; the extended result must check all 48.
    let end = DATA + 8192 - 44;
    t.machine.cpu.write(end, &[0xcc; 44]).unwrap();
    let partial = t.query_ex(false, CODE, M(aa), end);
    let partial_error = t.error();
    let null = t.query_ex(true, CODE, M(aw), 0);
    let null_error = t.error();
    t.run();
    for (out, result, last, error) in cases {
        assert_eq!((t.value(result), t.value(last)), (0, error));
        assert_eq!(t.value(out), 48);
        assert_eq!(t.read(out + 4, 52), [0xcc; 52]);
    }
    assert_eq!((t.value(ro), t.value(roe)), (0, 998));
    assert_eq!(t.read(READONLY, 48), [0xcc; 48]);
    assert_eq!((t.value(partial), t.value(partial_error)), (0, 998));
    assert_eq!(t.read(end, 44), [0xcc; 44]);
    assert_eq!((t.value(null), t.value(null_error)), (0, 998));
    let d = t.machine.ext.get::<adv_abi::ApiDiagnostics>();
    assert_eq!(d.stub_calls, 0);
    for api in ["user32.dll!GetClassInfoExA", "user32.dll!GetClassInfoExW"] {
        assert!(d.warnings.iter().any(|((name, reason), count)| name == api
            && reason.contains("procedure thunk")
            && *count == 1));
    }
}

#[test]
fn extended_class_query_failures_preserve_output_and_report_unavailable_thunks() {
    extended_failures(UnicornCpu::new(PARK_ADDR).unwrap());
    extended_failures(JitCpu::new(PARK_ADDR, false).unwrap());
}

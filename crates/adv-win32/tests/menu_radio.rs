//! Radio edits are observed through existing A/W menu queries and CheckMenuItem.
use adv_abi::{Conv, Registry};
use adv_cpu::{
    JitCpu, Region,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
use adv_win32::{Win32, Win32Config};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const CHECKED: u32 = 8;
const RADIO: u32 = 0x200;
const POSITION: u32 = 0x400;
const MARKER: u32 = 0x12345678;
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
#[derive(Clone, Copy)]
enum Arg {
    Value(u32),
    Saved(u32),
}
use Arg::{Saved as S, Value as V};
struct Fixture<B: TestCpu> {
    m: Machine<B>,
    code: Asm,
    next: u32,
    setup: Vec<u32>,
}
impl<B: TestCpu> Fixture<B> {
    fn new(cpu: B) -> Self {
        let mut registry = Registry::default();
        adv_win32::register(&mut registry);
        assert_eq!(
            registry
                .get("user32.dll", "CheckMenuRadioItem")
                .unwrap()
                .conv,
            Conv::Stdcall { argc: 5 }
        );
        assert!(registry.dynamic_available("user32.dll", "CheckMenuRadioItem"));
        assert!(registry.signature_conflicts().is_empty());
        let mut m = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
        let root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/menu-radio-tests");
        Win32::install(&mut m, Win32Config::new(&root, "fixture.exe", &root)).unwrap();
        m.ext.get_or_default::<adv_abi::ApiDiagnostics>();
        m.cpu.map(CODE, 0x10000, Prot::RX).unwrap();
        m.cpu.map(DATA, 0x10000, Prot::RW).unwrap();
        Self {
            m,
            code: Asm::new(CODE),
            next: DATA,
            setup: Vec::new(),
        }
    }
    fn bytes(&mut self, bytes: &[u8]) -> u32 {
        let p = self.next;
        self.next += (bytes.len() as u32).next_multiple_of(4);
        assert!(self.next < DATA + 0x10000);
        self.m.cpu.write(p, bytes).unwrap();
        p
    }
    fn call(&mut self, name: &str, args: &[Arg]) -> u32 {
        let (module, name) = name.split_once('!').unwrap_or(("user32.dll", name));
        let entry = self.m.resolve_import(module, name).unwrap();
        for &arg in args.iter().rev() {
            match arg {
                V(v) => {
                    self.code.push_imm(v);
                }
                S(p) => {
                    self.code.mov_eax_mem(p).push_eax();
                }
            }
        }
        let result = self.bytes(&[0; 4]);
        self.code.call(entry).mov_mem_eax(result);
        result
    }
    fn radio(&mut self, menu: Arg, first: u32, last: u32, checked: u32, flags: u32) -> (u32, u32) {
        self.call("kernel32.dll!SetLastError", &[V(MARKER)]);
        let result = self.call(
            "CheckMenuRadioItem",
            &[menu, V(first), V(last), V(checked), V(flags)],
        );
        let error = self.call("kernel32.dll!GetLastError", &[]);
        (result, error)
    }
    fn add(&mut self, menu: u32, position: u32, id: u32, kind: u32, submenu: Option<u32>) {
        let name = self.bytes(
            &"authored"
                .encode_utf16()
                .chain([0])
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>(),
        );
        let mut item = [0; 48];
        for (at, value) in [
            (0, 48_u32),
            (4, 0x16f),
            (8, kind),
            (12, CHECKED | 3 | if id == 100 { 0x1000 } else { 0 }),
            (16, id),
            (24, 0x11110000 + id),
            (28, 0x22220000 + id),
            (32, 0xabcd0000 + id),
            (36, name),
            (40, 8),
        ] {
            item[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
        let pointer = self.bytes(&item);
        if let Some(submenu) = submenu {
            self.code.mov_eax_mem(submenu).mov_mem_eax(pointer + 20);
        }
        let result = self.call("InsertMenuItemW", &[S(menu), V(position), V(1), V(pointer)]);
        // Assert all setup calls later, without interpreting an invalid oracle.
        self.setup.push(result);
    }
    fn info(&mut self, menu: u32, value: u32, position: bool, wide: bool) -> u32 {
        let mut bytes = [0xcc; 48];
        bytes[..4].copy_from_slice(&48_u32.to_le_bytes());
        bytes[4..8].copy_from_slice(&0x12f_u32.to_le_bytes());
        let p = self.bytes(&bytes);
        let name = if wide {
            "GetMenuItemInfoW"
        } else {
            "GetMenuItemInfoA"
        };
        let result = self.call(name, &[S(menu), V(value), V(u32::from(position)), V(p)]);
        self.setup.push(result);
        p
    }
    fn value(&self, p: u32) -> u32 {
        self.m.cpu.read_u32(p).unwrap()
    }
    fn run(&mut self) {
        self.code.bytes.extend_from_slice(&[0x31, 0xc0]);
        self.code.ret();
        self.m.cpu.write(CODE, &self.code.bytes).unwrap();
        self.m.cpu.region(CODE, self.code.bytes.len());
        self.m.spawn("menu radio guest", CODE, &[], 65536).unwrap();
        let result = self.m.run_virtual(1_000_000);
        assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
        for &p in &self.setup {
            assert_eq!(self.value(p), 1, "menu setup/query");
        }
    }
}

fn assert_info<B: TestCpu>(f: &Fixture<B>, p: u32, id: u32, kind: u32, state: u32) {
    assert_eq!(f.value(p + 8), kind);
    assert_eq!(f.value(p + 12), state);
    assert_eq!(f.value(p + 16), id);
    assert_eq!(f.value(p + 24), 0x11110000 + id);
    assert_eq!(f.value(p + 28), 0x22220000 + id);
    assert_eq!(f.value(p + 32), 0xabcd0000 + id);
}

fn states<B: TestCpu>(cpu: B) {
    let mut f = Fixture::new(cpu);
    let before = f.m.sys.objects.object_count();
    let menu = f.call("CreatePopupMenu", &[]);
    f.add(menu, 0, 100, RADIO, None);
    f.add(menu, 1, 102, RADIO | 0x100, None);
    f.add(menu, 2, 103, 0, None);
    let command = f.radio(S(menu), 100, 103, 102, 0);
    let first = f.info(menu, 100, false, false);
    let selected = f.info(menu, 102, false, true);
    let last = f.info(menu, 103, false, false);
    let old = f.call("CheckMenuItem", &[S(menu), V(102), V(0)]);
    let uncheck = f.info(menu, 1, true, false);
    let position = f.radio(S(menu), 0, 2, 2, POSITION);
    let final_first = f.info(menu, 0, true, true);
    let final_mid = f.info(menu, 1, true, false);
    let final_last = f.info(menu, 2, true, true);
    let destroyed = f.call("DestroyMenu", &[S(menu)]);
    f.run();
    assert_eq!((f.value(command.0), f.value(command.1)), (1, 87)); // Missing ID101 is skipped.
    assert_info(&f, first, 100, RADIO, 0x1003);
    assert_info(&f, selected, 102, RADIO | 0x100, 11);
    assert_info(&f, last, 103, 0, 3);
    assert_eq!(f.value(old), CHECKED);
    assert_info(&f, uncheck, 102, RADIO | 0x100, 3);
    assert_eq!((f.value(position.0), f.value(position.1)), (1, MARKER));
    assert_info(&f, final_first, 100, RADIO, 0x1003);
    assert_info(&f, final_mid, 102, RADIO | 0x100, 3);
    assert_info(&f, final_last, 103, RADIO, 11);
    assert_eq!(f.value(destroyed), 1);
    assert_eq!(f.m.sys.objects.object_count(), before + 1);
    assert!(f.m.ext.get::<adv_abi::ApiDiagnostics>().warnings.is_empty());
}

#[test]
fn command_and_position_groups_share_type_state_and_check_item_owner() {
    states(UnicornCpu::new(PARK_ADDR).unwrap());
    states(JitCpu::new(PARK_ADDR, false).unwrap());
    states(JitCpu::new(PARK_ADDR, true).unwrap());
}

fn failures<B: TestCpu>(cpu: B) {
    let mut f = Fixture::new(cpu);
    let before = f.m.sys.objects.object_count();
    let root = f.call("CreatePopupMenu", &[]);
    let child = f.call("CreatePopupMenu", &[]);
    f.add(child, 0, 102, RADIO, None);
    f.add(child, 1, 103, RADIO, None);
    f.add(root, 0, 100, RADIO, None);
    f.add(root, 1, 999, 0, Some(child));
    f.add(root, 2, 105, RADIO, None);
    let invalid = f.radio(V(0), 100, 105, 102, 0);
    let reversed = f.radio(S(root), 105, 100, 102, 0);
    let unsupported = f.radio(S(root), 100, 105, 102, 0x20000);
    let unchanged = f.info(root, 100, false, true);
    let recursive = f.radio(S(root), 102, 103, 102, 0);
    let checked = f.info(root, 102, false, false);
    let cleared = f.info(child, 103, false, true);
    let child_missing = f.radio(S(root), 101, 104, 104, 0);
    let child_cleared = f.info(child, 102, false, true);
    let cross = f.radio(S(root), 100, 105, 102, 0);
    let parent_first = f.info(root, 100, false, false);
    let parent_last = f.info(root, 105, false, true);
    let out_of_range = f.radio(S(root), 0, 7, 20, POSITION);
    let position_cleared = f.info(root, 0, true, true);
    let destroyed = f.call("DestroyMenu", &[S(root)]);
    let stale = f.radio(S(child), 102, 103, 102, 0);
    f.run();
    for ((value, error), expected) in [
        (invalid, 1401),
        (reversed, 1456),
        (unsupported, 50),
        (child_missing, 87),
        (cross, 87),
        (out_of_range, 87),
        (stale, 1401),
    ] {
        assert_eq!((f.value(value), f.value(error)), (0, expected));
    }
    assert_info(&f, unchanged, 100, RADIO, 0x100b);
    assert_eq!((f.value(recursive.0), f.value(recursive.1)), (1, MARKER));
    assert_info(&f, checked, 102, RADIO, 11);
    assert_info(&f, cleared, 103, RADIO, 3);
    assert_info(&f, child_cleared, 102, RADIO, 3);
    assert_info(&f, parent_first, 100, RADIO, 0x1003);
    assert_info(&f, parent_last, 105, RADIO, 3);
    assert_info(&f, position_cleared, 100, RADIO, 0x1003);
    assert_eq!(f.value(destroyed), 1);
    assert_eq!(f.m.sys.objects.object_count(), before + 1);
    assert!(
        f.m.ext
            .get::<adv_abi::ApiDiagnostics>()
            .warnings
            .keys()
            .any(|(api, why)| api == "user32.dll!CheckMenuRadioItem" && why.contains("flags"))
    );
}

#[test]
fn recursive_groups_and_native_missing_check_side_effects_have_bounded_failures() {
    failures(UnicornCpu::new(PARK_ADDR).unwrap());
    failures(JitCpu::new(PARK_ADDR, false).unwrap());
    failures(JitCpu::new(PARK_ADDR, true).unwrap());
}

//! Authored x86 procedures observe real A/W CREATESTRUCT notifications and rejection.
use adv_abi::{Conv, Registry};
use adv_cpu::{
    JitCpu, Region,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
use adv_win32::{
    Win32, Win32Config,
    user32::window::{WS_POPUP, WS_VISIBLE, wm},
};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const LOG: u32 = DATA + 0x8000;
#[path = "window_creation/capture.rs"]
mod capture;
#[path = "window_creation/deferred.rs"]
mod deferred;
#[path = "window_creation/geometry.rs"]
mod geometry;
#[path = "window_creation/style.rs"]
mod style;
#[path = "window_creation/z_order.rs"]
mod z_order;

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
    Imm(u32),
    Mem(u32),
}
use Arg::{Imm as I, Mem as M};

fn invoke(code: &mut Asm, function: u32, args: &[Arg]) {
    for arg in args.iter().rev() {
        match *arg {
            I(value) => {
                code.push_imm(value);
            }
            M(pointer) => {
                code.mov_eax_mem(pointer).push_eax();
            }
        }
    }
    code.call(function);
}
fn value(code: &mut Asm, value: u32) {
    code.bytes.push(0xb8);
    code.bytes.extend_from_slice(&value.to_le_bytes());
}
fn skip(code: &mut Asm, opcode: u8, body: impl FnOnce(&mut Asm)) {
    let patch = code.bytes.len() + 2;
    let next = code.here() + 6;
    code.bytes.extend_from_slice(&[0x0f, opcode, 0, 0, 0, 0]);
    body(code);
    let delta = code.here().wrapping_sub(next);
    code.bytes[patch..patch + 4].copy_from_slice(&delta.to_le_bytes());
}
fn on_message(code: &mut Asm, message: u32, body: impl FnOnce(&mut Asm)) {
    code.mov_eax_esp(8);
    code.bytes.push(0x3d);
    code.bytes.extend_from_slice(&message.to_le_bytes());
    skip(code, 0x85, body); // JNE skips the handler.
}
fn utf16(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect()
}

struct Fixture<B: TestCpu> {
    machine: Machine<B>,
    code: Asm,
    data: u32,
    procedure: u32,
}
impl<B: TestCpu> Fixture<B> {
    fn new(cpu: B) -> Self {
        let mut registry = Registry::default();
        adv_win32::register(&mut registry);
        let api = registry.get("user32.dll", "CreateWindowExW").unwrap();
        assert_eq!(api.conv, Conv::Stdcall { argc: 12 });
        assert!(registry.dynamic_available("user32.dll", "CreateWindowExW"));
        assert!(registry.signature_conflicts().is_empty());
        let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
        machine.ext.get_or_default::<adv_abi::ApiDiagnostics>();
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/window-creation-tests");
        let mut config = Win32Config::new(&root, "fixture.exe", &root);
        config.os.codepage = 936;
        Win32::install(&mut machine, config).unwrap();
        machine.cpu.map(CODE, 0x10000, Prot::RX).unwrap();
        machine.cpu.map(DATA, 0x10000, Prot::RW).unwrap();
        machine.cpu.write_u32(DATA, LOG).unwrap();
        Self {
            machine,
            code: Asm::new(CODE),
            data: DATA + 0x100,
            procedure: CODE + 0x8000,
        }
    }
    fn alloc(&mut self, size: u32) -> u32 {
        let pointer = self.data;
        self.data += size.next_multiple_of(4);
        assert!(self.data < LOG);
        pointer
    }
    fn bytes(&mut self, bytes: &[u8]) -> u32 {
        let pointer = self.alloc(bytes.len() as u32);
        self.machine.cpu.write(pointer, bytes).unwrap();
        pointer
    }
    fn text(&mut self, text: &str, wide: bool) -> u32 {
        let bytes = if wide {
            utf16(text)
        } else {
            let mut bytes = self.machine.ext.get::<Win32>().codec.encode(text);
            bytes.push(0);
            bytes
        };
        self.bytes(&bytes)
    }
    fn function(&mut self, name: &str) -> u32 {
        let (module, name) = name.split_once('!').unwrap_or(("user32.dll", name));
        self.machine.resolve_import(module, name).unwrap()
    }
    fn call(&mut self, name: &str, args: &[Arg]) -> u32 {
        let function = self.function(name);
        let result = self.alloc(4);
        invoke(&mut self.code, function, args);
        self.code.mov_mem_eax(result);
        result
    }
    fn callback(&mut self, wide: bool, capture: u32, reject: Option<(u32, u32)>) -> u32 {
        let start = self.procedure;
        let mut code = Asm::new(start);
        // Record every actual guest message, including destruction on rejection.
        code.bytes.extend_from_slice(&[0x8b, 0x15]);
        code.bytes.extend_from_slice(&DATA.to_le_bytes());
        for i in 0..4 {
            code.mov_eax_esp(4 + i * 4);
            code.bytes.extend_from_slice(&[0x89, 0x42, i * 4]);
        }
        code.bytes.extend_from_slice(&[0x83, 0xc2, 16, 0x89, 0x15]);
        code.bytes.extend_from_slice(&DATA.to_le_bytes());
        code.mov_eax_esp(4).mov_mem_eax(capture + 96);
        let copy = self.function(if wide {
            "kernel32.dll!lstrcpyW"
        } else {
            "kernel32.dll!lstrcpyA"
        });
        for (message, offset) in [(wm::NCCREATE, 0), (wm::CREATE, 48)] {
            on_message(&mut code, message, |code| {
                code.mov_eax_esp(16)
                    .mov_mem_eax(capture + 100 + offset / 12);
                for i in 0..12 {
                    code.bytes.extend_from_slice(&[0x8b, 0x54, 0x24, 16]); // EDX = CS.
                    code.bytes.extend_from_slice(&[0x8b, 0x42, i * 4]);
                    code.mov_mem_eax(capture + offset + u32::from(i) * 4);
                }
            });
        }
        on_message(&mut code, wm::NCCREATE, |code| {
            // Preserve NULL title and integer class atoms without dereferencing them.
            code.mov_eax_mem(capture + 36);
            code.bytes.extend_from_slice(&[0x85, 0xc0]);
            skip(code, 0x84, |code| {
                invoke(code, copy, &[I(capture + 112), M(capture + 36)]);
            });
            code.mov_eax_mem(capture + 40);
            code.bytes.extend_from_slice(&[0x3d, 0, 0, 1, 0]);
            skip(code, 0x82, |code| {
                invoke(code, copy, &[I(capture + 240), M(capture + 40)]);
            });
        });
        if let Some((message, result)) = reject {
            on_message(&mut code, message, |code| {
                value(code, result);
                code.ret_n(16);
            });
        }
        let default = self.function(if wide {
            "DefWindowProcW"
        } else {
            "DefWindowProcA"
        });
        code.jmp(default);
        self.machine.cpu.write(start, &code.bytes).unwrap();
        self.machine.cpu.region(start, code.bytes.len());
        self.procedure = code.here().next_multiple_of(16);
        start
    }
    fn class(&mut self, callback: u32, name: &str, wide: bool) -> u32 {
        self.class_with_menu(callback, name, wide, 0)
    }
    fn class_with_menu(&mut self, callback: u32, name: &str, wide: bool, menu: u32) -> u32 {
        let name = self.text(name, wide);
        let fields = [0, callback, 0, 16, CODE, 0, 0, 0, menu, name];
        let structure = self.bytes(
            &fields
                .into_iter()
                .flat_map(u32::to_le_bytes)
                .collect::<Vec<_>>(),
        );
        self.call(
            if wide {
                "RegisterClassW"
            } else {
                "RegisterClassA"
            },
            &[I(structure)],
        )
    }
    fn create(&mut self, class: Arg, title: u32, wide: bool, visible: bool) -> u32 {
        self.call(
            if wide {
                "CreateWindowExW"
            } else {
                "CreateWindowExA"
            },
            &[
                I(0x80),
                class,
                I(title),
                I(WS_POPUP | if visible { WS_VISIBLE } else { 0 }),
                I((-20_i32) as u32),
                I(30),
                I(120),
                I(80),
                I(0),
                I(0),
                I(CODE),
                I(0x87654321),
            ],
        )
    }
    fn run(&mut self) {
        value(&mut self.code, 0);
        self.code.ret();
        self.machine.cpu.write(CODE, &self.code.bytes).unwrap();
        self.machine.cpu.region(CODE, self.code.bytes.len());
        self.machine
            .spawn("window creation guest", CODE, &[], 65536)
            .unwrap();
        let result = self.machine.run_virtual(1_000_000);
        assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    }
    fn value(&self, pointer: u32) -> u32 {
        self.machine.cpu.read_u32(pointer).unwrap()
    }
    fn read(&self, pointer: u32, size: usize) -> Vec<u8> {
        let mut bytes = vec![0; size];
        self.machine.cpu.read(pointer, &mut bytes).unwrap();
        bytes
    }
    fn messages(&self, hwnd: u32) -> Vec<u32> {
        (LOG..self.value(DATA))
            .step_by(16)
            .filter(|&p| self.value(p) == hwnd)
            .map(|p| self.value(p + 4))
            .collect()
    }
}

fn creation<B: TestCpu>(cpu: B) {
    let mut test = Fixture::new(cpu);
    let mut cases = Vec::new();
    for (index, caller_wide, class_wide, atom) in [
        (0, true, true, false),
        (1, true, false, false),
        (2, false, true, false),
        (3, false, false, false),
        (4, true, true, true),
    ] {
        let capture = test.alloc(368);
        let callback = test.callback(class_wide, capture, None);
        let name = format!("类{index}");
        let registered = test.class(callback, &name, class_wide);
        let name_pointer = test.text(&name, caller_wide);
        let title = if atom {
            0
        } else {
            test.text("标题あ", caller_wide)
        };
        let hwnd = test.create(
            if atom { M(registered) } else { I(name_pointer) },
            title,
            caller_wide,
            false,
        );
        cases.push((
            capture,
            hwnd,
            registered,
            name,
            name_pointer,
            title,
            caller_wide,
            class_wide,
            atom,
        ));
    }
    test.run();
    for (
        capture,
        hwnd,
        registered,
        name,
        input_class,
        input_title,
        caller_wide,
        class_wide,
        atom,
    ) in cases
    {
        let hwnd = test.value(hwnd);
        assert_ne!(hwnd, 0);
        assert_eq!(test.value(capture + 96), hwnd);
        assert_eq!(test.read(capture, 48), test.read(capture + 48, 48));
        let cs: [u32; 12] = std::array::from_fn(|i| test.value(capture + i as u32 * 4));
        assert_eq!(
            &cs[..9],
            &[
                0x87654321,
                CODE,
                0,
                0,
                80,
                120,
                30,
                (-20_i32) as u32,
                WS_POPUP
            ]
        );
        assert_eq!(cs[11], 0x80);
        assert_eq!(test.value(capture + 100), test.value(capture + 104));
        assert_eq!(
            test.machine
                .ext
                .get::<adv_os::Heaps>()
                .owner(&test.machine.cpu, test.value(capture + 100)),
            None
        );
        if atom {
            assert_eq!((cs[9], cs[10]), (0, test.value(registered)));
        } else {
            let encoded = |text: &str| {
                if class_wide {
                    utf16(text)
                } else {
                    let mut bytes = test.machine.ext.get::<Win32>().codec.encode(text);
                    bytes.push(0);
                    bytes
                }
            };
            let title = encoded("标题あ");
            let name = encoded(&name);
            assert_eq!(test.read(capture + 112, title.len()), title);
            assert_eq!(test.read(capture + 240, name.len()), name);
            if caller_wide == class_wide {
                assert_eq!((cs[9], cs[10]), (input_title, input_class));
            } else {
                assert_ne!(cs[9], input_title);
                assert_ne!(cs[10], input_class);
                for p in [cs[9], cs[10]] {
                    assert_eq!(
                        test.machine
                            .ext
                            .get::<adv_os::Heaps>()
                            .owner(&test.machine.cpu, p),
                        None
                    );
                }
            }
        }
        let user = &test.machine.ext.get::<Win32>().user;
        let window = user.windows.get(&test.machine.sys.objects, &hwnd).unwrap();
        assert_eq!(window.unicode, class_wide);
        assert_eq!(window.text, if atom { "" } else { "标题あ" });
        assert_eq!((window.rect.left, window.rect.top), (-20, 30));
        assert_eq!(&test.messages(hwnd)[..2], &[wm::NCCREATE, wm::CREATE]);
    }
    assert_eq!(
        test.machine.ext.get::<adv_abi::ApiDiagnostics>().stub_calls,
        0
    );
}

#[test]
fn create_window_ex_w_uses_registered_encoding_atoms_and_real_create_structs() {
    creation(UnicornCpu::new(PARK_ADDR).unwrap());
    creation(JitCpu::new(PARK_ADDR, false).unwrap());
}

fn absent_class_menu<B: TestCpu>(cpu: B) {
    let mut test = Fixture::new(cpu);
    let mut cases = Vec::new();
    for (index, wide, named, style) in [
        (0, false, true, 0x00ca0000),
        (1, true, true, WS_POPUP),
        (2, false, false, WS_POPUP),
        (3, true, false, 0x00ca0000),
    ] {
        let capture = test.alloc(368);
        let callback = test.callback(wide, capture, None);
        let absent = if named {
            test.text("AbsentDefaultMenu", wide)
        } else {
            987
        };
        let name = format!("DefaultMenu{index}");
        let atom = test.class_with_menu(callback, &name, wide, absent);
        let hwnd = test.call(
            if wide {
                "CreateWindowExW"
            } else {
                "CreateWindowExA"
            },
            &[
                I(0),
                M(atom),
                I(0),
                I(style),
                I(20),
                I(30),
                I(120),
                I(80),
                I(0),
                I(0),
                I(CODE),
                I(0),
            ],
        );
        let menu = test.call("GetMenu", &[M(hwnd)]);
        cases.push((capture, atom, hwnd, menu));
    }
    test.run();
    for (capture, atom, hwnd, menu) in cases {
        assert_ne!(test.value(atom), 0);
        let hwnd = test.value(hwnd);
        assert_ne!(hwnd, 0);
        assert_eq!(test.value(menu), 0);
        assert_eq!(test.value(capture + 8), 0); // CREATESTRUCT.hMenu in NCCREATE.
        assert_eq!(test.value(capture + 56), 0); // Same value in CREATE.
        assert_eq!(&test.messages(hwnd)[..2], &[wm::NCCREATE, wm::CREATE]);
    }
}

#[test]
fn absent_default_class_menu_does_not_reject_window_creation() {
    // Native invisible-window oracle: both named/integer missing menu resources
    // leave the window without a menu, for overlapped windows and popups.
    absent_class_menu(UnicornCpu::new(PARK_ADDR).unwrap());
    absent_class_menu(JitCpu::new(PARK_ADDR, false).unwrap());
}

fn rejection<B: TestCpu>(cpu: B) {
    let mut test = Fixture::new(cpu);
    let initial_windows = test.machine.ext.get::<Win32>().user.windows.len();
    let mut rejected = Vec::new();
    for (index, message, result) in [(0, wm::NCCREATE, 0), (1, wm::CREATE, u32::MAX)] {
        let capture = test.alloc(368);
        let callback = test.callback(false, capture, Some((message, result)));
        let name = format!("拒否{index}");
        test.class(callback, &name, false);
        let class = test.text(&name, true);
        let title = test.text("中あ", true);
        let result = test.create(I(class), title, true, true);
        rejected.push((capture, result, message));
    }
    // Invalid high pointers fail before allocating or publishing a USER object.
    let invalid_class = test.create(I(0x700000), 0, true, false);
    let class_error = test.call("kernel32.dll!GetLastError", &[]);
    let class = test.text("BUTTON", true);
    let invalid_title = test.create(I(class), 0x700000, true, false);
    let title_error = test.call("kernel32.dll!GetLastError", &[]);
    test.run();
    for (capture, result, message) in rejected {
        assert_eq!(test.value(result), 0);
        let hwnd = test.value(capture + 96);
        assert_ne!(hwnd, 0);
        let expected = if message == wm::NCCREATE {
            vec![wm::NCCREATE, wm::NCDESTROY]
        } else {
            vec![wm::NCCREATE, wm::CREATE, wm::DESTROY, wm::NCDESTROY]
        };
        assert_eq!(test.messages(hwnd), expected);
        assert!(
            !test
                .machine
                .ext
                .get::<Win32>()
                .user
                .windows
                .contains_key(&test.machine.sys.objects, &hwnd)
        );
        for pointer in [
            test.value(capture + 100),
            test.value(capture + 36),
            test.value(capture + 40),
        ] {
            assert_eq!(
                test.machine
                    .ext
                    .get::<adv_os::Heaps>()
                    .owner(&test.machine.cpu, pointer),
                None
            );
        }
    }
    assert_eq!(
        (test.value(invalid_class), test.value(class_error)),
        (0, 998)
    );
    assert_eq!(
        (test.value(invalid_title), test.value(title_error)),
        (0, 998)
    );
    assert_eq!(
        test.machine.ext.get::<Win32>().user.windows.len(),
        initial_windows
    );
    assert_eq!(
        test.machine.ext.get::<adv_abi::ApiDiagnostics>().stub_calls,
        0
    );
}

#[test]
fn create_window_ex_w_rejection_reclaims_owner_and_temporary_parameters() {
    rejection(UnicornCpu::new(PARK_ADDR).unwrap());
    rejection(JitCpu::new(PARK_ADDR, false).unwrap());
}

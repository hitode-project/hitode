//! Authored guest window procedures exercise the common A/W message path.
//! The bounded buffer profile intentionally avoids native mixed-thunk overruns.
use adv_abi::{Conv, Registry};
use adv_cpu::{
    JitCpu, Region,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
use adv_win32::{
    Win32, Win32Config,
    user32::window::{WS_POPUP, wm},
};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const NEXT: u32 = DATA;
const LOG: u32 = DATA + 0x8000;
const READONLY: u32 = DATA + 0x10000;

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
enum Arg {
    Imm(u32),
    Mem(u32),
    Stack(u8),
}
use Arg::{Imm as I, Mem as M, Stack as S};

fn invoke(code: &mut Asm, function: u32, args: &[Arg]) {
    for (index, argument) in args.iter().rev().enumerate() {
        match argument {
            I(value) => {
                code.push_imm(*value);
            }
            M(pointer) => {
                code.mov_eax_mem(*pointer).push_eax();
            }
            S(offset) => {
                code.mov_eax_esp(*offset + index as u8 * 4).push_eax();
            }
        }
    }
    code.call(function);
}
fn value(code: &mut Asm, value: u32) {
    code.bytes.push(0xb8);
    code.bytes.extend_from_slice(&value.to_le_bytes());
}
fn conditional(code: &mut Asm, body: impl FnOnce(&mut Asm), zero: bool) {
    let patch = code.bytes.len() + 2;
    let next = code.here() + 6;
    if zero {
        code.jz(0);
    } else {
        code.jnz(0);
    }
    body(code);
    let delta = code.here().wrapping_sub(next);
    code.bytes[patch..patch + 4].copy_from_slice(&delta.to_le_bytes());
}
fn on_message(code: &mut Asm, message: u32, body: impl FnOnce(&mut Asm)) {
    code.mov_eax_esp(8);
    code.bytes.push(0x3d);
    code.bytes.extend_from_slice(&message.to_le_bytes());
    conditional(code, body, false);
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
    fn new(cpu: B, cp: u32) -> Self {
        let mut registry = Registry::default();
        adv_win32::register(&mut registry);
        for (name, argc) in [
            ("SetWindowTextW", 2),
            ("GetWindowTextW", 3),
            ("GetWindowTextLengthW", 1),
            ("SendMessageW", 4),
            ("PeekMessageW", 5),
            ("GetMessageW", 4),
            ("DispatchMessageW", 1),
        ] {
            assert_eq!(
                registry.get("user32.dll", name).unwrap().conv,
                Conv::Stdcall { argc }
            );
            assert!(registry.dynamic_available("user32.dll", name));
        }
        assert!(registry.signature_conflicts().is_empty());
        let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
        machine.ext.get_or_default::<adv_abi::ApiDiagnostics>();
        let root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/window-text-tests");
        let mut config = Win32Config::new(&root, "fixture.exe", &root);
        config.os.codepage = cp;
        Win32::install(&mut machine, config).unwrap();
        machine.cpu.map(CODE, 0x10000, Prot::RX).unwrap();
        machine.cpu.map(DATA, 0x10000, Prot::RW).unwrap();
        machine.cpu.map(READONLY, 4096, Prot::READ).unwrap();
        machine.cpu.write_u32(NEXT, LOG).unwrap();
        Self {
            machine,
            code: Asm::new(CODE),
            data: DATA + 0x100,
            procedure: CODE + 0x8000,
        }
    }
    fn alloc(&mut self, size: u32) -> u32 {
        let result = self.data;
        self.data += size.next_multiple_of(4);
        assert!(self.data < LOG);
        result
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
    fn set_error(&mut self, error: u32) {
        self.call("kernel32.dll!SetLastError", &[I(error)]);
    }
    fn error(&mut self) -> u32 {
        self.call("kernel32.dll!GetLastError", &[])
    }
    fn save(&mut self, pointer: u32) -> u32 {
        let result = self.alloc(4);
        self.code.mov_eax_mem(pointer).mov_mem_eax(result);
        result
    }
    fn callback(&mut self, wide: bool, copied: u32, reenter: Option<(u32, u32)>) -> u32 {
        let start = self.procedure;
        let mut code = Asm::new(start);
        code.bytes.extend_from_slice(&[0x8b, 0x15]); // edx = next log row.
        code.bytes.extend_from_slice(&NEXT.to_le_bytes());
        for i in 0..4 {
            code.mov_eax_esp(4 + i * 4);
            code.bytes.extend_from_slice(&[0x89, 0x42, i * 4]);
        }
        code.bytes.extend_from_slice(&[0x83, 0xc2, 16, 0x89, 0x15]);
        code.bytes.extend_from_slice(&NEXT.to_le_bytes());
        let copy = self.function(if wide {
            "kernel32.dll!lstrcpyW"
        } else {
            "kernel32.dll!lstrcpyA"
        });
        let default = self.function(if wide {
            "DefWindowProcW"
        } else {
            "DefWindowProcA"
        });
        let length = self.function("GetWindowTextLengthW");
        on_message(&mut code, 0x8033, |code| {
            value(code, 0xfedcba98);
            code.ret_n(16);
        });
        on_message(&mut code, wm::SETTEXT, |code| {
            if let Some((flag, result)) = reenter {
                code.cmp_mem_imm8(flag, 1);
                conditional(
                    code,
                    |code| {
                        code.mov_mem_imm(flag, 0);
                        invoke(code, length, &[S(4)]);
                        code.mov_mem_eax(result);
                    },
                    false,
                );
            }
            code.mov_eax_esp(16);
            code.bytes.extend_from_slice(&[0x85, 0xc0]); // NULL remains NULL at the callback.
            conditional(
                code,
                |code| {
                    invoke(code, copy, &[I(copied), S(16)]);
                },
                true,
            );
            invoke(code, default, &[S(4), S(8), S(12), S(16)]);
            value(code, 0x55); // Preserve a real guest WM_SETTEXT return value.
            code.ret_n(16);
        });
        code.jmp(default);
        self.machine.cpu.write(start, &code.bytes).unwrap();
        self.machine.cpu.region(start, code.bytes.len());
        self.procedure = code.here().next_multiple_of(16);
        start
    }
    fn window(&mut self, wide: bool, callback: u32) -> u32 {
        let class = self.text(if wide { "OwnedWide" } else { "OwnedAnsi" }, wide);
        let fields = [0, callback, 0, 0, CODE, 0, 0, 0, 0, class];
        let class_info = self.bytes(
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
            &[I(class_info)],
        );
        // CreateWindowExA also creates a genuinely Unicode window when the
        // class was registered with RegisterClassW. This existing common path
        // converts CREATESTRUCT strings to that class's ABI.
        let create_class = if wide {
            self.text("OwnedWide", false)
        } else {
            class
        };
        let title = self.text("old", false);
        self.call(
            "CreateWindowExA",
            &[
                I(0),
                I(create_class),
                I(title),
                I(WS_POPUP),
                I(0),
                I(0),
                I(100),
                I(100),
                I(0),
                I(0),
                I(CODE),
                I(0),
            ],
        )
    }
    fn run(&mut self) {
        value(&mut self.code, 0);
        self.code.ret();
        self.machine.cpu.write(CODE, &self.code.bytes).unwrap();
        self.machine.cpu.region(CODE, self.code.bytes.len());
        self.machine
            .spawn("window text guest", CODE, &[], 65536)
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
    fn rows(&self, hwnd: u32, message: u32) -> Vec<[u32; 4]> {
        (LOG..self.value(NEXT))
            .step_by(16)
            .map(|p| std::array::from_fn(|i| self.value(p + i as u32 * 4)))
            .filter(|row| row[0] == hwnd && row[1] == message)
            .collect()
    }
}

fn callbacks<B: TestCpu>(cpu: B) {
    let mut test = Fixture::new(cpu, 936);
    let copied_a = test.alloc(64);
    let copied_w = test.alloc(64);
    let flag = test.bytes(&1_u32.to_le_bytes());
    let nested = test.alloc(4);
    let proc_a = test.callback(false, copied_a, Some((flag, nested)));
    let proc_w = test.callback(true, copied_w, None);
    let a = test.window(false, proc_a);
    let w = test.window(true, proc_w);
    let input_a = test.text("中A", false);
    let input_w = test.text("中A", true);
    let output_w = test.alloc(32);
    let output_a = test.alloc(32);
    let set_a = test.call("SetWindowTextW", &[M(a), I(input_w)]);
    let set_w = test.call("SetWindowTextA", &[M(w), I(input_a)]);
    let aa = test.call("GetWindowTextLengthA", &[M(a)]);
    let ww = test.call("GetWindowTextLengthW", &[M(w)]);
    let aw = test.call("GetWindowTextLengthW", &[M(a)]);
    let wa = test.call("GetWindowTextLengthA", &[M(w)]);
    let read_w = test.call("GetWindowTextW", &[M(a), I(output_w), I(8)]);
    let read_a = test.call("GetWindowTextA", &[M(w), I(output_a), I(8)]);
    let clear_a = test.call("SetWindowTextW", &[M(a), I(0)]);
    let clear_w = test.call("SetWindowTextA", &[M(w), I(0)]);
    test.run();
    assert_eq!(
        (
            test.value(set_a),
            test.value(set_w),
            test.value(clear_a),
            test.value(clear_w)
        ),
        (0x55, 0x55, 0x55, 0x55)
    );
    assert_eq!(test.value(nested), 3); // Reentered before the outer WM_SETTEXT changed "old".
    assert_eq!(
        (
            test.value(aa),
            test.value(ww),
            test.value(aw),
            test.value(wa)
        ),
        (3, 2, 2, 3)
    );
    assert_eq!((test.value(read_w), test.value(read_a)), (2, 3));
    assert_eq!(test.read(copied_a, 4), [0xd6, 0xd0, b'A', 0]);
    assert_eq!(test.read(copied_w, 6), utf16("中A"));
    assert_eq!(test.read(output_a, 4), [0xd6, 0xd0, b'A', 0]);
    assert_eq!(test.read(output_w, 6), utf16("中A"));
    for (hwnd, set_count, length_count, capacities) in [
        (test.value(a), 2, 3, vec![4, 4, 16]),
        (test.value(w), 2, 2, vec![3, 8]),
    ] {
        let sets = test.rows(hwnd, wm::SETTEXT);
        assert_eq!(sets.len(), set_count);
        assert_eq!(sets[1][3], 0);
        assert_eq!(test.rows(hwnd, wm::GETTEXTLENGTH).len(), length_count);
        let gets = test.rows(hwnd, wm::GETTEXT);
        assert_eq!(gets.iter().map(|r| r[2]).collect::<Vec<_>>(), capacities);
        for row in sets.iter().chain(gets.iter()).filter(|r| r[3] != 0) {
            assert_eq!(
                test.machine.ext.get::<adv_os::Heaps>().owner(row[3]),
                None,
                "temporary text storage must be reclaimed after reentrant dispatch"
            );
        }
        assert_eq!(
            test.machine
                .ext
                .get::<Win32>()
                .user
                .windows
                .get(&test.machine.sys.objects, &hwnd)
                .unwrap()
                .text,
            ""
        );
    }
    assert_eq!(
        test.machine.ext.get::<adv_abi::ApiDiagnostics>().stub_calls,
        0
    );
}

#[test]
fn real_window_procedures_convert_text_reenter_and_keep_null_and_guest_results() {
    callbacks(UnicornCpu::new(PARK_ADDR).unwrap());
    callbacks(JitCpu::new(PARK_ADDR, false).unwrap());
    callbacks(JitCpu::new(PARK_ADDR, true).unwrap());
}

fn capacities<B: TestCpu>(cpu: B, cp: u32, text: &str) {
    let mut test = Fixture::new(cpu, cp);
    let copied = test.alloc(128);
    let proc_a = test.callback(false, copied, None);
    let proc_w = test.callback(true, copied, None);
    let a = test.window(false, proc_a);
    let w = test.window(true, proc_w);
    let input = test.text(text, true);
    test.call("SetWindowTextW", &[M(a), I(input)]);
    test.call("SetWindowTextW", &[M(w), I(input)]);
    let encoded = test.machine.ext.get::<Win32>().codec.encode(text);
    let first_bytes = test
        .machine
        .ext
        .get::<Win32>()
        .codec
        .encode(&text.chars().next().unwrap().to_string())
        .len() as u32;
    let mut outputs = Vec::new();
    for (hwnd, wide, capacity) in [
        (a, false, first_bytes),
        (w, false, first_bytes),
        (a, true, 2),
        (w, true, 2),
    ] {
        let output = test.bytes(&[0xcc; 32]);
        test.set_error(0x12345678);
        let result = test.call(
            if wide {
                "GetWindowTextW"
            } else {
                "GetWindowTextA"
            },
            &[M(hwnd), I(output), I(capacity)],
        );
        let error = test.error();
        outputs.push((wide, capacity, output, result, error));
    }
    let exact = test.bytes(&[0xcc; 32]);
    let complete = test.call(
        "GetWindowTextA",
        &[M(w), I(exact), I(encoded.len() as u32 + 1)],
    );
    let surrogate = test.text("😀X", true);
    test.call("SetWindowTextW", &[M(w), I(surrogate)]);
    let pair = test.bytes(&[0xcc; 8]);
    let split = test.call("GetWindowTextW", &[M(w), I(pair), I(2)]);
    test.run();
    for (wide, capacity, output, result, error) in outputs {
        assert_eq!(test.value(error), 0x12345678);
        let expected = if wide {
            utf16(&text.chars().next().unwrap().to_string())
        } else {
            vec![0]
        };
        assert_eq!(test.value(result), if wide { 1 } else { 0 });
        assert_eq!(test.read(output, expected.len()), expected);
        assert_eq!(
            test.read(output + capacity * if wide { 2 } else { 1 }, 4),
            [0xcc; 4]
        );
    }
    let mut encoded_nul = encoded.clone();
    encoded_nul.push(0);
    assert_eq!(test.value(complete), encoded.len() as u32);
    assert_eq!(test.read(exact, encoded_nul.len()), encoded_nul);
    assert_eq!(test.value(split), 1);
    assert_eq!(test.read(pair, 6), [0x3d, 0xd8, 0, 0, 0xcc, 0xcc]);
    let gets = test.rows(test.value(a), wm::GETTEXT);
    assert_eq!(gets[1][2], if cp == 65001 { 8 } else { 4 });
}

#[test]
fn text_capacity_counts_complete_ansi_characters_and_utf16_units_without_cross_thunk_overruns() {
    capacities(UnicornCpu::new(PARK_ADDR).unwrap(), 936, "中A");
    capacities(JitCpu::new(PARK_ADDR, true).unwrap(), 65001, "中A");
}

fn guards<B: TestCpu>(cpu: B, cp: u32) {
    let mut test = Fixture::new(cpu, cp);
    let copied = test.alloc(64);
    let procedure = test.callback(true, copied, None);
    let window = test.window(true, procedure);
    let output = test.bytes(&[0xcc; 32]);
    let before = test.save(NEXT);
    let mut no_work = Vec::new();
    for (hwnd, buffer, count) in [
        (M(window), output, 0),
        (I(0xbad), output, 0),
        (M(window), 0, 4),
    ] {
        test.set_error(0x12345678);
        let result = test.call("GetWindowTextW", &[hwnd, I(buffer), I(count)]);
        let error = test.error();
        no_work.push((result, error));
    }
    let after = test.save(NEXT);
    let mut failed = Vec::new();
    for (buffer, count, error) in [
        (output, u32::MAX, 87),
        (output, 65537, 87),
        (READONLY, 4, 998),
        (READONLY - 2, 2, 998),
        (0xffff_fffc, 4, 998),
    ] {
        let result = test.call("GetWindowTextW", &[M(window), I(buffer), I(count)]);
        let last_error = test.error();
        failed.push((result, last_error, error));
    }
    let bad_set = test.call("SetWindowTextW", &[M(window), I(1)]);
    let bad_set_error = test.error();
    let invalid_set = test.call("SetWindowTextW", &[I(0xbad), I(1)]);
    let invalid_set_error = test.error();
    let invalid_get = test.call("GetWindowTextW", &[I(0xbad), I(output + 16), I(4)]);
    let invalid_get_error = test.error();
    let invalid_length = test.call("GetWindowTextLengthW", &[I(0xbad)]);
    let invalid_length_error = test.error();
    let rejected_end = test.save(NEXT);
    let a = test.call("GetWindowTextA", &[M(window), I(output), I(16)]);
    let a_error = test.error();
    let w = test.call("GetWindowTextW", &[M(window), I(output), I(16)]);
    test.run();
    for (result, error) in no_work {
        assert_eq!((test.value(result), test.value(error)), (0, 0x12345678));
    }
    assert_eq!(test.value(before), test.value(after));
    assert_eq!(test.value(before), test.value(rejected_end));
    for (result, error, expected) in failed {
        assert_eq!((test.value(result), test.value(error)), (0, expected));
    }
    assert_eq!((test.value(bad_set), test.value(bad_set_error)), (0, 998));
    assert_eq!(
        (test.value(invalid_set), test.value(invalid_set_error)),
        (0, 1400)
    );
    assert_eq!(
        (test.value(invalid_get), test.value(invalid_get_error)),
        (0, 1400)
    );
    assert_eq!(
        (test.value(invalid_length), test.value(invalid_length_error)),
        (0, 1400)
    );
    assert_eq!(test.read(output + 16, 6), [0, 0, 0xcc, 0xcc, 0xcc, 0xcc]);
    assert_eq!(test.value(w), 3);
    assert_eq!(test.read(output, 8), utf16("old"));
    assert!(test.rows(test.value(window), wm::SETTEXT).is_empty());
    let warnings = &test.machine.ext.get::<adv_abi::ApiDiagnostics>().warnings;
    assert!(
        warnings
            .keys()
            .any(|key| key.0 == "user32.dll!GetWindowTextW")
    );
    if cp == 437 {
        assert_eq!((test.value(a), test.value(a_error)), (0, 50));
        assert!(
            warnings
                .keys()
                .any(|key| key.0 == "user32.dll!GetWindowTextA")
        );
    } else {
        assert_eq!(test.value(a), 3);
    }
    assert_eq!(
        test.machine.ext.get::<adv_abi::ApiDiagnostics>().stub_calls,
        0
    );
}

#[test]
fn window_text_guards_reject_bad_ranges_before_callbacks_and_preserve_unicode_without_ansi_codec() {
    guards(UnicornCpu::new(PARK_ADDR).unwrap(), 936);
    guards(JitCpu::new(PARK_ADDR, true).unwrap(), 437);
}

fn send_front<B: TestCpu>(cpu: B) {
    let mut test = Fixture::new(cpu, 936);
    let copied_a = test.alloc(64);
    let copied_w = test.alloc(64);
    let reenter = test.bytes(&1_u32.to_le_bytes());
    let nested_result = test.alloc(4);
    let proc_a = test.callback(false, copied_a, Some((reenter, nested_result)));
    let proc_w = test.callback(true, copied_w, None);
    let a = test.window(false, proc_a);
    let w = test.window(true, proc_w);
    let text = test.text("中A", true);
    let first_a = test.call("SendMessageW", &[M(a), I(wm::SETTEXT), I(0), I(text)]);
    let first_w = test.call("SendMessageW", &[M(w), I(wm::SETTEXT), I(0), I(text)]);
    let length = test.call("SendMessageW", &[M(a), I(wm::GETTEXTLENGTH), I(0), I(0)]);
    let output = test.alloc(32);
    let get = test.call("SendMessageW", &[M(a), I(wm::GETTEXT), I(8), I(output)]);
    // Ordinary nontext message arguments are opaque, including signed LPARAM
    // and LRESULT bit patterns; no text marshaling may reinterpret them.
    let ordinary = test.call(
        "SendMessageW",
        &[M(w), I(0x8033), I(0x12345678), I(0x87654321)],
    );
    let icon = test.call("SendMessageW", &[M(a), I(0x80), I(0), I(0x70000)]);
    let broadcast_text = test.text("星B", true);
    let broadcast = test.call(
        "SendMessageW",
        &[I(0xffff), I(wm::SETTEXT), I(0), I(broadcast_text)],
    );
    test.run();
    assert_eq!((test.value(first_a), test.value(first_w)), (0x55, 0x55));
    assert_eq!(test.value(nested_result), 3);
    assert_eq!((test.value(length), test.value(get)), (2, 2));
    assert_eq!(test.read(output, 6), utf16("中A"));
    assert_eq!(test.value(ordinary), 0xfedcba98);
    assert_eq!(
        test.rows(test.value(w), 0x8033),
        [[test.value(w), 0x8033, 0x12345678, 0x87654321]]
    );
    assert_eq!(
        test.rows(test.value(a), 0x80),
        [[test.value(a), 0x80, 0, 0x70000]]
    );
    assert_eq!(test.value(icon), 0); // Actual guest default-procedure return.
    assert_eq!(test.value(broadcast), 1);
    let mut ansi = test.machine.ext.get::<Win32>().codec.encode("星B");
    ansi.push(0);
    assert_eq!(test.read(copied_a, ansi.len()), ansi);
    assert_eq!(test.read(copied_w, 6), utf16("星B"));
    for hwnd in [test.value(a), test.value(w)] {
        assert_eq!(
            test.machine
                .ext
                .get::<Win32>()
                .user
                .windows
                .get(&test.machine.sys.objects, &hwnd)
                .unwrap()
                .text,
            "星B"
        );
    }
    let sets_a = test.rows(test.value(a), wm::SETTEXT);
    let sets_w = test.rows(test.value(w), wm::SETTEXT);
    assert_eq!(sets_w[0][3], text);
    assert_ne!(sets_a[0][3], text);
    for row in sets_a {
        assert_eq!(test.machine.ext.get::<adv_os::Heaps>().owner(row[3]), None);
    }
    assert_eq!(
        test.machine.ext.get::<adv_abi::ApiDiagnostics>().stub_calls,
        0
    );
}

#[test]
fn send_message_w_front_dispatches_opaque_parameters_unicode_text_and_real_guest_results() {
    send_front(UnicornCpu::new(PARK_ADDR).unwrap());
    send_front(JitCpu::new(PARK_ADDR, false).unwrap());
    send_front(JitCpu::new(PARK_ADDR, true).unwrap());
}

#[path = "window_text/queue_w.rs"]
mod queue_w;

//! MessageBoxA diagnostics come from real guest calls, independent of a logger.
use adv_abi::{ApiDiagnostics, Registry};
use adv_cpu::{
    JitCpu,
    backend::{Backend, Guest, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_os::Process;
use adv_unicorn::UnicornCpu;
use adv_win32::{Win32, Win32Config};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const TEXT: u32 = DATA + 0x100;
const CAPTION: u32 = DATA + 0x300;
const LAST_ERROR: u32 = 0x8765_abcd;

#[test]
fn unicode_message_box_records_text_independently_of_ansi_codepage() {
    let mut machine = fixture(JitCpu::new(PARK_ADDR, false).unwrap(), 932);
    let text = "初始化失败／初期化エラー 🌸";
    let caption = "Unicode";
    for (address, value) in [(TEXT, text), (CAPTION, caption)] {
        let bytes: Vec<_> = value
            .encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect();
        machine.cpu.write(address, &bytes).unwrap();
    }
    let api = machine.resolve_import("user32.dll", "MessageBoxW").unwrap();
    let mut code = Asm::new(CODE);
    code.push_imm(0)
        .push_imm(CAPTION)
        .push_imm(TEXT)
        .push_imm(0)
        .call(api);
    run(&mut machine, code);
    let diagnostics = machine.ext.get::<ApiDiagnostics>();
    let message = diagnostics.message_boxes.back().unwrap();
    assert_eq!(message.api, "user32.dll!MessageBoxW");
    assert_eq!(message.text, text);
    assert_eq!(message.caption, caption);
    assert_eq!(message.result, 1);
    assert_eq!(diagnostics.stub_calls, 0);
}

fn fixture<B: Backend>(cpu: B, codepage: u32) -> Machine<B> {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    let mut config = Win32Config::new(
        "/nonexistent/message-box-probe",
        "message-box-probe.exe",
        "/nonexistent/message-box-probe-data",
    );
    config.os.codepage = codepage;
    Win32::install(&mut machine, config).unwrap();
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    machine
}

fn run<B: Backend>(machine: &mut Machine<B>, mut code: Asm) {
    code.bytes.extend_from_slice(&[0x31, 0xc0]); // xor eax,eax: return exit code zero
    code.ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine
        .start_main(CODE, CODE, 65536, "message-box-probe.exe")
        .unwrap();
    assert!(matches!(
        machine.run_virtual(1_000_000),
        RunResult::Exited(0)
    ));
}

fn call<B: Backend>(
    machine: &mut Machine<B>,
    code: &mut Asm,
    text: u32,
    caption: u32,
    style: u32,
    index: u32,
) -> u32 {
    let set_error = machine
        .resolve_import("kernel32.dll", "SetLastError")
        .unwrap();
    let get_error = machine
        .resolve_import("kernel32.dll", "GetLastError")
        .unwrap();
    let message_box = machine.resolve_import("user32.dll", "MessageBoxA").unwrap();
    code.push_imm(LAST_ERROR).call(set_error);
    code.push_imm(style)
        .push_imm(caption)
        .push_imm(text)
        .push_imm(0)
        .call(message_box);
    let caller = code.here();
    code.mov_mem_eax(DATA + index * 8);
    code.call(get_error).mov_mem_eax(DATA + index * 8 + 4);
    caller
}

fn text_and_defaults<B: Backend>(cpu: B, codepage: u32) {
    let mut machine = fixture(cpu, codepage);
    let (text, caption) = if codepage == 932 {
        (
            "初期化に失敗しました: \"設定\"\n次の行",
            "起動 \"エラー\"\n確認",
        )
    } else {
        ("初始化失败: \"设置\"\n下一行", "启动 \"错误\"\n确认")
    };
    let codec = machine.ext.get::<Win32>().codec;
    let text_bytes = codec.encode(text);
    let caption_bytes = codec.encode(caption);
    assert_eq!(codec.decode(&text_bytes), text);
    assert_eq!(codec.decode(&caption_bytes), caption);
    machine
        .cpu
        .write(TEXT, &[text_bytes.as_slice(), &[0]].concat())
        .unwrap();
    machine
        .cpu
        .write(CAPTION, &[caption_bytes.as_slice(), &[0]].concat())
        .unwrap();
    let styles = [0, 1, 2, 3, 0x0002_0044, 5, 6, 0xf];
    let mut code = Asm::new(CODE);
    let callers: Vec<_> = styles
        .iter()
        .enumerate()
        .map(|(index, style)| call(&mut machine, &mut code, TEXT, CAPTION, *style, index as u32))
        .collect();
    run(&mut machine, code);
    let diagnostics = machine.ext.get::<ApiDiagnostics>();
    assert_eq!(diagnostics.message_boxes.len(), styles.len());
    assert_eq!(diagnostics.message_box_count, styles.len() as u64);
    assert_eq!(diagnostics.dropped_message_boxes, 0);
    assert_eq!(diagnostics.stub_calls, 0);
    for (index, (record, style)) in diagnostics.message_boxes.iter().zip(styles).enumerate() {
        let result = if matches!(style & 0xf, 3 | 4) { 6 } else { 1 };
        assert_eq!(record.api, "user32.dll!MessageBoxA");
        assert_eq!(record.caller, callers[index]);
        assert_eq!(record.context, 0);
        assert_eq!(record.caption, caption);
        assert_eq!(record.text, text);
        assert_eq!(record.style, style);
        assert_eq!(record.result, result);
        assert!(record.sequence > 0);
        assert!(record.preceding_lookup_failure.is_none());
        assert_eq!(
            machine.cpu.read_u32(DATA + index as u32 * 8).unwrap(),
            result
        );
        assert_eq!(
            machine.cpu.read_u32(DATA + index as u32 * 8 + 4).unwrap(),
            LAST_ERROR
        );
    }
    assert!(
        diagnostics
            .message_boxes
            .iter()
            .zip(diagnostics.message_boxes.iter().skip(1))
            .all(|(a, b)| a.sequence < b.sequence)
    );
    assert_eq!(machine.cpu.read_cstr(TEXT, 1024).unwrap(), text_bytes);
    assert_eq!(machine.cpu.read_cstr(CAPTION, 1024).unwrap(), caption_bytes);
}

#[test]
fn message_box_records_guest_codepage_quotes_newlines_and_unchanged_defaults() {
    for codepage in [932, 936] {
        text_and_defaults(JitCpu::new(PARK_ADDR, false).unwrap(), codepage);
        text_and_defaults(UnicornCpu::new(PARK_ADDR).unwrap(), codepage);
    }
}

#[test]
fn message_box_preserves_null_and_unreadable_string_fallbacks() {
    let mut machine = fixture(JitCpu::new(PARK_ADDR, false).unwrap(), 932);
    let mut code = Asm::new(CODE);
    let null_caller = call(&mut machine, &mut code, 0, 0, 0, 0);
    let unreadable_caller = call(&mut machine, &mut code, 0xdead_0000, 0xdead_1000, 4, 1);
    run(&mut machine, code);
    let diagnostics = machine.ext.get::<ApiDiagnostics>();
    assert_eq!(diagnostics.message_boxes.len(), 2);
    for (index, record) in diagnostics.message_boxes.iter().enumerate() {
        assert!(record.caption.is_empty());
        assert!(record.text.is_empty());
        assert_eq!(record.caller, [null_caller, unreadable_caller][index]);
        assert_eq!(record.result, [1, 6][index]);
        assert_eq!(
            machine.cpu.read_u32(DATA + index as u32 * 8 + 4).unwrap(),
            LAST_ERROR
        );
    }
}

#[test]
fn message_box_snapshots_the_preceding_guest_lookup_failure() {
    struct Empty;
    impl adv_os::FileSource for Empty {
        fn read(&self, _: &str) -> Option<Vec<u8>> {
            None
        }
    }
    let mut machine = fixture(JitCpu::new(PARK_ADDR, false).unwrap(), 932);
    let module = machine.load(&Empty, "kernel32.dll").unwrap();
    let module_base = adv_os::modules(&machine.ext).get(module).base;
    let lookup = machine
        .resolve_import("kernel32.dll", "GetProcAddress")
        .unwrap();
    let symbol = DATA + 0x500;
    let symbol_name = "AuthoredMessageBoxDiagnosticMissingExport";
    machine
        .cpu
        .write(symbol, format!("{symbol_name}\0").as_bytes())
        .unwrap();
    machine
        .cpu
        .write(TEXT, b"Feature lookup failed: \"probe\"\nNo game data\0")
        .unwrap();
    machine.cpu.write(CAPTION, b"Diagnostic\0").unwrap();
    let mut code = Asm::new(CODE);
    code.push_imm(symbol).push_imm(module_base).call(lookup);
    let lookup_caller = code.here();
    code.mov_mem_eax(DATA + 0x80);
    let message_caller = call(&mut machine, &mut code, TEXT, CAPTION, 0, 0);
    run(&mut machine, code);
    assert_eq!(machine.cpu.read_u32(DATA + 0x80).unwrap(), 0);
    assert_eq!(machine.cpu.read_u32(DATA + 4).unwrap(), LAST_ERROR);
    let diagnostics = machine.ext.get::<ApiDiagnostics>();
    let failure = diagnostics.dynamic_lookup_failures.back().unwrap();
    let message = diagnostics.message_boxes.back().unwrap();
    assert_eq!(failure.caller, lookup_caller);
    assert_eq!(failure.context, 0);
    assert_eq!(failure.module, "kernel32.dll");
    assert_eq!(failure.name, symbol_name);
    assert_eq!(message.caller, message_caller);
    assert_eq!(message.preceding_lookup_failure.as_ref(), Some(failure));
    assert!(failure.sequence < message.sequence);
}

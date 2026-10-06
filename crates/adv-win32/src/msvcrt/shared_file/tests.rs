//! Actual binary FILE use, overlay selection, share refusal and guarded names.
use super::*;
use adv_abi::Conv;
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
use std::sync::atomic::{AtomicUsize, Ordering};

const CODE: u32 = 0x400000;
const HANDLER: u32 = CODE + 3072;
const DATA: u32 = 0x410000;
const UNREADABLE: u32 = 0x420000;
const NAME_A: u32 = DATA + 2048;
const NAME_W: u32 = DATA + 2112;
const MODE_A: u32 = DATA + 2240;
const MODE_W: u32 = DATA + 2272;
const TEXT_MODE: u32 = DATA + 2304;
const MISSING: u32 = DATA + 2336;
const CALLBACK_COUNT: u32 = DATA + 2400;
const ERROR_PAIR: u32 = DATA + 2404;
const BLOCKER: u32 = DATA + 2408;
const PAYLOAD: &[u8] = b"A\r\nB\x1aC";
static NEXT: AtomicUsize = AtomicUsize::new(0);

fn closed(env: &mut Env<'_>) -> Outcome {
    let pointer = env.arg(0);
    assert!(!env.ext.get::<CrtState>().files.contains_key(&pointer));
    assert!(env.ext.get::<Heaps>().owner(pointer).is_none());
    Outcome::Return(1)
}

fn push_memory(code: &mut Asm, address: u32) {
    code.bytes.extend_from_slice(&[0xff, 0x35]);
    code.bytes.extend_from_slice(&address.to_le_bytes());
}
fn set_pair(code: &mut Asm, errno: u32) {
    code.call(errno).mov_mem_eax(ERROR_PAIR);
    code.bytes.extend_from_slice(&[0xc7, 0x00]);
    code.bytes.extend_from_slice(&79u32.to_le_bytes());
    code.bytes.extend_from_slice(&[0xc7, 0x40, 4]);
    code.bytes.extend_from_slice(&77u32.to_le_bytes());
}
fn error_outputs(code: &mut Asm, errno: u32, get_error: u32, out: u32) {
    code.call(errno).bytes.extend_from_slice(&[0x8b, 0x10]); // mov edx,[eax].
    code.bytes.extend_from_slice(&[0x89, 0x15]);
    code.bytes.extend_from_slice(&out.to_le_bytes());
    code.bytes.extend_from_slice(&[0x8b, 0x40, 4]);
    code.mov_mem_eax(out + 4)
        .call(get_error)
        .mov_mem_eax(out + 8);
}

fn guest<B: Backend>(cpu: B) {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    super::super::register(&mut registry);
    registry.add("fixture.dll", "closed", Conv::Cdecl { argc: 1 }, closed);
    for module in MODULES {
        for name in ["_fsopen", "_wfsopen"] {
            assert_eq!(
                registry.get(module, name).unwrap().conv,
                Conv::Cdecl { argc: 3 }
            );
            assert!(registry.dynamic_available(module, name));
        }
    }
    assert!(registry.signature_conflicts().is_empty());
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/crt-sharedfile-tests")
        .join(format!(
            "{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
    std::fs::create_dir_all(root.join("game")).unwrap();
    let filename = "文件.bin";
    let original = root.join("game").join(filename);
    std::fs::write(&original, b"original").unwrap();
    let mut config = crate::Win32Config::new(&root.join("game"), "fixture.exe", &root.join("data"));
    config.os.codepage = 936;
    config.os.write_game = false;
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    crate::Win32::install(&mut machine, config).unwrap();
    let overlay = machine
        .ext
        .get::<crate::Win32>()
        .vfs
        .write_path(filename)
        .unwrap();
    std::fs::write(&overlay, PAYLOAD).unwrap();
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    machine.cpu.map(UNREADABLE, 4096, Prot::WRITE).unwrap();
    let mut bytes = machine.ext.get::<crate::Win32>().codec.encode(filename);
    bytes.push(0);
    machine.cpu.write(NAME_A, &bytes).unwrap();
    let wide: Vec<u8> = filename
        .encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect();
    machine.cpu.write(NAME_W, &wide).unwrap();
    machine.cpu.write(MODE_A, b"rb\0").unwrap();
    machine
        .cpu
        .write(MODE_W, &[b'r', 0, b'b', 0, 0, 0])
        .unwrap();
    machine.cpu.write(TEXT_MODE, b"r\0").unwrap();
    machine.cpu.write(MISSING, b"absent.bin\0").unwrap();
    let get_error = machine
        .resolve_import("kernel32.dll", "GetLastError")
        .unwrap();
    let closed = machine.resolve_import("fixture.dll", "closed").unwrap();
    let mut main = Asm::new(CODE);
    for (index, module) in MODULES.iter().enumerate() {
        let read = machine.resolve_import(module, "fread").unwrap();
        let seek = machine.resolve_import(module, "fseek").unwrap();
        let tell = machine.resolve_import(module, "ftell").unwrap();
        let close = machine.resolve_import(module, "fclose").unwrap();
        let errno = machine.resolve_import(module, "_errno").unwrap();
        for (wide, name, mode) in [(false, NAME_A, MODE_A), (true, NAME_W, MODE_W)] {
            let open = machine
                .resolve_import(module, if wide { "_wfsopen" } else { "_fsopen" })
                .unwrap();
            let out = DATA + (index as u32 * 2 + u32::from(wide)) * 96;
            set_pair(&mut main, errno);
            main.push_imm(0x40)
                .push_imm(mode)
                .push_imm(name)
                .call(open)
                .add_esp(12)
                .mov_mem_eax(out);
            error_outputs(&mut main, errno, get_error, out + 4);
            push_memory(&mut main, out);
            main.push_imm(8)
                .push_imm(1)
                .push_imm(out + 64)
                .call(read)
                .add_esp(16)
                .mov_mem_eax(out + 16);
            main.push_imm(0).push_imm(0);
            push_memory(&mut main, out);
            main.call(seek).add_esp(12).mov_mem_eax(out + 20);
            push_memory(&mut main, out);
            main.call(tell).add_esp(4).mov_mem_eax(out + 24);
            push_memory(&mut main, out);
            main.call(close).add_esp(4).mov_mem_eax(out + 28);
            push_memory(&mut main, out);
            main.call(closed).add_esp(4).mov_mem_eax(out + 32);
        }
    }
    let module = "msvcr90.dll";
    let open = machine.resolve_import(module, "_fsopen").unwrap();
    let errno = machine.resolve_import(module, "_errno").unwrap();
    let set_handler = machine
        .resolve_import(module, "_set_invalid_parameter_handler")
        .unwrap();
    main.push_imm(HANDLER).call(set_handler).add_esp(4);
    for (index, (name, mode, share)) in [
        (MISSING, MODE_A, 0x40),
        (NAME_A, MODE_A, 0x10),
        (NAME_A, TEXT_MODE, 0x40),
        (0, MODE_A, 0x40),
        (NAME_A, MODE_A, 0x55),
    ]
    .into_iter()
    .enumerate()
    {
        let out = DATA + 768 + index as u32 * 20;
        set_pair(&mut main, errno);
        main.push_imm(share)
            .push_imm(mode)
            .push_imm(name)
            .call(open)
            .add_esp(12)
            .mov_mem_eax(out);
        error_outputs(&mut main, errno, get_error, out + 4);
    }
    let create = machine
        .resolve_import("kernel32.dll", "CreateFileA")
        .unwrap();
    let close_handle = machine
        .resolve_import("kernel32.dll", "CloseHandle")
        .unwrap();
    main.push_imm(0)
        .push_imm(0x80)
        .push_imm(3)
        .push_imm(0)
        .push_imm(0)
        .push_imm(0x80000000)
        .push_imm(NAME_A)
        .call(create)
        .mov_mem_eax(BLOCKER);
    set_pair(&mut main, errno);
    main.push_imm(0x40)
        .push_imm(MODE_A)
        .push_imm(NAME_A)
        .call(open)
        .add_esp(12)
        .mov_mem_eax(DATA + 900);
    error_outputs(&mut main, errno, get_error, DATA + 904);
    push_memory(&mut main, BLOCKER);
    main.call(close_handle);
    // A protected filename rejects before publishing a FILE; this is the
    // repository's checked-pointer boundary, not a claim about native AVs.
    main.push_imm(0x40)
        .push_imm(MODE_A)
        .push_imm(UNREADABLE)
        .call(open)
        .add_esp(12)
        .ret();
    assert!(main.bytes.len() < 3072);
    let mut handler = Asm::new(HANDLER);
    handler.bytes.extend_from_slice(&[0xff, 0x05]);
    handler
        .bytes
        .extend_from_slice(&CALLBACK_COUNT.to_le_bytes());
    handler.bytes.extend_from_slice(&[0x31, 0xc0]);
    handler.ret();
    machine.cpu.write(CODE, &main.bytes).unwrap();
    machine.cpu.write(HANDLER, &handler.bytes).unwrap();
    machine
        .spawn("shared CRT FILE fronts", CODE, &[], 65536)
        .unwrap();
    let result = machine.run_virtual(1_000_000);
    assert!(
        matches!(result, RunResult::Failed(ref error) if error.message.contains("420000")),
        "{result:?}"
    );
    for index in 0..6 {
        let out = DATA + index * 96;
        assert_ne!(machine.cpu.read_u32(out).unwrap(), 0);
        for (offset, expected) in [
            (4, 79),
            (8, 77),
            (12, 0),
            (16, 6),
            (20, 0),
            (24, 0),
            (28, 0),
            (32, 1),
        ] {
            assert_eq!(
                machine.cpu.read_u32(out + offset).unwrap(),
                expected,
                "out{out:#x}+{offset}"
            );
        }
        let mut bytes = [0; 6];
        machine.cpu.read(out + 64, &mut bytes).unwrap();
        assert_eq!(bytes, PAYLOAD);
    }
    for (index, expected) in [
        [0, 2, 2, 2],
        [0, 40, 50, 50],
        [0, 40, 50, 50],
        [0, 22, 77, 0],
        [0, 22, 0, 0],
    ]
    .into_iter()
    .enumerate()
    {
        for (word, expected) in expected.into_iter().enumerate() {
            assert_eq!(
                machine
                    .cpu
                    .read_u32(DATA + 768 + index as u32 * 20 + word as u32 * 4)
                    .unwrap(),
                expected
            );
        }
    }
    for (index, expected) in [0, 13, 32, 32].into_iter().enumerate() {
        assert_eq!(
            machine.cpu.read_u32(DATA + 900 + index as u32 * 4).unwrap(),
            expected
        );
    }
    assert_eq!(machine.cpu.read_u32(CALLBACK_COUNT).unwrap(), 2);
    assert!(machine.ext.get::<CrtState>().files.is_empty());
    assert_eq!(std::fs::read(original).unwrap(), b"original");
    assert_eq!(std::fs::read(overlay).unwrap(), PAYLOAD);
}

#[test]
fn shared_binary_fronts_use_overlay_stdio_owner_and_error_contract() {
    guest(JitCpu::new(PARK_ADDR, false).unwrap());
    guest(JitCpu::new(PARK_ADDR, true).unwrap());
    guest(UnicornCpu::new(PARK_ADDR).unwrap());
}

//! Binary VC80 oracle: crt-position-native/probe.c, SHA256
//! 63ABD139713D4E952C11B671E94C5EA3D2FFB78F6D6A22B000628E3CC3D67EEB.
use super::*;
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
use std::sync::atomic::{AtomicUsize, Ordering};

const CODE: u32 = 0x400000;
const HANDLER: u32 = CODE + 3072;
const DATA: u32 = 0x410000;
const NAME: u32 = DATA + 2048;
const MODE: u32 = NAME + 64;
const HIGH: u32 = MODE + 32;
const NEGATIVE: u32 = HIGH + 8;
const ERROR_PAIR: u32 = NEGATIVE + 8;
const COUNTER: u32 = ERROR_PAIR + 4;
const SEEN_ERRNO: u32 = COUNTER + 4;
const SEEN_DOS: u32 = SEEN_ERRNO + 4;
const SEEN_OUTPUT: u32 = SEEN_DOS + 4;
const WATCHED: u32 = SEEN_OUTPUT + 8;
static NEXT: AtomicUsize = AtomicUsize::new(0);

fn fixture<B: Backend>(cpu: B) -> Machine<B> {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    super::super::register(&mut registry);
    for module in MODULES {
        for name in ["fgetpos", "fsetpos"] {
            assert_eq!(
                registry.get(module, name).unwrap().conv,
                Conv::Cdecl { argc: 2 }
            );
            assert_eq!(
                adv_abi::signature::lookup(module, name).unwrap().conv,
                Some(Conv::Cdecl { argc: 2 })
            );
            assert!(registry.dynamic_available(module, name));
        }
    }
    assert!(registry.signature_conflicts().is_empty());
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/crt-position-tests")
        .join(format!(
            "{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
    std::fs::create_dir_all(root.join("game")).unwrap();
    std::fs::write(root.join("game/stream.bin"), b"original").unwrap();
    let mut config = crate::Win32Config::new(root.join("game"), "fixture.exe", root.join("data"));
    config.os.write_game = false;
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    crate::Win32::install(&mut machine, config).unwrap();
    let overlay = machine
        .ext
        .get::<crate::Win32>()
        .vfs
        .write_path("stream.bin")
        .unwrap();
    std::fs::write(overlay, b"abcdefgh").unwrap();
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 8192, Prot::RW).unwrap();
    machine.cpu.map(DATA + 8192, 4096, Prot::READ).unwrap();
    machine.cpu.map(DATA + 12288, 4096, Prot::WRITE).unwrap();
    machine.cpu.write(NAME, b"stream.bin\0").unwrap();
    machine.cpu.write(MODE, b"rb\0").unwrap();
    machine
        .cpu
        .write(HIGH, &0x1_0000_0005i64.to_le_bytes())
        .unwrap();
    machine.cpu.write(NEGATIVE, &(-1i64).to_le_bytes()).unwrap();
    let mut handler = Asm::new(HANDLER);
    handler.bytes.extend_from_slice(&[0xff, 0x05]); // inc [counter]
    handler.bytes.extend_from_slice(&COUNTER.to_le_bytes());
    handler
        .mov_eax_mem(ERROR_PAIR)
        .mov_eax_ptr_eax()
        .mov_mem_eax(SEEN_ERRNO);
    handler
        .mov_eax_mem(ERROR_PAIR)
        .bytes
        .extend_from_slice(&[0x8b, 0x40, 4]);
    handler.mov_mem_eax(SEEN_DOS);
    handler
        .mov_eax_mem(WATCHED)
        .mov_eax_ptr_eax()
        .mov_mem_eax(SEEN_OUTPUT);
    handler
        .mov_eax_mem(WATCHED)
        .bytes
        .extend_from_slice(&[0x8b, 0x40, 4]);
    handler.mov_mem_eax(SEEN_OUTPUT + 4).ret();
    machine.cpu.write(HANDLER, &handler.bytes).unwrap();
    machine
}
fn call(code: &mut Asm, api: u32, args: &[u32]) {
    for &argument in args.iter().rev() {
        code.push_imm(argument);
    }
    code.call(api);
    if !args.is_empty() {
        code.add_esp((args.len() * 4) as u8);
    }
}
fn push_memory(code: &mut Asm, address: u32) {
    code.bytes.extend_from_slice(&[0xff, 0x35]);
    code.bytes.extend_from_slice(&address.to_le_bytes());
}
fn position(code: &mut Asm, api: u32, stream_cell: u32, value: u32) {
    code.push_imm(value);
    push_memory(code, stream_cell);
    code.call(api).add_esp(8);
}
fn immediate(code: &mut Asm, address: u32, value: u32) {
    code.bytes.push(0xb8);
    code.bytes.extend_from_slice(&value.to_le_bytes());
    code.mov_mem_eax(address);
}
fn seed(code: &mut Asm, errno: u32, set_error: u32) {
    code.call(errno).mov_mem_eax(ERROR_PAIR);
    code.bytes.extend_from_slice(&[0xc7, 0x00]);
    code.bytes.extend_from_slice(&79u32.to_le_bytes());
    code.bytes.extend_from_slice(&[0xc7, 0x40, 4]);
    code.bytes.extend_from_slice(&91u32.to_le_bytes());
    immediate(code, COUNTER, 0);
    code.push_imm(77).call(set_error); // stdcall cleanup.
}
fn errors(code: &mut Asm, get_error: u32, row: u32) {
    code.mov_eax_mem(ERROR_PAIR)
        .mov_eax_ptr_eax()
        .mov_mem_eax(row);
    code.mov_eax_mem(ERROR_PAIR)
        .bytes
        .extend_from_slice(&[0x8b, 0x40, 4]);
    code.mov_mem_eax(row + 4)
        .call(get_error)
        .mov_mem_eax(row + 8);
    for (index, source) in [COUNTER, SEEN_ERRNO, SEEN_DOS, SEEN_OUTPUT, SEEN_OUTPUT + 4]
        .into_iter()
        .enumerate()
    {
        code.mov_eax_mem(source)
            .mov_mem_eax(row + 12 + index as u32 * 4);
    }
}
fn run<B: Backend>(machine: &mut Machine<B>, mut code: Asm) -> RunResult {
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    assert!(code.bytes.len() < (HANDLER - CODE) as usize);
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine
        .spawn("authored CRT stream position", CODE, &[], 65536)
        .unwrap();
    machine.run_virtual(1_000_000)
}
fn quad<B: Backend>(machine: &Machine<B>, address: u32) -> i64 {
    let mut bytes = [0; 8];
    machine.cpu.read(address, &mut bytes).unwrap();
    i64::from_le_bytes(bytes)
}
fn guest<B: Backend>(cpu: impl Fn() -> B) {
    let mut machine = fixture(cpu());
    let set_error = machine
        .resolve_import("kernel32.dll", "SetLastError")
        .unwrap();
    let get_error = machine
        .resolve_import("kernel32.dll", "GetLastError")
        .unwrap();
    let mut code = Asm::new(CODE);
    for (index, module) in MODULES.into_iter().enumerate() {
        let row = DATA + index as u32 * 256;
        let fopen = machine.resolve_import(module, "fopen").unwrap();
        let fread = machine.resolve_import(module, "fread").unwrap();
        let fclose = machine.resolve_import(module, "fclose").unwrap();
        let get = machine.resolve_import(module, "fgetpos").unwrap();
        let set = machine.resolve_import(module, "fsetpos").unwrap();
        let errno = machine.resolve_import(module, "_errno").unwrap();
        let has_handler = module != "msvcrt.dll";
        if has_handler {
            call(
                &mut code,
                machine
                    .resolve_import(module, "_set_invalid_parameter_handler")
                    .unwrap(),
                &[HANDLER],
            );
        }
        call(&mut code, fopen, &[NAME, MODE]);
        code.mov_mem_eax(row);
        seed(&mut code, errno, set_error);
        push_memory(&mut code, row);
        code.push_imm(3)
            .push_imm(1)
            .push_imm(DATA + 1024 + index as u32 * 8)
            .call(fread)
            .add_esp(16);
        code.bytes.extend_from_slice(&[0x89, 0x25]);
        code.bytes.extend_from_slice(&(row + 8).to_le_bytes());
        position(&mut code, get, row, row + 64);
        code.mov_mem_eax(row + 4);
        code.bytes.extend_from_slice(&[0x89, 0x25]);
        code.bytes.extend_from_slice(&(row + 12).to_le_bytes());
        position(&mut code, set, row, HIGH);
        code.mov_mem_eax(row + 16);
        position(&mut code, get, row, row + 72);
        code.mov_mem_eax(row + 20);
        position(&mut code, set, row, row + 64);
        code.mov_mem_eax(row + 24);
        push_memory(&mut code, row);
        code.push_imm(2)
            .push_imm(1)
            .push_imm(DATA + 1028 + index as u32 * 8)
            .call(fread)
            .add_esp(16)
            .mov_mem_eax(row + 28);
        errors(&mut code, get_error, row + 96);
        position(&mut code, set, row, NEGATIVE);
        code.mov_mem_eax(row + 32);
        errors(&mut code, get_error, row + 128);
        position(&mut code, get, row, row + 80);
        code.mov_mem_eax(row + 36);
        immediate(&mut code, WATCHED, row + 88);
        immediate(&mut code, row + 88, 0x5a5a5a5a);
        immediate(&mut code, row + 92, 0x5a5a5a5a);
        if has_handler {
            seed(&mut code, errno, set_error);
            call(&mut code, get, &[0, row + 88]);
            code.mov_mem_eax(row + 40);
            errors(&mut code, get_error, row + 160);
        }
        push_memory(&mut code, row);
        code.call(fclose).add_esp(4);
        if has_handler {
            seed(&mut code, errno, set_error);
            position(&mut code, get, row, row + 88);
            code.mov_mem_eax(row + 44);
            errors(&mut code, get_error, row + 192);
        }
        seed(&mut code, errno, set_error);
        position(&mut code, set, row, row + 64);
        code.mov_mem_eax(row + 48);
        errors(&mut code, get_error, row + 224);
    }
    let result = run(&mut machine, code);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    for index in 0..3 {
        let row = DATA + index * 256;
        for offset in [4, 16, 20, 24, 36] {
            assert_eq!(machine.cpu.read_u32(row + offset).unwrap(), 0);
        }
        for offset in [32, 48] {
            assert_eq!(machine.cpu.read_u32(row + offset).unwrap(), u32::MAX);
        }
        assert_eq!(
            machine.cpu.read_u32(row + 8).unwrap(),
            machine.cpu.read_u32(row + 12).unwrap()
        );
        assert_eq!(machine.cpu.read_u32(row + 28).unwrap(), 2);
        assert_eq!(quad(&machine, row + 64), 3);
        assert_eq!(quad(&machine, row + 72), 0x1_0000_0005);
        assert_eq!(quad(&machine, row + 80), 5);
        if index > 0 {
            assert_eq!(quad(&machine, row + 88), -1);
            for offset in [40, 44] {
                assert_eq!(machine.cpu.read_u32(row + offset).unwrap(), u32::MAX);
            }
        }
        for (offset, expected) in [
            (96, [79, 91, 77, 0]),
            (128, [22, 131, 131, 0]),
            (160, [22, 91, 0, 1]),
            (192, [9, 0, 0, 1]),
            (224, [22, 91, 77, 0]),
        ] {
            if index == 0 && matches!(offset, 160 | 192) {
                continue;
            }
            for (i, expected) in expected.into_iter().enumerate() {
                assert_eq!(
                    machine.cpu.read_u32(row + offset + i as u32 * 4).unwrap(),
                    expected
                );
            }
        }
        for (offset, errno, dos) in [(160, 22, 91), (192, 9, 0)] {
            if index == 0 {
                continue;
            }
            assert_eq!(machine.cpu.read_u32(row + offset + 16).unwrap(), errno);
            assert_eq!(machine.cpu.read_u32(row + offset + 20).unwrap(), dos);
            assert_eq!(quad(&machine, row + offset + 24), 0x5a5a5a5a5a5a5a5a);
        }
        let mut payload = [0; 5];
        machine
            .cpu
            .read(DATA + 1024 + index * 8, &mut payload[..3])
            .unwrap();
        machine
            .cpu
            .read(DATA + 1028 + index * 8, &mut payload[3..])
            .unwrap();
        assert_eq!(&payload, b"abcde");
    }
    assert!(machine.ext.get::<CrtState>().files.is_empty());
    assert_eq!(
        std::fs::read(
            machine
                .ext
                .get::<crate::Win32>()
                .vfs
                .lookup("stream.bin")
                .unwrap()
        )
        .unwrap(),
        b"abcdefgh"
    );
    // Each guard fails before a seek/query can modify the stream or prefix.
    for (module, output) in MODULES
        .into_iter()
        .flat_map(|module| [true, false].map(|output| (module, output)))
    {
        let mut machine = fixture(cpu());
        let mut code = Asm::new(CODE);
        let fopen = machine.resolve_import(module, "fopen").unwrap();
        call(&mut code, fopen, &[NAME, MODE]);
        code.mov_mem_eax(DATA);
        machine.cpu.write(HIGH, &3i64.to_le_bytes()).unwrap();
        position(
            &mut code,
            machine.resolve_import(module, "fsetpos").unwrap(),
            DATA,
            HIGH,
        );
        let pointer = if output { DATA + 8188 } else { DATA + 12284 };
        if output {
            machine.cpu.write(pointer, &[0x5a; 4]).unwrap();
        }
        let api = machine
            .resolve_import(module, if output { "fgetpos" } else { "fsetpos" })
            .unwrap();
        position(&mut code, api, DATA, pointer);
        assert!(matches!(run(&mut machine, code), RunResult::Failed(_)));
        let stream = machine.cpu.read_u32(DATA).unwrap();
        assert_eq!(
            machine
                .sys
                .objects
                .host::<crate::FileObj>(
                    machine
                        .ext
                        .get::<CrtState>()
                        .files
                        .get(&stream)
                        .unwrap()
                        .handle
                )
                .unwrap()
                .file
                .stream_position()
                .unwrap(),
            3
        );
        if output {
            assert_eq!(machine.cpu.read_u32(pointer).unwrap(), 0x5a5a5a5a);
        }
    }
}

#[test]
fn legacy_fpos_roundtrip_aliases_callback_order_and_atomic_guards() {
    guest(|| UnicornCpu::new(PARK_ADDR).unwrap());
    guest(|| JitCpu::new(PARK_ADDR, false).unwrap());
}

//! VC80/90 byte contract: crt-byte-native/probe.c, SHA256
//! 9FAF9F421F48D681FFF8516EABB0F80E5C920119B9AF07A636A9D428DE5B974A.
use super::super::CrtFile;
use super::*;
use adv_abi::{Conv, signature::AbiKind};
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt},
};
use adv_machine::{
    Clock, Machine, RunResult,
    layout::{PARK_ADDR, teb},
};
use adv_os::Heaps;
use adv_unicorn::UnicornCpu;
use iced_x86::code_asm::*;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const NAME: u32 = DATA + 2048;
const MODE: u32 = DATA + 2240;
const VALUES: [i32; 6] = [0, 10, 26, 0x80, 0x1ff, -1];
const PAYLOAD: [u8; 6] = [0, 10, 26, 0x80, 0xff, 0xff];
const ORIGINAL: &[u8] = b"authored original must remain unchanged";
static NEXT: AtomicUsize = AtomicUsize::new(0);

fn closed(env: &mut Env<'_>) -> Outcome {
    let stream = env.arg(0);
    assert!(!env.ext.get::<CrtState>().files.contains_key(&stream));
    assert!(env.ext.get::<Heaps>().owner(env.cpu, stream).is_none());
    Outcome::Return(1)
}
fn readonly(env: &mut Env<'_>) -> Outcome {
    let stream = env.arg(0);
    env.cpu.protect(stream & !4095, 4096, Prot::READ).unwrap();
    Outcome::Return(1)
}
fn fixture<B: Backend>(cpu: B) -> (Machine<B>, PathBuf) {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    super::super::register(&mut registry);
    registry.add("fixture.dll", "closed", Conv::Cdecl { argc: 1 }, closed);
    registry.add("fixture.dll", "readonly", Conv::Cdecl { argc: 1 }, readonly);
    for module in MODULES {
        for name in ["fputc", "putc"] {
            let signature = adv_abi::signature::lookup(module, name).unwrap();
            assert_eq!(signature.conv, Some(Conv::Cdecl { argc: 2 }));
            assert_eq!(signature.return_kind, AbiKind::I32);
            assert_eq!(signature.parameters[0].kind, AbiKind::I32);
            assert_eq!(signature.parameters[1].kind, AbiKind::Pointer);
            assert!(registry.dynamic_available(module, name));
        }
    }
    assert!(registry.signature_conflicts().is_empty());
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/crt-byte-tests")
        .join(format!(
            "{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
    std::fs::create_dir_all(root.join("game")).unwrap();
    for index in 0..3 {
        std::fs::write(
            root.join("game").join(format!("output{index}.bin")),
            ORIGINAL,
        )
        .unwrap();
    }
    let mut config = crate::Win32Config::new(root.join("game"), "fixture.exe", root.join("data"));
    config.os.write_game = false;
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    crate::Win32::install(&mut machine, config).unwrap();
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    for index in 0..3u32 {
        machine
            .cpu
            .write(NAME + index * 32, format!("output{index}.bin\0").as_bytes())
            .unwrap();
    }
    machine.cpu.write(MODE, b"wb\0rb\0wt\0").unwrap();
    (machine, root)
}
fn write<B: Backend>(machine: &mut Machine<B>, address: u32, mut code: CodeAssembler) {
    machine
        .cpu
        .write(address, &code.assemble(u64::from(address)).unwrap())
        .unwrap();
}
fn seed<B: Backend>(
    machine: &mut Machine<B>,
    code: &mut CodeAssembler,
    module: &str,
    pointer: u32,
) {
    let errno = machine.resolve_import(module, "_errno").unwrap();
    let set_error = machine
        .resolve_import("kernel32.dll", "SetLastError")
        .unwrap();
    code.call(u64::from(errno)).unwrap();
    code.mov(dword_ptr(pointer as i32), eax).unwrap();
    code.mov(dword_ptr(eax), 79).unwrap();
    code.mov(dword_ptr(eax + 4), 91).unwrap();
    code.push(77).unwrap();
    code.call(u64::from(set_error)).unwrap();
}
fn errors<B: Backend>(machine: &mut Machine<B>, code: &mut CodeAssembler, cell: u32, output: u32) {
    code.mov(eax, dword_ptr(cell as i32)).unwrap();
    code.mov(edx, dword_ptr(eax)).unwrap();
    code.mov(dword_ptr(output as i32), edx).unwrap();
    code.mov(edx, dword_ptr(eax + 4)).unwrap();
    code.mov(dword_ptr((output + 4) as i32), edx).unwrap();
    let get_error = machine
        .resolve_import("kernel32.dll", "GetLastError")
        .unwrap();
    code.call(u64::from(get_error)).unwrap();
    code.mov(dword_ptr((output + 8) as i32), eax).unwrap();
}
fn open(code: &mut CodeAssembler, entry: u32, name: u32, mode: u32, out: u32) {
    for argument in [0x40, mode, name] {
        code.push(argument).unwrap();
    }
    code.call(u64::from(entry)).unwrap();
    code.add(esp, 12).unwrap();
    code.mov(dword_ptr(out as i32), eax).unwrap();
}
fn execute<B: Backend>(machine: &mut Machine<B>, main: CodeAssembler) -> RunResult {
    write(machine, CODE, main);
    machine
        .spawn("CRT binary character output", CODE, &[], 65536)
        .unwrap();
    machine.run_virtual(1_000_000)
}

fn guest<B: Backend>(cpu: impl Fn() -> B) {
    let (mut machine, root) = fixture(cpu());
    let closed = machine.resolve_import("fixture.dll", "closed").unwrap();
    let mut main = CodeAssembler::new(32).unwrap();
    main.mov(dword_ptr((DATA + 600) as i32), esp).unwrap();
    for (index, module) in MODULES.into_iter().enumerate() {
        let out = DATA + index as u32 * 160;
        let shared = machine.resolve_import(module, "_fsopen").unwrap();
        let close = machine.resolve_import(module, "fclose").unwrap();
        let getpos = machine.resolve_import(module, "ftell").unwrap();
        let read = machine.resolve_import(module, "fread").unwrap();
        open(&mut main, shared, NAME + index as u32 * 32, MODE, out);
        seed(&mut machine, &mut main, module, out + 4);
        for (slot, value) in VALUES.into_iter().enumerate() {
            let put = machine
                .resolve_import(module, if slot % 2 == 0 { "fputc" } else { "putc" })
                .unwrap();
            main.push(dword_ptr(out as i32)).unwrap();
            main.push(value).unwrap();
            main.call(u64::from(put)).unwrap();
            main.add(esp, 8).unwrap();
            main.mov(dword_ptr((out + 16 + slot as u32 * 4) as i32), eax)
                .unwrap();
        }
        errors(&mut machine, &mut main, out + 4, out + 40);
        main.push(dword_ptr(out as i32)).unwrap();
        main.call(u64::from(getpos)).unwrap();
        main.add(esp, 4).unwrap();
        main.mov(dword_ptr((out + 52) as i32), eax).unwrap();
        for entry in [close, closed] {
            main.push(dword_ptr(out as i32)).unwrap();
            main.call(u64::from(entry)).unwrap();
            main.add(esp, 4).unwrap();
        }
        open(
            &mut main,
            shared,
            NAME + index as u32 * 32,
            MODE + 3,
            out + 8,
        );
        seed(&mut machine, &mut main, module, out + 4);
        for slot in 0..8u32 {
            let get = machine
                .resolve_import(module, if slot % 2 == 0 { "fgetc" } else { "getc" })
                .unwrap();
            main.push(dword_ptr((out + 8) as i32)).unwrap();
            main.call(u64::from(get)).unwrap();
            main.add(esp, 4).unwrap();
            main.mov(
                dword_ptr((DATA + 2560 + index as u32 * 64 + slot * 4) as i32),
                eax,
            )
            .unwrap();
        }
        let seek = machine.resolve_import(module, "fseek").unwrap();
        main.push(0).unwrap();
        main.push(0).unwrap();
        main.push(dword_ptr((out + 8) as i32)).unwrap();
        main.call(u64::from(seek)).unwrap();
        main.add(esp, 12).unwrap();
        let put = machine.resolve_import(module, "fputc").unwrap();
        main.push(dword_ptr((out + 8) as i32)).unwrap();
        main.push(65).unwrap();
        main.call(u64::from(put)).unwrap();
        main.add(esp, 8).unwrap();
        main.mov(dword_ptr((out + 56) as i32), eax).unwrap();
        errors(&mut machine, &mut main, out + 4, out + 60);
        main.push(dword_ptr((out + 8) as i32)).unwrap();
        main.push(16).unwrap();
        main.push(1).unwrap();
        main.push(out + 96).unwrap();
        main.call(u64::from(read)).unwrap();
        main.add(esp, 16).unwrap();
        main.mov(dword_ptr((out + 72) as i32), eax).unwrap();
        main.push(dword_ptr((out + 8) as i32)).unwrap();
        main.call(u64::from(close)).unwrap();
        main.add(esp, 4).unwrap();
        main.mov(dword_ptr((out + 76) as i32), esp).unwrap();
    }
    let handler = CODE + 3072;
    let mut callback = CodeAssembler::new(32).unwrap();
    callback.inc(dword_ptr((DATA + 608) as i32)).unwrap();
    callback.ret().unwrap();
    write(&mut machine, handler, callback);
    let setter = machine
        .resolve_import("msvcr80.dll", "_set_invalid_parameter_handler")
        .unwrap();
    main.push(handler).unwrap();
    main.call(u64::from(setter)).unwrap();
    main.add(esp, 4).unwrap();
    seed(&mut machine, &mut main, "msvcr80.dll", DATA + 612);
    let put = machine.resolve_import("msvcr80.dll", "fputc").unwrap();
    main.push(0).unwrap();
    main.push(65).unwrap();
    main.call(u64::from(put)).unwrap();
    main.add(esp, 8).unwrap();
    main.mov(dword_ptr((DATA + 616) as i32), eax).unwrap();
    errors(&mut machine, &mut main, DATA + 612, DATA + 620);
    main.xor(eax, eax).unwrap();
    main.ret().unwrap();
    let result = execute(&mut machine, main);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    for index in 0..3u32 {
        let out = DATA + index * 160;
        for (slot, byte) in PAYLOAD.into_iter().enumerate() {
            assert_eq!(
                machine.cpu.read_u32(out + 16 + slot as u32 * 4).unwrap(),
                u32::from(byte)
            );
        }
        for (offset, expected) in [
            (40, 79),
            (44, 91),
            (48, 77),
            (52, 6),
            (56, u32::MAX),
            (60, 9),
            (64, 91),
            (68, 77),
            (72, 6),
        ] {
            assert_eq!(machine.cpu.read_u32(out + offset).unwrap(), expected);
        }
        assert_eq!(
            machine.cpu.read_u32(out + 76).unwrap(),
            machine.cpu.read_u32(DATA + 600).unwrap()
        );
        let mut bytes = [0; 6];
        machine.cpu.read(out + 96, &mut bytes).unwrap();
        assert_eq!(bytes, PAYLOAD);
        let name = format!("output{index}.bin");
        assert_eq!(
            std::fs::read(root.join("game").join(&name)).unwrap(),
            ORIGINAL
        );
        let overlay = machine.ext.get::<crate::Win32>().vfs.lookup(&name).unwrap();
        assert_eq!(std::fs::read(overlay).unwrap(), PAYLOAD);
    }
    for (offset, expected) in [(608, 1), (616, u32::MAX), (620, 22), (624, 91), (628, 0)] {
        assert_eq!(machine.cpu.read_u32(DATA + offset).unwrap(), expected);
    }
    for module in 0..3u32 {
        for (slot, expected) in [0, 10, 26, 128, 255, 255, u32::MAX, u32::MAX]
            .into_iter()
            .enumerate()
        {
            assert_eq!(
                machine
                    .cpu
                    .read_u32(DATA + 2560 + module * 64 + slot as u32 * 4)
                    .unwrap(),
                expected
            );
        }
    }
    assert!(machine.ext.get::<CrtState>().files.is_empty());

    // Text, closed, pseudo and inaccessible FILEs never advance the underlying
    // file or error cells. Text metadata remains on the same FILE owner.
    for case in ["text", "closed", "pseudo", "protected"] {
        let (mut machine, root) = fixture(cpu());
        let put = machine.resolve_import("msvcr90.dll", "fputc").unwrap();
        let open_fn = machine
            .resolve_import(
                "msvcr90.dll",
                if case == "text" { "fopen" } else { "_fsopen" },
            )
            .unwrap();
        let mut main = CodeAssembler::new(32).unwrap();
        if case == "text" {
            main.push(MODE + 6).unwrap();
            main.push(NAME).unwrap();
            main.call(u64::from(open_fn)).unwrap();
            main.add(esp, 8).unwrap();
            main.mov(dword_ptr(DATA as i32), eax).unwrap();
        } else if case == "pseudo" {
            main.mov(dword_ptr(DATA as i32), DATA + 1024).unwrap();
        } else {
            open(&mut main, open_fn, NAME, MODE, DATA);
        }
        seed(&mut machine, &mut main, "msvcr90.dll", DATA + 4);
        if case == "closed" || case == "protected" {
            let entry = machine
                .resolve_import(
                    if case == "closed" {
                        "msvcr90.dll"
                    } else {
                        "fixture.dll"
                    },
                    if case == "closed" {
                        "fclose"
                    } else {
                        "readonly"
                    },
                )
                .unwrap();
            main.push(dword_ptr(DATA as i32)).unwrap();
            main.call(u64::from(entry)).unwrap();
            main.add(esp, 4).unwrap();
        }
        main.push(dword_ptr(DATA as i32)).unwrap();
        main.push(10).unwrap();
        main.call(u64::from(put)).unwrap();
        main.add(esp, 8).unwrap();
        main.ret().unwrap();
        let result = execute(&mut machine, main);
        assert!(matches!(result, RunResult::Failed(_)), "{result:?}");
        let errno = machine.cpu.read_u32(DATA + 4).unwrap();
        assert_eq!(machine.cpu.read_u32(errno).unwrap(), 79);
        assert_eq!(machine.cpu.read_u32(errno + 4).unwrap(), 91);
        assert_eq!(
            machine
                .cpu
                .read_u32(machine.sys.contexts[0].teb + teb::LAST_ERROR)
                .unwrap(),
            77
        );
        assert_eq!(
            std::fs::read(root.join("game/output0.bin")).unwrap(),
            ORIGINAL
        );
        if case != "pseudo" {
            let overlay = machine
                .ext
                .get::<crate::Win32>()
                .vfs
                .lookup("output0.bin")
                .unwrap();
            assert!(std::fs::read(overlay).unwrap().is_empty());
        }
        if case == "text" {
            let stream = machine.cpu.read_u32(DATA).unwrap();
            let owner: &CrtFile = machine.ext.get::<CrtState>().files.get(&stream).unwrap();
            assert!(!owner.binary);
        }
        if case != "protected" {
            assert!(
                machine
                    .ext
                    .get::<adv_abi::ApiDiagnostics>()
                    .warnings
                    .keys()
                    .any(|(api, _)| api == "msvcr90.dll!fputc")
            );
        }
    }
}

#[test]
fn typed_binary_byte_output_uses_stdio_overlay_lifetime_and_error_owners() {
    guest(|| UnicornCpu::new(PARK_ADDR).unwrap());
    guest(|| JitCpu::new(PARK_ADDR, false).unwrap());
    guest(|| JitCpu::new(PARK_ADDR, true).unwrap());
}

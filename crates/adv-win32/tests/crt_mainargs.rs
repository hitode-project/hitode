//! Authored startup data: command-line owner, environment snapshot and x86 cdecl ABI.
use adv_abi::{GuestPtr, Registry, WideStr};
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Env, Machine, Outcome, RunResult, layout::PARK_ADDR};
use adv_os::Heaps;
use adv_unicorn::UnicornCpu;
use adv_win32::{Win32, Win32Config};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const READ_ONLY: u32 = 0x420000;
const STARTUP: u32 = DATA + 256;
const NAME: u32 = DATA + 512;
const VALUE: u32 = DATA + 576;
const SENTINEL: u32 = 0x7777_7777;

fn vector(env: &Env<'_>, pointer: u32, wide: bool) -> Vec<String> {
    let mut strings = Vec::new();
    for i in 0..100 {
        let p = env.cpu.read_u32(pointer + i * 4).unwrap();
        if p == 0 {
            return strings;
        }
        let units = if wide {
            WideStr(GuestPtr::new(p)).read(env.cpu, 32768).unwrap()
        } else {
            let bytes = env.cpu.read_cstr(p, 65536).unwrap();
            env.ext.get::<Win32>().codec.to_utf16(0, &bytes)
        };
        strings.push(String::from_utf16(&units).unwrap());
    }
    panic!("unterminated CRT vector");
}
fn observe(env: &mut Env<'_>) -> Outcome {
    let argc = env.cpu.read_u32(DATA).unwrap();
    for (offset, wide) in [(0, false), (32, true)] {
        assert_eq!(env.cpu.read_u32(DATA + offset).unwrap(), argc);
        assert_eq!(env.cpu.read_u32(DATA + offset + 12).unwrap(), 0);
        assert_eq!(
            env.cpu.read_u32(DATA + offset + 16).unwrap(),
            env.cpu.read_u32(DATA + offset + 4).unwrap()
        );
        assert_eq!(
            env.cpu.read_u32(DATA + offset + 20).unwrap(),
            env.cpu.read_u32(DATA + offset + 8).unwrap()
        );
        let argv = vector(env, env.cpu.read_u32(DATA + offset + 4).unwrap(), wide);
        if argc == 1 {
            assert_eq!(argv, [env.ext.get::<Win32>().exe_path()]);
        } else {
            assert_eq!(argc, 4);
            assert_eq!(argv, [r"C:\Game Path\表.exe", "", "表", "ab\"c"]);
        }
        let entries = vector(env, env.cpu.read_u32(DATA + offset + 8).unwrap(), wide);
        assert!(entries.iter().any(|entry| entry == "CRT_SNAPSHOT=表"));
    }
    assert_eq!(env.cpu.read_u32(DATA + 96).unwrap(), 79); // preserved errno
    assert_eq!(adv_win32::util::last_error(env), 73);
    env.cpu.write_u32(DATA + 100, 1).unwrap();
    Outcome::Return(0)
}
fn machine<B: Backend>(cpu: B) -> Machine<B> {
    let mut reg = Registry::default();
    adv_win32::register(&mut reg);
    adv_win32::msvcrt::register(&mut reg);
    reg.add(
        "fixture.dll",
        "observe",
        adv_machine::Conv::Cdecl { argc: 0 },
        observe,
    );
    assert!(reg.signature_conflicts().is_empty());
    for module in ["msvcrt.dll", "msvcr80.dll", "msvcr90.dll"] {
        for name in ["__getmainargs", "__wgetmainargs"] {
            assert!(reg.dynamic_available(module, name));
        }
    }
    assert!(!reg.dynamic_available("ucrtbase.dll", "__getmainargs"));
    let mut machine = Machine::new(cpu, reg, Clock::virtual_time()).unwrap();
    Win32::install(
        &mut machine,
        Win32Config::new(
            "/nonexistent/mainargs",
            "表 fixture with sufficient room for the authored command line.exe",
            "/nonexistent/mainargs-data",
        ),
    )
    .unwrap();
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    machine.cpu.map(READ_ONLY, 4096, Prot::READ).unwrap();
    machine.cpu.write_u32(STARTUP, 0).unwrap();
    machine
}
fn command<B: Backend>(machine: &mut Machine<B>, line: &str) {
    let (w, heaps) = machine.ext.get2_mut::<Win32, Heaps>();
    let mut ansi = w.codec.encode(line);
    ansi.push(0);
    let wide: Vec<_> = line
        .encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect();
    // Seed the actual shared Win32 buffers; no second command-line state.
    let a = w.static_bytes(
        "cmdline_a",
        &mut machine.cpu,
        &mut machine.sys,
        heaps,
        &ansi,
    );
    let b = w.static_bytes(
        "cmdline_w",
        &mut machine.cpu,
        &mut machine.sys,
        heaps,
        &wide,
    );
    assert!(heaps.size(0, a).unwrap() >= ansi.len() as u32);
    assert!(heaps.size(0, b).unwrap() >= wide.len() as u32);
    machine.cpu.write(a, &ansi).unwrap();
    machine.cpu.write(b, &wide).unwrap();
}
fn invoke(code: &mut Asm, target: u32, args: [u32; 5], result: u32) {
    for arg in args.into_iter().rev() {
        code.push_imm(arg);
    }
    code.call(target).add_esp(20).mov_mem_eax(result);
}
fn run<B: Backend>(machine: &mut Machine<B>, mut code: Asm) -> RunResult {
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine
        .spawn("CRT startup arguments", CODE, &[], 65536)
        .unwrap();
    machine.run_virtual(1_000_000)
}
fn success<B: Backend>(cpu: B, module: &str, custom: bool) {
    let mut machine = machine(cpu);
    if custom {
        command(&mut machine, r#""C:\Game Path\表.exe" "" 表 "ab\"c""#);
    }
    for (at, text) in [(NAME, "CRT_SNAPSHOT"), (VALUE, "表")] {
        machine
            .cpu
            .write(
                at,
                &text
                    .encode_utf16()
                    .chain([0])
                    .flat_map(u16::to_le_bytes)
                    .collect::<Vec<_>>(),
            )
            .unwrap();
    }
    let setter = machine
        .resolve_import("kernel32.dll", "SetEnvironmentVariableW")
        .unwrap();
    let last_error = machine
        .resolve_import("kernel32.dll", "SetLastError")
        .unwrap();
    let errno = machine.resolve_import(module, "_errno").unwrap();
    let mut code = Asm::new(CODE);
    code.push_imm(VALUE).push_imm(NAME).call(setter);
    code.push_imm(73).call(last_error);
    code.call(errno).mov_mem_eax(DATA + 92);
    code.bytes.extend_from_slice(&[0xc7, 0x00, 79, 0, 0, 0]);
    for (name, off) in [("__getmainargs", 0), ("__wgetmainargs", 32)] {
        let target = machine.resolve_import(module, name).unwrap();
        invoke(
            &mut code,
            target,
            [DATA + off, DATA + off + 4, DATA + off + 8, 1, STARTUP],
            DATA + off + 12,
        );
        invoke(
            &mut code,
            target,
            [DATA + off, DATA + off + 16, DATA + off + 20, 0, STARTUP],
            DATA + off + 12,
        );
    }
    code.mov_eax_mem(DATA + 92)
        .mov_eax_ptr_eax()
        .mov_mem_eax(DATA + 96);
    let observer = machine.resolve_import("fixture.dll", "observe").unwrap();
    code.call(observer);
    assert!(matches!(run(&mut machine, code), RunResult::Exited(0)));
    assert_eq!(machine.cpu.read_u32(DATA + 100).unwrap(), 1);
}
#[test]
fn main_arguments_share_command_and_environment_owners_across_crts_and_backends() {
    for module in ["msvcrt.dll", "msvcr80.dll", "msvcr90.dll"] {
        for custom in [false, true] {
            for jit in [false, true] {
                success(JitCpu::new(PARK_ADDR, jit).unwrap(), module, custom);
            }
            success(UnicornCpu::new(PARK_ADDR).unwrap(), module, custom);
        }
    }
}
fn protected<B: Backend>(cpu: B, wide: bool, which: usize) {
    let mut machine = machine(cpu);
    machine.cpu.write(DATA, &[0x77; 64]).unwrap();
    let target = machine
        .resolve_import(
            "msvcr90.dll",
            if wide {
                "__wgetmainargs"
            } else {
                "__getmainargs"
            },
        )
        .unwrap();
    let mut args = [DATA, DATA + 4, DATA + 8, 0, STARTUP];
    if which < 3 {
        args[which] = if which == 2 { DATA + 4094 } else { READ_ONLY };
    } else {
        args[4] = 0;
    }
    let mut code = Asm::new(CODE);
    invoke(&mut code, target, args, DATA + 12);
    let RunResult::Failed(failure) = run(&mut machine, code) else {
        panic!("protected startup output accepted");
    };
    assert!(
        failure.message.contains("ABI guest memory fault"),
        "{}",
        failure.message
    );
    let mut bytes = [0; 64];
    machine.cpu.read(DATA, &mut bytes).unwrap();
    assert_eq!(bytes, [0x77; 64]);
}
#[test]
fn startup_output_preflight_and_input_fault_leave_every_output_unchanged() {
    for wide in [false, true] {
        for which in 0..4 {
            for jit in [false, true] {
                protected(JitCpu::new(PARK_ADDR, jit).unwrap(), wide, which);
            }
            protected(UnicornCpu::new(PARK_ADDR).unwrap(), wide, which);
        }
    }
}
fn glob<B: Backend>(cpu: B, wide: bool) {
    let mut machine = machine(cpu);
    command(&mut machine, "fixture.exe *.txt");
    machine.cpu.write(DATA, &[0x77; 128]).unwrap();
    let target = machine
        .resolve_import(
            "msvcr90.dll",
            if wide {
                "__wgetmainargs"
            } else {
                "__getmainargs"
            },
        )
        .unwrap();
    let errno = machine.resolve_import("msvcr90.dll", "_errno").unwrap();
    let mut code = Asm::new(CODE);
    invoke(
        &mut code,
        target,
        [DATA, DATA + 4, DATA + 8, 1, STARTUP],
        DATA + 12,
    );
    invoke(
        &mut code,
        target,
        [DATA + 32, DATA + 36, DATA + 40, 0, STARTUP],
        DATA + 44,
    );
    invoke(
        &mut code,
        target,
        [DATA + 64, DATA + 68, DATA + 72, 1, STARTUP],
        DATA + 76,
    );
    code.call(errno).mov_eax_ptr_eax().mov_mem_eax(DATA + 96);
    assert!(matches!(run(&mut machine, code), RunResult::Exited(0)));
    for off in [0, 4, 8, 64, 68, 72] {
        assert_eq!(machine.cpu.read_u32(DATA + off).unwrap(), SENTINEL);
    }
    assert_eq!(machine.cpu.read_u32(DATA + 12).unwrap(), u32::MAX);
    assert_eq!(machine.cpu.read_u32(DATA + 44).unwrap(), 0);
    assert_eq!(machine.cpu.read_u32(DATA + 76).unwrap(), u32::MAX);
    assert_eq!(machine.cpu.read_u32(DATA + 32).unwrap(), 2);
    assert_eq!(machine.cpu.read_u32(DATA + 96).unwrap(), 40);
}
#[test]
fn unsupported_glob_expansion_is_negative_before_and_after_plain_vector_creation() {
    for wide in [false, true] {
        for jit in [false, true] {
            glob(JitCpu::new(PARK_ADDR, jit).unwrap(), wide);
        }
        glob(UnicornCpu::new(PARK_ADDR).unwrap(), wide);
    }
}

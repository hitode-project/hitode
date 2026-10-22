//! Real thread-owned windows retain subscriptions without fabricating input packets.
use adv_abi::{
    ApiDiagnostics, Conv, Registry,
    types::{GESTUREINFO, TOUCHINPUT},
};
use adv_cpu::{
    JitCpu,
    backend::{GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
use adv_win32::{Win32, Win32Config};
#[path = "support/test_cpu.rs"]
mod test_cpu;
use test_cpu::TestCpu;
const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
#[derive(Clone, Copy)]
enum Arg {
    I(u32),
    M(u32),
}
use Arg::{I, M};
fn call<B: TestCpu>(m: &mut Machine<B>, code: &mut Asm, name: &str, args: &[Arg], output: u32) {
    let (dll, name) = name.split_once('!').unwrap_or(("user32.dll", name));
    for arg in args.iter().rev() {
        match *arg {
            I(v) => {
                code.push_imm(v);
            }
            M(p) => {
                code.mov_eax_mem(p).push_eax();
            }
        }
    }
    code.call(m.resolve_import(dll, name).unwrap())
        .mov_mem_eax(output);
}
fn touch<B: TestCpu>(cpu: B) {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    registry.add(
        "<touch-fixture>",
        "subscription",
        Conv::Stdcall { argc: 2 },
        |env| {
            let expected = env.arg(1);
            let flags = env
                .ext
                .get::<Win32>()
                .user
                .windows
                .get(&env.sys.objects, &env.arg(0))
                .and_then(|w| w.touch_flags);
            adv_machine::Outcome::Return(u32::from(
                flags
                    == if expected == u32::MAX {
                        None
                    } else {
                        Some(expected)
                    },
            ))
        },
    );
    assert!(registry.signature_conflicts().is_empty());
    let mut m = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/touch-profile-tests");
    Win32::install(&mut m, Win32Config::new(&root, "fixture.exe", &root)).unwrap();
    m.ext.get_or_default::<ApiDiagnostics>();
    m.cpu.map(CODE, 4096, Prot::RX).unwrap();
    m.cpu.map(DATA, 8192, Prot::RW).unwrap();
    m.cpu.write(DATA + 768, b"STATIC\0").unwrap();
    let mut gesture = vec![0xcc; std::mem::size_of::<GESTUREINFO>()];
    gesture[..4].copy_from_slice(&(std::mem::size_of::<GESTUREINFO>() as u32).to_le_bytes());
    m.cpu.write(DATA + 1024, &gesture).unwrap();
    m.cpu.write(DATA + 4096, &gesture).unwrap();
    m.cpu.protect(DATA + 4096, 4096, Prot::READ).unwrap();
    m.cpu.write(DATA + 1152, &[0xcd; 40]).unwrap();
    let mut code = Asm::new(CODE);
    let mut helper = Asm::new(CODE + 3072);
    let create = [
        I(0),
        I(DATA + 768),
        I(0),
        I(0x80000000),
        I(0),
        I(0),
        I(20),
        I(20),
        I(0),
        I(0),
        I(0),
        I(0),
    ];
    call(&mut m, &mut code, "CreateWindowExA", &create, DATA);
    for output in [DATA + 20, DATA + 24] {
        call(
            &mut m,
            &mut code,
            "kernel32.dll!CreateEventA",
            &[I(0), I(1), I(0), I(0)],
            output,
        );
    }
    call(
        &mut m,
        &mut code,
        "kernel32.dll!CreateThread",
        &[I(0), I(0), I(CODE + 3072), I(0), I(0), I(DATA + 28)],
        DATA + 16,
    );
    call(
        &mut m,
        &mut code,
        "kernel32.dll!WaitForSingleObject",
        &[M(DATA + 20), I(u32::MAX)],
        DATA + 32,
    );
    call(&mut m, &mut helper, "CreateWindowExA", &create, DATA + 8);
    call(
        &mut m,
        &mut helper,
        "kernel32.dll!SetEvent",
        &[M(DATA + 20)],
        DATA + 36,
    );
    call(
        &mut m,
        &mut helper,
        "kernel32.dll!WaitForSingleObject",
        &[M(DATA + 24), I(u32::MAX)],
        DATA + 40,
    );
    call(
        &mut m,
        &mut helper,
        "DestroyWindow",
        &[M(DATA + 8)],
        DATA + 44,
    );
    helper.ret_n(4);
    let mut row = DATA + 128;
    let mut expected = Vec::new();
    for (name, args, result, error) in [
        ("UnregisterTouchWindow", vec![M(DATA)], 1, None),
        ("RegisterTouchWindow", vec![M(DATA), I(4)], 0, Some(87)),
        (
            "RegisterTouchWindow",
            vec![I(0xdeadbeef), I(0)],
            0,
            Some(1400),
        ),
        ("RegisterTouchWindow", vec![M(DATA + 8), I(0)], 0, Some(5)),
        ("UnregisterTouchWindow", vec![M(DATA + 8)], 0, Some(5)),
        ("RegisterTouchWindow", vec![M(DATA), I(3)], 1, None),
        ("<touch-fixture>!subscription", vec![M(DATA), I(3)], 1, None),
        ("RegisterTouchWindow", vec![M(DATA), I(0)], 1, None),
        ("<touch-fixture>!subscription", vec![M(DATA), I(0)], 1, None),
        ("UnregisterTouchWindow", vec![M(DATA)], 1, None),
        (
            "<touch-fixture>!subscription",
            vec![M(DATA), I(u32::MAX)],
            1,
            None,
        ),
        ("UnregisterTouchWindow", vec![M(DATA)], 1, None),
        (
            "GetTouchInputInfo",
            vec![
                M(DATA + 20),
                I(1),
                I(DATA + 1152),
                I(std::mem::size_of::<TOUCHINPUT>() as u32),
            ],
            0,
            Some(6),
        ),
        (
            "GetTouchInputInfo",
            vec![I(0), I(1), I(DATA + 1152), I(0)],
            0,
            Some(87),
        ),
        (
            "GetGestureInfo",
            vec![M(DATA + 20), I(DATA + 1024)],
            0,
            Some(6),
        ),
        (
            "GetGestureInfo",
            vec![M(DATA + 20), I(DATA + 4096)],
            0,
            Some(6),
        ),
        ("GetGestureInfo", vec![I(0), I(0)], 0, Some(87)),
        ("GetGestureInfo", vec![I(0), I(DATA + 1200)], 0, Some(87)),
        ("CloseTouchInputHandle", vec![M(DATA + 20)], 0, Some(6)),
        ("CloseTouchInputHandle", vec![I(0)], 0, Some(6)),
        ("CloseGestureInfoHandle", vec![M(DATA + 20)], 0, Some(6)),
        ("CloseGestureInfoHandle", vec![I(0)], 0, Some(6)),
        ("kernel32.dll!SetEvent", vec![M(DATA + 20)], 1, None),
    ] {
        call(&mut m, &mut code, name, &args, row);
        call(&mut m, &mut code, "kernel32.dll!GetLastError", &[], row + 4);
        expected.push((row, result, error));
        row += 8;
    }
    call(
        &mut m,
        &mut code,
        "kernel32.dll!SetEvent",
        &[M(DATA + 24)],
        DATA + 48,
    );
    call(
        &mut m,
        &mut code,
        "kernel32.dll!WaitForSingleObject",
        &[M(DATA + 16), I(u32::MAX)],
        DATA + 52,
    );
    call(&mut m, &mut code, "DestroyWindow", &[M(DATA)], DATA + 56);
    code.ret();
    m.cpu.write(CODE, &code.bytes).unwrap();
    m.cpu.write(CODE + 3072, &helper.bytes).unwrap();
    m.cpu.code_region(CODE, code.bytes.len());
    m.cpu.code_region(CODE + 3072, helper.bytes.len());
    m.spawn("touch subscriptions", CODE, &[], 65536).unwrap();
    let outcome = m.run_virtual(1_000_000);
    assert!(matches!(outcome, RunResult::Exited(_)), "{outcome:?}");
    for (row, result, error) in expected {
        assert_eq!(m.cpu.read_u32(row).unwrap(), result, "call at {row:x}");
        if let Some(error) = error {
            assert_eq!(m.cpu.read_u32(row + 4).unwrap(), error, "error at {row:x}");
        }
    }
    for p in [DATA + 1024, DATA + 4096] {
        let mut actual = vec![0; gesture.len()];
        m.cpu.read(p, &mut actual).unwrap();
        assert_eq!(actual, gesture);
    }
    let mut actual = [0; 40];
    m.cpu.read(DATA + 1152, &mut actual).unwrap();
    assert_eq!(actual, [0xcd; 40]);
    assert!(m.ext.get::<ApiDiagnostics>().warnings.is_empty());
}
#[test]
// spec: WIN32-USER-13 PERS-ABI-1
fn touch_subscriptions_validate_real_thread_ownership_and_packet_handles_never_alias_events() {
    touch(JitCpu::new(PARK_ADDR, false).unwrap());
    touch(JitCpu::new(PARK_ADDR, true).unwrap());
    touch(UnicornCpu::new(PARK_ADDR).unwrap());
}

//! Authored guest calls share the configured identity, never the host account.
use adv_abi::{Conv, Registry};
use adv_cpu::{
    JitCpu,
    backend::{Backend, Guest, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
use adv_win32::{Win32, Win32Config};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const LOG: u32 = DATA + 3072;
const SIZE: u32 = DATA + 128;
const OUT: u32 = DATA + 256;

struct Fixture<B: Backend> {
    machine: Machine<B>,
    code: Asm,
    next: u32,
}
impl<B: Backend> Fixture<B> {
    fn new(cpu: B, codepage: u32, user: &str) -> Self {
        let mut registry = Registry::default();
        adv_win32::register(&mut registry);
        for name in ["GetUserNameA", "GetUserNameW"] {
            assert_eq!(
                registry.get("advapi32.dll", name).unwrap().conv,
                Conv::Stdcall { argc: 2 }
            );
            assert!(registry.dynamic_available("advapi32.dll", name));
        }
        assert!(registry.signature_conflicts().is_empty());
        let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/user-identity-tests");
        let mut config = Win32Config::new(&root, "fixture.exe", &root);
        config.os.codepage = codepage;
        config.os.user_name = user.to_owned();
        Win32::install(&mut machine, config).unwrap();
        machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
        machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
        Self {
            machine,
            code: Asm::new(CODE),
            next: LOG,
        }
    }
    fn call(&mut self, module: &str, name: &str, args: &[u32]) -> u32 {
        for &argument in args.iter().rev() {
            self.code.push_imm(argument);
        }
        let entry = self.machine.resolve_import(module, name).unwrap();
        let output = self.next;
        self.next += 4;
        self.code.call(entry).mov_mem_eax(output);
        output
    }
    fn user(&mut self, wide: bool, buffer: u32, size: u32) -> u32 {
        self.call(
            "advapi32.dll",
            if wide { "GetUserNameW" } else { "GetUserNameA" },
            &[buffer, size],
        )
    }
    fn save(&mut self, source: u32) -> u32 {
        let output = self.next;
        self.next += 4;
        self.code.mov_eax_mem(source).mov_mem_eax(output);
        output
    }
    fn set(&mut self, pointer: u32, value: u32) {
        self.code.bytes.extend_from_slice(&[0xc7, 0x05]);
        self.code.bytes.extend_from_slice(&pointer.to_le_bytes());
        self.code.bytes.extend_from_slice(&value.to_le_bytes());
    }
    fn run(&mut self) {
        self.code.bytes.extend_from_slice(&[0x31, 0xc0]);
        self.code.ret();
        self.machine.cpu.write(CODE, &self.code.bytes).unwrap();
        self.machine
            .spawn("virtual username guest", CODE, &[], 65536)
            .unwrap();
        let result = self.machine.run_virtual(1_000_000);
        assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    }
    fn value(&self, pointer: u32) -> u32 {
        self.machine.cpu.read_u32(pointer).unwrap()
    }
    fn bytes(&self, pointer: u32, count: usize) -> Vec<u8> {
        let mut bytes = vec![0; count];
        self.machine.cpu.read(pointer, &mut bytes).unwrap();
        bytes
    }
}
fn utf16(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect()
}

fn identity<B: Backend>(cpu: B, cp: u32, username: &str, max_char: u32) {
    let mut test = Fixture::new(cpu, cp, username);
    let mut ansi = test.machine.ext.get::<Win32>().codec.encode(username);
    ansi.push(0);
    let wide = utf16(username);
    let acount = ansi.len() as u32;
    let wcount = wide.len() as u32 / 2;
    test.machine.cpu.write(DATA, b"USERNAME\0").unwrap();
    test.machine
        .cpu
        .write(DATA + 32, &utf16("USERNAME"))
        .unwrap();
    test.machine
        .cpu
        .write(DATA + 64, &utf16("Other user"))
        .unwrap();
    test.machine.cpu.write(OUT, &[0xcc; 1024]).unwrap();

    test.set(SIZE, 0);
    let aq = test.user(false, 0, SIZE);
    let aq_error = test.call("kernel32.dll", "GetLastError", &[]);
    let ar = test.save(SIZE);
    test.set(SIZE, 0);
    let wq = test.user(true, OUT, SIZE);
    let wq_error = test.call("kernel32.dll", "GetLastError", &[]);
    let wr = test.save(SIZE);
    let initial_env = test.call(
        "kernel32.dll",
        "GetEnvironmentVariableA",
        &[DATA, OUT + 128, 64],
    );
    let change = test.call(
        "kernel32.dll",
        "SetEnvironmentVariableW",
        &[DATA + 32, DATA + 64],
    );
    let changed_env = test.call(
        "kernel32.dll",
        "GetEnvironmentVariableW",
        &[DATA + 32, OUT + 256, 64],
    );
    // Success threshold is the actual encoding, even when query estimated more.
    test.set(SIZE, acount);
    let a = test.user(false, OUT, SIZE);
    let awritten = test.save(SIZE);
    test.set(SIZE, wcount);
    let w = test.user(true, OUT + 384, SIZE);
    let wwritten = test.save(SIZE);
    let remove = test.call("kernel32.dll", "SetEnvironmentVariableW", &[DATA + 32, 0]);
    let gone = test.call(
        "kernel32.dll",
        "GetEnvironmentVariableA",
        &[DATA, OUT + 512, 64],
    );
    let gone_error = test.call("kernel32.dll", "GetLastError", &[]);
    test.set(SIZE, wcount * max_char);
    let conservative = test.user(false, OUT + 640, SIZE);
    let conservative_written = test.save(SIZE);
    test.set(SIZE, wcount);
    let unchanged = test.user(true, OUT + 768, SIZE);
    test.run();
    assert_eq!(
        (test.value(aq), test.value(aq_error), test.value(ar)),
        (0, 122, wcount * max_char)
    );
    assert_eq!(
        (test.value(wq), test.value(wq_error), test.value(wr)),
        (0, 122, wcount)
    );
    assert_eq!(test.value(initial_env), acount - 1);
    assert_eq!(test.bytes(OUT + 128, ansi.len()), ansi);
    assert_eq!((test.value(change), test.value(changed_env)), (1, 10));
    assert_eq!(
        test.bytes(OUT + 256, utf16("Other user").len()),
        utf16("Other user")
    );
    assert_eq!(
        (
            test.value(a),
            test.value(awritten),
            test.value(w),
            test.value(wwritten)
        ),
        (1, acount, 1, wcount)
    );
    assert_eq!(
        (test.value(remove), test.value(gone), test.value(gone_error)),
        (1, 0, 203)
    );
    assert_eq!(
        (
            test.value(conservative),
            test.value(conservative_written),
            test.value(unchanged)
        ),
        (1, acount, 1)
    );
    for offset in [0, 640] {
        assert_eq!(test.bytes(OUT + offset, ansi.len()), ansi);
        assert_eq!(test.bytes(OUT + offset + acount, 4), [0xcc; 4]);
    }
    for offset in [384, 768] {
        assert_eq!(test.bytes(OUT + offset, wide.len()), wide);
    }
    assert_eq!(test.machine.ext.get::<Win32>().cfg.os.user_name, username);
    assert!(
        test.machine
            .ext
            .try_get::<adv_abi::ApiDiagnostics>()
            .is_none_or(|d| d.warnings.is_empty())
    );
}

#[test]
fn username_aw_query_and_copy_share_guest_identity_independent_of_mutable_environment() {
    for (cp, user, max) in [
        (932, "遊戯Player", 2),
        (936, "玩家Player", 2),
        (1252, "Joueuré", 1),
        (65001, "玩家Player", 4),
    ] {
        identity(UnicornCpu::new(PARK_ADDR).unwrap(), cp, user, max);
        identity(JitCpu::new(PARK_ADDR, false).unwrap(), cp, user, max);
    }
}

fn guard<B: Backend>(cpu: B) {
    let mut test = Fixture::new(cpu, 932, "Player");
    let readonly = DATA + 4096;
    test.machine.cpu.map(readonly, 4096, Prot::READ).unwrap();
    test.machine.cpu.write(OUT, &[0xcc; 32]).unwrap();
    test.machine.cpu.write(DATA + 4094, &[0xcc; 4]).unwrap();
    test.machine.cpu.write_u32(readonly + 128, 1).unwrap();
    let readonly_size = test.user(false, OUT, readonly + 128);
    let readonly_size_error = test.call("kernel32.dll", "GetLastError", &[]);
    test.set(SIZE, 0);
    let small_bad_output = test.user(false, 1, SIZE);
    let small_error = test.call("kernel32.dll", "GetLastError", &[]);
    let required = test.save(SIZE);
    let mut results = Vec::new();
    for (wide, buffer, size) in [
        (false, 1, SIZE),
        (true, readonly, SIZE),
        (true, DATA + 4094, SIZE),
        (false, 0xffff_fffc, SIZE),
        (false, OUT, 0),
    ] {
        test.set(SIZE, 64);
        let result = test.user(wide, buffer, size);
        let error = test.call("kernel32.dll", "GetLastError", &[]);
        let unchanged = test.save(SIZE);
        results.push((result, error, unchanged));
    }
    test.run();
    assert_eq!(
        (test.value(readonly_size), test.value(readonly_size_error)),
        (0, 998)
    );
    assert_eq!(test.value(readonly + 128), 1);
    assert_eq!(
        (
            test.value(small_bad_output),
            test.value(small_error),
            test.value(required)
        ),
        (0, 122, 14)
    );
    for (result, error, unchanged) in results {
        assert_eq!(
            (test.value(result), test.value(error), test.value(unchanged)),
            (0, 998, 64)
        );
    }
    assert_eq!(test.bytes(OUT, 32), [0xcc; 32]);
    assert_eq!(test.bytes(DATA + 4094, 4), [0xcc; 4]);
}

#[test]
fn username_outputs_are_guarded_and_missing_ansi_codec_warns_without_changing_unicode_identity() {
    guard(UnicornCpu::new(PARK_ADDR).unwrap());
    guard(JitCpu::new(PARK_ADDR, false).unwrap());
    let mut unsupported = Fixture::new(UnicornCpu::new(PARK_ADDR).unwrap(), 437, "Player");
    unsupported.machine.cpu.write(OUT, &[0xcc; 32]).unwrap();
    unsupported.set(SIZE, 64);
    let a = unsupported.user(false, OUT, SIZE);
    let error = unsupported.call("kernel32.dll", "GetLastError", &[]);
    let size = unsupported.save(SIZE);
    let w = unsupported.user(true, OUT + 64, SIZE);
    let wsize = unsupported.save(SIZE);
    unsupported.run();
    assert_eq!(
        (
            unsupported.value(a),
            unsupported.value(error),
            unsupported.value(size)
        ),
        (0, 50, 64)
    );
    assert_eq!(unsupported.bytes(OUT, 32), [0xcc; 32]);
    assert_eq!((unsupported.value(w), unsupported.value(wsize)), (1, 7));
    assert_eq!(unsupported.bytes(OUT + 64, 14), utf16("Player"));
    let diagnostics = unsupported.machine.ext.get::<adv_abi::ApiDiagnostics>();
    assert_eq!(diagnostics.warnings.len(), 1);
    assert_eq!(
        diagnostics.warnings.keys().next().unwrap().0,
        "advapi32.dll!GetUserNameA"
    );
    assert_eq!(diagnostics.stub_calls, 0);
}

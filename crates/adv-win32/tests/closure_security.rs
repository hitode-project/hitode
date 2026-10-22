//! Ordinary-user security operations preserve identity and never manufacture privileges.
use adv_abi::Registry;
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const SID: u32 = DATA + 0x1000;
const SD: u32 = DATA + 0x1100;
const STR: u32 = DATA + 0x1200;
const OUT: u32 = DATA + 0x1400;
const HANDLE: u32 = DATA + 0x1800;
const STATE: u32 = DATA + 0x1900;
const PREV: u32 = DATA + 0x1a00;
const INFO: u32 = DATA + 0x1b00;
const MARK: u32 = 0x12345678;
#[derive(Clone, Copy)]
enum Arg {
    I(u32),
    L(u32),
}
use Arg::{I, L};
struct Calls<B: Backend> {
    m: Machine<B>,
    code: Asm,
    expected: Vec<(String, u32, u32)>,
}
impl<B: Backend> Calls<B> {
    fn call(&mut self, name: &str, args: &[Arg], value: u32, error: Option<u32>) {
        let (module, name) = name.split_once('!').unwrap_or(("advapi32.dll", name));
        let api = self.m.resolve_import(module, name).unwrap();
        let set = self
            .m
            .resolve_import("kernel32.dll", "SetLastError")
            .unwrap();
        let get = self
            .m
            .resolve_import("kernel32.dll", "GetLastError")
            .unwrap();
        self.code.push_imm(MARK).call(set);
        for arg in args.iter().rev() {
            match arg {
                I(v) => {
                    self.code.push_imm(*v);
                }
                L(p) => {
                    self.code.mov_eax_mem(*p).push_eax();
                }
            }
        }
        let at = DATA + self.expected.len() as u32 * 8;
        self.code
            .call(api)
            .mov_mem_eax(at)
            .call(get)
            .mov_mem_eax(at + 4);
        self.expected
            .push((name.into(), value, error.unwrap_or(MARK)));
    }
    fn word(&mut self, address: u32, value: u32) {
        self.code.mov_mem_imm(address, value);
    }
    fn capture(&mut self, address: u32, output: u32) {
        self.code.mov_eax_mem(address).mov_mem_eax(output);
    }
}
fn run<B: Backend>(cpu: B) {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    let mut m = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    let mut cfg = adv_win32::Win32Config::new(".", "fixture.exe", "target/closure-security-tests");
    cfg.os.user_name = "AuthoredUser".into();
    adv_win32::Win32::install(&mut m, cfg).unwrap();
    m.cpu.map(CODE, 16384, Prot::RX).unwrap();
    m.cpu.map(DATA, 8192, Prot::RW).unwrap();
    m.cpu
        .write(SID, &adv_os::identity::current_user_sid())
        .unwrap();
    m.cpu
        .write(SID + 64, &[1, 1, 0, 0, 0, 0, 0, 5, 99, 0, 0, 0])
        .unwrap();
    m.cpu.write(SID + 96, &[2; 8]).unwrap();
    m.cpu
        .write(
            SD,
            &[
                1, 0, 1, 128, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            ],
        )
        .unwrap();
    m.cpu
        .write(SD + 20, &adv_os::identity::current_user_sid())
        .unwrap();
    for (at, text) in [
        (STR, "SeDebugPrivilege"),
        (STR + 64, "AuthoredUnknownPrivilege"),
        (STR + 128, "offline.invalid"),
    ] {
        m.cpu.write(at, &[text.as_bytes(), &[0]].concat()).unwrap();
    }
    m.cpu.write(OUT, &[0xcd; 512]).unwrap();
    let mut f = Calls {
        m,
        code: Asm::new(CODE),
        expected: Vec::new(),
    };
    f.call("IsValidSid", &[I(SID)], 1, None);
    f.call("IsValidSid", &[I(SID + 96)], 0, None);
    f.call(
        "GetSecurityDescriptorOwner",
        &[I(SD), I(OUT), I(OUT + 4)],
        1,
        None,
    );
    f.capture(OUT, DATA + 0x900);
    f.capture(OUT + 4, DATA + 0x904);
    f.call(
        "LookupPrivilegeValueA",
        &[I(0), I(STR), I(OUT + 8)],
        1,
        None,
    );
    f.capture(OUT + 8, DATA + 0x908);
    f.call(
        "LookupPrivilegeValueA",
        &[I(0), I(STR + 64), I(OUT + 8)],
        0,
        Some(1313),
    );
    f.call(
        "LookupPrivilegeValueA",
        &[I(STR + 128), I(STR), I(OUT + 8)],
        0,
        Some(1722),
    );
    f.word(OUT + 300, 0);
    f.word(OUT + 304, 0);
    f.word(OUT + 308, MARK);
    f.call(
        "LookupAccountSidA",
        &[
            I(0),
            I(SID),
            I(OUT + 32),
            I(OUT + 300),
            I(OUT + 96),
            I(OUT + 304),
            I(OUT + 308),
        ],
        0,
        Some(122),
    );
    f.capture(OUT + 300, DATA + 0x90c);
    f.capture(OUT + 304, DATA + 0x910);
    f.capture(OUT + 308, DATA + 0x914);
    f.word(OUT + 300, 64);
    f.word(OUT + 304, 64);
    f.call(
        "LookupAccountSidA",
        &[
            I(0),
            I(SID),
            I(OUT + 32),
            I(OUT + 300),
            I(OUT + 96),
            I(OUT + 304),
            I(OUT + 308),
        ],
        1,
        None,
    );
    f.call(
        "LookupAccountSidA",
        &[
            I(0),
            I(SID + 64),
            I(OUT + 32),
            I(OUT + 300),
            I(OUT + 96),
            I(OUT + 304),
            I(OUT + 308),
        ],
        0,
        Some(1332),
    );
    f.call(
        "OpenProcessToken",
        &[I(u32::MAX), I(0xf01ff), I(HANDLE)],
        1,
        None,
    );
    f.call(
        "kernel32.dll!DuplicateHandle",
        &[
            I(u32::MAX),
            L(HANDLE),
            I(u32::MAX),
            I(HANDLE + 8),
            I(8),
            I(0),
            I(0),
        ],
        1,
        None,
    );
    f.call(
        "DuplicateTokenEx",
        &[L(HANDLE), I(0), I(0), I(2), I(2), I(HANDLE + 4)],
        1,
        None,
    );
    f.call(
        "GetTokenInformation",
        &[L(HANDLE + 4), I(8), I(INFO), I(4), I(INFO + 256)],
        1,
        None,
    );
    f.capture(INFO, DATA + 0x918);
    f.call(
        "GetTokenInformation",
        &[L(HANDLE + 4), I(9), I(INFO), I(4), I(INFO + 256)],
        1,
        None,
    );
    f.capture(INFO, DATA + 0x91c);
    f.word(STATE, 1);
    f.word(STATE + 4, 20);
    f.word(STATE + 8, 0);
    f.word(STATE + 12, 2);
    f.call(
        "AdjustTokenPrivileges",
        &[L(HANDLE), I(0), I(STATE), I(64), I(PREV), I(PREV + 128)],
        1,
        Some(1300),
    );
    f.capture(PREV, DATA + 0x920);
    f.word(STATE + 4, 19);
    f.word(PREV, MARK);
    f.call(
        "AdjustTokenPrivileges",
        &[L(HANDLE), I(0), I(STATE), I(4), I(PREV), I(PREV + 128)],
        0,
        Some(122),
    );
    f.capture(PREV, DATA + 0x924);
    f.capture(PREV + 128, DATA + 0x928);
    f.call(
        "AdjustTokenPrivileges",
        &[L(HANDLE), I(0), I(STATE), I(64), I(PREV), I(PREV + 128)],
        1,
        Some(0),
    );
    f.capture(PREV + 12, DATA + 0x92c);
    f.call(
        "GetTokenInformation",
        &[L(HANDLE), I(3), I(INFO), I(128), I(INFO + 256)],
        1,
        None,
    );
    f.capture(INFO + 12, DATA + 0x930);
    f.call(
        "GetTokenInformation",
        &[L(HANDLE + 8), I(3), I(INFO), I(128), I(INFO + 256)],
        1,
        None,
    );
    f.capture(INFO + 12, DATA + 0x93c);
    f.call(
        "AdjustTokenPrivileges",
        &[L(HANDLE + 8), I(0), I(STATE), I(0), I(0), I(0)],
        0,
        Some(5),
    );
    f.call(
        "GetTokenInformation",
        &[L(HANDLE + 4), I(3), I(INFO), I(128), I(INFO + 256)],
        1,
        None,
    );
    f.capture(INFO + 12, DATA + 0x934);
    f.call(
        "AdjustTokenPrivileges",
        &[L(HANDLE), I(0), I(PREV), I(0), I(0), I(0)],
        1,
        Some(0),
    );
    f.call(
        "GetTokenInformation",
        &[L(HANDLE), I(3), I(INFO), I(128), I(INFO + 256)],
        1,
        None,
    );
    f.capture(INFO + 12, DATA + 0x938);
    f.word(STATE + 4, 23);
    f.word(STATE + 12, 4);
    f.call(
        "AdjustTokenPrivileges",
        &[L(HANDLE + 4), I(0), I(STATE), I(0), I(0), I(0)],
        1,
        Some(0),
    );
    f.call(
        "GetTokenInformation",
        &[L(HANDLE + 4), I(3), I(INFO), I(128), I(INFO + 256)],
        1,
        None,
    );
    f.capture(INFO, DATA + 0x940);
    f.word(STATE + 12, 2);
    f.call(
        "AdjustTokenPrivileges",
        &[L(HANDLE + 4), I(0), I(STATE), I(0), I(0), I(0)],
        1,
        Some(1300),
    );
    f.call("ImpersonateLoggedOnUser", &[L(HANDLE + 4)], 1, None);
    f.call("RevertToSelf", &[], 1, None);
    f.call("RevertToSelf", &[], 1, None);
    f.call(
        "CreateProcessWithTokenW",
        &[L(HANDLE), I(0), I(0), I(0), I(0), I(0), I(0), I(0), I(0)],
        0,
        Some(1314),
    );
    f.call("ImpersonateLoggedOnUser", &[I(0)], 0, Some(6));
    f.call("kernel32.dll!CloseHandle", &[L(HANDLE)], 1, None);
    f.call(
        "GetTokenInformation",
        &[L(HANDLE + 8), I(3), I(INFO), I(128), I(INFO + 256)],
        1,
        None,
    );
    f.capture(INFO, DATA + 0x944);
    for at in [HANDLE + 8, HANDLE + 4] {
        f.call("kernel32.dll!CloseHandle", &[L(at)], 1, None);
    }
    f.word(DATA + 0x700, 0);
    f.code.mov_eax_mem(DATA + 0x700).ret();
    f.m.cpu.write(CODE, &f.code.bytes).unwrap();
    f.m.spawn("ordinary security", CODE, &[], 65536).unwrap();
    let result = f.m.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    for (i, (name, value, error)) in f.expected.into_iter().enumerate() {
        assert_eq!(
            f.m.cpu.read_u32(DATA + i as u32 * 8).unwrap(),
            value,
            "{name} row {i}"
        );
        assert_eq!(
            f.m.cpu.read_u32(DATA + i as u32 * 8 + 4).unwrap(),
            error,
            "{name} error row {i}"
        );
    }
    for (offset, value) in [
        (0, SD + 20),
        (4, 1),
        (8, 20),
        (12, 13),
        (16, 8),
        (20, MARK),
        (24, 2),
        (28, 2),
        (32, 0),
        (36, MARK),
        (40, 16),
        (44, 0),
        (48, 2),
        (52, 0),
        (56, 0),
        (60, 2),
        (64, 4),
        (68, 5),
    ] {
        assert_eq!(
            f.m.cpu.read_u32(DATA + 0x900 + offset).unwrap(),
            value,
            "capture {offset}"
        );
    }
    assert_eq!(f.m.cpu.read_cstr(OUT + 32, 64).unwrap(), b"AuthoredUser");
    assert_eq!(f.m.cpu.read_cstr(OUT + 96, 64).unwrap(), b"PAL-HLE");
}
#[test]
// spec: SYS-SECURITY-1, SYS-SECURITY-2
fn ordinary_security_queries_and_token_mutations_share_real_state_on_three_backends() {
    for jit in [false, true] {
        let mut cpu = JitCpu::new(PARK_ADDR, jit).unwrap();
        cpu.set_async_compilation(false);
        run(cpu);
    }
    run(UnicornCpu::new(PARK_ADDR).unwrap());
}

//! Public cdecl byte calls against VC80 captures, over the same locale owner.
use super::*;
use adv_abi::Conv;
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const OUTPUT: u32 = DATA + 8192;
const MODULES: [&str; 3] = ["msvcrt.dll", "msvcr80.dll", "msvcr90.dll"];
const NAMES: [&str; 13] = [
    "tolower", "toupper", "isalpha", "isalnum", "isdigit", "isspace", "iscntrl", "isupper",
    "islower", "isxdigit", "ispunct", "isprint", "isgraph",
];
// Independent FNV32 hashes of VC80's integer results. Predicates include EOF
// then all256 bytes; fold hashes include the256 bytes only. No host locale.
const GOLDEN: [[u32; 13]; 4] = [
    [
        0x81aa4dc5, 0x8ec9fdc5, 0x5a8e92e5, 0x74f789a5, 0xe270bbd5, 0x5c3ccf95, 0xb054e735,
        0x54b7b745, 0xcddea0b5, 0xbb70de15, 0x9ef13115, 0x05275aa5, 0x1ef7da65,
    ],
    [
        0x81aa4dc5, 0x8ec9fdc5, 0x60c3f0cd, 0x7b2ce78d, 0xe270bbd5, 0x5c3ccf95, 0x586be715,
        0x54b7b745, 0xcddea0b5, 0xbb70de15, 0xa1672265, 0x35b7685d, 0x548c44dd,
    ],
    [
        0x81aa4dc5, 0x8ec9fdc5, 0x28a50dbd, 0x430e047d, 0xe270bbd5, 0x5c3ccf95, 0xb054e735,
        0x54b7b745, 0xcddea0b5, 0xbb70de15, 0x9ef13115, 0xfb2293fd, 0x19f7707d,
    ],
    [
        0x03517775, 0x0f091875, 0x753c29a4, 0x7428f9e8, 0x735681d1, 0xa9887f9d, 0x2ab97d35,
        0xdeb07215, 0xc6fed5f7, 0xbb70de15, 0x6af369f5, 0x7fb3c9e8, 0x08e626a8,
    ],
];
const LAST_ERROR: u32 = 0x12345678;

fn profile(env: &mut Env<'_>) -> Outcome {
    let module = MODULES[env.arg(0) as usize];
    let codepage = env.arg(1);
    let state = env
        .ext
        .get_or_default::<CrtState>()
        .locales
        .entry(module.to_owned())
        .or_default();
    state.categories[1] = Locale {
        language: match codepage {
            0 => Language::C,
            932 => Language::Japanese,
            936 => Language::Chinese,
            _ => Language::English,
        },
        codepage,
    };
    state.ever_non_c |= codepage != 0;
    Outcome::Return(0)
}
fn readonly(env: &mut Env<'_>) -> Outcome {
    let pointer = env.arg(0);
    assert!(env.ext.get::<Heaps>().owner(pointer - 2).is_some());
    env.cpu
        .protect((pointer - 2) & !4095, 4096, Prot::READ)
        .unwrap();
    Outcome::Return(0)
}
fn fixture<B: Backend>(cpu: B) -> Machine<B> {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    super::super::super::register(&mut registry);
    registry.add("fixture.dll", "profile", Conv::Cdecl { argc: 2 }, profile);
    registry.add("fixture.dll", "readonly", Conv::Cdecl { argc: 1 }, readonly);
    for module in MODULES {
        assert_eq!(
            registry.get(module, "setlocale").unwrap().conv,
            Conv::Cdecl { argc: 2 }
        );
        for name in NAMES {
            assert_eq!(
                registry.get(module, name).unwrap().conv,
                Conv::Cdecl { argc: 1 }
            );
            assert!(registry.dynamic_available(module, name));
        }
    }
    assert!(registry.signature_conflicts().is_empty());
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/crt-byte-front-tests");
    let mut config = crate::Win32Config::new(&root, "fixture.exe", &root);
    config.os.codepage = 936; // The default C profile and other CRT owners stay independent.
    crate::Win32::install(&mut machine, config).unwrap();
    machine.cpu.map(CODE, 16384, Prot::RX).unwrap();
    machine.cpu.map(DATA, 256 * 1024, Prot::RW).unwrap();
    machine
}
fn hash(bytes: &[u8]) -> u32 {
    bytes.iter().fold(2166136261, |h, &b| {
        (h ^ u32::from(b)).wrapping_mul(16777619)
    })
}
fn loop_bytes(code: &mut Asm, entry: u32, output: u32, eof: bool) {
    code.bytes.push(0xbf);
    code.bytes.extend_from_slice(&output.to_le_bytes()); // EDI=output.
    if eof {
        code.push_imm(u32::MAX).call(entry).add_esp(4);
        code.bytes.extend_from_slice(&[0x89, 0x07, 0x83, 0xc7, 4]);
    }
    code.bytes.extend_from_slice(&[0xbb, 0, 0, 0, 0]); // EBX=byte0.
    let start = code.here();
    code.bytes.push(0x53);
    code.call(entry).add_esp(4); // push EBX / public cdecl call.
    code.bytes
        .extend_from_slice(&[0x89, 0x07, 0x83, 0xc7, 4, 0x43, 0x81, 0xfb, 0, 1, 0, 0]);
    code.jnz(start);
}
fn success<B: Backend>(cpu: B) {
    let mut machine = fixture(cpu);
    let protect = machine.resolve_import("fixture.dll", "readonly").unwrap();
    let set_error = machine
        .resolve_import("kernel32.dll", "SetLastError")
        .unwrap();
    let get_error = machine
        .resolve_import("kernel32.dll", "GetLastError")
        .unwrap();
    let ctype = machine
        .resolve_import("msvcr90.dll", "__pctype_func")
        .unwrap();
    let mut code = Asm::new(CODE);
    code.bytes.extend_from_slice(&[0x89, 0xe5]); // callee-saved EBP holds initialESP.
    let mut expected = Vec::new();
    let mut out = OUTPUT;
    for (index, name) in [
        "C",
        "Japanese_Japan.932",
        "Chinese (Simplified)_China.936",
        "English_United States.1252",
    ]
    .into_iter()
    .enumerate()
    {
        machine
            .cpu
            .write(
                DATA + 1024 + index as u32 * 128,
                &name.bytes().chain([0]).collect::<Vec<_>>(),
            )
            .unwrap();
    }
    for (module_index, module) in MODULES.into_iter().enumerate() {
        let errno = machine.resolve_import(module, "_errno").unwrap();
        let set = machine.resolve_import(module, "setlocale").unwrap();
        code.call(errno).bytes.extend_from_slice(&[0xc7, 0x00]);
        code.bytes.extend_from_slice(&79u32.to_le_bytes());
        code.push_imm(LAST_ERROR).call(set_error);
        for name in ["tolower", "toupper"] {
            // Native pristine-C EOF contract.
            let entry = machine.resolve_import(module, name).unwrap();
            code.push_imm(u32::MAX).call(entry).add_esp(4).mov_mem_eax(
                DATA + module_index as u32 * 8 + if name == "toupper" { 4 } else { 0 },
            );
        }
        for (profile_index, cp) in [0, 932, 936, 1252].into_iter().enumerate() {
            code.push_imm(DATA + 1024 + profile_index as u32 * 128)
                .push_imm(2)
                .call(set)
                .add_esp(8);
            for (function, name) in NAMES.into_iter().enumerate() {
                let entry = machine.resolve_import(module, name).unwrap();
                let eof = function >= 2;
                loop_bytes(&mut code, entry, out, eof);
                let length = if eof { 257 * 4 } else { 256 * 4 };
                expected.push((
                    out,
                    length,
                    GOLDEN[profile_index][function],
                    module,
                    cp,
                    name,
                ));
                out += length;
            }
            let check = DATA + 256 + (module_index as u32 * 4 + profile_index as u32) * 8;
            code.call(errno).bytes.extend_from_slice(&[0x8b, 0x00]);
            code.mov_mem_eax(check)
                .call(get_error)
                .mov_mem_eax(check + 4);
        }
    }
    // A getter's real table owner may be made read-only. The public byte calls
    // read shared host classification semantics; none republishes guest memory.
    code.call(ctype)
        .mov_mem_eax(DATA + 512)
        .push_eax()
        .call(protect)
        .add_esp(4);
    for module in MODULES {
        for name in NAMES {
            code.push_imm(0x53)
                .call(machine.resolve_import(module, name).unwrap())
                .add_esp(4);
        }
    }
    code.call(ctype).mov_mem_eax(DATA + 516);
    code.bytes.extend_from_slice(&[0x89, 0xe0]);
    code.mov_mem_eax(DATA + 520);
    code.bytes.extend_from_slice(&[0x89, 0xe8]);
    code.mov_mem_eax(DATA + 524);
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    assert!(code.bytes.len() < 16384 && out < DATA + 256 * 1024);
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine
        .spawn("CRT byte consumers cdecl", CODE, &[], 65536)
        .unwrap();
    let result = machine.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    for (address, length, golden, module, cp, name) in expected {
        let mut bytes = vec![0; length as usize];
        machine.cpu.read(address, &mut bytes).unwrap();
        assert_eq!(hash(&bytes), golden, "{module}!{name} cp{cp}");
    }
    for module in 0..3 {
        for function in 0..2 {
            assert_eq!(
                machine
                    .cpu
                    .read_u32(DATA + module * 8 + function * 4)
                    .unwrap(),
                u32::MAX
            );
        }
    }
    for index in 0..12 {
        assert_eq!(machine.cpu.read_u32(DATA + 256 + index * 8).unwrap(), 79);
        assert_eq!(
            machine.cpu.read_u32(DATA + 260 + index * 8).unwrap(),
            LAST_ERROR
        );
    }
    assert_eq!(
        machine.cpu.read_u32(DATA + 512).unwrap(),
        machine.cpu.read_u32(DATA + 516).unwrap()
    );
    assert_eq!(
        machine.cpu.read_u32(DATA + 520).unwrap(),
        machine.cpu.read_u32(DATA + 524).unwrap()
    );
    assert!(
        machine
            .ext
            .get_or_default::<adv_abi::ApiDiagnostics>()
            .warnings
            .is_empty()
    );
}

fn reject<B: Backend>(cpu: B, name: &str, value: i32, cp: u32, restore_c: bool, reason: &str) {
    let mut machine = fixture(cpu);
    let configure = machine.resolve_import("fixture.dll", "profile").unwrap();
    let set = machine.resolve_import("msvcr90.dll", "setlocale").unwrap();
    let errno = machine.resolve_import("msvcr90.dll", "_errno").unwrap();
    let set_error = machine
        .resolve_import("kernel32.dll", "SetLastError")
        .unwrap();
    let entry = machine.resolve_import("msvcr90.dll", name).unwrap();
    machine
        .cpu
        .write(DATA + 1024, b"English_United States.1252\0")
        .unwrap();
    machine.cpu.write(DATA + 1088, b"C\0").unwrap();
    let mut code = Asm::new(CODE);
    if cp == 1252 && value == -1 {
        code.push_imm(DATA + 1024).push_imm(2).call(set).add_esp(8);
        if restore_c {
            code.push_imm(DATA + 1088).push_imm(2).call(set).add_esp(8);
        }
    } else {
        code.push_imm(cp).push_imm(2).call(configure).add_esp(8);
    }
    code.call(errno)
        .mov_mem_eax(DATA + 128)
        .bytes
        .extend_from_slice(&[0xc7, 0x00]);
    code.bytes.extend_from_slice(&79u32.to_le_bytes());
    code.push_imm(LAST_ERROR).call(set_error);
    code.push_imm(value as u32)
        .call(entry)
        .add_esp(4)
        .mov_mem_imm(DATA, 0xbad)
        .ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine
        .spawn("unsupported CRT byte profile", CODE, &[], 65536)
        .unwrap();
    let result = machine.run_virtual(1_000_000);
    assert!(
        matches!(result,RunResult::Failed(ref error) if error.message.contains(reason)),
        "{result:?}"
    );
    assert_eq!(machine.cpu.read_u32(DATA).unwrap(), 0);
    assert_eq!(
        machine
            .cpu
            .read_u32(machine.cpu.read_u32(DATA + 128).unwrap())
            .unwrap(),
        79
    );
    let thunk = (entry - adv_machine::layout::THUNK_BASE) / adv_machine::layout::THUNK_STRIDE;
    let environment = Env {
        context: machine.sys.current,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk,
    };
    assert_eq!(crate::util::last_error(&environment), LAST_ERROR);
    if restore_c {
        assert!(machine.ext.get::<CrtState>().locales["msvcr90.dll"].ever_non_c);
        assert_eq!(
            machine.ext.get::<CrtState>().locales["msvcr90.dll"].categories[1].codepage,
            0
        );
    }
    let diagnostics = machine.ext.get::<adv_abi::ApiDiagnostics>();
    assert_eq!(diagnostics.stub_calls, 0);
    assert_eq!(diagnostics.warnings.len(), 1);
    let ((api, _), &count) = diagnostics.warnings.iter().next().unwrap();
    assert_eq!(api, &format!("msvcr90.dll!{name}"));
    assert_eq!(count, 1);
}
fn guest<B: Backend>(mut cpu: impl FnMut() -> B) {
    success(cpu());
    for (name, value, cp, restore_c, reason) in [
        ("isalpha", 0x82a0, 932, false, "packed DBCS"),
        ("tolower", -2, 0, false, "other integers"),
        ("isprint", 0x53, 65001, false, "LC_CTYPE"),
        ("tolower", -1, 1252, false, "EOF case"),
        ("toupper", -1, 1252, true, "EOF case"),
    ] {
        reject(cpu(), name, value, cp, restore_c, reason);
    }
}
#[test]
fn byte_fronts_use_vc80_tables_case_locale_abi_and_bounded_profile() {
    guest(|| JitCpu::new(PARK_ADDR, false).unwrap());
    guest(|| JitCpu::new(PARK_ADDR, true).unwrap());
    guest(|| UnicornCpu::new(PARK_ADDR).unwrap());
}

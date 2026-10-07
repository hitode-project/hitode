//! Authored VC90 oracle: target/engine-loop/20261007-resume/crt-directory-native/native.jsonl.
//! Probe SHA256 B2F1C83EC5362145C183AB9154269D3918F8F148A42EDB1A239351B9BCE7175C.
use super::*;
use adv_cpu::{
    JitCpu,
    backend::{Backend, Guest, asm::Asm},
};
use adv_machine::{
    Clock, Machine, RunResult,
    layout::{PARK_ADDR, THUNK_BASE, THUNK_STRIDE},
};
use adv_unicorn::UnicornCpu;
use std::path::PathBuf;

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const PATH: u32 = DATA + 0x1000;

fn fixture<B: Backend>(cpu: B, name: &str) -> (Machine<B>, PathBuf) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/crt-directory-tests")
        .join(format!(
            "{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    std::fs::create_dir_all(root.join("game/existing")).unwrap();
    std::fs::write(root.join("game/original.dat"), b"original").unwrap();
    let mut registry = Registry::default();
    crate::register(&mut registry);
    super::super::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    for module in MODULES {
        for name in ["_mkdir", "_wmkdir"] {
            assert_eq!(
                registry.get(module, name).unwrap().conv,
                Conv::Cdecl { argc: 1 }
            );
            let signature = adv_abi::signature::lookup(module, name).unwrap();
            assert_eq!(signature.conv, Some(Conv::Cdecl { argc: 1 }));
            assert_eq!(signature.parameters.len(), 1);
            assert!(registry.dynamic_available(module, name));
        }
    }
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    let mut config = crate::Win32Config::new(root.join("game"), "authored.exe", root.join("data"));
    config.os.codepage = 936;
    crate::Win32::install(&mut machine, config).unwrap();
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 8192, Prot::RW).unwrap();
    (machine, root)
}
fn wide<B: Backend>(machine: &mut Machine<B>, pointer: u32, path: &str) {
    let bytes: Vec<_> = path
        .encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect();
    machine.cpu.write(pointer, &bytes).unwrap();
}
fn ansi<B: Backend>(machine: &mut Machine<B>, pointer: u32, path: &[u8]) {
    machine.cpu.write(pointer, &[path, &[0]].concat()).unwrap();
}
fn push_call(code: &mut Asm, api: u32, args: &[u32]) {
    for &arg in args.iter().rev() {
        code.push_imm(arg);
    }
    code.call(api).add_esp((args.len() * 4) as u8);
}
fn observed(code: &mut Asm, api: u32, path: u32, errno: u32, get_error: u32, record: u32) {
    push_call(code, api, &[path]);
    code.mov_mem_eax(record);
    code.call(errno).mov_eax_ptr_eax().mov_mem_eax(record + 4);
    code.call(get_error).mov_mem_eax(record + 8);
}
fn seed(code: &mut Asm, errno: u32, set_error: u32) {
    code.call(errno);
    code.bytes.extend_from_slice(&[0xc7, 0x00]);
    code.bytes.extend_from_slice(&79u32.to_le_bytes());
    code.push_imm(0x12345678).call(set_error);
}
fn actual<B: Backend>(cpu: B, name: &str) {
    let (mut machine, root) = fixture(cpu, name);
    let original = std::fs::metadata(root.join("game/original.dat")).unwrap();
    let parent_modified = std::fs::metadata(root.join("game/existing"))
        .unwrap()
        .modified()
        .unwrap();
    ansi(&mut machine, PATH, b"Japanese_Japan.932");
    let setter = machine.resolve_import("msvcr90.dll", "setlocale").unwrap();
    let set_error = machine
        .resolve_import("kernel32.dll", "SetLastError")
        .unwrap();
    let get_error = machine
        .resolve_import("kernel32.dll", "GetLastError")
        .unwrap();
    let mut code = Asm::new(CODE);
    push_call(&mut code, setter, &[2, PATH]);
    code.bytes.extend_from_slice(&[0x89, 0xe0]);
    code.mov_mem_eax(DATA + 0x300);
    let mut golden = Vec::new();
    let mut paths = Vec::new();
    for (index, module) in MODULES.into_iter().enumerate() {
        let a = PATH + 0x100 + index as u32 * 0x100;
        let w = a + 0x80;
        let mut ansi_path = b"C:\\Game\\existing\\".to_vec();
        ansi_path.extend_from_slice(&[0xc8, 0xd5, 0xb1, 0xbe]);
        ansi_path.extend_from_slice(index.to_string().as_bytes()); // 日本, GBK/Win32 ACP936.
        ansi(&mut machine, a, &ansi_path);
        wide(
            &mut machine,
            w,
            &format!("C:\\Game\\existing\\wide_日本{index}"),
        );
        let errno = machine.resolve_import(module, "_errno").unwrap();
        let mkdir = machine.resolve_import(module, "_mkdir").unwrap();
        let wmkdir = machine.resolve_import(module, "_wmkdir").unwrap();
        for (api, path, result, err, last_error) in [
            (mkdir, a, 0, 79, 0x12345678),
            (wmkdir, w, 0, 79, 0x12345678),
            (wmkdir, w, u32::MAX, 17, 183),
        ] {
            seed(&mut code, errno, set_error);
            let record = DATA + golden.len() as u32 * 12;
            observed(&mut code, api, path, errno, get_error, record);
            golden.push((result, err, last_error));
        }
        paths.push((format!("日本{index}"), format!("wide_日本{index}")));
    }
    let errno = machine.resolve_import("msvcr90.dll", "_errno").unwrap();
    let wmkdir = machine.resolve_import("msvcr90.dll", "_wmkdir").unwrap();
    for (index, path) in ["C:\\Game\\original.dat", "C:\\Game\\absent\\leaf", ""]
        .into_iter()
        .enumerate()
    {
        let pointer = PATH + 0x500 + index as u32 * 0x100;
        wide(&mut machine, pointer, path);
        seed(&mut code, errno, set_error);
        let record = DATA + golden.len() as u32 * 12;
        observed(&mut code, wmkdir, pointer, errno, get_error, record);
        golden.push((
            u32::MAX,
            if index == 0 { 17 } else { 2 },
            if index == 0 { 183 } else { 3 },
        ));
    }
    seed(&mut code, errno, set_error);
    let record = DATA + golden.len() as u32 * 12;
    observed(&mut code, wmkdir, 0, errno, get_error, record);
    golden.push((u32::MAX, 2, 3));
    code.bytes.extend_from_slice(&[0x89, 0xe0]);
    code.mov_mem_eax(DATA + 0x304);
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine
        .spawn("six legacy cdecl directory fronts", CODE, &[], 65536)
        .unwrap();
    let run = machine.run_virtual(1_000_000);
    assert!(matches!(run, RunResult::Exited(0)), "{run:?}");
    assert_eq!(
        machine.cpu.read_u32(DATA + 0x300).unwrap(),
        machine.cpu.read_u32(DATA + 0x304).unwrap()
    );
    for (index, (result, errno, last_error)) in golden.into_iter().enumerate() {
        let record = DATA + index as u32 * 12;
        assert_eq!(machine.cpu.read_u32(record).unwrap(), result);
        assert_eq!(machine.cpu.read_u32(record + 4).unwrap(), errno);
        assert_eq!(machine.cpu.read_u32(record + 8).unwrap(), last_error);
    }
    for (a, w) in paths {
        for path in [a, w] {
            assert!(root.join("data/overlay/existing").join(&path).is_dir());
            assert!(!root.join("game/existing").join(path).exists());
        }
    }
    assert!(!root.join("game/absent").exists());
    assert!(!root.join("data/overlay/absent").exists());
    assert_eq!(
        std::fs::read(root.join("game/original.dat")).unwrap(),
        b"original"
    );
    assert_eq!(
        std::fs::metadata(root.join("game/original.dat"))
            .unwrap()
            .modified()
            .unwrap(),
        original.modified().unwrap()
    );
    assert_eq!(
        std::fs::metadata(root.join("game/existing"))
            .unwrap()
            .modified()
            .unwrap(),
        parent_modified
    );
}
#[test]
fn six_real_cdecl_aw_fronts_match_native_errno_and_create_only_the_requested_overlay_leaf() {
    actual(UnicornCpu::new(PARK_ADDR).unwrap(), "unicorn");
    actual(JitCpu::new(PARK_ADDR, false).unwrap(), "jit");
}

fn env(machine: &mut Machine<JitCpu>, entry: u32) -> Env<'_> {
    Env {
        context: machine.sys.current,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: (entry - THUNK_BASE) / THUNK_STRIDE,
    }
}
fn invoke(machine: &mut Machine<JitCpu>, module: &str, name: &str, pointer: u32) -> Outcome {
    let entry = machine.resolve_import(module, name).unwrap();
    machine.cpu.write_u32(DATA + 4, pointer).unwrap();
    let handler = (machine.sys.thunks.resolver() as &dyn std::any::Any)
        .downcast_ref::<Registry>()
        .unwrap()
        .get(module, name)
        .unwrap()
        .handler;
    handler(&mut env(machine, entry))
}
fn returned(outcome: Outcome) -> u32 {
    match outcome {
        Outcome::Return(value) => value,
        _ => panic!("unexpected outcome"),
    }
}
fn errno(machine: &mut Machine<JitCpu>, module: &str) -> u32 {
    let cell = returned(invoke(machine, module, "_errno", 0));
    machine.cpu.read_u32(cell).unwrap()
}
#[test]
fn checked_paths_and_unsupported_encoding_fail_without_filesystem_or_other_crt_state_changes() {
    let (mut machine, root) = fixture(JitCpu::new(PARK_ADDR, false).unwrap(), "guarded");
    machine
        .spawn("directory guard", PARK_ADDR, &[], 65536)
        .unwrap();
    machine.cpu.set_reg(Reg::Esp, DATA);
    let other = returned(invoke(&mut machine, "msvcr80.dll", "_errno", 0));
    machine.cpu.write_u32(other, 79).unwrap();
    let current = returned(invoke(&mut machine, "msvcr90.dll", "_errno", 0));
    machine.cpu.write_u32(current, 79).unwrap();
    assert!(matches!(
        invoke(&mut machine, "msvcr90.dll", "_wmkdir", 0xdead0000),
        Outcome::Fail(_)
    ));
    assert_eq!(errno(&mut machine, "msvcr90.dll"), 79);
    machine.cpu.map(0x450000, 4096, Prot::RW).unwrap();
    machine.cpu.write(0x450ffe, &[b'a', 0]).unwrap();
    assert!(matches!(
        invoke(&mut machine, "msvcr90.dll", "_wmkdir", 0x450ffe),
        Outcome::Fail(_)
    ));
    assert_eq!(errno(&mut machine, "msvcr90.dll"), 79);
    machine.cpu.write(PATH, &[0, 0xd8, 0, 0]).unwrap();
    assert_eq!(
        returned(invoke(&mut machine, "msvcr90.dll", "_wmkdir", PATH)),
        u32::MAX
    );
    assert_eq!(errno(&mut machine, "msvcr90.dll"), 42);
    machine.ext.get_mut::<crate::Win32>().codec = adv_os::Codec::new(437);
    ansi(&mut machine, PATH, b"C:\\Game\\not-created");
    assert_eq!(
        returned(invoke(&mut machine, "msvcr90.dll", "_mkdir", PATH)),
        u32::MAX
    );
    assert_eq!(errno(&mut machine, "msvcr90.dll"), 40);
    // W is independent of ACP and reads a caller's read-only UTF16 input.
    wide(&mut machine, 0x450000, "C:\\Game\\existing\\guarded");
    machine.cpu.protect(0x450000, 4096, Prot::READ).unwrap();
    assert_eq!(
        returned(invoke(&mut machine, "msvcr90.dll", "_wmkdir", 0x450000)),
        0
    );
    assert_eq!(errno(&mut machine, "msvcr90.dll"), 40);
    assert_eq!(errno(&mut machine, "msvcr80.dll"), 79);
    assert!(root.join("data/overlay/existing/guarded").is_dir());
    assert!(!root.join("game/existing/guarded").exists());
    assert!(!root.join("data/overlay/not-created").exists());
    let warnings = &machine.ext.get::<adv_abi::ApiDiagnostics>().warnings;
    assert!(
        warnings
            .keys()
            .any(|(api, reason)| api == "msvcr90.dll!_mkdir" && reason.contains("ACP"))
    );
    assert!(
        warnings
            .keys()
            .any(|(api, reason)| api == "msvcr90.dll!_wmkdir" && reason.contains("UTF16"))
    );
    assert_eq!(
        std::fs::read(root.join("game/original.dat")).unwrap(),
        b"original"
    );
}

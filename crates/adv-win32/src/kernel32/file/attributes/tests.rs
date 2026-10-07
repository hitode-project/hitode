//! Authored native NORMAL oracle: target/engine-loop/20261007-resume/attributes-native/native.jsonl.
//! Probe source SHA256 0E4B295777AB73676A5FA207B63769ABCBC03BEF3C2055A8FFF457F299043D38.
use super::*;
use adv_cpu::{
    JitCpu,
    backend::{Backend, Prot, Reg, asm::Asm},
};
use adv_machine::{Clock, Conv, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
use std::path::PathBuf;

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const OUT: u32 = 0x420000;

fn fixture<B: Backend>(cpu: B, name: &str) -> (Machine<B>, PathBuf) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/file-attribute-tests")
        .join(format!(
            "{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    std::fs::create_dir_all(root.join("game")).unwrap();
    let mut registry = Registry::default();
    crate::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    for (name, argc) in [
        ("GetFileAttributesA", 1),
        ("GetFileAttributesW", 1),
        ("SetFileAttributesA", 2),
        ("SetFileAttributesW", 2),
    ] {
        assert_eq!(
            registry.get(K32, name).unwrap().conv,
            Conv::Stdcall { argc }
        );
    }
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    Win32::install(
        &mut machine,
        crate::Win32Config::new(root.join("game"), "authored.exe", root.join("data")),
    )
    .unwrap();
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    machine.cpu.map(OUT, 8192, Prot::RW).unwrap();
    (machine, root)
}
fn env<B: Backend>(machine: &mut Machine<B>) -> Env<'_> {
    Env {
        context: machine.sys.current,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: 0,
    }
}
fn ansi<B: Backend>(machine: &mut Machine<B>, pointer: u32, bytes: &[u8]) {
    machine.cpu.write(pointer, &[bytes, &[0]].concat()).unwrap();
}
fn wide<B: Backend>(machine: &mut Machine<B>, pointer: u32, text: &str) {
    let bytes: Vec<_> = text
        .encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect();
    machine.cpu.write(pointer, &bytes).unwrap();
}
fn invoke<B: Backend>(
    machine: &mut Machine<B>,
    handler: adv_machine::Handler,
    args: &[u32],
) -> u32 {
    for (index, value) in args.iter().enumerate() {
        machine
            .cpu
            .write_u32(DATA + 4 + index as u32 * 4, *value)
            .unwrap();
    }
    let before = machine.sys.objects.object_count();
    let outcome = handler(&mut env(machine));
    assert_eq!(
        machine.sys.objects.object_count(),
        before,
        "attribute views do not create guest handles"
    );
    match outcome {
        Outcome::Return(value) => value,
        _ => panic!("synchronous attribute query"),
    }
}
fn error<B: Backend>(machine: &mut Machine<B>) -> u32 {
    crate::util::last_error(&env(machine))
}
fn call(code: &mut Asm, api: u32, args: &[u32], output: u32, get_error: u32) {
    for &arg in args.iter().rev() {
        code.push_imm(arg);
    }
    code.call(api).mov_mem_eax(output);
    code.call(get_error).mov_mem_eax(output + 4);
}
fn actual<B: Backend>(cpu: B, name: &str) {
    let (mut machine, root) = fixture(cpu, name);
    let file = root.join("game/日本.dat");
    std::fs::write(&file, b"read-only original layer").unwrap();
    let mut permissions = std::fs::metadata(&file).unwrap().permissions();
    permissions.set_readonly(true); // Only this authored target fixture.
    std::fs::set_permissions(&file, permissions).unwrap();
    std::fs::create_dir(root.join("game/folder")).unwrap();
    let original_metadata = std::fs::metadata(&file).unwrap();
    #[cfg(windows)]
    let original_host_attributes = {
        use std::os::windows::fs::MetadataExt;
        original_metadata.file_attributes()
    };
    wide(&mut machine, DATA + 0x400, ".\\日本.dat");
    ansi(
        &mut machine,
        DATA + 0x500,
        &[b'.', b'\\', 0x93, 0xfa, 0x96, 0x7b, b'.', b'd', b'a', b't'],
    );
    ansi(&mut machine, DATA + 0x600, b".\\folder");
    wide(&mut machine, DATA + 0x700, ".\\folder");
    let get_a = machine.resolve_import(K32, "GetFileAttributesA").unwrap();
    let get_w = machine.resolve_import(K32, "GetFileAttributesW").unwrap();
    let set_a = machine.resolve_import(K32, "SetFileAttributesA").unwrap();
    let set_w = machine.resolve_import(K32, "SetFileAttributesW").unwrap();
    let find_a = machine.resolve_import(K32, "FindFirstFileA").unwrap();
    let find_w = machine.resolve_import(K32, "FindFirstFileW").unwrap();
    let close = machine.resolve_import(K32, "CloseHandle").unwrap();
    let get_error = machine.resolve_import(K32, "GetLastError").unwrap();
    let set_error = machine.resolve_import(K32, "SetLastError").unwrap();
    let exit = machine.resolve_import(K32, "ExitProcess").unwrap();
    let mut code = Asm::new(CODE);
    code.push_imm(0x12345678).call(set_error);
    code.bytes.extend_from_slice(&[0x89, 0xe0]);
    code.mov_mem_eax(DATA + 0x30);
    call(&mut code, get_w, &[DATA + 0x400], DATA + 0x40, get_error);
    call(
        &mut code,
        set_w,
        &[DATA + 0x400, 128],
        DATA + 0x48,
        get_error,
    );
    call(&mut code, get_a, &[DATA + 0x500], DATA + 0x50, get_error);
    call(
        &mut code,
        set_a,
        &[DATA + 0x500, 128],
        DATA + 0x58,
        get_error,
    );
    call(&mut code, get_w, &[DATA + 0x400], DATA + 0x60, get_error);
    call(
        &mut code,
        set_a,
        &[DATA + 0x600, 128],
        DATA + 0x68,
        get_error,
    );
    call(&mut code, get_w, &[DATA + 0x700], DATA + 0x70, get_error);
    code.push_imm(OUT)
        .push_imm(DATA + 0x500)
        .call(find_a)
        .mov_mem_eax(DATA + 0x78);
    code.bytes.push(0x50); // push eax, close this owned find handle.
    code.call(close);
    code.push_imm(OUT + 4096)
        .push_imm(DATA + 0x400)
        .call(find_w)
        .mov_mem_eax(DATA + 0x7c);
    code.bytes.push(0x50);
    code.call(close);
    code.bytes.extend_from_slice(&[0x89, 0xe0]);
    code.mov_mem_eax(DATA + 0x34);
    code.push_imm(0).call(exit);
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine
        .spawn("attributes stdcall", CODE, &[], 65536)
        .unwrap();
    let run = machine.run_virtual(1_000_000);
    assert!(matches!(run, RunResult::Exited(0)), "{run:?}");
    assert_eq!(
        machine.cpu.read_u32(DATA + 0x30).unwrap(),
        machine.cpu.read_u32(DATA + 0x34).unwrap()
    );
    for (index, expected) in [32, 1, 128, 1, 128, 1, 16].into_iter().enumerate() {
        assert_eq!(
            machine
                .cpu
                .read_u32(DATA + 0x40 + index as u32 * 8)
                .unwrap(),
            expected
        );
        assert_eq!(
            machine
                .cpu
                .read_u32(DATA + 0x44 + index as u32 * 8)
                .unwrap(),
            0x12345678
        );
    }
    assert_ne!(machine.cpu.read_u32(DATA + 0x78).unwrap(), u32::MAX);
    assert_ne!(machine.cpu.read_u32(DATA + 0x7c).unwrap(), u32::MAX);
    assert_eq!(machine.cpu.read_u32(OUT).unwrap(), 128);
    assert_eq!(machine.cpu.read_u32(OUT + 4096).unwrap(), 128);
    assert_eq!(
        machine
            .ext
            .get::<Win32>()
            .vfs
            .list("C:\\Game")
            .unwrap()
            .into_iter()
            .find(|e| e.name == "日本.dat")
            .unwrap()
            .attributes,
        128
    );
    assert_eq!(std::fs::read(&file).unwrap(), b"read-only original layer");
    assert_eq!(
        std::fs::metadata(&file).unwrap().permissions().readonly(),
        original_metadata.permissions().readonly()
    );
    assert_eq!(
        std::fs::metadata(&file).unwrap().modified().unwrap(),
        original_metadata.modified().unwrap()
    );
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        assert_eq!(
            std::fs::metadata(&file).unwrap().file_attributes(),
            original_host_attributes
        );
    }
    assert!(!root.join("data/overlay/日本.dat").exists());
}
#[test]
fn actual_x86_normal_aw_and_find_data_share_guest_metadata_without_mutating_original_layers() {
    actual(UnicornCpu::new(PARK_ADDR).unwrap(), "unicorn");
    actual(JitCpu::new(PARK_ADDR, false).unwrap(), "jit");
}

#[test]
fn backing_instance_replacement_rename_and_overlay_switch_do_not_reuse_stale_normal_state() {
    let (mut machine, root) = fixture(JitCpu::new(PARK_ADDR, false).unwrap(), "lifecycle");
    machine.spawn("attributes", PARK_ADDR, &[], 65536).unwrap();
    machine.cpu.set_reg(Reg::Esp, DATA);
    let owned = machine
        .ext
        .get::<Win32>()
        .vfs
        .write_path("life.dat")
        .unwrap();
    std::fs::write(&owned, b"old").unwrap();
    ansi(&mut machine, DATA + 0x400, b".\\life.dat");
    assert_eq!(
        invoke(&mut machine, set_a_ABI.handler, &[DATA + 0x400, 128]),
        1
    );
    assert_eq!(
        invoke(&mut machine, get_a_ABI.handler, &[DATA + 0x400]),
        128
    );
    // Holding the old real backing instance prevents inode/file-index reuse.
    let old = std::fs::File::open(&owned).unwrap();
    let old_identity = adv_os::vfs::file_identity(&old).unwrap();
    let delete = (machine.sys.thunks.resolver() as &dyn std::any::Any)
        .downcast_ref::<Registry>()
        .unwrap()
        .get(K32, "DeleteFileA")
        .unwrap()
        .handler;
    assert_eq!(invoke(&mut machine, delete, &[DATA + 0x400]), 1);
    assert_eq!(
        invoke(&mut machine, get_a_ABI.handler, &[DATA + 0x400]),
        u32::MAX
    );
    assert_eq!(error(&mut machine), 2);
    std::fs::write(&owned, b"new").unwrap();
    let new = std::fs::File::open(&owned).unwrap();
    assert_ne!(old_identity, adv_os::vfs::file_identity(&new).unwrap());
    assert_eq!(invoke(&mut machine, get_a_ABI.handler, &[DATA + 0x400]), 32);
    assert_eq!(
        invoke(&mut machine, set_a_ABI.handler, &[DATA + 0x400, 128]),
        1
    );
    let renamed = owned.with_file_name("renamed.dat");
    std::fs::rename(&owned, &renamed).unwrap();
    ansi(&mut machine, DATA + 0x400, b".\\renamed.dat");
    assert_eq!(
        invoke(&mut machine, get_a_ABI.handler, &[DATA + 0x400]),
        128
    );
    std::fs::write(&renamed, b"subsequent backing write").unwrap();
    assert_eq!(invoke(&mut machine, get_a_ABI.handler, &[DATA + 0x400]), 32);
    std::fs::write(root.join("game/switch.dat"), b"base").unwrap();
    ansi(&mut machine, DATA + 0x400, b".\\switch.dat");
    assert_eq!(
        invoke(&mut machine, set_a_ABI.handler, &[DATA + 0x400, 128]),
        1
    );
    let overlay = machine
        .ext
        .get::<Win32>()
        .vfs
        .write_path("switch.dat")
        .unwrap();
    std::fs::write(overlay, b"overlay").unwrap();
    assert_eq!(invoke(&mut machine, get_a_ABI.handler, &[DATA + 0x400]), 32);
    assert_eq!(
        std::fs::read(root.join("game/switch.dat")).unwrap(),
        b"base"
    );
}

#[test]
fn unsupported_flags_codepages_and_bad_paths_fail_without_publishing_attribute_changes() {
    let (mut machine, root) = fixture(JitCpu::new(PARK_ADDR, false).unwrap(), "failures");
    machine.spawn("attributes", PARK_ADDR, &[], 65536).unwrap();
    machine.cpu.set_reg(Reg::Esp, DATA);
    std::fs::write(root.join("game/state.dat"), b"original").unwrap();
    ansi(&mut machine, DATA + 0x400, b".\\state.dat");
    wide(&mut machine, DATA + 0x500, ".\\state.dat");
    assert_eq!(
        invoke(&mut machine, set_a_ABI.handler, &[DATA + 0x400, 128]),
        1
    );
    for flags in [0, 1, 2, 32, 160, 0x80000000] {
        assert_eq!(
            invoke(&mut machine, set_w_ABI.handler, &[DATA + 0x500, flags]),
            0
        );
        assert_eq!(error(&mut machine), 50);
        assert_eq!(
            invoke(&mut machine, get_a_ABI.handler, &[DATA + 0x400]),
            128
        );
    }
    for (path, expected) in [
        (".\\missing.dat", 2),
        (".\\absent\\missing.dat", 3),
        ("", 123),
    ] {
        wide(&mut machine, DATA + 0x600, path);
        assert_eq!(
            invoke(&mut machine, set_w_ABI.handler, &[DATA + 0x600, 128]),
            0
        );
        assert_eq!(error(&mut machine), expected);
        assert_eq!(
            invoke(&mut machine, get_w_ABI.handler, &[DATA + 0x600]),
            u32::MAX
        );
        assert_eq!(error(&mut machine), expected);
    }
    for (path, expected) in [(0, 87), (0xdead0000, 998)] {
        assert_eq!(invoke(&mut machine, set_w_ABI.handler, &[path, 128]), 0);
        assert_eq!(error(&mut machine), expected);
        assert_eq!(invoke(&mut machine, get_a_ABI.handler, &[path]), u32::MAX);
        assert_eq!(error(&mut machine), expected);
    }
    machine.cpu.write(DATA + 0x600, &[0, 0xd8, 0, 0]).unwrap();
    assert_eq!(
        invoke(&mut machine, set_w_ABI.handler, &[DATA + 0x600, 128]),
        0
    );
    assert_eq!(error(&mut machine), 1113);
    machine.ext.get_mut::<Win32>().codec = adv_os::Codec::new(437);
    assert_eq!(
        invoke(&mut machine, set_a_ABI.handler, &[DATA + 0x400, 128]),
        0
    );
    assert_eq!(error(&mut machine), 50);
    assert_eq!(
        invoke(&mut machine, get_a_ABI.handler, &[DATA + 0x400]),
        u32::MAX
    );
    assert_eq!(error(&mut machine), 50);
    assert_eq!(
        invoke(&mut machine, get_w_ABI.handler, &[DATA + 0x500]),
        128
    );
    let warnings = &machine.ext.get::<adv_abi::ApiDiagnostics>().warnings;
    assert!(
        warnings
            .keys()
            .any(|(api, reason)| api == "kernel32.dll!SetFileAttributesW"
                && reason.contains("0x000000a0"))
    );
    assert!(warnings.keys().any(|(api,reason)| api == "kernel32.dll!SetFileAttributesA" && reason.contains("ACP")));
    assert_eq!(
        std::fs::read(root.join("game/state.dat")).unwrap(),
        b"original"
    );
    assert!(!root.join("data/overlay/state.dat").exists());
}

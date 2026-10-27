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
    // Retain the old instance to exercise native pending-delete lifetime.
    let old = std::fs::File::open(&owned).unwrap();
    let old_identity = adv_os::vfs::file_identity(&old).unwrap();
    #[cfg(windows)]
    {
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
        // POSIX disposition unlinks the name although the retained FD lives on.
        assert_eq!(error(&mut machine), 2);
        drop(old);
    }
    #[cfg(not(windows))]
    adv_os::vfs::host::remove_file(&owned).unwrap(); // Attribute lifetime, independent of host NT support.
    assert_eq!(
        invoke(&mut machine, get_a_ABI.handler, &[DATA + 0x400]),
        u32::MAX
    );
    assert_eq!(error(&mut machine), 2);
    adv_os::vfs::host::write(&owned, b"new").unwrap();
    let new = std::fs::File::open(&owned).unwrap();
    assert_ne!(old_identity, adv_os::vfs::file_identity(&new).unwrap());
    assert_eq!(invoke(&mut machine, get_a_ABI.handler, &[DATA + 0x400]), 32);
    assert_eq!(
        invoke(&mut machine, set_a_ABI.handler, &[DATA + 0x400, 128]),
        1
    );
    let renamed = owned.with_file_name("renamed.dat");
    adv_os::vfs::host::rename(&owned, &renamed).unwrap();
    ansi(&mut machine, DATA + 0x400, b".\\renamed.dat");
    assert_eq!(
        invoke(&mut machine, get_a_ABI.handler, &[DATA + 0x400]),
        128
    );
    std::fs::write(&renamed, b"subsequent backing write").unwrap();
    assert_eq!(invoke(&mut machine, get_a_ABI.handler, &[DATA + 0x400]), 32);
    adv_os::vfs::host::write(&root.join("game/switch.dat"), b"base").unwrap();
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
    adv_os::vfs::host::write(&overlay, b"overlay").unwrap();
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

struct AttributeProvider {
    full: bool,
    paths: Vec<String>,
    allocations: Vec<u32>,
    revoked_page: u32,
}
fn attribute_hook(env: &mut Env<'_>) -> Outcome {
    let (object, output) = (env.arg(0), env.arg(1));
    assert_eq!(env.cpu.read_u32(object).unwrap(), 24);
    assert_eq!(env.cpu.read_u32(object + 4).unwrap(), 0);
    assert_eq!(env.cpu.read_u32(object + 12).unwrap(), 0x40);
    assert_eq!(env.cpu.read_u32(object + 16).unwrap(), 0);
    assert_eq!(env.cpu.read_u32(object + 20).unwrap(), 0);
    let name = env.cpu.read_u32(object + 8).unwrap();
    let length = env.cpu.read_u16(name).unwrap();
    assert_eq!(env.cpu.read_u16(name + 2).unwrap(), length + 2);
    let pointer = env.cpu.read_u32(name + 4).unwrap();
    let units: Vec<_> = (0..u32::from(length) / 2)
        .map(|index| env.cpu.read_u16(pointer + index * 2).unwrap())
        .collect();
    assert_eq!(env.cpu.read_u16(pointer + u32::from(length)).unwrap(), 0);
    let path = String::from_utf16(&units).unwrap();
    assert!(path.starts_with(r"\??\C:\Game\"));
    assert_eq!(output % 8, 0);
    assert!(
        env.ext
            .get::<adv_os::Heaps>()
            .owner(env.cpu, object)
            .is_some()
    );
    let provider = env.ext.get_mut::<AttributeProvider>();
    provider.paths.push(path);
    provider.allocations.push(object);
    let call = provider.paths.len();
    let offset = if provider.full { 48 } else { 32 };
    env.cpu.write_u32(output + offset, 0x21).unwrap();
    if call == 4 {
        let page = output & !4095;
        env.cpu.protect(page, 4096, Prot::WRITE).unwrap();
        env.ext.get_mut::<AttributeProvider>().revoked_page = page;
    }
    Outcome::Return(if call == 3 { 0xc0000034 } else { 0 })
}
fn install_attribute_hook(env: &mut Env<'_>) -> Outcome {
    let names: &[&str] = if env.ext.get::<AttributeProvider>().full {
        &["NtQueryFullAttributesFile"]
    } else {
        &["NtQueryAttributesFile", "NtQueryFullAttributesFile"]
    };
    for &name in names {
        let entry = super::super::native::entry(env, name).unwrap();
        let mut jump = vec![0xe9];
        jump.extend((CODE + 0x600).wrapping_sub(entry + 5).to_le_bytes());
        env.cpu.write(entry, &jump).unwrap();
    }
    Outcome::Return(0)
}
fn restore_attribute_page(env: &mut Env<'_>) -> Outcome {
    let page = env.ext.get::<AttributeProvider>().revoked_page;
    assert_ne!(page, 0);
    env.cpu.protect(page, 4096, Prot::RW).unwrap();
    Outcome::Return(0)
}

#[test]
fn a_w_attributes_use_only_live_attribute_hooks_and_release_native_scratch() {
    for full in [false, true] {
        for jit in [false, true] {
            let (mut machine, root) = fixture(
                JitCpu::new(PARK_ADDR, jit).unwrap(),
                "native-attribute-hook",
            );
            std::fs::write(root.join("game/ordinary.dat"), b"authored backing file").unwrap();
            let expected_fast = machine
                .ext
                .get::<Win32>()
                .vfs
                .file_attributes("ordinary.dat")
                .unwrap();
            machine.ext.insert(AttributeProvider {
                full,
                paths: Vec::new(),
                allocations: Vec::new(),
                revoked_page: 0,
            });
            let registry = machine.sys.thunks.resolver_mut::<Registry>().unwrap();
            for (name, argc, handler) in [
                ("query", 2, attribute_hook as adv_machine::Handler),
                ("install", 0, install_attribute_hook as adv_machine::Handler),
                ("restore", 0, restore_attribute_page as adv_machine::Handler),
            ] {
                registry.add("attribute-provider", name, Conv::Stdcall { argc }, handler);
            }
            {
                let (win32, modules) = machine.ext.get2_mut::<Win32, adv_os::Modules>();
                adv_os::Loader::new(&mut machine.cpu, &mut machine.sys, modules, &win32.vfs)
                    .load("ntdll.dll")
                    .unwrap();
            }
            ansi(&mut machine, DATA + 0x300, b".\\virtual.cfg");
            wide(&mut machine, DATA + 0x400, ".\\virtual-名.cfg");
            ansi(&mut machine, DATA + 0x500, b".\\ordinary.dat");
            assert!(
                machine
                    .ext
                    .get::<Win32>()
                    .vfs
                    .file_attributes("virtual.cfg")
                    .is_err()
            );
            let get_a = machine.resolve_import(K32, "GetFileAttributesA").unwrap();
            let get_w = machine.resolve_import(K32, "GetFileAttributesW").unwrap();
            let set_error = machine.resolve_import(K32, "SetLastError").unwrap();
            let get_error = machine.resolve_import(K32, "GetLastError").unwrap();
            let query = machine
                .resolve_import("attribute-provider", "query")
                .unwrap();
            let install = machine
                .resolve_import("attribute-provider", "install")
                .unwrap();
            let restore = machine
                .resolve_import("attribute-provider", "restore")
                .unwrap();
            let mut hook = Asm::new(CODE + 0x600);
            hook.jmp(query);
            machine.cpu.write(CODE + 0x600, &hook.bytes).unwrap();
            // A reader override alone must not reroute attribute queries.
            let read_entry =
                super::super::native::entry(&mut env(&mut machine), "NtReadFile").unwrap();
            let mut jump = vec![0xe9];
            jump.extend((CODE + 0x600).wrapping_sub(read_entry + 5).to_le_bytes());
            machine.cpu.write(read_entry, &jump).unwrap();
            let mut code = Asm::new(CODE);
            code.bytes.extend([0x89, 0xe0]); // mov eax, esp
            code.mov_mem_eax(OUT + 64);
            code.push_imm(0x12345678).call(set_error);
            call(&mut code, get_a, &[DATA + 0x500], OUT, get_error);
            code.call(install);
            code.push_imm(0x12345678).call(set_error);
            call(&mut code, get_a, &[DATA + 0x300], OUT + 8, get_error);
            call(&mut code, get_w, &[DATA + 0x400], OUT + 16, get_error);
            // A failed hook controls even a path that the backing VFS contains.
            call(&mut code, get_a, &[DATA + 0x500], OUT + 24, get_error);
            call(&mut code, get_w, &[DATA + 0x400], OUT + 32, get_error);
            code.call(restore);
            code.bytes.extend([0x89, 0xe0]);
            code.mov_mem_eax(OUT + 68);
            code.bytes.extend([0x31, 0xc0]);
            code.ret();
            machine.cpu.write(CODE, &code.bytes).unwrap();
            machine
                .spawn("authored attribute hook", CODE, &[], 65536)
                .unwrap();
            let objects = machine.sys.objects.object_count();
            let result = machine.run_virtual(1_000_000);
            assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
            for (offset, expected) in [
                (0, expected_fast),
                (4, 0x12345678),
                (8, 0x21),
                (12, 0x12345678),
                (16, 0x21),
                (20, 0x12345678),
                (24, u32::MAX),
                (28, 2),
                (32, u32::MAX),
                (36, 998),
            ] {
                assert_eq!(
                    machine.cpu.read_u32(OUT + offset).unwrap(),
                    expected,
                    "output {offset}"
                );
            }
            assert_eq!(
                machine.cpu.read_u32(OUT + 64).unwrap(),
                machine.cpu.read_u32(OUT + 68).unwrap()
            );
            assert_eq!(machine.sys.objects.object_count(), objects);
            let provider = machine.ext.get::<AttributeProvider>();
            assert_eq!(
                provider.paths,
                [
                    r"\??\C:\Game\virtual.cfg",
                    r"\??\C:\Game\virtual-名.cfg",
                    r"\??\C:\Game\ordinary.dat",
                    r"\??\C:\Game\virtual-名.cfg",
                ]
            );
            for allocation in &provider.allocations {
                assert!(
                    machine
                        .ext
                        .get::<adv_os::Heaps>()
                        .owner(&machine.cpu, *allocation)
                        .is_none(),
                    "native scratch is released on success, NT failure and bad output"
                );
            }
        }
    }
}

#[test]
fn wide_path_exists_uses_attributes_and_preserves_null_error() {
    let (mut machine, root) = fixture(JitCpu::new(PARK_ADDR, false).unwrap(), "path-exists");
    std::fs::write(root.join("game/存档.dat"), b"authored save").unwrap();
    std::fs::create_dir(root.join("game/folder")).unwrap();
    for (index, text) in ["存档.dat", "folder", "missing.dat", ""].iter().enumerate() {
        wide(&mut machine, DATA + 0x400 + index as u32 * 0x100, text);
    }
    let exists = machine
        .resolve_import("shlwapi.dll", "PathFileExistsW")
        .unwrap();
    let get_error = machine.resolve_import(K32, "GetLastError").unwrap();
    let set_error = machine.resolve_import(K32, "SetLastError").unwrap();
    let exit = machine.resolve_import(K32, "ExitProcess").unwrap();
    let mut code = Asm::new(CODE);
    for (index, pointer) in [DATA + 0x400, DATA + 0x500, DATA + 0x600, DATA + 0x700, 0]
        .into_iter()
        .enumerate()
    {
        code.push_imm(0x1357).call(set_error);
        call(
            &mut code,
            exists,
            &[pointer],
            OUT + index as u32 * 8,
            get_error,
        );
    }
    code.push_imm(0).call(exit);
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine.spawn("path-exists", CODE, &[], 65536).unwrap();
    assert!(matches!(
        machine.run_virtual(1_000_000),
        RunResult::Exited(0)
    ));
    for (index, (found, error)) in [(1, 0x1357), (1, 0x1357), (0, 2), (0, 3), (0, 0x1357)]
        .into_iter()
        .enumerate()
    {
        assert_eq!(machine.cpu.read_u32(OUT + index as u32 * 8).unwrap(), found);
        assert_eq!(
            machine.cpu.read_u32(OUT + index as u32 * 8 + 4).unwrap(),
            error
        );
    }
    drop(machine);
    std::fs::remove_dir_all(root).unwrap();
}

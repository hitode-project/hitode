use crate::{FileObj, Win32, Win32Config};
use adv_abi::{Conv, Registry};
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Env, Machine, Outcome, RunResult, layout::PARK_ADDR};
use adv_os::{Heaps, Loader, Modules};
use adv_pe::ImportName;
use std::path::PathBuf;

mod write;

const CODE: u32 = 0x0040_0000;
const DATA: u32 = 0x0041_0000;
const OUT: u32 = 0x0042_0000;
const UNMAPPED: u32 = 0x0043_0000;
const HANDLE: u32 = 0x123a_bc00;
const SIZE: u64 = 0x1_0000_0007;
const SENTINEL: u32 = 0x1234_5678;
const K32: &str = "kernel32.dll";
const PROVIDER: &str = "authored-file-provider";

#[derive(Default)]
struct Provider {
    hooks: Vec<(u32, u32)>,
    context: Option<u32>,
    object_count: usize,
    paths: Vec<String>,
    options: Vec<u32>,
    allocations: Vec<u32>,
    reads: Vec<(u64, u32)>,
    queries: Vec<u32>,
    positions: Vec<u64>,
    cursor: u64,
    closes: usize,
    opened: bool,
    failure: u32,
    size: Option<u64>,
}

fn observe(env: &Env<'_>) {
    let provider = env.ext.get::<Provider>();
    assert_eq!(Some(env.context), provider.context, "same guest thread");
    assert_eq!(env.sys.objects.object_count(), provider.object_count);
    assert!(env.sys.objects.get(HANDLE).is_none(), "opaque NT handle");
    assert!(env.sys.objects.all_hosts::<FileObj>().next().is_none());
}

fn install(env: &mut Env<'_>) -> Outcome {
    assert!(env.sys.objects.all_hosts::<FileObj>().next().is_none());
    let provider = env.ext.get_mut::<Provider>();
    provider.context = Some(env.context);
    provider.object_count = env.sys.objects.object_count();
    for &(entry, target) in &provider.hooks {
        let mut jump = Asm::new(entry);
        jump.jmp(target);
        env.cpu.write(entry, &jump.bytes).unwrap();
    }
    Outcome::Return(0)
}

fn fail(env: &mut Env<'_>) -> Outcome {
    env.ext.get_mut::<Provider>().failure = env.arg(0);
    Outcome::Return(0)
}

fn restore(env: &mut Env<'_>) -> Outcome {
    observe(env);
    env.cpu.protect(DATA, 4096, Prot::RW).unwrap();
    Outcome::Return(0)
}

fn open(env: &mut Env<'_>) -> Outcome {
    observe(env);
    let (output, access, object, iosb) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3));
    assert_ne!(access & 0x8000_0000, 0);
    assert_eq!(access & 0x4000_0000, 0);
    assert_eq!(env.arg(6), 5);
    assert_eq!(env.arg(7), 1, "FILE_OPEN");
    assert_ne!(env.arg(8) & 0x20, 0, "synchronous file");
    assert_eq!(env.arg(5), 0x80, "cache hints are not file attributes");
    let options = env.arg(8);
    assert_eq!(env.cpu.read_u32(object).unwrap(), 24);
    let name = env.cpu.read_u32(object + 8).unwrap();
    let length = env.cpu.read_u16(name).unwrap();
    let pointer = env.cpu.read_u32(name + 4).unwrap();
    let units: Vec<_> = (0..u32::from(length) / 2)
        .map(|index| env.cpu.read_u16(pointer + index * 2).unwrap())
        .collect();
    let provider = env.ext.get_mut::<Provider>();
    provider.paths.push(String::from_utf16(&units).unwrap());
    provider.options.push(options);
    provider.allocations.push(object);
    if provider.failure == 2 {
        return Outcome::Return(0xc000_0034);
    }
    assert!(!provider.opened);
    provider.opened = true;
    provider.cursor = 0;
    env.cpu.write_u32(output, HANDLE).unwrap();
    env.cpu.write_u32(iosb, 0).unwrap();
    env.cpu.write_u32(iosb + 4, 1).unwrap();
    Outcome::Return(0)
}

fn read_u64(cpu: &dyn Guest, pointer: u32) -> u64 {
    u64::from(cpu.read_u32(pointer).unwrap())
        | (u64::from(cpu.read_u32(pointer + 4).unwrap()) << 32)
}

fn read(env: &mut Env<'_>) -> Outcome {
    observe(env);
    assert_eq!(env.arg(0), HANDLE);
    let (iosb, buffer, capacity, offset) = (env.arg(4), env.arg(5), env.arg(6), env.arg(7));
    assert!(capacity <= 4, "open and size must not prefetch the stream");
    assert!(
        offset == 0 || read_u64(env.cpu, offset) == u64::MAX - 1,
        "use the provider's shared file pointer"
    );
    let provider = env.ext.get_mut::<Provider>();
    assert!(provider.opened);
    provider.reads.push((provider.cursor, capacity));
    env.cpu.write_u32(iosb + 4, 0).unwrap();
    if provider.failure == 1 {
        return Outcome::Return(0xc000_0022);
    }
    if provider.cursor >= SIZE {
        return Outcome::Return(0xc000_0011);
    }
    let count = u64::from(capacity).min(SIZE - provider.cursor) as u32;
    // A sparse authored stream needs no full-file allocation or backing asset.
    let bytes: Vec<_> = (provider.cursor..provider.cursor + u64::from(count))
        .map(|position| {
            if position < 7 {
                b"shadow!"[position as usize]
            } else if position >= SIZE - 4 {
                b"tail"[(position - (SIZE - 4)) as usize]
            } else {
                b'.'
            }
        })
        .collect();
    env.cpu.write(buffer, &bytes).unwrap();
    provider.cursor += u64::from(count);
    env.cpu.write_u32(iosb, 0).unwrap();
    env.cpu.write_u32(iosb + 4, count).unwrap();
    Outcome::Return(0)
}

fn query(env: &mut Env<'_>) -> Outcome {
    observe(env);
    assert_eq!(env.arg(0), HANDLE);
    let (iosb, output, length, class) = (env.arg(1), env.arg(2), env.arg(3), env.arg(4));
    let provider = env.ext.get_mut::<Provider>();
    assert!(provider.opened);
    provider.queries.push(class);
    match class {
        5 => {
            assert!(length >= 24);
            env.cpu.write(output, &[0; 24]).unwrap();
            env.cpu
                .write(output + 8, &provider.size.unwrap_or(SIZE).to_le_bytes())
                .unwrap();
        }
        14 => {
            assert!(length >= 8);
            env.cpu
                .write(output, &provider.cursor.to_le_bytes())
                .unwrap();
        }
        _ => panic!("unexpected file information class {class}"),
    };
    env.cpu.write_u32(iosb, 0).unwrap();
    // An authored native probe confirms fixed query results need no Information count.
    env.cpu.write_u32(iosb + 4, 0).unwrap();
    if provider.failure == 3 {
        env.cpu.protect(DATA, 4096, Prot::READ).unwrap();
    }
    Outcome::Return(0)
}

fn set(env: &mut Env<'_>) -> Outcome {
    observe(env);
    assert_eq!(env.arg(0), HANDLE);
    assert_eq!(env.arg(3), 8);
    assert_eq!(env.arg(4), 14);
    let (iosb, input) = (env.arg(1), env.arg(2));
    let position = read_u64(env.cpu, input);
    assert!((position as i64) >= 0);
    let provider = env.ext.get_mut::<Provider>();
    assert!(provider.opened);
    provider.positions.push(position);
    provider.cursor = position;
    env.cpu.write_u32(iosb, 0).unwrap();
    env.cpu.write_u32(iosb + 4, 0).unwrap();
    Outcome::Return(0)
}

fn close(env: &mut Env<'_>) -> Outcome {
    observe(env);
    assert_eq!(env.arg(0), HANDLE);
    let provider = env.ext.get_mut::<Provider>();
    assert!(provider.opened, "close exactly once");
    provider.closes += 1;
    provider.opened = false;
    Outcome::Return(0)
}

fn fixture(name: &str) -> (Machine<JitCpu>, PathBuf) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/scratch/file-bridge-tests")
        .join(format!(
            "{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    std::fs::create_dir_all(root.join("game")).unwrap();
    std::fs::write(root.join("game/shadow.dat"), b"original").unwrap();
    let mut registry = Registry::default();
    crate::register(&mut registry);
    for (name, argc, handler) in [
        ("open", 11, open as adv_abi::Handler),
        ("read", 9, read as adv_abi::Handler),
        ("query", 5, query as adv_abi::Handler),
        ("set", 5, set as adv_abi::Handler),
        ("close", 1, close as adv_abi::Handler),
        ("install", 0, install as adv_abi::Handler),
        ("fail", 1, fail as adv_abi::Handler),
        ("restore", 0, restore as adv_abi::Handler),
    ] {
        registry.add(PROVIDER, name, Conv::Stdcall { argc }, handler);
    }
    assert!(registry.signature_conflicts().is_empty());
    let mut machine = Machine::new(
        JitCpu::new(PARK_ADDR, false).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    Win32::install(
        &mut machine,
        Win32Config::new(root.join("game"), "authored.exe", root.join("data")),
    )
    .unwrap();
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    machine.cpu.map(OUT, 4096, Prot::RW).unwrap();
    machine.cpu.write(DATA + 0x400, b".\\shadow.dat\0").unwrap();
    let wide: Vec<_> = ".\\shadow.dat"
        .encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect();
    machine.cpu.write(DATA + 0x500, &wide).unwrap();
    {
        let (win32, modules) = machine.ext.get2_mut::<Win32, Modules>();
        Loader::new(&mut machine.cpu, &mut machine.sys, modules, &win32.vfs)
            .load("ntdll.dll")
            .unwrap();
    }
    let mut provider = Provider::default();
    for (nt, hook) in [
        ("NtCreateFile", "open"),
        ("NtReadFile", "read"),
        ("NtQueryInformationFile", "query"),
        ("NtSetInformationFile", "set"),
        ("NtClose", "close"),
    ] {
        let target = machine.resolve_import(PROVIDER, hook).unwrap();
        let entry = native_entry(&mut machine, nt);
        provider.hooks.push((entry, target));
    }
    machine.ext.insert(provider);
    (machine, root)
}

fn api(machine: &mut Machine<JitCpu>, name: &str) -> u32 {
    machine.resolve_import(K32, name).unwrap()
}

fn native_entry(machine: &mut Machine<JitCpu>, name: &str) -> u32 {
    let (win32, modules) = machine.ext.get2_mut::<Win32, Modules>();
    let module = modules.by_name("ntdll.dll").unwrap();
    Loader::new(&mut machine.cpu, &mut machine.sys, modules, &win32.vfs)
        .resolve_dynamic(
            module,
            &ImportName::Name {
                hint: 0,
                name: name.into(),
            },
        )
        .unwrap()
        .0
}

fn call(code: &mut Asm, target: u32, args: &[u32], output: u32, errors: (u32, u32)) {
    code.push_imm(SENTINEL).call(errors.0);
    for &arg in args.iter().rev() {
        code.push_imm(arg);
    }
    code.call(target).mov_mem_eax(output);
    code.call(errors.1).mov_mem_eax(output + 4);
}

fn finish(machine: &mut Machine<JitCpu>, mut code: Asm) {
    code.bytes.extend([0x89, 0xe0]); // mov eax, esp
    code.mov_mem_eax(OUT + 124);
    code.bytes.extend([0x31, 0xc0]);
    code.ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine
        .spawn("authored Win32 NT bridge", CODE, &[], 65536)
        .unwrap();
    let result = machine.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    assert_eq!(
        machine.cpu.read_u32(OUT + 120).unwrap(),
        machine.cpu.read_u32(OUT + 124).unwrap()
    );
    assert!(machine.sys.objects.get(HANDLE).is_none());
    assert!(machine.sys.objects.all_hosts::<FileObj>().next().is_none());
    for &allocation in &machine.ext.get::<Provider>().allocations {
        assert_eq!(machine.ext.get::<Heaps>().owner(allocation), None);
    }
}

fn outputs(machine: &Machine<JitCpu>, expected: &[(u32, u32)]) {
    for &(offset, value) in expected {
        assert_eq!(
            machine.cpu.read_u32(OUT + offset).unwrap(),
            value,
            "output {offset}"
        );
    }
}

#[test]
fn a_w_live_nt_handles_keep_provider_size_cursor_and_original_vfs_independent() {
    for wide in [false, true] {
        let (mut machine, root) = fixture(if wide { "wide" } else { "ansi" });
        let create = api(
            &mut machine,
            if wide { "CreateFileW" } else { "CreateFileA" },
        );
        let read = api(&mut machine, "ReadFile");
        let size = api(&mut machine, "GetFileSize");
        let size_ex = api(&mut machine, "GetFileSizeEx");
        let seek = api(&mut machine, "SetFilePointer");
        let seek_ex = api(&mut machine, "SetFilePointerEx");
        let close = api(&mut machine, "CloseHandle");
        let errors = (
            api(&mut machine, "SetLastError"),
            api(&mut machine, "GetLastError"),
        );
        let install = machine.resolve_import(PROVIDER, "install").unwrap();
        let native_read = native_entry(&mut machine, "NtReadFile");
        let path = DATA + if wide { 0x500 } else { 0x400 };
        let mut code = Asm::new(CODE);
        code.bytes.extend([0x89, 0xe0]);
        code.mov_mem_eax(OUT + 120);
        call(
            &mut code,
            create,
            &[path, 0x8000_0000, 5, 0, 3, 0x80, 0],
            OUT,
            errors,
        );
        code.push_imm(0)
            .push_imm(DATA + 0x100)
            .push_imm(8)
            .push_imm(DATA + 0x300);
        code.mov_eax_mem(OUT)
            .push_eax()
            .call(read)
            .mov_mem_eax(OUT + 8);
        code.mov_eax_mem(OUT)
            .push_eax()
            .call(close)
            .mov_mem_eax(OUT + 16);
        code.call(install);
        call(
            &mut code,
            create,
            &[path, 0x8000_0000, 5, 0, 3, 0x1000_0080, 0],
            OUT + 24,
            errors,
        );
        call(&mut code, size, &[HANDLE, DATA + 0x108], OUT + 32, errors);
        call(
            &mut code,
            size_ex,
            &[HANDLE, DATA + 0x110],
            OUT + 40,
            errors,
        );
        call(
            &mut code,
            read,
            &[HANDLE, DATA + 0x200, 3, DATA + 0x118, 0],
            OUT + 48,
            errors,
        );
        // A direct NT read advances the only cursor before the Win32 CUR seek.
        call(
            &mut code,
            native_read,
            &[HANDLE, 0, 0, 0, DATA + 0x120, DATA + 0x208, 1, 0, 0],
            OUT + 56,
            errors,
        );
        call(&mut code, seek, &[HANDLE, 1, 0, 1], OUT + 64, errors);
        call(
            &mut code,
            read,
            &[HANDLE, DATA + 0x210, 2, DATA + 0x128, 0],
            OUT + 72,
            errors,
        );
        call(
            &mut code,
            seek_ex,
            &[HANDLE, (-2i32) as u32, u32::MAX, DATA + 0x130, 2],
            OUT + 80,
            errors,
        );
        call(
            &mut code,
            read,
            &[HANDLE, DATA + 0x220, 4, DATA + 0x138, 0],
            OUT + 88,
            errors,
        );
        code.mov_mem_imm(DATA + 0x140, SENTINEL);
        call(
            &mut code,
            read,
            &[HANDLE, DATA + 0x230, 4, DATA + 0x140, 0],
            OUT + 96,
            errors,
        );
        call(&mut code, close, &[HANDLE], OUT + 104, errors);
        finish(&mut machine, code);
        outputs(
            &machine,
            &[
                (4, 0),
                (8, 1),
                (16, 1),
                (24, HANDLE),
                (28, 0),
                (32, 7),
                (36, SENTINEL),
                (40, 1),
                (44, SENTINEL),
                (48, 1),
                (52, SENTINEL),
                (56, 0),
                (60, SENTINEL),
                // Successful seeks leave the last error unchanged.
                (64, 5),
                (68, SENTINEL),
                (72, 1),
                (76, SENTINEL),
                (80, 1),
                (84, SENTINEL),
                (88, 1),
                (92, SENTINEL),
                (96, 1),
                (100, SENTINEL),
                (104, 1),
                (108, SENTINEL),
            ],
        );
        assert_ne!(machine.cpu.read_u32(OUT).unwrap(), HANDLE);
        assert_eq!(machine.cpu.read_u32(DATA + 0x100).unwrap(), 8);
        assert_eq!(machine.cpu.read_u32(DATA + 0x108).unwrap(), 1);
        assert_eq!(read_u64(&machine.cpu, DATA + 0x110), SIZE);
        assert_eq!(machine.cpu.read_u32(DATA + 0x118).unwrap(), 3);
        assert_eq!(machine.cpu.read_u32(DATA + 0x124).unwrap(), 1);
        assert_eq!(machine.cpu.read_u32(DATA + 0x128).unwrap(), 2);
        assert_eq!(read_u64(&machine.cpu, DATA + 0x130), SIZE - 2);
        assert_eq!(machine.cpu.read_u32(DATA + 0x138).unwrap(), 2);
        assert_eq!(machine.cpu.read_u32(DATA + 0x140).unwrap(), 0);
        for (pointer, expected) in [
            (DATA + 0x300, &b"original"[..]),
            (DATA + 0x200, &b"sha"[..]),
            (DATA + 0x208, &b"d"[..]),
            (DATA + 0x210, &b"w!"[..]),
            (DATA + 0x220, &b"il"[..]),
        ] {
            let mut bytes = vec![0; expected.len()];
            machine.cpu.read(pointer, &mut bytes).unwrap();
            assert_eq!(bytes, expected);
        }
        let provider = machine.ext.get::<Provider>();
        assert_eq!(provider.paths, [r"\??\C:\Game\shadow.dat"]);
        assert_ne!(provider.options[0] & 0x800, 0, "FILE_RANDOM_ACCESS");
        assert_eq!(
            provider.reads,
            [(0, 3), (3, 1), (5, 2), (SIZE - 2, 4), (SIZE, 4)]
        );
        assert_eq!(provider.positions, [5, SIZE - 2]);
        assert_eq!(provider.queries, [5, 5, 14, 5]);
        assert_eq!(provider.closes, 1);
        assert!(!provider.opened);
        assert_eq!(
            std::fs::read(root.join("game/shadow.dat")).unwrap(),
            b"original"
        );
        assert!(!root.join("data/overlay/shadow.dat").exists());
    }
}

#[test]
fn malformed_outputs_negative_seeks_and_native_failures_do_not_consume_the_stream() {
    let (mut machine, _) = fixture("errors");
    let create = api(&mut machine, "CreateFileA");
    let read = api(&mut machine, "ReadFile");
    let seek = api(&mut machine, "SetFilePointer");
    let seek_ex = api(&mut machine, "SetFilePointerEx");
    let close = api(&mut machine, "CloseHandle");
    let errors = (
        api(&mut machine, "SetLastError"),
        api(&mut machine, "GetLastError"),
    );
    let install = machine.resolve_import(PROVIDER, "install").unwrap();
    let fail = machine.resolve_import(PROVIDER, "fail").unwrap();
    let mut code = Asm::new(CODE);
    code.bytes.extend([0x89, 0xe0]);
    code.mov_mem_eax(OUT + 120).call(install);
    call(
        &mut code,
        create,
        &[DATA + 0x400, 0x8000_0000, 5, 0, 3, 0x80, 0],
        OUT,
        errors,
    );
    call(
        &mut code,
        read,
        &[HANDLE, DATA + 0x200, 3, UNMAPPED, 0],
        OUT + 8,
        errors,
    );
    call(&mut code, seek, &[HANDLE, u32::MAX, 0, 0], OUT + 16, errors);
    call(
        &mut code,
        seek_ex,
        &[HANDLE, u32::MAX, u32::MAX, DATA + 0x100, 0],
        OUT + 24,
        errors,
    );
    code.push_imm(1)
        .call(fail)
        .mov_mem_imm(DATA + 0x108, SENTINEL);
    call(
        &mut code,
        read,
        &[HANDLE, DATA + 0x200, 3, DATA + 0x108, 0],
        OUT + 32,
        errors,
    );
    call(&mut code, close, &[HANDLE], OUT + 40, errors);
    code.push_imm(2).call(fail);
    // The hook's missing-file result also wins over an existing backing file.
    call(
        &mut code,
        create,
        &[DATA + 0x400, 0x8000_0000, 5, 0, 3, 0x80, 0],
        OUT + 48,
        errors,
    );
    finish(&mut machine, code);
    outputs(
        &machine,
        &[
            (0, HANDLE),
            (4, 0),
            (8, 0),
            (12, 998),
            (16, u32::MAX),
            (20, 131),
            (24, 0),
            (28, 131),
            (32, 0),
            (36, 5),
            (40, 1),
            (44, SENTINEL),
            (48, u32::MAX),
            (52, 2),
        ],
    );
    assert_eq!(machine.cpu.read_u32(DATA + 0x108).unwrap(), 0);
    let provider = machine.ext.get::<Provider>();
    assert_eq!(
        provider.reads,
        [(0, 3)],
        "invalid output is rejected before NtReadFile"
    );
    assert!(
        provider.positions.is_empty(),
        "negative seek is rejected before NtSetInformationFile"
    );
    assert_eq!(provider.cursor, 0);
    assert_eq!(provider.paths.len(), 2);
    assert_eq!(provider.closes, 1);
    assert!(!provider.opened);
}

#[test]
fn output_permissions_are_rechecked_and_ambiguous_size_success_clears_last_error() {
    let (mut machine, _) = fixture("output-boundaries");
    machine.ext.get_mut::<Provider>().size = Some(0x1_ffff_ffff);
    let create = api(&mut machine, "CreateFileA");
    let size = api(&mut machine, "GetFileSize");
    let size_ex = api(&mut machine, "GetFileSizeEx");
    let seek = api(&mut machine, "SetFilePointer");
    let seek_ex = api(&mut machine, "SetFilePointerEx");
    let close = api(&mut machine, "CloseHandle");
    let errors = (
        api(&mut machine, "SetLastError"),
        api(&mut machine, "GetLastError"),
    );
    let install = machine.resolve_import(PROVIDER, "install").unwrap();
    let fail = machine.resolve_import(PROVIDER, "fail").unwrap();
    let restore = machine.resolve_import(PROVIDER, "restore").unwrap();
    for offset in [0x110, 0x120, 0x124, 0x140, 0x144] {
        machine.cpu.write_u32(DATA + offset, SENTINEL).unwrap();
    }
    let mut code = Asm::new(CODE);
    code.bytes.extend([0x89, 0xe0]);
    code.mov_mem_eax(OUT + 120).call(install);
    call(
        &mut code,
        create,
        &[DATA + 0x400, 0x8000_0000, 5, 0, 3, 0x0800_0080, 0],
        OUT,
        errors,
    );
    call(&mut code, size, &[HANDLE, DATA + 0x100], OUT + 8, errors);
    code.push_imm(3).call(fail);
    // Each output is writable on entry, then the native query revokes it.
    for (target, args, offset) in [
        (size, vec![HANDLE, DATA + 0x110], 16),
        (size_ex, vec![HANDLE, DATA + 0x120], 24),
        (seek, vec![HANDLE, 0, DATA + 0x130, 1], 32),
        (seek_ex, vec![HANDLE, 0, 0, DATA + 0x140, 1], 40),
    ] {
        code.call(restore);
        call(&mut code, target, &args, OUT + offset, errors);
    }
    call(&mut code, close, &[HANDLE], OUT + 48, errors);
    finish(&mut machine, code);
    outputs(
        &machine,
        &[
            (0, HANDLE),
            (4, 0),
            (8, u32::MAX),
            (12, 0),
            (16, u32::MAX),
            (20, 998),
            (24, 0),
            (28, 998),
            (32, u32::MAX),
            (36, 998),
            (40, 0),
            (44, 998),
            (48, 1),
            (52, SENTINEL),
        ],
    );
    assert_eq!(machine.cpu.read_u32(DATA + 0x100).unwrap(), 1);
    for offset in [0x110, 0x120, 0x124, 0x140, 0x144] {
        assert_eq!(machine.cpu.read_u32(DATA + offset).unwrap(), SENTINEL);
    }
    assert_eq!(machine.cpu.read_u32(DATA + 0x130).unwrap(), 0);
    let provider = machine.ext.get::<Provider>();
    assert_ne!(provider.options[0] & 4, 0, "FILE_SEQUENTIAL_ONLY");
    assert_eq!(provider.queries, [5, 5, 5, 14, 14]);
    assert!(provider.reads.is_empty());
    assert_eq!(provider.closes, 1);
}

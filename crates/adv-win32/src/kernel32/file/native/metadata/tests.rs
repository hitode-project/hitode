//! Authored native providers supply metadata without any shadow host file object.
use super::*;
use adv_abi::{Conv, Registry};
use adv_cpu::{
    JitCpu,
    backend::{Backend, Guest, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_os::{Loader, Modules};
use adv_pe::ImportName;
use adv_unicorn::UnicornCpu;
use std::sync::atomic::{AtomicUsize, Ordering};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const HANDLE: u32 = 0x123abc00;
static NEXT: AtomicUsize = AtomicUsize::new(0);

#[derive(Default)]
struct Provider {
    hooks: Vec<(u32, u32)>,
    queries: Vec<u32>,
    sets: Vec<u32>,
    length: u64,
    attrs: u32,
    closes: u32,
    failure: bool,
    flushes: usize,
    times: Vec<u64>,
}

fn install(env: &mut Env<'_>) -> Outcome {
    for &(entry, target) in &env.ext.get::<Provider>().hooks {
        let mut jump = Asm::new(entry);
        jump.jmp(target);
        env.cpu.write(entry, &jump.bytes).unwrap();
    }
    Outcome::Return(0)
}
fn fail(env: &mut Env<'_>) -> Outcome {
    env.ext.get_mut::<Provider>().failure = true;
    Outcome::Return(0)
}
fn open(env: &mut Env<'_>) -> Outcome {
    assert_eq!(
        (env.arg(1), env.arg(6), env.arg(7), env.arg(8)),
        (0x00100100, 7, 1, 0x60)
    );
    env.cpu.write_u32(env.arg(0), HANDLE).unwrap();
    Outcome::Return(0)
}
fn query(env: &mut Env<'_>) -> Outcome {
    assert_eq!(env.arg(0), HANDLE);
    assert!(env.sys.objects.get(HANDLE).is_none());
    if env.ext.get::<Provider>().failure {
        return Outcome::Return(0xc0000022);
    }
    let class = env.arg(4);
    env.ext.get_mut::<Provider>().queries.push(class);
    let mut bytes = vec![0; env.arg(3) as usize];
    match class {
        4 => {
            assert_eq!(bytes.len(), 40);
            for (index, value) in [100u64, 200, 300, 400].into_iter().enumerate() {
                bytes[index * 8..index * 8 + 8].copy_from_slice(&value.to_le_bytes());
            }
            bytes[32..36].copy_from_slice(&env.ext.get::<Provider>().attrs.to_le_bytes());
        }
        5 => {
            assert_eq!(bytes.len(), 24);
            bytes[8..16].copy_from_slice(&env.ext.get::<Provider>().length.to_le_bytes());
            bytes[16..20].copy_from_slice(&3u32.to_le_bytes());
        }
        6 => bytes[..8].copy_from_slice(&0x1020304050607080u64.to_le_bytes()),
        14 => bytes[..8].copy_from_slice(&5u64.to_le_bytes()),
        _ => panic!("unexpected class {class}"),
    }
    let (output, iosb) = (env.arg(2), env.arg(1));
    let sleep = env
        .sys
        .thunks
        .resolve(env.cpu, "kernel32.dll", "Sleep")
        .unwrap();
    Outcome::call(sleep, [1], move |env, _| {
        env.cpu.write(output, &bytes).unwrap();
        env.cpu.write_u32(iosb + 4, bytes.len() as u32).unwrap();
        Outcome::Return(0)
    })
}
fn volume(env: &mut Env<'_>) -> Outcome {
    assert_eq!((env.arg(0), env.arg(3), env.arg(4)), (HANDLE, 20, 1));
    env.cpu.write_u32(env.arg(2) + 8, 79).unwrap();
    Outcome::Return(0)
}
fn set(env: &mut Env<'_>) -> Outcome {
    assert_eq!(env.arg(0), HANDLE);
    let class = env.arg(4);
    env.ext.get_mut::<Provider>().sets.push(class);
    match class {
        20 => {
            let mut bytes = [0; 8];
            env.cpu.read(env.arg(2), &mut bytes).unwrap();
            env.ext.get_mut::<Provider>().length = u64::from_le_bytes(bytes);
        }
        4 => {
            let mut bytes = [0; 40];
            env.cpu.read(env.arg(2), &mut bytes).unwrap();
            let flags = u32::from_le_bytes(bytes[32..36].try_into().unwrap());
            if flags == 0 {
                env.ext.get_mut::<Provider>().times = bytes[..24]
                    .chunks_exact(8)
                    .map(|bytes| u64::from_le_bytes(bytes.try_into().unwrap()))
                    .collect();
            } else {
                assert_eq!(&bytes[..32], &[0; 32]);
                env.ext.get_mut::<Provider>().attrs = flags;
            }
        }
        _ => panic!("unexpected class {class}"),
    }
    Outcome::Return(0)
}
fn flush(env: &mut Env<'_>) -> Outcome {
    assert_eq!(env.arg(0), HANDLE);
    env.ext.get_mut::<Provider>().flushes += 1;
    let iosb = env.arg(1);
    let sleep = env
        .sys
        .thunks
        .resolve(env.cpu, "kernel32.dll", "Sleep")
        .unwrap();
    Outcome::call(sleep, [1], move |env, _| {
        env.cpu.write(iosb, &[0; 8]).unwrap();
        Outcome::Return(0)
    })
}
fn close(env: &mut Env<'_>) -> Outcome {
    assert_eq!(env.arg(0), HANDLE);
    env.ext.get_mut::<Provider>().closes += 1;
    Outcome::Return(0)
}

fn native<B: Backend>(machine: &mut Machine<B>, name: &str) -> u32 {
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
fn call<B: Backend>(
    machine: &mut Machine<B>,
    code: &mut Asm,
    name: &str,
    args: &[u32],
    output: u32,
) {
    for arg in args.iter().rev() {
        code.push_imm(*arg);
    }
    code.call(machine.resolve_import("kernel32.dll", name).unwrap())
        .mov_mem_eax(output);
}

fn guest<B: Backend>(cpu: B) {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    for (name, argc, handler) in [
        ("install", 0, install as adv_abi::Handler),
        ("fail", 0, fail as adv_abi::Handler),
        ("open", 11, open as adv_abi::Handler),
        ("query", 5, query as adv_abi::Handler),
        ("volume", 5, volume as adv_abi::Handler),
        ("set", 5, set as adv_abi::Handler),
        ("close", 1, close as adv_abi::Handler),
        ("flush", 2, flush as adv_abi::Handler),
    ] {
        registry.add(
            "authored-metadata-provider",
            name,
            Conv::Stdcall { argc },
            handler,
        );
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/scratch/personality-files/metadata")
        .join(format!(
            "{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
    std::fs::create_dir_all(root.join("game")).unwrap();
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    Win32::install(
        &mut machine,
        crate::Win32Config::new(root.join("game"), "authored.exe", root.join("data")),
    )
    .unwrap();
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    machine.cpu.write(DATA, b"metadata.dat\0").unwrap();
    let (win32, modules) = machine.ext.get2_mut::<Win32, Modules>();
    Loader::new(&mut machine.cpu, &mut machine.sys, modules, &win32.vfs)
        .load("ntdll.dll")
        .unwrap();
    let mut provider = Provider {
        length: 10,
        attrs: 0x20,
        ..Default::default()
    };
    for (name, hook) in [
        ("NtCreateFile", "open"),
        ("NtQueryInformationFile", "query"),
        ("NtQueryVolumeInformationFile", "volume"),
        ("NtSetInformationFile", "set"),
        ("NtClose", "close"),
        ("NtFlushBuffersFile", "flush"),
    ] {
        let entry = native(&mut machine, name);
        let target = machine
            .resolve_import("authored-metadata-provider", hook)
            .unwrap();
        provider.hooks.push((entry, target));
    }
    machine.ext.insert(provider);
    let mut code = Asm::new(CODE);
    code.call(
        machine
            .resolve_import("authored-metadata-provider", "install")
            .unwrap(),
    );
    call(
        &mut machine,
        &mut code,
        "GetFileTime",
        &[HANDLE, DATA + 256, DATA + 264, DATA + 272],
        DATA + 128,
    );
    call(
        &mut machine,
        &mut code,
        "GetFileInformationByHandle",
        &[HANDLE, DATA + 512],
        DATA + 132,
    );
    call(
        &mut machine,
        &mut code,
        "SetEndOfFile",
        &[HANDLE],
        DATA + 136,
    );
    call(
        &mut machine,
        &mut code,
        "SetFileAttributesA",
        &[DATA, 0x80],
        DATA + 140,
    );
    call(
        &mut machine,
        &mut code,
        "SetFileTime",
        &[HANDLE, DATA + 256, DATA + 264, DATA + 272],
        DATA + 152,
    );
    call(
        &mut machine,
        &mut code,
        "FlushFileBuffers",
        &[HANDLE],
        DATA + 156,
    );
    code.call(
        machine
            .resolve_import("authored-metadata-provider", "fail")
            .unwrap(),
    );
    call(
        &mut machine,
        &mut code,
        "GetFileTime",
        &[HANDLE, DATA + 768, 0, 0],
        DATA + 144,
    );
    call(&mut machine, &mut code, "GetLastError", &[], DATA + 148);
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine
        .spawn("authored native metadata", CODE, &[], 65536)
        .unwrap();
    let result = machine.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    for offset in [128, 132, 136, 140, 152, 156] {
        assert_eq!(machine.cpu.read_u32(DATA + offset).unwrap(), 1);
    }
    assert_eq!(machine.cpu.read_u32(DATA + 144).unwrap(), 0);
    assert_eq!(machine.cpu.read_u32(DATA + 148).unwrap(), 5);
    assert_eq!(machine.cpu.read_u32(DATA + 256).unwrap(), 100);
    assert_eq!(machine.cpu.read_u32(DATA + 264).unwrap(), 200);
    assert_eq!(machine.cpu.read_u32(DATA + 272).unwrap(), 300);
    let mut fields = [0; 13];
    for (index, field) in fields.iter_mut().enumerate() {
        *field = machine.cpu.read_u32(DATA + 512 + index as u32 * 4).unwrap();
    }
    assert_eq!(
        fields,
        [
            0x20, 100, 0, 200, 0, 300, 0, 79, 0, 10, 3, 0x10203040, 0x50607080
        ]
    );
    let provider = machine.ext.get::<Provider>();
    assert_eq!(provider.queries, [4, 4, 5, 6, 14]);
    assert_eq!(provider.sets, [20, 4, 4]);
    assert_eq!(provider.times, [100, 200, 300]);
    assert_eq!(provider.flushes, 1);
    assert_eq!(
        (provider.length, provider.attrs, provider.closes),
        (5, 0x80, 1)
    );
    assert!(
        machine
            .ext
            .try_get::<adv_abi::ApiDiagnostics>()
            .is_none_or(|diagnostics| diagnostics.warnings.is_empty())
    );
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn metadata_and_mutations_consume_the_live_nt_results_and_resume_after_waits() {
    guest(JitCpu::new(PARK_ADDR, false).unwrap());
    guest(UnicornCpu::new(PARK_ADDR).unwrap());
}

#[test]
fn native_host_mutations_follow_opened_instance_and_share_the_existing_cursor() {
    use crate::kernel32::file::locking::tests::{env, fixture};
    let (mut machine, root) = fixture("native-host-metadata");
    std::fs::write(root.join("game/original.dat"), b"original payload").unwrap();
    let handle =
        crate::kernel32::open_with_share(&mut env(&mut machine), "original.dat", 0x40100100, 3, 7);
    assert_ne!(handle, u32::MAX);
    let original = machine
        .ext
        .get::<Win32>()
        .vfs
        .lookup("original.dat")
        .unwrap();
    let renamed = original.parent().unwrap().join("renamed.dat");
    std::fs::rename(&original, &renamed).unwrap();
    std::fs::write(&original, b"replacement").unwrap();
    let mut basic = [0; 40];
    basic[32..36].copy_from_slice(&0x80u32.to_le_bytes());
    machine.cpu.write(DATA, &basic).unwrap();
    assert_eq!(
        crate::ntdll::file::set(
            &mut env(&mut machine),
            adv_abi::Handle(handle),
            adv_abi::GuestPtr::new(DATA + 128),
            adv_abi::GuestPtr::new(DATA),
            40,
            4
        ),
        0
    );
    assert_eq!(
        machine
            .ext
            .get::<Win32>()
            .vfs
            .file_attributes("renamed.dat")
            .unwrap(),
        0x80
    );
    assert_eq!(
        machine
            .ext
            .get::<Win32>()
            .vfs
            .file_attributes("original.dat")
            .unwrap(),
        0x20
    );
    machine.cpu.write(DATA, &5u64.to_le_bytes()).unwrap();
    assert_eq!(
        crate::ntdll::file::set(
            &mut env(&mut machine),
            adv_abi::Handle(handle),
            adv_abi::GuestPtr::new(DATA + 128),
            adv_abi::GuestPtr::new(DATA),
            8,
            20
        ),
        0
    );
    assert_eq!(std::fs::read(&renamed).unwrap(), b"origi");
    assert_eq!(std::fs::read(&original).unwrap(), b"replacement");
    assert_eq!(
        machine
            .sys
            .objects
            .host::<crate::FileObj>(handle)
            .unwrap()
            .file
            .stream_position()
            .unwrap(),
        0
    );
    assert_eq!(
        crate::ntdll::file::metadata::flush(
            &mut env(&mut machine),
            adv_abi::Handle(handle),
            adv_abi::GuestPtr::new(DATA + 128)
        ),
        0
    );
    let mut requested = [0; 40];
    requested[..8].copy_from_slice(&1u64.to_le_bytes());
    machine.cpu.write(DATA, &requested).unwrap();
    assert_eq!(
        crate::ntdll::file::set(
            &mut env(&mut machine),
            adv_abi::Handle(handle),
            adv_abi::GuestPtr::new(DATA + 128),
            adv_abi::GuestPtr::new(DATA),
            40,
            4
        ),
        0xc00000bbu32 as i32
    );
    assert_eq!(std::fs::read(&renamed).unwrap(), b"origi");
    assert!(
        machine
            .ext
            .get::<adv_abi::ApiDiagnostics>()
            .warnings
            .keys()
            .any(|(_, reason)| reason.contains("unchanged timestamps"))
    );
    machine.sys.objects.close(handle);
    std::fs::remove_dir_all(root).unwrap();
}

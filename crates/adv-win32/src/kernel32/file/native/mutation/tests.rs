//! Authored writable-file mutations and live native callback contracts.
use super::*;
use adv_abi::{Conv, GuestPtr, Handle, Registry};
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
static NEXT: AtomicUsize = AtomicUsize::new(0);

fn fixture<B: Backend>(cpu: B) -> (Machine<B>, std::path::PathBuf) {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/scratch/personality-files/mutations")
        .join(format!(
            "{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
    std::fs::create_dir_all(root.join("game")).unwrap();
    std::fs::create_dir_all(root.join("data/overlay")).unwrap();
    let mut registry = Registry::default();
    crate::register(&mut registry);
    for (name, argc, handler) in [
        ("install", 0, install as adv_abi::Handler),
        ("set", 5, set as adv_abi::Handler),
        ("close", 1, closed as adv_abi::Handler),
    ] {
        registry.add(
            "authored-mutation-provider",
            name,
            Conv::Stdcall { argc },
            handler,
        );
    }
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    crate::Win32::install(
        &mut machine,
        crate::Win32Config::new(root.join("game"), "authored.exe", root.join("data")),
    )
    .unwrap();
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    (machine, root)
}
fn env<B: Backend>(machine: &mut Machine<B>) -> Env<'_> {
    Env {
        context: 0,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: 0,
    }
}
fn returned(outcome: Outcome) -> u32 {
    match outcome {
        Outcome::Return(value) => value,
        _ => panic!("unexpected continuation"),
    }
}
fn last<B: Backend>(machine: &mut Machine<B>) -> u32 {
    crate::util::last_error(&env(machine))
}

#[cfg(windows)]
#[test]
fn rename_retains_the_owner_and_posix_delete_keeps_aliases_alive() {
    let (mut machine, root) = fixture(JitCpu::new(PARK_ADDR, false).unwrap());
    machine
        .spawn("authored owner", PARK_ADDR, &[], 65536)
        .unwrap();
    std::fs::write(root.join("data/overlay/source.dat"), b"authored").unwrap();
    let h =
        crate::kernel32::open_with_share(&mut env(&mut machine), "source.dat", 0x80000000, 3, 7);
    assert_ne!(h, u32::MAX);
    let alias = machine.sys.objects.duplicate(h).unwrap();
    let owner = machine.sys.objects.object_id(h);
    let identity = machine
        .sys
        .objects
        .host::<crate::FileObj>(h)
        .unwrap()
        .file
        .cached_identity();
    assert_eq!(
        returned(rename(
            &mut env(&mut machine),
            "source.dat".into(),
            "renamed.dat".into()
        )),
        1
    );
    assert!(!root.join("data/overlay/source.dat").exists());
    assert_eq!(machine.sys.objects.object_id(alias), owner);
    let file = machine.sys.objects.host::<crate::FileObj>(alias).unwrap();
    assert_eq!(file.file.cached_identity(), identity);
    assert_eq!(file.guest_path, r"C:\Game\renamed.dat");
    assert_eq!(
        returned(delete(&mut env(&mut machine), "renamed.dat".into())),
        1
    );
    assert!(
        machine
            .sys
            .objects
            .host::<crate::FileObj>(h)
            .unwrap()
            .file
            .standard_information()
            .unwrap()
            .delete_pending
    );
    // DeleteFile uses POSIX disposition: the name leaves at once while every
    // alias keeps the original instance alive.
    assert!(!root.join("data/overlay/renamed.dat").exists());
    assert_eq!(
        crate::kernel32::open_with_share(&mut env(&mut machine), "renamed.dat", 0x80000000, 3, 7),
        u32::MAX
    );
    assert_eq!(last(&mut machine), 2);
    assert_eq!(
        crate::ntdll::file::close(&mut env(&mut machine), Handle(h)),
        0
    );
    assert_eq!(
        machine
            .sys
            .objects
            .host::<crate::FileObj>(alias)
            .unwrap()
            .file
            .cached_identity(),
        identity
    );
    assert_eq!(
        crate::ntdll::file::close(&mut env(&mut machine), Handle(alias)),
        0
    );
    assert!(!root.join("data/overlay/renamed.dat").exists());
    assert_eq!(machine.sys.objects.all_hosts::<crate::FileObj>().count(), 0);
    drop(machine);
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(windows)]
#[test]
fn mutation_failures_preserve_sources_and_destinations() {
    let (mut machine, root) = fixture(JitCpu::new(PARK_ADDR, false).unwrap());
    machine
        .spawn("authored failures", PARK_ADDR, &[], 65536)
        .unwrap();
    for (path, bytes) in [
        ("source.dat", b"source".as_slice()),
        ("target.dat", b"target".as_slice()),
    ] {
        std::fs::write(root.join("data/overlay").join(path), bytes).unwrap();
    }
    assert_eq!(
        returned(rename(
            &mut env(&mut machine),
            "source.dat".into(),
            "target.dat".into()
        )),
        0
    );
    assert_eq!(last(&mut machine), 183);
    assert_eq!(
        std::fs::read(root.join("data/overlay/target.dat")).unwrap(),
        b"target"
    );
    // This is a synthetic guest volume, never a path on the host drive.
    let other_volume = format!("{}:\\target.dat", 'D');
    for (target, error) in [(other_volume, 17), (r"missing\target.dat".into(), 3)] {
        assert_eq!(
            returned(rename(
                &mut env(&mut machine),
                "source.dat".into(),
                target.into()
            )),
            0
        );
        assert_eq!(last(&mut machine), error);
    }
    assert_eq!(
        returned(rename(
            &mut env(&mut machine),
            "source.dat".into(),
            "source.dat".into()
        )),
        1
    );
    let h =
        crate::kernel32::open_with_share(&mut env(&mut machine), "source.dat", 0x80000000, 3, 1);
    assert_eq!(
        returned(delete(&mut env(&mut machine), "source.dat".into())),
        0
    );
    assert_eq!(last(&mut machine), 32);
    assert_eq!(
        std::fs::read(root.join("data/overlay/source.dat")).unwrap(),
        b"source"
    );
    crate::ntdll::file::close(&mut env(&mut machine), Handle(h));
    std::fs::write(root.join("game/layer.dat"), b"base").unwrap();
    std::fs::write(root.join("data/overlay/layer.dat"), b"shadow").unwrap();
    assert_eq!(
        returned(delete(&mut env(&mut machine), "layer.dat".into())),
        0
    );
    assert_eq!(last(&mut machine), 50);
    assert_eq!(std::fs::read(root.join("game/layer.dat")).unwrap(), b"base");
    assert_eq!(
        std::fs::read(root.join("data/overlay/layer.dat")).unwrap(),
        b"shadow"
    );
    assert_eq!(machine.sys.objects.all_hosts::<crate::FileObj>().count(), 0);
    drop(machine);
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(windows)]
#[test]
fn native_disposition_is_cancellable_and_invalid_setters_preserve_bytes() {
    let (mut machine, root) = fixture(JitCpu::new(PARK_ADDR, false).unwrap());
    machine
        .spawn("authored NT setters", PARK_ADDR, &[], 65536)
        .unwrap();
    std::fs::write(root.join("data/overlay/source.dat"), b"source").unwrap();
    let handle = returned(operations::open(
        &mut env(&mut machine),
        "source.dat",
        0x00110080,
        7,
        1,
        0x60,
        Box::new(|_, result| Outcome::Return(result.unwrap())),
    ));
    machine.cpu.write(DATA, &[1; 24]).unwrap();
    assert_eq!(
        crate::ntdll::file::set(
            &mut env(&mut machine),
            Handle(handle),
            GuestPtr::new(DATA + 64),
            GuestPtr::new(DATA),
            1,
            13
        ),
        0
    );
    assert!(
        machine
            .sys
            .objects
            .host::<crate::FileObj>(handle)
            .unwrap()
            .file
            .standard_information()
            .unwrap()
            .delete_pending
    );
    machine.cpu.write(DATA, &[0; 24]).unwrap();
    assert_eq!(
        crate::ntdll::file::set(
            &mut env(&mut machine),
            Handle(handle),
            GuestPtr::new(DATA + 64),
            GuestPtr::new(DATA),
            1,
            13
        ),
        0
    );
    assert!(
        !machine
            .sys
            .objects
            .host::<crate::FileObj>(handle)
            .unwrap()
            .file
            .standard_information()
            .unwrap()
            .delete_pending
    );
    for (replace, root_handle, length, status) in [
        (0, 0, 11, 0xc0000004u32),
        (1, 0, 24, 0xc00000bb),
        (0, 1, 24, 0xc00000bb),
    ] {
        machine.cpu.write(DATA, &[0; 24]).unwrap();
        machine.cpu.write(DATA, &[replace]).unwrap();
        machine.cpu.write_u32(DATA + 4, root_handle).unwrap();
        assert_eq!(
            crate::ntdll::file::set(
                &mut env(&mut machine),
                Handle(handle),
                GuestPtr::new(DATA + 64),
                GuestPtr::new(DATA),
                length,
                10
            ) as u32,
            status
        );
        assert_eq!(
            std::fs::read(root.join("data/overlay/source.dat")).unwrap(),
            b"source"
        );
    }
    assert_eq!(
        crate::ntdll::file::close(&mut env(&mut machine), Handle(handle)),
        0
    );
    assert_eq!(
        std::fs::read(root.join("data/overlay/source.dat")).unwrap(),
        b"source"
    );
    drop(machine);
    std::fs::remove_dir_all(root).unwrap();
}

struct Provider {
    hooks: Vec<(u32, u32)>,
    mode: u32,
    sets: usize,
    closes: usize,
    handle: u32,
}
fn install(env: &mut Env<'_>) -> Outcome {
    for &(entry, target) in &env.ext.get::<Provider>().hooks {
        let mut code = Asm::new(entry);
        code.jmp(target);
        env.cpu.write(entry, &code.bytes).unwrap();
    }
    Outcome::Return(0)
}
fn set(env: &mut Env<'_>) -> Outcome {
    let args = [env.arg(0), env.arg(1), env.arg(2), env.arg(3), env.arg(4)];
    assert!(env.sys.objects.host::<crate::FileObj>(args[0]).is_some());
    // DeleteFile opens DELETE | FILE_READ_ATTRIBUTES; rename adds SYNCHRONIZE.
    assert_eq!(
        env.sys
            .objects
            .host::<crate::FileObj>(args[0])
            .unwrap()
            .access,
        if args[4] == 64 {
            0x00010080
        } else {
            0x00110080
        }
    );
    let p = env.ext.get_mut::<Provider>();
    p.sets += 1;
    p.handle = args[0];
    let mode = p.mode;
    let sleep = env
        .sys
        .thunks
        .resolve(env.cpu, "kernel32.dll", "Sleep")
        .unwrap();
    Outcome::call(sleep, [1], move |env, _| {
        if mode == 2 {
            return Outcome::Return(0xc0000022);
        }
        if args[4] == 10 {
            assert_eq!(env.cpu.read_u32(args[2] + 4).unwrap(), 0);
            let name: Vec<_> = r"\??\C:\Game\hooked.dat"
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect();
            assert!(name.len() as u32 <= args[3] - 12);
            env.cpu.write(args[2] + 12, &name).unwrap();
            env.cpu.write_u32(args[2] + 8, name.len() as u32).unwrap();
        } else {
            assert_eq!((args[4], args[3]), (64, 4));
            assert_eq!(env.cpu.read_u32(args[2]).unwrap(), 3);
            if mode == 3 {
                // Guest changes delete into FILE_DISPOSITION_DO_NOT_DELETE.
                env.cpu.write_u32(args[2], 0).unwrap();
            }
        }
        Outcome::Return(crate::ntdll::file::set(
            env,
            Handle(args[0]),
            GuestPtr::new(args[1]),
            GuestPtr::new(args[2]),
            args[3],
            args[4] as i32,
        ) as u32)
    })
}
fn closed(env: &mut Env<'_>) -> Outcome {
    let handle = env.arg(0);
    assert_eq!(handle, env.ext.get::<Provider>().handle);
    env.ext.get_mut::<Provider>().closes += 1;
    let override_status = matches!(env.ext.get::<Provider>().mode, 4 | 5);
    let sleep = env
        .sys
        .thunks
        .resolve(env.cpu, "kernel32.dll", "Sleep")
        .unwrap();
    Outcome::call(sleep, [1], move |env, _| {
        let status = crate::ntdll::file::close(env, Handle(handle)) as u32;
        assert_eq!(status, 0);
        Outcome::Return(if override_status { 0xc0000022 } else { status })
    })
}
fn native<B: Backend>(machine: &mut Machine<B>, name: &str) -> u32 {
    let (win32, modules) = machine.ext.get2_mut::<Win32, Modules>();
    let id = modules.by_name("ntdll.dll").unwrap();
    Loader::new(&mut machine.cpu, &mut machine.sys, modules, &win32.vfs)
        .resolve_dynamic(
            id,
            &ImportName::Name {
                hint: 0,
                name: name.into(),
            },
        )
        .unwrap()
        .0
}
#[cfg(windows)]
fn hooks<B: Backend>(cpu: B, mode: u32) {
    let (mut machine, root) = fixture(cpu);
    std::fs::write(root.join("data/overlay/source.dat"), b"source").unwrap();
    for (offset, path) in [(0, "source.dat"), (64, "target.dat")] {
        machine
            .cpu
            .write(DATA + offset, &[path.as_bytes(), &[0]].concat())
            .unwrap();
    }
    let (win32, modules) = machine.ext.get2_mut::<Win32, Modules>();
    Loader::new(&mut machine.cpu, &mut machine.sys, modules, &win32.vfs)
        .load("ntdll.dll")
        .unwrap();
    let mut entries = vec![];
    for (entry, name) in [("NtSetInformationFile", "set"), ("NtClose", "close")] {
        let entry = native(&mut machine, entry);
        let target = machine
            .resolve_import("authored-mutation-provider", name)
            .unwrap();
        entries.push((entry, target));
    }
    machine.ext.insert(Provider {
        hooks: entries,
        mode,
        sets: 0,
        closes: 0,
        handle: 0,
    });
    let mut code = Asm::new(CODE);
    code.call(
        machine
            .resolve_import("authored-mutation-provider", "install")
            .unwrap(),
    );
    code.push_imm(0x13572468);
    code.call(
        machine
            .resolve_import("kernel32.dll", "SetLastError")
            .unwrap(),
    );
    if matches!(mode, 3 | 5) {
        code.push_imm(DATA);
        code.call(
            machine
                .resolve_import("kernel32.dll", "DeleteFileA")
                .unwrap(),
        );
    } else {
        code.push_imm(DATA + 64).push_imm(DATA);
        code.call(machine.resolve_import("kernel32.dll", "MoveFileA").unwrap());
    }
    code.mov_mem_eax(DATA + 128);
    code.call(
        machine
            .resolve_import("kernel32.dll", "GetLastError")
            .unwrap(),
    )
    .mov_mem_eax(DATA + 132);
    code.bytes.extend([0x31, 0xc0]);
    code.ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine
        .spawn("authored mutation hook", CODE, &[], 65536)
        .unwrap();
    let result = machine.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    assert_eq!(
        machine.cpu.read_u32(DATA + 128).unwrap(),
        u32::from(mode != 2)
    );
    if mode == 2 {
        assert_eq!(machine.cpu.read_u32(DATA + 132).unwrap(), 5);
    } else {
        assert_eq!(machine.cpu.read_u32(DATA + 132).unwrap(), 0x13572468);
    }
    let p = machine.ext.get::<Provider>();
    assert_eq!((p.sets, p.closes), (1, 1));
    assert_eq!(machine.sys.objects.all_hosts::<crate::FileObj>().count(), 0);
    assert_eq!(
        root.join("data/overlay/source.dat").exists(),
        matches!(mode, 2 | 3)
    );
    assert_eq!(
        root.join("data/overlay/hooked.dat").exists(),
        matches!(mode, 1 | 4)
    );
    assert!(!root.join("data/overlay/target.dat").exists());
    drop(machine);
    std::fs::remove_dir_all(root).unwrap();
}
#[cfg(windows)]
#[test]
fn live_mutation_hooks_change_parameters_suspend_and_report_failures() {
    for mode in 1..=5 {
        hooks(JitCpu::new(PARK_ADDR, false).unwrap(), mode);
        hooks(UnicornCpu::new(PARK_ADDR).unwrap(), mode);
    }
}

use super::*;
use crate::Win32Config;
use adv_abi::{Conv, Registry};
use adv_cpu::{
    JitCpu,
    backend::{Guest, Prot},
};
use adv_machine::{Clock, Machine};
use adv_os::{Loader, Modules};
use adv_pe::ImportName;

const OPAQUE_HANDLE: u32 = 0x123abc00;
struct Provider {
    mode: u32,
    source: Vec<u8>,
    reads: usize,
    closes: usize,
    scratch: u32,
    result: Option<Result<Vec<u8>, u32>>,
}
fn open(env: &mut Env<'_>) -> Outcome {
    let (output, attrs) = (env.arg(0), env.arg(2));
    env.ext.get_mut::<Provider>().scratch = attrs;
    if env.ext.get::<Provider>().mode == 3 {
        return Outcome::Return(0xc0000034);
    }
    env.cpu.write_u32(output, OPAQUE_HANDLE).unwrap();
    Outcome::Return(0)
}
fn read_chunk(env: &mut Env<'_>) -> Outcome {
    assert_eq!(env.arg(0), OPAQUE_HANDLE);
    assert!(env.sys.objects.get(OPAQUE_HANDLE).is_none());
    let (iosb, buffer, capacity, offset) = (env.arg(4), env.arg(5), env.arg(6), env.arg(7));
    let position = env.cpu.read_u32(offset).unwrap() as usize;
    let provider = env.ext.get_mut::<Provider>();
    provider.reads += 1;
    if provider.mode == 1 {
        return Outcome::Return(0xc0000022);
    }
    if provider.mode == 2 {
        env.cpu.write_u32(iosb + 4, capacity + 1).unwrap();
        return Outcome::Return(0);
    }
    if position >= provider.source.len() {
        return Outcome::Return(END_OF_FILE);
    }
    let count = (provider.source.len() - position).min(capacity as usize);
    env.cpu
        .write(buffer, &provider.source[position..position + count])
        .unwrap();
    env.cpu.write_u32(iosb + 4, count as u32).unwrap();
    Outcome::Return(0)
}
fn close(env: &mut Env<'_>) -> Outcome {
    assert_eq!(env.arg(0), OPAQUE_HANDLE);
    env.ext.get_mut::<Provider>().closes += 1;
    Outcome::Return(if env.ext.get::<Provider>().mode == 5 {
        0xc0000008
    } else {
        0
    })
}
fn start(env: &mut Env<'_>) -> Outcome {
    assert!(active(env).unwrap());
    let limit = if env.ext.get::<Provider>().mode == 4 {
        5
    } else {
        CHUNK as usize + 17
    };
    read(
        env,
        r"C:\Game\owned.dll".into(),
        limit,
        Box::new(|env, result| {
            env.ext.get_mut::<Provider>().result = Some(result);
            Outcome::Return(37)
        }),
    )
}

#[test]
fn live_hooks_keep_opaque_handles_and_close_on_read_failures() {
    for mode in 0..6 {
        let mut registry = Registry::default();
        crate::register(&mut registry);
        for (name, argc, handler) in [
            ("open", 11, open as adv_abi::Handler),
            ("read", 9, read_chunk as adv_abi::Handler),
            ("close", 1, close as adv_abi::Handler),
            ("start", 0, start as adv_abi::Handler),
        ] {
            registry.add("file-provider", name, Conv::Stdcall { argc }, handler);
        }
        let mut machine = Machine::new(
            JitCpu::new(adv_machine::layout::PARK_ADDR, false).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        Win32::install(
            &mut machine,
            Win32Config::new(
                "/nonexistent/nt-file",
                "fixture.exe",
                "/nonexistent/nt-file-data",
            ),
        )
        .unwrap();
        let source: Vec<_> = (0..CHUNK + 17).map(|i| i as u8).collect();
        machine.ext.insert(Provider {
            mode,
            source,
            reads: 0,
            closes: 0,
            scratch: 0,
            result: None,
        });
        {
            let (win32, modules) = machine.ext.get2_mut::<Win32, Modules>();
            Loader::new(&mut machine.cpu, &mut machine.sys, modules, &win32.vfs)
                .load("ntdll.dll")
                .unwrap();
        }
        for (nt, provider) in [
            ("NtCreateFile", "open"),
            ("NtReadFile", "read"),
            ("NtClose", "close"),
        ] {
            let target = machine.resolve_import("file-provider", provider).unwrap();
            let address = {
                let (win32, modules) = machine.ext.get2_mut::<Win32, Modules>();
                let module = modules.by_name("ntdll.dll").unwrap();
                Loader::new(&mut machine.cpu, &mut machine.sys, modules, &win32.vfs)
                    .resolve_dynamic(
                        module,
                        &ImportName::Name {
                            hint: 0,
                            name: nt.into(),
                        },
                    )
                    .unwrap()
                    .0
            };
            let mut branch = vec![0xe9];
            branch.extend_from_slice(&target.wrapping_sub(address + 5).to_le_bytes());
            machine
                .cpu
                .protect(address & !4095, 4096, Prot::RWX)
                .unwrap();
            machine.cpu.write(address, &branch).unwrap();
            machine
                .cpu
                .protect(address & !4095, 4096, Prot::RX)
                .unwrap();
        }
        let target = machine.resolve_import("file-provider", "start").unwrap();
        machine
            .spawn("native file callbacks", target, &[], 65536)
            .unwrap();
        let outcome = machine.run_virtual(1_000_000);
        assert!(
            !matches!(outcome, adv_machine::RunResult::Failed(_)),
            "{outcome:?}"
        );
        let provider = machine.ext.get::<Provider>();
        assert_eq!(provider.closes, usize::from(mode != 3));
        match mode {
            0 => {
                assert_eq!(
                    provider.result.as_ref().unwrap().as_ref().unwrap(),
                    &provider.source
                );
                assert_eq!(provider.reads, 3);
            }
            1 => assert_eq!(provider.result, Some(Err(0xc0000022))),
            2 => assert_eq!(provider.result, Some(Err(INVALID_PARAMETER))),
            3 => assert_eq!(provider.result, Some(Err(0xc0000034))),
            4 => assert_eq!(provider.result, Some(Err(FILE_TOO_LARGE))),
            _ => assert_eq!(provider.result, Some(Err(0xc0000008))),
        }
        let scratch = provider.scratch;
        assert_eq!(machine.ext.get_mut::<Heaps>().size(0, scratch), None);
    }
}

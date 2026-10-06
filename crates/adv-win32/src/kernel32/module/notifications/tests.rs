//! Authored guest callbacks exercise terminal APIs under kernel-object pressure.
use crate::{Win32, Win32Config};
use adv_abi::Registry;
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Conv, Machine, Object, ObjectLimits, Objects, Outcome, RunResult};
use adv_os::{Module, ModuleKind, module::Lifetime};
use adv_unicorn::UnicornCpu;

const CODE: u32 = 0x400000;
const PEER: u32 = CODE + 256;
const DATA: u32 = 0x410000;
const PARK: u32 = adv_machine::layout::PARK_ADDR;
#[derive(Default)]
struct Calls(Vec<[u32; 4]>);

fn machine<B: Backend>(cpu: B, capacity: usize) -> Machine<B> {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    registry.add("fixture.dll", "notify", Conv::Stdcall { argc: 4 }, |env| {
        let lock = env.ext.get::<Win32>().loader_lock.expect("notification lock");
        assert!(matches!(env.sys.objects.get(lock), Some(Object::Mutex { owner: Some(owner), recursion }) if *owner == env.context && *recursion > 0));
        let record = core::array::from_fn(|index| env.arg(index as u32));
        env.ext.get_mut::<Calls>().0.push(record);
        Outcome::Return(1)
    });
    registry.add(
        "fixture.dll",
        "before_process_exit",
        Conv::Stdcall { argc: 0 },
        |env| {
            assert!(
                env.ext.get::<Win32>().loader_lock.is_none(),
                "disabled thread notices must not allocate a lock"
            );
            Outcome::Return(0)
        },
    );
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    Win32::install(
        &mut machine,
        Win32Config::new(
            "/nonexistent/terminal-notifications",
            "fixture.exe",
            "/nonexistent/terminal-notifications-data",
        ),
    )
    .unwrap();
    machine.ext.insert(Calls::default());
    machine.sys.objects = Objects::with_limits(ObjectLimits {
        objects: capacity,
        ..ObjectLimits::default()
    });
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    machine
}

fn callback<B: Backend>(machine: &mut Machine<B>, address: u32, tag: u32) {
    let observe = machine.resolve_import("fixture.dll", "notify").unwrap();
    let sleep = machine.resolve_import("kernel32.dll", "Sleep").unwrap();
    let mut code = Asm::new(address);
    // Preserve each original argument while pushing reserved, reason and base.
    for _ in 0..3 {
        code.bytes.extend_from_slice(&[0x8b, 0x44, 0x24, 12]);
        code.push_eax();
    }
    code.push_imm(tag).call(observe);
    // A real yield inside a detach callback must retain the loader lock.
    code.push_imm(0).call(sleep);
    code.bytes.extend_from_slice(&[0xb8, 1, 0, 0, 0]);
    code.ret_n(12);
    machine.cpu.write(address, &code.bytes).unwrap();
}

fn modules<B: Backend>(machine: &mut Machine<B>, thread_calls: bool, tls: bool) {
    let mut dependency = None;
    for index in 0..2u32 {
        let base = 0x500000 + index * 4096;
        assert!(machine.sys.vm.reserve_at(base, 4096));
        machine.cpu.map(base, 4096, Prot::RX).unwrap();
        let tls_target = CODE + 512 + index * 128;
        let entry = tls_target + 64;
        callback(machine, tls_target, (index + 1) * 10);
        callback(machine, entry, (index + 1) * 10 + 1);
        let mut lifetime = Lifetime {
            attach_started: true,
            mapped: true,
            thread_calls,
            callbacks: if tls { vec![tls_target] } else { vec![] },
            ..Lifetime::default()
        };
        lifetime.dependencies.extend(dependency);
        dependency = Some(adv_os::modules_mut(&mut machine.ext).add(Module {
            name: format!("terminal-{index}.dll"),
            path: format!("terminal-{index}.dll"),
            kind: ModuleKind::Image,
            base,
            size: 4096,
            is_dll: true,
            entry,
            stack_reserve: 65536,
            exports_by_name: Default::default(),
            exports_by_ordinal: Default::default(),
            resources: None,
            data_file: None,
            load_count: 1,
            ldr_entry: 0,
            ldr_size: 0,
            lifetime,
        }));
    }
}

fn program<B: Backend>(machine: &mut Machine<B>, thread: bool, before_process_exit: bool) {
    let api = machine
        .resolve_import(
            "kernel32.dll",
            if thread { "ExitThread" } else { "ExitProcess" },
        )
        .unwrap();
    let mut code = Asm::new(CODE);
    code.push_imm(7).call(api);
    // A terminal API return must be observable even if execution later faults.
    code.bytes.extend_from_slice(&[0xff, 0x05]);
    code.bytes.extend_from_slice(&DATA.to_le_bytes());
    code.bytes.extend_from_slice(&[0x0f, 0x0b]);
    machine.cpu.write(CODE, &code.bytes).unwrap();
    let first = machine.spawn("first", CODE, &[], 65536).unwrap();
    if thread {
        let join = machine
            .resolve_import("kernel32.dll", "WaitForSingleObject")
            .unwrap();
        let exit = machine
            .resolve_import("kernel32.dll", "ExitProcess")
            .unwrap();
        let mut peer = Asm::new(PEER);
        peer.push_imm(u32::MAX)
            .push_imm(machine.sys.context(first).thread_handle)
            .call(join);
        if before_process_exit {
            let check = machine
                .resolve_import("fixture.dll", "before_process_exit")
                .unwrap();
            peer.call(check);
        }
        peer.push_imm(9).call(exit);
        machine.cpu.write(PEER, &peer.bytes).unwrap();
        machine.spawn("surviving peer", PEER, &[], 65536).unwrap();
    }
}

fn empty<B: Backend>(cpu: B, thread: bool) {
    let capacity = if thread { 2 } else { 1 };
    let mut machine = machine(cpu, capacity);
    program(&mut machine, thread, false);
    let result = machine.run_virtual(1_000_000);
    assert!(
        matches!(result, RunResult::Exited(code) if code == if thread { 9 } else { 7 }),
        "{result:?}"
    );
    assert!(machine.ext.get::<Calls>().0.is_empty());
    assert!(machine.ext.get::<Win32>().loader_lock.is_none());
    assert_eq!(machine.sys.objects.object_count(), capacity);
    assert_eq!(machine.cpu.read_u32(DATA).unwrap(), 0);
    if thread {
        assert_eq!(machine.sys.context(0).exit_code(), Some(7));
    }
}

fn exhausted<B: Backend>(cpu: B, thread: bool) {
    let mut machine = machine(cpu, if thread { 2 } else { 1 });
    modules(&mut machine, true, true);
    program(&mut machine, thread, false);
    let result = machine.run_virtual(1_000_000);
    let RunResult::Failed(error) = result else {
        panic!("notification allocation failure accepted: {result:?}")
    };
    assert!(
        error
            .message
            .contains("terminal DLL/TLS notification loader lock allocation failed"),
        "{}",
        error.message
    );
    assert!(
        machine.ext.get::<Calls>().0.is_empty(),
        "must not bypass callback serialization on exhaustion"
    );
    assert_eq!(
        machine.cpu.read_u32(DATA).unwrap(),
        0,
        "terminal API returned"
    );
}

fn expected(reason: u32) -> Vec<[u32; 4]> {
    [20, 21, 10, 11]
        .into_iter()
        .map(|tag| {
            [
                tag,
                if tag >= 20 { 0x501000 } else { 0x500000 },
                reason,
                u32::from(reason == 0 && tag % 10 == 1),
            ]
        })
        .collect()
}

fn ordered<B: Backend>(cpu: B, thread: bool) {
    let mut machine = machine(cpu, 16);
    modules(&mut machine, true, true);
    program(&mut machine, thread, false);
    let result = machine.run_virtual(1_000_000);
    assert!(
        matches!(result, RunResult::Exited(code) if code == if thread { 9 } else { 7 }),
        "{result:?}"
    );
    let mut calls = if thread { expected(3) } else { vec![] };
    calls.extend(expected(0));
    assert_eq!(machine.ext.get::<Calls>().0, calls);
    assert_eq!(machine.cpu.read_u32(DATA).unwrap(), 0);
    let lock = machine.ext.get::<Win32>().loader_lock.unwrap();
    assert!(matches!(
        machine.sys.objects.get(lock),
        Some(Object::Mutex {
            owner: None,
            recursion: 0
        })
    ));
}

fn disabled<B: Backend>(cpu: B) {
    let mut machine = machine(cpu, 16);
    modules(&mut machine, false, false);
    program(&mut machine, true, true);
    let result = machine.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(9)), "{result:?}");
    let expected = expected(0)
        .into_iter()
        .filter(|record| record[0] % 10 == 1)
        .collect::<Vec<_>>();
    assert_eq!(machine.ext.get::<Calls>().0, expected);
}

fn library_exit<B: Backend>(cpu: B, pending: bool, capacity: usize) {
    let mut machine = machine(cpu, capacity);
    if pending {
        modules(&mut machine, true, true);
    }
    let api = machine
        .resolve_import("kernel32.dll", "FreeLibraryAndExitThread")
        .unwrap();
    let mut code = Asm::new(CODE);
    code.push_imm(13)
        .push_imm(if pending { 0x501000 } else { 0 })
        .call(api);
    code.bytes.extend_from_slice(&[0xff, 0x05]);
    code.bytes.extend_from_slice(&DATA.to_le_bytes());
    code.bytes.extend_from_slice(&[0x0f, 0x0b]);
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine.spawn("unload and exit", CODE, &[], 65536).unwrap();
    let result = machine.run_virtual(1_000_000);
    if pending && capacity == 1 {
        let RunResult::Failed(error) = result else {
            panic!("required detaches bypassed: {result:?}")
        };
        assert!(
            error
                .message
                .contains("terminal DLL/TLS notification loader lock allocation failed"),
            "{}",
            error.message
        );
        assert!(machine.ext.get::<Calls>().0.is_empty());
        assert_eq!(
            adv_os::modules(&machine.ext)
                .by_name("terminal-1.dll")
                .map(|id| adv_os::modules(&machine.ext).get(id).lifetime.external_refs),
            Some(1)
        );
    } else {
        assert!(matches!(result, RunResult::Exited(13)), "{result:?}");
        if pending {
            assert_eq!(
                machine.ext.get::<Calls>().0,
                [
                    [20, 0x501000, 0, 0],
                    [21, 0x501000, 0, 0],
                    [10, 0x500000, 3, 0],
                    [11, 0x500000, 3, 0],
                    [10, 0x500000, 0, 0],
                    [11, 0x500000, 0, 1],
                ]
            );
            assert!(
                adv_os::modules(&machine.ext)
                    .by_name("terminal-1.dll")
                    .is_none()
            );
        } else {
            assert!(machine.ext.get::<Calls>().0.is_empty());
            assert!(machine.ext.get::<Win32>().loader_lock.is_none());
        }
    }
    assert_eq!(
        machine.cpu.read_u32(DATA).unwrap(),
        0,
        "terminal API returned"
    );
}

#[test]
fn startup_gate_makes_system_modules_resident_without_loading_module_queries() {
    let mut machine = machine(JitCpu::new(PARK, false).unwrap(), 16);
    machine.spawn("startup contract", CODE, &[], 65536).unwrap();
    let enter = machine
        .resolve_import(super::INTERNAL, "startup_enter")
        .unwrap();
    assert_eq!(machine.sys.process_init.front().unwrap().target, enter);
    assert!(adv_os::modules(&machine.ext).by_name("ntdll.dll").is_none());
    let mut references = None;
    let mut env = adv_machine::Env {
        context: 0,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: 0,
    };
    for _ in 0..2 {
        assert!(matches!(super::startup_enter(&mut env), Outcome::Return(1)));
        // The gate runs before guest initializers, so their ordinary module
        // queries can already observe readable, pinned system PE images.
        let current = ["ntdll.dll", "kernel32.dll"].map(|name| {
            let modules = adv_os::modules(env.ext);
            let module = modules.get(modules.by_name(name).unwrap());
            assert!(module.lifetime.pinned);
            assert!(env.cpu.accessible(module.base, 2, Prot::READ));
            assert_eq!(env.cpu.read_u32(module.base).unwrap() & 0xffff, 0x5a4d);
            let (base, refs) = (module.base, module.lifetime.external_refs);
            assert!(matches!(
                super::super::get_module_handle(&mut env, Some(name.into())),
                Outcome::Return(handle) if handle == base
            ));
            (base, refs)
        });
        if let Some(expected) = references {
            assert_eq!(
                current, expected,
                "startup must not retain resident images again"
            );
        } else {
            references = Some(current);
        }
        assert!(matches!(
            super::super::get_module_handle(&mut env, Some("unrelated-not-loaded.dll".into())),
            Outcome::Return(0)
        ));
        assert!(
            adv_os::modules(env.ext)
                .by_name("unrelated-not-loaded.dll")
                .is_none()
        );
        assert!(matches!(super::startup_leave(&mut env), Outcome::Return(1)));
        let lock = env.ext.get::<Win32>().loader_lock.unwrap();
        assert!(matches!(
            env.sys.objects.get(lock),
            Some(Object::Mutex {
                owner: None,
                recursion: 0
            })
        ));
    }
}

#[test]
fn both_terminal_apis_need_no_new_lock_without_notifications_even_at_capacity() {
    for thread in [false, true] {
        for jit in [false, true] {
            empty(JitCpu::new(PARK, jit).unwrap(), thread);
        }
        empty(UnicornCpu::new(PARK).unwrap(), thread);
    }
}
#[test]
fn both_terminal_apis_fail_explicitly_when_pending_notification_lock_cannot_be_allocated() {
    for thread in [false, true] {
        for jit in [false, true] {
            exhausted(JitCpu::new(PARK, jit).unwrap(), thread);
        }
        exhausted(UnicornCpu::new(PARK).unwrap(), thread);
    }
}
#[test]
fn dll_tls_dependency_order_and_lock_survive_yield_until_both_terminal_apis_finish() {
    for thread in [false, true] {
        for jit in [false, true] {
            ordered(JitCpu::new(PARK, jit).unwrap(), thread);
        }
        ordered(UnicornCpu::new(PARK).unwrap(), thread);
    }
}
#[test]
fn disabled_thread_notices_need_no_lock_but_process_detach_still_runs() {
    for jit in [false, true] {
        disabled(JitCpu::new(PARK, jit).unwrap());
    }
    disabled(UnicornCpu::new(PARK).unwrap());
}

#[test]
fn free_library_and_exit_never_returns_and_keeps_required_detaches() {
    for (pending, capacity) in [(false, 1), (true, 1), (true, 16)] {
        for jit in [false, true] {
            library_exit(JitCpu::new(PARK, jit).unwrap(), pending, capacity);
        }
        library_exit(UnicornCpu::new(PARK).unwrap(), pending, capacity);
    }
}

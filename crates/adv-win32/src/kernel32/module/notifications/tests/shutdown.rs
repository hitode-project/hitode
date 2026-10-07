use super::*;
use adv_abi::ApiDiagnostics;

fn reentrant_callback<B: Backend>(machine: &mut Machine<B>, address: u32, tag: u32) {
    let observe = machine.resolve_import("fixture.dll", "notify").unwrap();
    let shutdown = machine
        .resolve_import("ntdll.dll", "LdrShutdownProcess")
        .unwrap();
    let sleep = machine.resolve_import("kernel32.dll", "Sleep").unwrap();
    let mut code = Asm::new(address);
    code.call(shutdown);
    for _ in 0..3 {
        code.bytes.extend_from_slice(&[0x8b, 0x44, 0x24, 12]);
        code.push_eax();
    }
    code.push_imm(tag).call(observe).push_imm(0).call(sleep);
    code.bytes.extend_from_slice(&[0xb8, 1, 0, 0, 0]);
    code.ret_n(12);
    machine.cpu.write(address, &code.bytes).unwrap();
}

fn returning_shutdown<B: Backend>(cpu: B) {
    let mut machine = machine(cpu, 16);
    machine.ext.insert(ApiDiagnostics::default());
    assert_eq!(
        machine
            .sys
            .thunks
            .resolver()
            .host_fn("ntdll.dll", "LdrShutdownProcess")
            .unwrap()
            .conv,
        Conv::Stdcall { argc: 0 }
    );
    modules(&mut machine, true, true);
    for index in 0..2 {
        reentrant_callback(&mut machine, CODE + 512 + index * 128, (index + 1) * 10);
        reentrant_callback(&mut machine, CODE + 576 + index * 128, (index + 1) * 10 + 1);
    }
    let shutdown = machine
        .resolve_import("ntdll.dll", "LdrShutdownProcess")
        .unwrap();
    let set_error = machine
        .resolve_import("kernel32.dll", "SetLastError")
        .unwrap();
    let get_error = machine
        .resolve_import("kernel32.dll", "GetLastError")
        .unwrap();
    let get_exit = machine
        .resolve_import("kernel32.dll", "GetExitCodeThread")
        .unwrap();
    let set_event = machine.resolve_import("kernel32.dll", "SetEvent").unwrap();
    let join = machine
        .resolve_import("kernel32.dll", "WaitForSingleObject")
        .unwrap();
    let exit = machine
        .resolve_import("kernel32.dll", "ExitProcess")
        .unwrap();
    let thread_exit = machine
        .resolve_import("kernel32.dll", "ExitThread")
        .unwrap();
    let event = machine.sys.objects.insert(Object::Event {
        manual_reset: true,
        signaled: false,
    });
    let mut peer = Asm::new(PEER);
    peer.push_imm(u32::MAX)
        .push_imm(event)
        .call(join)
        .push_imm(21)
        .call(thread_exit);
    machine.cpu.write(PEER, &peer.bytes).unwrap();
    let main = machine
        .spawn("loader shutdown caller", CODE, &[], 65536)
        .unwrap();
    let peer = machine
        .spawn("surviving loader peer", PEER, &[], 65536)
        .unwrap();
    let peer_handle = machine.sys.context(peer).thread_handle;
    let mut code = Asm::new(CODE);
    code.push_imm(0x13572468).call(set_error);
    code.bytes.extend_from_slice(&[0x89, 0xe0]);
    code.mov_mem_eax(DATA + 16).call(shutdown);
    code.bytes.extend_from_slice(&[0x89, 0xe0]);
    code.mov_mem_eax(DATA + 20)
        .call(get_error)
        .mov_mem_eax(DATA + 24);
    code.call(shutdown).call(get_error).mov_mem_eax(DATA + 28);
    code.push_imm(DATA + 32)
        .push_imm(peer_handle)
        .call(get_exit)
        .mov_mem_eax(DATA + 36);
    code.push_imm(event).call(set_event);
    code.push_imm(u32::MAX).push_imm(peer_handle).call(join);
    code.push_imm(7).call(exit);
    code.bytes.extend_from_slice(&[0xff, 0x05]);
    code.bytes.extend_from_slice(&DATA.to_le_bytes());
    code.bytes.extend_from_slice(&[0x0f, 0x0b]);
    machine.cpu.write(CODE, &code.bytes).unwrap();
    let result = machine.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(7)), "{result:?}");
    assert_eq!(machine.sys.context(main).exit_code(), Some(7));
    assert_eq!(machine.sys.context(peer).exit_code(), Some(21));
    let mut calls = expected(0);
    calls.extend(expected(3));
    assert_eq!(
        machine.ext.get::<Calls>().0,
        calls,
        "process detach once, ordinary thread detach afterwards"
    );
    assert_eq!(
        machine.cpu.read_u32(DATA + 16).unwrap(),
        machine.cpu.read_u32(DATA + 20).unwrap(),
        "stdcall0 balance"
    );
    for offset in [24, 28] {
        assert_eq!(machine.cpu.read_u32(DATA + offset).unwrap(), 0x13572468);
    }
    assert_eq!(
        machine.cpu.read_u32(DATA + 32).unwrap(),
        259,
        "Ldr shutdown must not terminate the peer"
    );
    assert_eq!(machine.cpu.read_u32(DATA + 36).unwrap(), 1);
    assert_eq!(
        machine.cpu.read_u32(DATA).unwrap(),
        0,
        "ExitProcess remains terminal"
    );
    let lock = machine.ext.get::<Win32>().loader_lock.unwrap();
    assert!(matches!(
        machine.sys.objects.get(lock),
        Some(Object::Mutex {
            owner: None,
            recursion: 0
        })
    ));
    for (_, module) in adv_os::modules(&machine.ext).iter() {
        assert!(module.lifetime.mapped);
    }
    let diagnostics = machine.ext.get::<ApiDiagnostics>();
    assert!(diagnostics.warnings.is_empty());
    assert!(diagnostics.gaps.is_empty());
    assert_eq!(diagnostics.stub_calls, 0);
}

#[test]
fn loader_shutdown_returns_once_through_reentrant_yielding_callbacks_and_keeps_peers_alive() {
    returning_shutdown(UnicornCpu::new(PARK).unwrap());
    returning_shutdown(JitCpu::new(PARK, false).unwrap());
    let mut jit = JitCpu::new(PARK, true).unwrap();
    jit.set_async_compilation(false);
    returning_shutdown(jit);
}

fn capacity<B: Backend>(cpu: B, pending: bool) {
    let mut machine = machine(cpu, 1);
    if pending {
        modules(&mut machine, true, true);
    }
    let shutdown = machine
        .resolve_import("ntdll.dll", "LdrShutdownProcess")
        .unwrap();
    let exit = machine
        .resolve_import("kernel32.dll", "ExitProcess")
        .unwrap();
    let mut code = Asm::new(CODE);
    code.call(shutdown);
    code.bytes.extend_from_slice(&[0xff, 0x05]);
    code.bytes.extend_from_slice(&DATA.to_le_bytes());
    code.push_imm(7).call(exit);
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine
        .spawn("shutdown at capacity", CODE, &[], 65536)
        .unwrap();
    let result = machine.run_virtual(1_000_000);
    if pending {
        let RunResult::Failed(error) = result else {
            panic!("pending callbacks bypassed: {result:?}")
        };
        assert!(
            error
                .message
                .contains("notification loader lock allocation failed")
        );
        assert_eq!(machine.cpu.read_u32(DATA).unwrap(), 0);
    } else {
        assert!(matches!(result, RunResult::Exited(7)), "{result:?}");
        assert_eq!(machine.cpu.read_u32(DATA).unwrap(), 1);
        assert!(machine.ext.get::<Win32>().loader_lock.is_none());
    }
    assert!(machine.ext.get::<Calls>().0.is_empty());
}

#[test]
fn loader_shutdown_does_not_need_an_empty_lock_or_bypass_required_serialization_at_capacity() {
    for pending in [false, true] {
        capacity(UnicornCpu::new(PARK).unwrap(), pending);
        capacity(JitCpu::new(PARK, false).unwrap(), pending);
    }
}

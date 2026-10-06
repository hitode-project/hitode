use super::*;
use adv_cpu::{
    JitCpu,
    backend::{Guest, Reg},
};
use adv_machine::{Clock, ObjectLimits, Objects, RunResult, layout::PARK_ADDR};

fn fixture() -> (Machine<JitCpu>, u32) {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    adv_win32::ole32::register(&mut registry);
    register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    assert!(
        !registry
            .missing_signatures()
            .iter()
            .any(|name| name.starts_with("quartz.dll!"))
    );
    let mut machine = Machine::new(
        JitCpu::new(PARK_ADDR, false).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    adv_win32::Win32::install(
        &mut machine,
        adv_win32::Win32Config::new(".", "dshow-fixture.exe", "target/dshow-fixture-tests"),
    )
    .unwrap();
    install(&mut machine).unwrap();
    machine.spawn("authored", PARK_ADDR, &[], 65536).unwrap();
    let data = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, 8192, Prot::RW)
        .unwrap();
    machine.cpu.set_reg(Reg::Esp, data + 4000);
    machine.cpu.write_u32(data + 4000, PARK_ADDR).unwrap();
    (machine, data)
}
fn env(machine: &mut Machine<JitCpu>) -> Env<'_> {
    Env {
        context: machine.sys.current,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: 0,
    }
}
fn result(outcome: Outcome) -> u32 {
    match outcome {
        Outcome::Return(value) => value,
        _ => panic!("expected immediate COM return"),
    }
}
fn graph(machine: &mut Machine<JitCpu>, data: u32, iid: Iid) -> u32 {
    assert_eq!(result(create_graph(&mut env(machine), iid, 0, data)), 0);
    machine.cpu.read_u32(data).unwrap()
}
fn method(machine: &mut Machine<JitCpu>, interface: &str, name: &str, args: &[u32]) -> Outcome {
    let binding = bindings()
        .iter()
        .find(|b| b.signature.module == interface && b.signature.name == name)
        .unwrap();
    assert_eq!(args.len(), binding.signature.stack_words() as usize);
    call(&mut env(machine), binding, args)
}
fn query_interface(machine: &mut Machine<JitCpu>, data: u32, this: u32, iid: Iid) -> u32 {
    machine.cpu.write(data + 32, &iid).unwrap();
    assert_eq!(query(&mut env(machine), this, data + 32, data + 64), 0);
    machine.cpu.read_u32(data + 64).unwrap()
}

#[test]
fn all_graph_aliases_share_canonical_identity_and_release_every_allocation() {
    let (mut machine, data) = fixture();
    let before = machine.sys.objects.object_count();
    // Creating an alias directly still yields the graph's canonical IUnknown.
    let control = graph(&mut machine, data, IID_CONTROL);
    let canonical = query_interface(&mut machine, data, control, adv_os::com::IUNKNOWN);
    assert_ne!(control, canonical);
    let mut aliases = vec![control, canonical];
    for (_, iid) in GRAPH_INTERFACES {
        let pointer = query_interface(&mut machine, data, control, iid);
        let unknown = query_interface(&mut machine, data, pointer, adv_os::com::IUNKNOWN);
        assert_eq!(unknown, canonical);
        assert_eq!(
            release(&mut env(&mut machine), unknown),
            aliases.len() as u32 + 1
        );
        aliases.push(pointer);
    }
    let base = query_interface(&mut machine, data, canonical, IID_FILTER_GRAPH);
    assert_eq!(base, canonical);
    aliases.push(base);
    let event = query_interface(&mut machine, data, control, IID_EVENT_EX);
    aliases.push(event);
    let event_base = query_interface(&mut machine, data, event, IID_EVENT);
    assert_eq!(event_base, event);
    aliases.push(event_base);
    let dispatch = query_interface(&mut machine, data, event, IID_DISPATCH);
    assert_eq!(dispatch, control);
    aliases.push(dispatch);
    assert_eq!(machine.sys.objects.object_count(), before + 1);
    let pointers: BTreeSet<_> = aliases.iter().copied().collect();
    let total = aliases.len();
    for (index, pointer) in aliases.into_iter().enumerate() {
        assert_eq!(
            release(&mut env(&mut machine), pointer),
            (total - index - 1) as u32
        );
    }
    assert_eq!(machine.sys.objects.object_count(), before);
    let com = machine.ext.get::<State>().com;
    for pointer in pointers {
        assert!(com.get::<Payload>(&machine.sys.objects, pointer).is_none());
        let heaps = machine.ext.get_mut::<Heaps>();
        assert_eq!(heaps.size(heaps.process_heap, pointer), None);
    }
}

#[test]
fn unsupported_media_does_not_change_stopped_state_or_generate_completion() {
    let (mut machine, data) = fixture();
    let graph = graph(&mut machine, data, IID_GRAPH);
    let control = query_interface(&mut machine, data, graph, IID_CONTROL);
    let events = query_interface(&mut machine, data, graph, IID_EVENT_EX);
    let seeking = query_interface(&mut machine, data, graph, IID_SEEKING);
    let movie: Vec<u8> = "authored-movie.mpg\0"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    machine.cpu.write(data + 128, &movie).unwrap();
    for _ in 0..2 {
        assert_eq!(
            result(method(
                &mut machine,
                "IGraphBuilder",
                "RenderFile",
                &[graph, data + 128, 0]
            )),
            VFW_E_CANNOT_RENDER
        );
    }
    machine.cpu.write_u32(data + 80, 0xa5a5a5a5).unwrap();
    assert_eq!(
        result(method(
            &mut machine,
            "IGraphBuilder",
            "AddSourceFilter",
            &[graph, data + 128, 0, data + 80]
        )),
        VFW_E_CANNOT_RENDER
    );
    assert_eq!(machine.cpu.read_u32(data + 80).unwrap(), 0);
    assert_eq!(
        result(method(
            &mut machine,
            "IGraphBuilder",
            "AddFilter",
            &[graph, data + 128, 0]
        )),
        E_NOTIMPL
    );
    assert_eq!(
        result(method(&mut machine, "IMediaControl", "Run", &[control])),
        VFW_E_NOT_CONNECTED
    );
    assert_eq!(
        result(method(&mut machine, "IMediaControl", "Pause", &[control])),
        VFW_E_NOT_CONNECTED
    );
    assert_eq!(
        result(method(
            &mut machine,
            "IMediaControl",
            "GetState",
            &[control, 100, data + 80]
        )),
        0
    );
    assert_eq!(machine.cpu.read_u32(data + 80).unwrap(), 0);
    assert_eq!(
        result(method(&mut machine, "IMediaControl", "Stop", &[control])),
        0
    );
    assert_eq!(
        result(method(
            &mut machine,
            "IMediaSeeking",
            "GetCapabilities",
            &[seeking, data + 80]
        )),
        0
    );
    assert_eq!(machine.cpu.read_u32(data + 80).unwrap(), 0);
    assert_eq!(
        result(method(
            &mut machine,
            "IMediaSeeking",
            "GetDuration",
            &[seeking, data + 80]
        )),
        E_NOTIMPL
    );
    assert_eq!(
        result(method(
            &mut machine,
            "IMediaEventEx",
            "GetEvent",
            &[events, data + 80, data + 84, data + 88, 0]
        )),
        E_ABORT
    );
    assert_eq!(
        result(method(
            &mut machine,
            "IMediaEventEx",
            "WaitForCompletion",
            &[events, 0, data + 80]
        )),
        E_ABORT
    );
    assert_eq!(machine.cpu.read_u32(data + 80).unwrap(), 0);
    assert_eq!(
        result(method(
            &mut machine,
            "IMediaEventEx",
            "GetEventHandle",
            &[events, data + 80]
        )),
        0
    );
    let handle = machine.cpu.read_u32(data + 80).unwrap();
    assert!(matches!(
        machine.sys.objects.get(handle),
        Some(Object::Event {
            manual_reset: true,
            signaled: false
        })
    ));
    let diagnostics = machine.ext.get::<adv_abi::ApiDiagnostics>();
    assert_eq!(diagnostics.stub_calls, 0);
    assert!(
        diagnostics
            .warnings
            .iter()
            .any(|((api, _), &count)| api.ends_with("IGraphBuilder::RenderFile") && count == 2)
    );
    for pointer in [graph, control, seeking, events] {
        release(&mut env(&mut machine), pointer);
    }
    assert!(machine.sys.objects.get(handle).is_none());
}

#[test]
fn empty_filter_enumeration_and_clone_own_independent_real_com_objects() {
    let (mut machine, data) = fixture();
    let graph = graph(&mut machine, data, IID_GRAPH);
    assert_eq!(
        result(method(
            &mut machine,
            "IGraphBuilder",
            "EnumFilters",
            &[graph, data + 80]
        )),
        0
    );
    let enumerator = machine.cpu.read_u32(data + 80).unwrap();
    assert_eq!(
        result(method(
            &mut machine,
            "IEnumFilters",
            "Next",
            &[enumerator, 1, data + 84, data + 88]
        )),
        1
    );
    assert_eq!(machine.cpu.read_u32(data + 88).unwrap(), 0);
    assert_eq!(
        result(method(
            &mut machine,
            "IEnumFilters",
            "Next",
            &[enumerator, 2, data + 84, 0]
        )),
        E_POINTER
    );
    assert_eq!(
        result(method(
            &mut machine,
            "IEnumFilters",
            "Next",
            &[enumerator, 1, data + 84, 0]
        )),
        1
    );
    assert_eq!(
        result(method(&mut machine, "IEnumFilters", "Reset", &[enumerator])),
        0
    );
    assert_eq!(
        result(method(
            &mut machine,
            "IEnumFilters",
            "Skip",
            &[enumerator, 1]
        )),
        E_INVALIDARG
    );
    assert_eq!(
        result(method(
            &mut machine,
            "IEnumFilters",
            "Clone",
            &[enumerator, data + 80]
        )),
        0
    );
    let clone = machine.cpu.read_u32(data + 80).unwrap();
    assert_ne!(clone, enumerator);
    assert_eq!(release(&mut env(&mut machine), graph), 0);
    assert_eq!(
        result(method(
            &mut machine,
            "IEnumFilters",
            "Next",
            &[clone, 1, data + 84, data + 88]
        )),
        1
    );
    assert_eq!(release(&mut env(&mut machine), clone), 0);
    assert_eq!(release(&mut env(&mut machine), enumerator), 0);
}

#[test]
fn event_configuration_and_positive_timeout_use_shared_wait_objects() {
    let (mut machine, data) = fixture();
    let events = graph(&mut machine, data, IID_EVENT_EX);
    assert_eq!(
        result(method(
            &mut machine,
            "IMediaEventEx",
            "SetNotifyWindow",
            &[events, 123, 0x8001, 7]
        )),
        0
    );
    assert_eq!(
        result(method(
            &mut machine,
            "IMediaEventEx",
            "SetNotifyFlags",
            &[events, 1]
        )),
        0
    );
    assert_eq!(
        result(method(
            &mut machine,
            "IMediaEventEx",
            "SetNotifyFlags",
            &[events, 2]
        )),
        E_INVALIDARG
    );
    assert_eq!(
        result(method(
            &mut machine,
            "IMediaEventEx",
            "GetNotifyFlags",
            &[events, data + 80]
        )),
        0
    );
    assert_eq!(machine.cpu.read_u32(data + 80).unwrap(), 1);
    let before = machine.sys.clock.now_us();
    let outcome = method(
        &mut machine,
        "IMediaEventEx",
        "GetEvent",
        &[events, data + 80, data + 84, data + 88, 17],
    );
    let Outcome::Block {
        wait: Wait::Object { handle, deadline },
        then,
        ..
    } = outcome
    else {
        panic!("positive timeout must wait in the machine");
    };
    assert_eq!(deadline, Some(before + 17_000));
    assert!(matches!(
        machine.sys.objects.get(handle),
        Some(Object::Event {
            signaled: false,
            ..
        })
    ));
    assert_eq!(
        result(then(
            &mut env(&mut machine),
            adv_machine::Resume::Woke(adv_machine::WakeReason::Timeout)
        )),
        E_ABORT
    );
    assert_eq!(release(&mut env(&mut machine), events), 0);
    assert!(machine.sys.objects.get(handle).is_none());
}

#[test]
fn failed_activation_and_partial_interface_registration_publish_nothing() {
    let (mut machine, data) = fixture();
    assert_eq!(
        result(create_graph(&mut env(&mut machine), IID_GRAPH, 1, data)),
        CLASS_E_NOAGGREGATION
    );
    assert_eq!(machine.cpu.read_u32(data).unwrap(), 0);
    assert_eq!(
        result(create_graph(&mut env(&mut machine), [0x42; 16], 0, data)),
        E_NOINTERFACE
    );
    machine.cpu.protect(data + 4096, 4096, Prot::READ).unwrap();
    assert_eq!(
        result(create_graph(
            &mut env(&mut machine),
            IID_GRAPH,
            0,
            data + 4096
        )),
        E_POINTER
    );
    assert!(machine.ext.try_get::<State>().is_none());
    machine.sys.objects = Objects::with_limits(ObjectLimits {
        aliases: 3,
        ..Default::default()
    });
    assert!(initialize(&mut env(&mut machine)));
    let probe = alloc(&mut env(&mut machine), 4).unwrap();
    free(&mut env(&mut machine), probe);
    assert_eq!(
        result(create_graph(&mut env(&mut machine), IID_GRAPH, 0, data)),
        E_OUTOFMEMORY
    );
    assert_eq!(machine.cpu.read_u32(data).unwrap(), 0);
    assert_eq!(machine.sys.objects.object_count(), 0);
    assert_eq!(
        machine
            .sys
            .objects
            .alias_count(machine.ext.get::<State>().com.namespace()),
        0
    );
    let after = alloc(&mut env(&mut machine), 4).unwrap();
    assert_eq!(
        after, probe,
        "every unpublished interface allocation was freed"
    );
    free(&mut env(&mut machine), after);
}

#[test]
fn generated_stdcall_thunks_execute_with_correct_stack_cleanup() {
    let (mut machine, data) = fixture();
    let graph = graph(&mut machine, data, IID_GRAPH);
    let seeking = query_interface(&mut machine, data, graph, IID_SEEKING);
    let table = machine.cpu.read_u32(seeking).unwrap();
    // SetRate takes a by-value double (two words) after this. An SDK-projected
    // callee pop of twelve bytes is essential even on its failure path.
    let target = machine.cpu.read_u32(table + 17 * 4).unwrap();
    use iced_x86::code_asm::*;
    let mut asm = CodeAssembler::new(32).unwrap();
    asm.mov(ebx, esp).unwrap();
    asm.push(0x3ff00000).unwrap();
    asm.push(0).unwrap();
    asm.push(seeking as i32).unwrap();
    asm.call(target as u64).unwrap();
    asm.mov(dword_ptr(data + 80), eax).unwrap();
    asm.mov(eax, esp).unwrap();
    asm.sub(eax, ebx).unwrap();
    asm.mov(dword_ptr(data + 84), eax).unwrap();
    asm.xor(eax, eax).unwrap();
    asm.ret().unwrap();
    let code = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, 4096, Prot::RWX)
        .unwrap();
    machine
        .cpu
        .write(code, &asm.assemble(code as u64).unwrap())
        .unwrap();
    let probe = machine.spawn("stdcall-probe", code, &[], 65536).unwrap();
    machine.sys.current = probe;
    machine.sys.terminate_other_contexts(0);
    let run = machine.run();
    assert!(matches!(run, RunResult::Exited(0)), "{run:?}");
    assert_eq!(machine.cpu.read_u32(data + 80).unwrap(), E_NOTIMPL);
    assert_eq!(machine.cpu.read_u32(data + 84).unwrap(), 0);
    release(&mut env(&mut machine), seeking);
    release(&mut env(&mut machine), graph);
}

#[cfg(windows)]
#[test]
fn sdk_compiled_empty_graph_matches_native_identity_state_enumeration_and_event_contract() {
    use adv_os::Process;
    use std::os::windows::process::CommandExt;
    use std::{path::Path, process::Command};
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let directory = root.join("target/dshow-sdk-tests");
    let build = Command::new("powershell.exe")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(root.join("tools/fixtures/build-dshow-empty-graph.ps1"))
        .arg("-OutputDirectory")
        .arg(&directory)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let native = directory.join("native");
    std::fs::create_dir_all(&native).unwrap();
    let status = Command::new(directory.join("fixture_dshow_empty_graph.exe"))
        .current_dir(&native)
        .creation_flags(0x08000000)
        .status()
        .unwrap();
    assert!(status.success(), "native exit {:?}", status.code());
    let reference = std::fs::read(native.join("dshow-empty-graph.bin")).unwrap();
    assert_eq!(reference.len(), 50 * 4);
    for jit in [false, true] {
        let profile = directory.join(if jit { "jit" } else { "interpreter" });
        let config =
            adv_win32::Win32Config::new(&directory, "fixture_dshow_empty_graph.exe", &profile);
        let files = adv_os::Vfs::new(&config.os);
        let mut registry = Registry::default();
        adv_win32::register(&mut registry);
        adv_win32::ole32::register(&mut registry);
        register(&mut registry);
        let mut machine = Machine::new(
            JitCpu::new(PARK_ADDR, jit).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        adv_win32::Win32::install(&mut machine, config).unwrap();
        install(&mut machine).unwrap();
        let exe = machine
            .load(&files, "fixture_dshow_empty_graph.exe")
            .unwrap();
        machine.start_process(exe).unwrap();
        let run = machine.run_virtual(2_000_000);
        assert!(matches!(run, RunResult::Exited(0)), "JIT {jit}: {run:?}");
        let actual = std::fs::read(profile.join("overlay/dshow-empty-graph.bin")).unwrap();
        assert_eq!(actual, reference, "SDK fixture, JIT enabled {jit}");
        assert_eq!(machine.ext.get::<adv_abi::ApiDiagnostics>().stub_calls, 0);
    }
}

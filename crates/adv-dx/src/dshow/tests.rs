use adv_abi::Registry;
use adv_cpu::JitCpu;
use adv_cpu::backend::{Guest, GuestExt, Prot, Reg};
use adv_machine::{Clock, Env, Machine, Outcome};

use super::*;

const DATA: u32 = 0x500000;
const STACK: u32 = 0x600000;

pub(super) fn machine() -> Machine<JitCpu> {
    let mut reg = Registry::default();
    adv_win32::register(&mut reg);
    adv_win32::ole32::register(&mut reg);
    register(&mut reg);
    let mut m = Machine::new(
        JitCpu::new(adv_machine::layout::PARK_ADDR, true).unwrap(),
        reg,
        Clock::virtual_time(),
    )
    .unwrap();
    adv_win32::Win32::install(
        &mut m,
        adv_win32::Win32Config::new(".", "dshow-test.exe", "target/dshow-test"),
    )
    .unwrap();
    install(&mut m);
    m.cpu.map(DATA, 4096, Prot::RW).unwrap();
    m.cpu.map(STACK, 4096, Prot::RW).unwrap();
    m
}

fn handle(m: &mut Machine<JitCpu>, module: &str, name: &str, args: &[u32]) -> u32 {
    m.resolve_import(module, name).unwrap();
    let thunk = m
        .sys
        .thunks
        .iter()
        .find(|(_, t)| t.module == module && t.name == name)
        .unwrap()
        .0;
    let handler = m.sys.thunks.get(thunk).f.handler;
    m.cpu.set_reg(Reg::Esp, STACK);
    for (i, &arg) in args.iter().enumerate() {
        m.cpu.write_u32(STACK + 4 + i as u32 * 4, arg).unwrap();
    }
    let outcome = handler(&mut Env {
        context: m.sys.current,
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk,
    });
    match outcome {
        Outcome::Return(value) => value,
        _ => panic!("{name} needed a guest call"),
    }
}

pub(super) fn method(m: &mut Machine<JitCpu>, name: &str, args: &[u32]) -> u32 {
    handle(m, "quartz.dll", name, args)
}

pub(super) fn activate(m: &mut Machine<JitCpu>, class: Iid, iid: Iid) -> u32 {
    m.cpu.write(DATA + 256, &class).unwrap();
    m.cpu.write(DATA + 272, &iid).unwrap();
    let hr = handle(
        m,
        "ole32.dll",
        "CoCreateInstance",
        &[DATA + 256, 0, 1, DATA + 272, DATA + 128],
    );
    assert_eq!(hr, S_OK);
    m.cpu.read_u32(DATA + 128).unwrap()
}

pub(super) fn query(m: &mut Machine<JitCpu>, object: u32, iid: Iid) -> u32 {
    m.cpu.write(DATA + 256, &iid).unwrap();
    let hr = method(
        m,
        "IMediaControl::QueryInterface",
        &[object, DATA + 256, DATA + 128],
    );
    assert_eq!(hr, S_OK);
    m.cpu.read_u32(DATA + 128).unwrap()
}

pub(super) fn env(m: &mut Machine<JitCpu>) -> Env<'_> {
    let thunk = m.sys.thunks.iter().next().unwrap().0;
    Env {
        context: m.sys.current,
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk,
    }
}

#[test]
fn disabled_or_invalid_source_rejects_connection_without_publishing_streams() {
    let mut m = machine();
    let graph = activate(&mut m, CLSID_FILTERGRAPH, IID_IGRAPHBUILDER);
    let splitter = activate(&mut m, CLSID_MPEG1_SPLITTER, IID_IBASEFILTER);
    assert_eq!(
        method(&mut m, "IFilterGraph2::AddFilter", &[graph, splitter, 0]),
        S_OK
    );
    let input = m.ext.get::<State>().filters[&splitter].pins[0];
    // An authored reader's Release target lets the test verify that the
    // QueryInterface reference is returned on failure, without a media asset.
    let reader = DATA + 512;
    let table = DATA + 520;
    let release_target = DATA + 544;
    m.cpu.write_u32(reader, table).unwrap();
    m.cpu.write_u32(table + 8, release_target).unwrap();
    let mt = MediaType {
        major: MEDIATYPE_STREAM,
        sub: MEDIASUBTYPE_MPEG1SYSTEM,
        fixed_size: true,
        temporal: false,
        sample_size: 1,
        format_type: GUID_NULL,
        format: Vec::new(),
    };
    let outcome =
        filter::attach_source(&mut env(&mut m), input, reader, reader, mt, vec![0x55; 32]);
    let Outcome::Call {
        target, args, then, ..
    } = outcome
    else {
        panic!("a rejected probe must release its reader");
    };
    assert_eq!(target, release_target);
    assert_eq!(args, [reader]);
    assert!(matches!(
        then(&mut env(&mut m), adv_machine::Resume::Returned(0)),
        Outcome::Return(VFW_E_UNSUPPORTED_STREAM)
    ));
    let state = m.ext.get::<State>();
    assert!(state.filters[&splitter].source.is_none());
    assert_eq!(state.filters[&splitter].pins, [input]);
    assert_eq!(state.pins[&input].peer, 0);
    assert!(state.sessions.is_empty());
    let diagnostics = m.ext.get::<adv_abi::ApiDiagnostics>();
    assert_eq!(diagnostics.stub_calls, 0);
    assert!(
        diagnostics
            .warnings
            .keys()
            .any(|(api, _)| api == "MPEG-1 Stream Splitter")
    );
    let events = query(&mut m, graph, IID_IMEDIAEVENTEX);
    assert_eq!(
        method(
            &mut m,
            "IMediaEventEx::GetEvent",
            &[events, DATA, DATA + 4, DATA + 8, 0]
        ),
        0x8000_4004
    );
}

#[test]
fn failed_task_memory_outputs_leave_the_shared_allocation_ledger_unchanged() {
    let mut m = machine();
    let decoder = activate(&mut m, CLSID_MPEG_VIDEO_CODEC, IID_IBASEFILTER);
    let pin = m.ext.get::<State>().filters[&decoder].pins[0];
    let read_only = 0x700000;
    m.cpu.map(read_only, 4096, Prot::READ).unwrap();
    let mt = MediaType::video_rgb(MEDIASUBTYPE_RGB24, 2, 2, 333_333);
    let before = m
        .ext
        .get_or_default::<adv_os::com::TaskMemory>()
        .allocations
        .clone();
    for output in [0, read_only] {
        assert_eq!(method(&mut m, "IPin::QueryId", &[pin, output]), E_POINTER);
        assert_eq!(m.ext.get::<adv_os::com::TaskMemory>().allocations, before);
        assert_eq!(mt.write(&mut env(&mut m), output), Err(E_POINTER));
        assert_eq!(m.ext.get::<adv_os::com::TaskMemory>().allocations, before);
    }
}

#[test]
fn pushed_samples_are_rejected_and_processed_output_is_checked_and_cleared() {
    let mut m = machine();
    let decoder = activate(&mut m, CLSID_MPEG_VIDEO_CODEC, IID_IBASEFILTER);
    let pin = m.ext.get::<State>().filters[&decoder].pins[0];
    let input = query(&mut m, pin, IID_IMEMINPUTPIN);
    assert_eq!(
        method(&mut m, "IMemInputPin::Receive", &[input, 0xdead_beef]),
        E_NOTIMPL
    );
    m.cpu.write_u32(DATA + 16, 3).unwrap();
    assert_eq!(
        method(
            &mut m,
            "IMemInputPin::ReceiveMultiple",
            &[input, 0xdead_beef, 3, DATA + 16]
        ),
        E_NOTIMPL
    );
    assert_eq!(m.cpu.read_u32(DATA + 16).unwrap(), 0);
    let read_only = 0x700000;
    m.cpu.map(read_only, 4096, Prot::READ).unwrap();
    m.cpu.write_u32(read_only, 37).unwrap();
    for output in [0, read_only] {
        assert_eq!(
            method(
                &mut m,
                "IMemInputPin::ReceiveMultiple",
                &[input, 0xdead_beef, 3, output]
            ),
            E_POINTER
        );
    }
    assert_eq!(m.cpu.read_u32(read_only).unwrap(), 37);
    let diagnostics = m.ext.get::<adv_abi::ApiDiagnostics>();
    assert_eq!(diagnostics.stub_calls, 0);
    for name in ["IMemInputPin::Receive", "IMemInputPin::ReceiveMultiple"] {
        assert_eq!(
            diagnostics.warnings.get(&(
                name.to_owned(),
                "pushed media samples are not supported".to_owned()
            )),
            Some(&1)
        );
    }
}

#[test]
fn graph_faces_share_identity_and_report_state() {
    let mut m = machine();
    let graph = activate(&mut m, CLSID_FILTERGRAPH, IID_IGRAPHBUILDER);
    let control = query(&mut m, graph, IID_IMEDIACONTROL);
    let seeking = query(&mut m, control, IID_IMEDIASEEKING);
    assert_eq!(query(&mut m, seeking, IID_IGRAPHBUILDER), graph);
    assert_eq!(
        method(&mut m, "IMediaControl::GetState", &[control, 0, DATA]),
        S_OK
    );
    assert_eq!(m.cpu.read_u32(DATA).unwrap(), graph::STOPPED);
    assert_eq!(
        method(&mut m, "IMediaSeeking::GetDuration", &[seeking, DATA]),
        S_OK
    );
    assert_eq!(m.cpu.read_u32(DATA).unwrap(), 0);
}

#[test]
fn system_filters_expose_pins_and_join_the_graph() {
    let mut m = machine();
    let graph = activate(&mut m, CLSID_FILTERGRAPH, IID_IGRAPHBUILDER);
    let splitter = activate(&mut m, CLSID_MPEG1_SPLITTER, IID_IBASEFILTER);
    let decoder = activate(&mut m, CLSID_MPEG_VIDEO_CODEC, IID_IBASEFILTER);
    for filter in [splitter, decoder] {
        assert_eq!(
            method(&mut m, "IFilterGraph2::AddFilter", &[graph, filter, 0]),
            S_OK
        );
    }
    assert_eq!(
        method(&mut m, "IBaseFilter::EnumPins", &[decoder, DATA]),
        S_OK
    );
    let pins = m.cpu.read_u32(DATA).unwrap();
    assert_eq!(
        method(&mut m, "IEnumPins::Next", &[pins, 4, DATA + 16, DATA + 8]),
        S_FALSE
    );
    assert_eq!(m.cpu.read_u32(DATA + 8).unwrap(), 2);
    let (input, output) = (
        m.cpu.read_u32(DATA + 16).unwrap(),
        m.cpu.read_u32(DATA + 20).unwrap(),
    );
    assert_eq!(method(&mut m, "IPin::QueryDirection", &[input, DATA]), S_OK);
    assert_eq!(m.cpu.read_u32(DATA).unwrap(), filter::INPUT);
    assert_eq!(
        method(&mut m, "IPin::QueryDirection", &[output, DATA]),
        S_OK
    );
    assert_eq!(m.cpu.read_u32(DATA).unwrap(), filter::OUTPUT);
    // A decoder offers no output types until its input is connected.
    assert_eq!(
        method(&mut m, "IPin::EnumMediaTypes", &[output, DATA]),
        S_OK
    );
    let types = m.cpu.read_u32(DATA).unwrap();
    assert_eq!(
        method(&mut m, "IEnumMediaTypes::Next", &[types, 1, DATA + 16, 0]),
        S_FALSE
    );
    // Splitter input proposes an MPEG-1 system stream.
    assert_eq!(
        method(&mut m, "IBaseFilter::EnumPins", &[splitter, DATA]),
        S_OK
    );
    let pins = m.cpu.read_u32(DATA).unwrap();
    assert_eq!(
        method(&mut m, "IEnumPins::Next", &[pins, 1, DATA + 16, 0]),
        S_OK
    );
    let input = m.cpu.read_u32(DATA + 16).unwrap();
    assert_eq!(method(&mut m, "IPin::EnumMediaTypes", &[input, DATA]), S_OK);
    let types = m.cpu.read_u32(DATA).unwrap();
    assert_eq!(
        method(&mut m, "IEnumMediaTypes::Next", &[types, 1, DATA + 16, 0]),
        S_OK
    );
    let mt = m.cpu.read_u32(DATA + 16).unwrap();
    let mut major = [0; 16];
    m.cpu.read(mt, &mut major).unwrap();
    assert_eq!(major, MEDIATYPE_STREAM);
}

#[test]
fn rgb_conversion_writes_bottom_up_rows() {
    let frame = adv_device::media::VideoFrame {
        pts_us: 0,
        width: 2,
        height: 2,
        bgra: vec![1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255, 10, 11, 12, 255],
    };
    let mt = MediaType::video_rgb(MEDIASUBTYPE_RGB24, 2, 2, 333_333);
    let bytes = stream::convert(&frame, &mt);
    // Stride pads 6 bytes to 8; the top source row lands last.
    assert_eq!(
        bytes,
        vec![7, 8, 9, 10, 11, 12, 0, 0, 1, 2, 3, 4, 5, 6, 0, 0]
    );
}

/// The SDK-compiled empty-graph probe must record the same identity, state,
/// enumeration and event results here as under native quartz.
#[cfg(windows)]
#[test]
fn sdk_compiled_empty_graph_matches_native_identity_state_enumeration_and_event_contract() {
    use adv_machine::RunResult;
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
            JitCpu::new(adv_machine::layout::PARK_ADDR, jit).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        adv_win32::Win32::install(&mut machine, config).unwrap();
        install(&mut machine);
        let exe = machine
            .load(&files, "fixture_dshow_empty_graph.exe")
            .unwrap();
        machine.start_process(exe).unwrap();
        let run = machine.run_virtual(2_000_000);
        assert!(matches!(run, RunResult::Exited(0)), "JIT {jit}: {run:?}");
        let actual = std::fs::read(profile.join("overlay/dshow-empty-graph.bin")).unwrap();
        assert_eq!(actual, reference, "SDK fixture, JIT enabled {jit}");
        // Diagnostics exist only once something reports a stub call.
        let stubs = machine
            .ext
            .try_get::<adv_abi::ApiDiagnostics>()
            .map_or(0, |diagnostics| diagnostics.stub_calls);
        assert_eq!(stubs, 0);
    }
}

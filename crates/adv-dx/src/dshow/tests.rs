use adv_abi::Registry;
use adv_cpu::JitCpu;
use adv_cpu::backend::{Guest, GuestExt, Prot, Reg};
use adv_machine::{Clock, Env, Machine, Outcome};

use super::*;

pub(crate) const DATA: u32 = 0x500000;
pub(crate) const STACK: u32 = 0x600000;

pub(crate) fn machine() -> Machine<JitCpu> {
    let mut reg = Registry::default();
    crate::test_support::register(&mut reg);
    register(&mut reg);
    for (slot, argc) in [
        (0, 3),
        (1, 1),
        (2, 1),
        (8, 4),
        (9, 4),
        (12, 5),
        (16, 1),
        (17, 2),
        (18, 1),
        (19, 1),
        (21, 8),
        (22, 5),
        (100, 2),
        (114, 1),
        (115, 1),
        (116, 1),
        (117, 7),
    ] {
        reg.add_with_data(
            "authored.dmo",
            &format!("slot{slot}"),
            Conv::Stdcall { argc },
            fixture_dmo,
            slot as u64,
        );
    }
    crate::wmvcore::register(&mut reg);
    let mut m = Machine::new(
        JitCpu::new(adv_machine::layout::PARK_ADDR, true).unwrap(),
        reg,
        Clock::virtual_time(),
    )
    .unwrap();
    crate::test_support::install(&mut m);
    install(&mut m);
    m.cpu.map(DATA, 4096, Prot::RW).unwrap();
    m.cpu.map(STACK, 4096, Prot::RW).unwrap();
    m
}

pub(crate) fn handle(m: &mut Machine<JitCpu>, module: &str, name: &str, args: &[u32]) -> u32 {
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
    let outcome = crate::test_support::activate(&mut env(m), class, iid, 0, DATA + 128);
    let Outcome::Return(hr) = outcome else {
        panic!("activation needed a guest call")
    };
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

pub(crate) fn env(m: &mut Machine<JitCpu>) -> Env<'_> {
    let thunk = m.sys.thunks.iter().next().unwrap().0;
    Env {
        context: m.sys.current,
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk,
    }
}

// spec: MEDIA-FAIL-1
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
// spec: MEDIA-STREAM-2
fn pushed_samples_are_rejected_and_processed_output_is_checked_and_cleared() {
    let mut m = machine();
    let splitter = activate(&mut m, CLSID_MPEG1_SPLITTER, IID_IBASEFILTER);
    let pin = m.ext.get::<State>().filters[&splitter].pins[0];
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
// spec: MEDIA-GRAPH-1
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
    // spec: MEDIA-GRAPH-15
    // Nothing in an empty graph seeks.
    assert_eq!(
        method(&mut m, "IMediaSeeking::GetDuration", &[seeking, DATA]),
        E_NOTIMPL
    );
}

#[test]
// spec: MEDIA-GRAPH-9
fn event_timeouts_leave_no_event_code() {
    let mut m = machine();
    let graph = activate(&mut m, CLSID_FILTERGRAPH, IID_IGRAPHBUILDER);
    let events = query(&mut m, graph, IID_IMEDIAEVENTEX);
    m.cpu.write_u32(DATA, 1).unwrap();
    assert_eq!(
        method(
            &mut m,
            "IMediaEventEx::GetEvent",
            &[events, DATA, DATA + 4, DATA + 8, 0]
        ),
        0x8000_4004
    );
    assert_eq!(m.cpu.read_u32(DATA).unwrap(), 0);
}

#[test]
// spec: MEDIA-GRAPH-21, MEDIA-GRAPH-22
fn completion_and_disabled_notification_follow_quartz() {
    const EC_COMPLETE: u32 = 1;
    const EC_STATUS: u32 = 0x8000;
    const E_ABORT: u32 = 0x8000_4004;
    let mut m = machine();
    let graph = activate(&mut m, CLSID_FILTERGRAPH, IID_IGRAPHBUILDER);
    let events = query(&mut m, graph, IID_IMEDIAEVENTEX);
    let sink = query(&mut m, graph, IID_IMEDIAEVENTSINK);
    let control = query(&mut m, graph, IID_IMEDIACONTROL);
    let get_event = |m: &mut Machine<JitCpu>| {
        method(
            m,
            "IMediaEventEx::GetEvent",
            &[events, DATA, DATA + 4, DATA + 8, 0],
        )
    };
    let notify = |m: &mut Machine<JitCpu>, code: u32| {
        assert_eq!(
            method(m, "IMediaEventSink::Notify", &[sink, code, 0, 0]),
            S_OK
        );
    };
    let wait =
        |m: &mut Machine<JitCpu>| method(m, "IMediaEventEx::WaitForCompletion", &[events, 0, DATA]);
    assert_eq!(method(&mut m, "IMediaControl::Run", &[control]), S_OK);
    m.ext
        .get_mut::<State>()
        .graphs
        .get_mut(&graph)
        .unwrap()
        .expected = 1;

    // Disabling notification discards the queue and drops later events, but
    // completion still ends WaitForCompletion.
    notify(&mut m, EC_STATUS);
    assert_eq!(
        method(&mut m, "IMediaEventEx::SetNotifyFlags", &[events, 1]),
        S_OK
    );
    assert_eq!(get_event(&mut m), E_ABORT);
    notify(&mut m, EC_STATUS);
    assert_eq!(wait(&mut m), E_ABORT);
    notify(&mut m, EC_COMPLETE);
    assert_eq!(get_event(&mut m), E_ABORT);
    assert_eq!(wait(&mut m), S_OK);
    assert_eq!(m.cpu.read_u32(DATA).unwrap(), EC_COMPLETE);

    // With notification, completion is queued and stays queued for GetEvent;
    // a new run removes it and the completion.
    assert_eq!(
        method(&mut m, "IMediaEventEx::SetNotifyFlags", &[events, 0]),
        S_OK
    );
    assert_eq!(method(&mut m, "IMediaControl::Pause", &[control]), S_OK);
    assert_eq!(method(&mut m, "IMediaControl::Run", &[control]), S_OK);
    assert_eq!(wait(&mut m), E_ABORT);
    m.ext
        .get_mut::<State>()
        .graphs
        .get_mut(&graph)
        .unwrap()
        .expected = 1;
    notify(&mut m, EC_COMPLETE);
    assert_eq!(wait(&mut m), S_OK);
    assert_eq!(get_event(&mut m), S_OK);
    assert_eq!(m.cpu.read_u32(DATA).unwrap(), EC_COMPLETE);
    notify(&mut m, EC_COMPLETE);
    assert_eq!(method(&mut m, "IMediaControl::Pause", &[control]), S_OK);
    notify(&mut m, EC_STATUS);
    assert_eq!(method(&mut m, "IMediaControl::Run", &[control]), S_OK);
    assert_eq!(get_event(&mut m), S_OK);
    assert_eq!(m.cpu.read_u32(DATA).unwrap(), EC_STATUS);
    assert_eq!(get_event(&mut m), E_ABORT);
}

/// An expected guest-call argument that matches any value, such as a pointer
/// the host allocated.
const ANY: u32 = u32::MAX;

/// Enter a quartz method and return its first outcome without resuming it.
pub(crate) fn start_method(m: &mut Machine<JitCpu>, name: &str, args: &[u32]) -> Outcome {
    m.resolve_import("quartz.dll", name).unwrap();
    let thunk = m
        .sys
        .thunks
        .iter()
        .find(|(_, t)| t.module == "quartz.dll" && t.name == name)
        .unwrap()
        .0;
    let handler = m.sys.thunks.get(thunk).f.handler;
    m.cpu.set_reg(Reg::Esp, STACK);
    for (i, &arg) in args.iter().enumerate() {
        m.cpu.write_u32(STACK + 4 + i as u32 * 4, arg).unwrap();
    }
    handler(&mut Env {
        context: m.sys.current,
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk,
    })
}

/// Run a quartz method that calls back into the guest exactly once per item
/// of `expected`, checking each call's target and arguments.
fn method_calls(
    m: &mut Machine<JitCpu>,
    name: &str,
    args: &[u32],
    expected: &[(u32, &[u32], u32)],
) -> u32 {
    let mut outcome = start_method(m, name, args);
    for &(target, args, value) in expected {
        let Outcome::Call {
            target: called,
            args: passed,
            then,
            ..
        } = outcome
        else {
            panic!("{name} must call {target:#x}");
        };
        let passed: Vec<u32> = passed
            .iter()
            .zip(args.iter().chain(std::iter::repeat(&0)))
            .map(|(&p, &a)| if a == ANY { ANY } else { p })
            .collect();
        assert_eq!((called, passed.as_slice()), (target, args));
        outcome = then(&mut env(m), adv_machine::Resume::Returned(value));
    }
    match outcome {
        Outcome::Return(value) => value,
        _ => panic!("{name} made an unexpected guest call"),
    }
}

#[test]
// spec: MEDIA-GRAPH-1
fn graph_holds_its_site_and_answers_get_site_through_it() {
    let mut m = machine();
    let graph = activate(&mut m, CLSID_FILTERGRAPH, IID_IGRAPHBUILDER);
    let with_site = query(&mut m, graph, IID_IOBJECTWITHSITE);
    let (output, iid) = (DATA + 16, DATA + 256);
    m.cpu.write_u32(output, 0xdead).unwrap();
    assert_eq!(
        method(
            &mut m,
            "IObjectWithSite::GetSite",
            &[with_site, iid, output]
        ),
        0x8000_4005
    );
    assert_eq!(m.cpu.read_u32(output).unwrap(), 0);
    // Two guest sites whose QueryInterface, AddRef and Release are distinct targets.
    let sites = [DATA + 512, DATA + 576];
    for (i, &site) in sites.iter().enumerate() {
        let table = site + 4;
        m.cpu.write_u32(site, table).unwrap();
        for slot in 0..3 {
            m.cpu
                .write_u32(table + slot * 4, 0x7000_0000 + (i as u32) * 16 + slot)
                .unwrap();
        }
    }
    let (query_first, add_first, release_first) = (0x7000_0000, 0x7000_0001, 0x7000_0002);
    let (add_second, release_second) = (0x7000_0011, 0x7000_0012);
    let set = "IObjectWithSite::SetSite";
    let first: &[u32] = &[sites[0]];
    assert_eq!(
        method_calls(
            &mut m,
            set,
            &[with_site, sites[0]],
            &[(add_first, first, 2)]
        ),
        S_OK
    );
    assert_eq!(
        method_calls(
            &mut m,
            "IObjectWithSite::GetSite",
            &[with_site, iid, output],
            &[(query_first, &[sites[0], iid, output], S_OK)],
        ),
        S_OK
    );
    let second: &[u32] = &[sites[1]];
    assert_eq!(
        method_calls(
            &mut m,
            set,
            &[with_site, sites[1]],
            &[(add_second, second, 2), (release_first, first, 1)],
        ),
        S_OK
    );
    // Releasing the graph's last references returns the site's reference.
    method(&mut m, "IObjectWithSite::Release", &[with_site]);
    assert_eq!(
        method_calls(
            &mut m,
            "IFilterGraph2::Release",
            &[graph],
            &[(release_second, second, 1)],
        ),
        0
    );
    assert!(m.ext.get::<State>().graphs.is_empty());
}

// spec: MEDIA-GRAPH-11, MEDIA-GRAPH-12
#[test]
fn graph_names_members_counts_changes_and_hands_out_its_clock() {
    let mut m = machine();
    let graph = activate(&mut m, CLSID_FILTERGRAPH, IID_IGRAPHBUILDER);
    let version = query(&mut m, graph, IID_IGRAPHVERSION);
    let media = query(&mut m, graph, IID_IMEDIAFILTER);
    let filters = [DATA + 512, DATA + 640];
    for (i, &filter) in filters.iter().enumerate() {
        m.cpu.write_u32(filter, filter + 4).unwrap();
        for slot in 0..16 {
            let target = 0x7200_0000 + (i as u32) * 0x100 + slot;
            m.cpu.write_u32(filter + 4 + slot * 4, target).unwrap();
        }
    }
    let clock = DATA + 768;
    m.cpu.write_u32(clock, clock + 4).unwrap();
    for slot in 0..3 {
        m.cpu
            .write_u32(clock + 4 + slot * 4, 0x7300_0000 + slot)
            .unwrap();
    }
    let output = DATA + 16;
    let versioned = |m: &mut Machine<JitCpu>| {
        assert_eq!(
            method(m, "IGraphVersion::QueryVersion", &[version, output]),
            S_OK
        );
        m.cpu.read_u32(output).unwrap()
    };
    assert_eq!(versioned(&mut m), 0);
    let name = DATA + 64;
    let wide = |text: &str| -> Vec<u8> {
        text.encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect()
    };
    // A nameless filter takes the counter's first name and no clock yet.
    let [first, second] = filters;
    assert_eq!(
        method_calls(
            &mut m,
            "IFilterGraph2::AddFilter",
            &[graph, first, 0],
            &[
                (0x7200_0001, &[first], 2),
                (0x7200_000d, &[first, graph, ANY], S_OK),
                (0x7200_0008, &[first, 0], S_OK),
            ],
        ),
        S_OK
    );
    assert_eq!(versioned(&mut m), 1);
    m.cpu.write(name, &wide("0001")).unwrap();
    // A taken name gets the counter appended.
    assert_eq!(
        method_calls(
            &mut m,
            "IFilterGraph2::AddFilter",
            &[graph, second, name],
            &[
                (0x7200_0101, &[second], 2),
                (0x7200_010d, &[second, graph, ANY], S_OK),
                (0x7200_0108, &[second, 0], S_OK),
            ],
        ),
        VFW_S_DUPLICATE_NAME
    );
    m.cpu.write(name, &wide("0001 0002")).unwrap();
    assert_eq!(
        method_calls(
            &mut m,
            "IFilterGraph2::FindFilterByName",
            &[graph, name, output],
            &[(0x7200_0101, &[second], 3)],
        ),
        S_OK
    );
    assert_eq!(m.cpu.read_u32(output).unwrap(), second);
    // Every member receives the clock the graph keeps and hands back.
    assert_eq!(
        method_calls(
            &mut m,
            "IMediaFilter::SetSyncSource",
            &[media, clock],
            &[
                (0x7200_0008, &[first, clock], S_OK),
                (0x7200_0108, &[second, clock], S_OK),
                (0x7300_0001, &[clock], 2),
            ],
        ),
        S_OK
    );
    assert_eq!(
        method_calls(
            &mut m,
            "IMediaFilter::GetSyncSource",
            &[media, output],
            &[(0x7300_0001, &[clock], 3)],
        ),
        S_OK
    );
    assert_eq!(m.cpu.read_u32(output).unwrap(), clock);
    assert_eq!(
        method_calls(
            &mut m,
            "IMediaFilter::SetSyncSource",
            &[media, 0],
            &[
                (0x7200_0008, &[first, 0], S_OK),
                (0x7200_0108, &[second, 0], S_OK),
                (0x7300_0002, &[clock], 1),
            ],
        ),
        S_OK
    );
    assert_eq!(
        method(&mut m, "IMediaFilter::GetSyncSource", &[media, output]),
        S_FALSE
    );
    assert_eq!(m.cpu.read_u32(output).unwrap(), 0);
    // Automation interfaces describe themselves with one type library entry.
    let video = query(&mut m, graph, IID_IBASICVIDEO2);
    assert_eq!(
        method(&mut m, "IBasicVideo2::GetTypeInfoCount", &[video, output]),
        S_OK
    );
    assert_eq!(m.cpu.read_u32(output).unwrap(), 1);
}

// spec: MEDIA-GRAPH-17
#[test]
fn graph_controls_without_a_renderer_report_what_windows_reports() {
    let mut m = machine();
    let graph = activate(&mut m, CLSID_FILTERGRAPH, IID_IGRAPHBUILDER);
    let audio = query(&mut m, graph, IID_IBASICAUDIO);
    let video = query(&mut m, graph, IID_IBASICVIDEO2);
    let window = query(&mut m, graph, IID_IVIDEOWINDOW);
    assert_eq!(
        method(&mut m, "IBasicAudio::put_Volume", &[audio, -10i32 as u32]),
        E_NOTIMPL
    );
    assert_eq!(
        method(&mut m, "IBasicAudio::get_Balance", &[audio, DATA]),
        E_NOTIMPL
    );
    assert_eq!(
        method(&mut m, "IBasicVideo2::get_BitRate", &[video, DATA]),
        E_NOINTERFACE
    );
    assert_eq!(
        method(&mut m, "IVideoWindow::SetWindowForeground", &[window, 0]),
        E_NOINTERFACE
    );
}

// spec: MEDIA-GRAPH-10
#[test]
fn aggregated_graph_keeps_its_inner_count_and_forwards_other_faces_to_the_outer() {
    let mut m = machine();
    let (outer, output) = (DATA + 512, DATA + 128);
    m.cpu.write_u32(outer, outer + 4).unwrap();
    for slot in 0..3 {
        m.cpu
            .write_u32(outer + 4 + slot * 4, 0x7100_0000 + slot)
            .unwrap();
    }
    let (outer_query, outer_add, outer_release) = (0x7100_0000, 0x7100_0001, 0x7100_0002);
    let create = |m: &mut Machine<JitCpu>, iid| {
        m.cpu.write_u32(output, 0xdead).unwrap();
        let outcome =
            crate::test_support::activate(&mut env(m), CLSID_FILTERGRAPH, iid, outer, output);
        let Outcome::Return(hr) = outcome else {
            panic!("activation needed a guest call")
        };
        (hr, m.cpu.read_u32(output).unwrap())
    };
    assert_eq!(create(&mut m, IID_IFILTERGRAPH2), (E_NOINTERFACE, 0));
    let (hr, inner) = create(&mut m, adv_os::com::IUNKNOWN);
    assert_eq!(hr, S_OK);
    assert_eq!(method(&mut m, "IUnknown::AddRef", &[inner]), 2);
    assert_eq!(method(&mut m, "IUnknown::Release", &[inner]), 1);
    let iid = DATA + 256;
    m.cpu.write(iid, &adv_os::com::IUNKNOWN).unwrap();
    assert_eq!(
        method(&mut m, "IUnknown::QueryInterface", &[inner, iid, output]),
        S_OK
    );
    assert_eq!(m.cpu.read_u32(output).unwrap(), inner);
    assert_eq!(method(&mut m, "IUnknown::Release", &[inner]), 1);
    // Other interfaces retain the outer object, never the inner count.
    m.cpu.write(iid, &IID_IFILTERGRAPH2).unwrap();
    let outer_only: &[u32] = &[outer];
    assert_eq!(
        method_calls(
            &mut m,
            "IUnknown::QueryInterface",
            &[inner, iid, output],
            &[(outer_add, outer_only, 2)],
        ),
        S_OK
    );
    let graph = m.cpu.read_u32(output).unwrap();
    assert_ne!(graph, inner);
    let unknown_iid = DATA + 272;
    m.cpu.write(unknown_iid, &[0x5a; 16]).unwrap();
    assert_eq!(
        method(
            &mut m,
            "IUnknown::QueryInterface",
            &[inner, unknown_iid, output]
        ),
        E_NOINTERFACE
    );
    assert_eq!(m.cpu.read_u32(output).unwrap(), 0);
    for (iid, args) in [
        (iid, [graph, iid, output]),
        (unknown_iid, [graph, unknown_iid, output]),
    ] {
        assert_eq!(
            method_calls(
                &mut m,
                "IFilterGraph2::QueryInterface",
                &args,
                &[(outer_query, &[outer, iid, output], S_OK)],
            ),
            S_OK
        );
    }
    assert_eq!(
        method_calls(
            &mut m,
            "IMediaControl::AddRef",
            &[graph + 8],
            &[(outer_add, outer_only, 3)],
        ),
        3
    );
    assert_eq!(
        method_calls(
            &mut m,
            "IFilterGraph2::Release",
            &[graph],
            &[(outer_release, outer_only, 1)],
        ),
        1
    );
    assert_eq!(method(&mut m, "IUnknown::Release", &[inner]), 0);
    assert!(m.ext.get::<State>().graphs.is_empty());
}

// spec: MEDIA-GRAPH-10
#[test]
fn aggregated_host_filters_hand_out_their_inner_unknown_and_forward_to_the_outer() {
    let mut m = machine();
    let (outer, output, iid) = (DATA + 512, DATA + 128, DATA + 256);
    m.cpu.write_u32(outer, outer + 4).unwrap();
    for slot in 0..3 {
        m.cpu
            .write_u32(outer + 4 + slot * 4, 0x7100_0000 + slot)
            .unwrap();
    }
    let (outer_add, outer_release) = (0x7100_0001, 0x7100_0002);
    let outer_only: &[u32] = &[outer];
    for class in [
        CLSID_MPEG1_SPLITTER,
        CLSID_MPEG_VIDEO_CODEC,
        CLSID_MPEG_AUDIO_CODEC,
        CLSID_WMASF_READER,
        vmr::CLSID_VIDEO_MIXING_RENDERER,
    ] {
        let create = |m: &mut Machine<JitCpu>, iid| {
            m.cpu.write_u32(output, 0xdead).unwrap();
            let outcome = crate::test_support::activate(&mut env(m), class, iid, outer, output);
            let Outcome::Return(hr) = outcome else {
                panic!("activation needed a guest call")
            };
            (hr, m.cpu.read_u32(output).unwrap())
        };
        assert_eq!(create(&mut m, IID_IBASEFILTER), (E_NOINTERFACE, 0));
        let (hr, inner) = create(&mut m, adv_os::com::IUNKNOWN);
        assert_eq!(hr, S_OK);
        assert_eq!(method(&mut m, "IUnknown::AddRef", &[inner]), 2);
        assert_eq!(method(&mut m, "IUnknown::Release", &[inner]), 1);
        m.cpu.write(iid, &IID_IBASEFILTER).unwrap();
        assert_eq!(
            method_calls(
                &mut m,
                "IUnknown::QueryInterface",
                &[inner, iid, output],
                &[(outer_add, outer_only, 2)],
            ),
            S_OK
        );
        let filter = m.cpu.read_u32(output).unwrap();
        assert_ne!(filter, inner);
        assert_eq!(
            method_calls(
                &mut m,
                "IBaseFilter::Release",
                &[filter],
                &[(outer_release, outer_only, 1)],
            ),
            1
        );
        assert_eq!(method(&mut m, "IUnknown::Release", &[inner]), 0);
    }
    assert!(m.ext.get::<State>().filters.is_empty());
}

#[test]
// spec: MEDIA-GRAPH-1
fn filter_enumerators_fall_out_of_sync_on_removal_until_reset() {
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
        method(&mut m, "IFilterGraph2::EnumFilters", &[graph, DATA]),
        S_OK
    );
    let filters = m.cpu.read_u32(DATA).unwrap();
    assert_eq!(
        method(&mut m, "IFilterGraph2::RemoveFilter", &[graph, splitter]),
        S_OK
    );
    assert_eq!(
        method(&mut m, "IEnumFilters::Next", &[filters, 1, DATA + 16, 0]),
        VFW_E_ENUM_OUT_OF_SYNC
    );
    assert_eq!(
        method(&mut m, "IEnumFilters::Skip", &[filters, 1]),
        VFW_E_ENUM_OUT_OF_SYNC
    );
    // Reset takes the live list, which now holds only the decoder; skipping
    // from its end is refused.
    assert_eq!(method(&mut m, "IEnumFilters::Reset", &[filters]), S_OK);
    assert_eq!(method(&mut m, "IEnumFilters::Skip", &[filters, 2]), S_FALSE);
    assert_eq!(
        method(&mut m, "IEnumFilters::Skip", &[filters, 1]),
        E_INVALIDARG
    );
}

#[test]
// spec: MEDIA-GRAPH-23
fn removal_disconnects_guest_pins_and_keeps_a_filter_that_refuses() {
    let mut m = machine();
    let graph = activate(&mut m, CLSID_FILTERGRAPH, IID_IGRAPHBUILDER);
    // Guest filter, pin enumerator, pin and connected peer.
    let objects = [DATA + 0x400, DATA + 0x500, DATA + 0x600, DATA + 0x700];
    for (i, obj) in objects.into_iter().enumerate() {
        m.cpu.write_u32(obj, obj + 4).unwrap();
        for slot in 0..18u32 {
            m.cpu
                .write_u32(obj + 4 + slot * 4, 0x7200_0000 + (i as u32) * 0x100 + slot)
                .unwrap();
        }
    }
    let [filter, pins, pin, peer] = objects;
    assert_eq!(
        method_calls(
            &mut m,
            "IFilterGraph2::AddFilter",
            &[graph, filter, 0],
            &[
                (0x7200_0001, &[filter], 2),
                (0x7200_000d, &[filter, graph, ANY], S_OK),
                (0x7200_0008, &[filter, 0], S_OK),
            ],
        ),
        S_OK
    );
    // EnumPins, Next and ConnectedTo hand out the objects; the peer refuses
    // Disconnect, so every reference is returned and the filter stays.
    let mut outcome = start_method(&mut m, "IFilterGraph2::RemoveFilter", &[graph, filter]);
    for (target, output, value) in [
        (0x7200_000a, Some((1, pins)), S_OK),
        (0x7200_0103, Some((2, pin)), S_OK),
        (0x7200_0206, Some((1, peer)), S_OK),
        (0x7200_0305, None, VFW_E_NOT_STOPPED),
        (0x7200_0302, None, 0),
        (0x7200_0202, None, 0),
        (0x7200_0102, None, 0),
    ] {
        let Outcome::Call {
            target: called,
            args,
            then,
            ..
        } = outcome
        else {
            panic!("RemoveFilter must call {target:#x}");
        };
        assert_eq!(called, target);
        if let Some((index, object)) = output {
            m.cpu.write_u32(args[index], object).unwrap();
        }
        outcome = then(&mut env(&mut m), adv_machine::Resume::Returned(value));
    }
    assert!(matches!(outcome, Outcome::Return(VFW_E_NOT_STOPPED)));
    assert_eq!(
        m.ext.get::<State>().graphs[&graph].members.len(),
        1,
        "a refused removal keeps the member"
    );
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
    // spec: MEDIA-GRAPH-24
    // Feeding a filter's output back into its own input closes a loop.
    let outcome = start_method(
        &mut m,
        "IFilterGraph2::ConnectDirect",
        &[graph, output, input, 0],
    );
    assert_eq!(finish_calls(&mut m, outcome), VFW_E_CIRCULAR_GRAPH);
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
// spec: MEDIA-STREAM-1
fn rgb_conversion_writes_bottom_up_rows() {
    let bgra = vec![1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255, 10, 11, 12, 255];
    let mt = MediaType::video_rgb(MEDIASUBTYPE_RGB24, 2, 2, 333_333);
    let bytes = stream::convert_bgra(2, 2, &bgra, &mt);
    // Stride pads 6 bytes to 8; the top source row lands last.
    assert_eq!(
        bytes,
        vec![7, 8, 9, 10, 11, 12, 0, 0, 1, 2, 3, 4, 5, 6, 0, 0]
    );
}

#[test]
fn compressed_media_types_round_trip_codec_formats() {
    for format in [
        CodecFormat::Video {
            fourcc: u32::from_le_bytes(*b"WMV3"),
            width: 640,
            height: 480,
            bit_count: 24,
            frame_100ns: 333_667,
            extradata: vec![0x4f, 0xd1, 0x1a, 0x01],
        },
        CodecFormat::Audio {
            format_tag: 0x161,
            channels: 2,
            rate: 44_100,
            bytes_per_second: 16_000,
            block_align: 2_973,
            bits: 16,
            extradata: vec![0, 0x88, 0, 0, 0x17, 0, 0, 0, 0, 0],
        },
    ] {
        let mt = MediaType::compressed(&format, 128_000);
        assert_eq!(mt.codec_format(), Some(format));
    }
}

#[test]
// spec: MEDIA-DMO-1
fn dmo_wrapper_publishes_decoder_pins_once_for_windows_media_decoders() {
    let mut m = machine();
    let filter = activate(&mut m, dmo::CLSID_DMO_WRAPPER, IID_IBASEFILTER);
    let wrapper = query(&mut m, filter, dmo::IID_IDMOWRAPPERFILTER);
    let pins = |m: &mut Machine<JitCpu>| {
        assert_eq!(method(m, "IBaseFilter::EnumPins", &[filter, DATA]), S_OK);
        let pins = m.cpu.read_u32(DATA).unwrap();
        method(m, "IEnumPins::Next", &[pins, 4, DATA + 16, DATA + 8]);
        m.cpu.read_u32(DATA + 8).unwrap()
    };
    assert_eq!(pins(&mut m), 0);
    // Category GUIDs are ignored; the DMO class alone selects the decoder.
    m.cpu.write(DATA + 64, &[0x11; 16]).unwrap();
    m.cpu.write(DATA + 80, &[0; 16]).unwrap();
    assert_eq!(
        method(
            &mut m,
            "IDMOWrapperFilter::Init",
            &[wrapper, DATA + 64, DATA + 80]
        ),
        dmo::REGDB_E_CLASSNOTREG
    );
    assert_eq!(pins(&mut m), 0);
    m.cpu.write(DATA + 64, &dmo::CLSID_WMV_DECODER).unwrap();
    assert_eq!(
        method(
            &mut m,
            "IDMOWrapperFilter::Init",
            &[wrapper, DATA + 64, DATA + 80]
        ),
        S_OK
    );
    assert_eq!(pins(&mut m), 2);
    let (input, output) = (
        m.cpu.read_u32(DATA + 16).unwrap(),
        m.cpu.read_u32(DATA + 20).unwrap(),
    );
    for (pin, dir) in [(input, filter::INPUT), (output, filter::OUTPUT)] {
        assert_eq!(method(&mut m, "IPin::QueryDirection", &[pin, DATA]), S_OK);
        assert_eq!(m.cpu.read_u32(DATA).unwrap(), dir);
    }
    // An unconnected decoder offers no output types.
    assert_eq!(
        method(&mut m, "IPin::EnumMediaTypes", &[output, DATA]),
        S_OK
    );
    let types = m.cpu.read_u32(DATA).unwrap();
    assert_eq!(
        method(&mut m, "IEnumMediaTypes::Next", &[types, 1, DATA + 16, 0]),
        S_FALSE
    );
    assert_eq!(
        method(
            &mut m,
            "IDMOWrapperFilter::Init",
            &[wrapper, DATA + 64, DATA + 80]
        ),
        VFW_E_WRONG_STATE
    );
    assert_eq!(pins(&mut m), 2);
}

#[test]
fn allocator_get_buffer_blocks_until_release_or_decommit_and_honors_nowait() {
    let mut m = machine();
    let allocator = sample::create_allocator(&mut env(&mut m), 1, 64).unwrap();
    assert_eq!(
        method(
            &mut m,
            "IMemAllocator::GetBuffer",
            &[allocator, DATA, 0, 0, 0]
        ),
        S_OK
    );
    let first = m.cpu.read_u32(DATA).unwrap();
    assert_ne!(first, 0);
    // AM_GBF_NOWAIT (4) returns VFW_E_TIMEOUT immediately when all buffers are out.
    assert_eq!(
        method(
            &mut m,
            "IMemAllocator::GetBuffer",
            &[allocator, DATA + 4, 0, 0, 4]
        ),
        VFW_E_TIMEOUT
    );

    let start_blocking_get = |m: &mut Machine<JitCpu>, out: u32| -> Outcome {
        m.resolve_import("quartz.dll", "IMemAllocator::GetBuffer")
            .unwrap();
        let thunk = m
            .sys
            .thunks
            .iter()
            .find(|(_, t)| t.module == "quartz.dll" && t.name == "IMemAllocator::GetBuffer")
            .unwrap()
            .0;
        let handler = m.sys.thunks.get(thunk).f.handler;
        m.cpu.set_reg(Reg::Esp, STACK);
        for (i, arg) in [allocator, out, 0, 0, 0].into_iter().enumerate() {
            m.cpu.write_u32(STACK + 4 + i as u32 * 4, arg).unwrap();
        }
        handler(&mut Env {
            context: m.sys.current,
            cpu: &mut m.cpu,
            sys: &mut m.sys,
            ext: &mut m.ext,
            thunk,
        })
    };

    let Outcome::Block {
        wait: adv_machine::Wait::Object {
            handle,
            deadline: None,
        },
        then,
        ..
    } = start_blocking_get(&mut m, DATA + 8)
    else {
        panic!("exhausted committed allocator must block on its wake event");
    };
    assert_eq!(method(&mut m, "IMediaSample::Release", &[first]), 0);
    assert!(matches!(
        m.sys.objects.get(handle),
        Some(adv_machine::Object::Event { signaled: true, .. })
    ));
    let resumed = then(
        &mut env(&mut m),
        adv_machine::Resume::Woke(adv_machine::WakeReason::Signaled),
    );
    assert!(matches!(resumed, Outcome::Return(S_OK)));
    let recycled = m.cpu.read_u32(DATA + 8).unwrap();
    assert_eq!(recycled, first);

    let Outcome::Block { then, .. } = start_blocking_get(&mut m, DATA + 12) else {
        panic!("allocator must block again once its buffer is taken");
    };
    assert_eq!(
        method(&mut m, "IMemAllocator::Decommit", &[allocator]),
        S_OK
    );
    let decommitted = then(
        &mut env(&mut m),
        adv_machine::Resume::Woke(adv_machine::WakeReason::Signaled),
    );
    assert!(matches!(decommitted, Outcome::Return(VFW_E_NOT_COMMITTED)));
    assert_eq!(method(&mut m, "IMediaSample::Release", &[recycled]), 0);
    assert_eq!(method(&mut m, "IMemAllocator::Release", &[allocator]), 0);
}

#[test]
// spec: MEDIA-DMO-4, MEDIA-FAIL-2
fn dmo_graph_forwards_seeks_orders_controls_and_ignores_decommit_aborts() {
    let mut m = machine();
    let graph = activate(&mut m, CLSID_FILTERGRAPH, IID_IGRAPHBUILDER);
    let control = query(&mut m, graph, IID_IMEDIACONTROL);
    let seeking = query(&mut m, graph, IID_IMEDIASEEKING);
    let sink = query(&mut m, graph, IID_IMEDIAEVENTSINK);
    let events = query(&mut m, graph, IID_IMEDIAEVENTEX);

    let dmo_filter = activate(&mut m, dmo::CLSID_DMO_WRAPPER, IID_IBASEFILTER);
    let wrapper = query(&mut m, dmo_filter, dmo::IID_IDMOWRAPPERFILTER);
    m.cpu.write(DATA + 64, &dmo::CLSID_WMV_DECODER).unwrap();
    m.cpu.write(DATA + 80, &[0; 16]).unwrap();
    assert_eq!(
        method(
            &mut m,
            "IDMOWrapperFilter::Init",
            &[wrapper, DATA + 64, DATA + 80]
        ),
        S_OK
    );

    // Guest filters: source (0x500400) and renderer (0x500500), plus upstream
    // connector, IMediaSeeking and the downstream input pin.
    let (source, renderer, connector, upstream_seek, downstream) = (
        DATA + 0x400,
        DATA + 0x500,
        DATA + 0x600,
        DATA + 0x700,
        DATA + 0x800,
    );
    for (i, obj) in [source, renderer, connector, upstream_seek, downstream]
        .into_iter()
        .enumerate()
    {
        let table = obj + 4;
        m.cpu.write_u32(obj, table).unwrap();
        for slot in 0..18u32 {
            m.cpu
                .write_u32(table + slot * 4, 0x7100_0000 + (i as u32) * 0x100 + slot)
                .unwrap();
        }
    }
    assert_eq!(
        method_calls(
            &mut m,
            "IFilterGraph2::AddFilter",
            &[graph, source, 0],
            &[
                (0x7100_0001, &[source], 2),
                (0x7100_000d, &[source, graph, ANY], S_OK),
                (0x7100_0008, &[source, 0], S_OK),
            ],
        ),
        S_OK
    );
    assert_eq!(
        method(&mut m, "IFilterGraph2::AddFilter", &[graph, dmo_filter, 0]),
        S_OK
    );
    assert_eq!(
        method_calls(
            &mut m,
            "IFilterGraph2::AddFilter",
            &[graph, renderer, 0],
            &[
                (0x7100_0101, &[renderer], 2),
                (0x7100_010d, &[renderer, graph, ANY], S_OK),
                (0x7100_0108, &[renderer, 0], S_OK),
            ],
        ),
        S_OK
    );

    let input_pin = m.ext.get::<State>().filters[&dmo_filter].pins[0];
    let mut learn = dmo::learn_upstream(&mut env(&mut m), input_pin, connector);
    // 1. connector->QueryPinInfo writes `source`.
    let Outcome::Call {
        target, args, then, ..
    } = learn
    else {
        panic!("expected QueryPinInfo");
    };
    assert_eq!(target, 0x7100_0208);
    m.cpu.write_u32(args[1], source).unwrap();
    learn = then(&mut env(&mut m), adv_machine::Resume::Returned(S_OK));
    // 2. source->QueryInterface(IID_IMEDIASEEKING) writes `upstream_seek`.
    let Outcome::Call {
        target, args, then, ..
    } = learn
    else {
        panic!("expected QueryInterface");
    };
    assert_eq!(target, 0x7100_0000);
    m.cpu.write_u32(args[2], upstream_seek).unwrap();
    learn = then(&mut env(&mut m), adv_machine::Resume::Returned(S_OK));
    // 3. source->Release.
    let Outcome::Call { target, then, .. } = learn else {
        panic!("expected source Release");
    };
    assert_eq!(target, 0x7100_0002);
    learn = then(&mut env(&mut m), adv_machine::Resume::Returned(1));
    // 4. upstream_seek->GetDuration writes 90_000_000.
    let Outcome::Call {
        target, args, then, ..
    } = learn
    else {
        panic!("expected GetDuration");
    };
    assert_eq!(target, 0x7100_030a);
    m.cpu.write(args[1], &90_000_000i64.to_le_bytes()).unwrap();
    assert!(matches!(
        then(&mut env(&mut m), adv_machine::Resume::Returned(S_OK)),
        Outcome::Return(S_OK)
    ));

    // The graph asks its guest members for IMediaSeeking, newest first; the
    // host timeline answers for the DMO stage and its upstream.
    let refused: &[(u32, &[u32], u32)] = &[
        (0x7100_0100, &[renderer, ANY, ANY], E_NOINTERFACE),
        (0x7100_0000, &[source, ANY, ANY], E_NOINTERFACE),
    ];
    assert_eq!(
        method_calls(
            &mut m,
            "IMediaSeeking::GetDuration",
            &[seeking, DATA],
            refused
        ),
        S_OK
    );
    let mut dur = [0u8; 8];
    m.cpu.read(DATA, &mut dur).unwrap();
    assert_eq!(i64::from_le_bytes(dur), 90_000_000);

    // SetPositions forwards to upstream IMediaSeeking (slot 14).
    m.cpu
        .write(DATA + 16, &25_000_000i64.to_le_bytes())
        .unwrap();
    m.cpu
        .write(DATA + 24, &80_000_000i64.to_le_bytes())
        .unwrap();
    let seek_thunk = {
        m.resolve_import("quartz.dll", "IMediaSeeking::SetPositions")
            .unwrap();
        m.sys
            .thunks
            .iter()
            .find(|(_, t)| t.module == "quartz.dll" && t.name == "IMediaSeeking::SetPositions")
            .unwrap()
            .0
    };
    m.cpu.set_reg(Reg::Esp, STACK);
    for (i, arg) in [seeking, DATA + 16, 1, DATA + 24, 1]
        .into_iter()
        .enumerate()
    {
        m.cpu.write_u32(STACK + 4 + i as u32 * 4, arg).unwrap();
    }
    let handler = m.sys.thunks.get(seek_thunk).f.handler;
    let mut outcome = handler(&mut Env {
        context: m.sys.current,
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk: seek_thunk,
    });
    for &(target, args, value) in refused {
        let Outcome::Call {
            target: called,
            args: passed,
            then,
            ..
        } = outcome
        else {
            panic!("SetPositions must ask {target:#x} for IMediaSeeking");
        };
        assert_eq!((called, passed[0]), (target, args[0]));
        outcome = then(&mut env(&mut m), adv_machine::Resume::Returned(value));
    }
    let Outcome::Call {
        target, args, then, ..
    } = outcome
    else {
        panic!("SetPositions must call upstream seeker");
    };
    assert_eq!(
        (target, args[0], args[2], args[4]),
        (0x7100_030e, upstream_seek, 1, 1)
    );
    let mut forwarded = [0u8; 8];
    m.cpu.read(args[1], &mut forwarded).unwrap();
    assert_eq!(i64::from_le_bytes(forwarded), 25_000_000);
    assert!(matches!(
        then(&mut env(&mut m), adv_machine::Resume::Returned(S_OK)),
        Outcome::Return(S_OK)
    ));

    // Pause and Run invoke downstream renderer first and upstream source last.
    assert_eq!(
        method_calls(
            &mut m,
            "IMediaControl::Pause",
            &[control],
            &[
                // spec: MEDIA-GRAPH-12
                // No member has a clock and no system clock exists here;
                // the first member added is asked first.
                (0x7100_0000, &[source, ANY, ANY], E_NOINTERFACE),
                (0x7100_0100, &[renderer, ANY, ANY], E_NOINTERFACE),
                // Members are sorted by their connections before each change.
                (0x7100_010a, &[renderer, ANY], E_NOTIMPL),
                (0x7100_000a, &[source, ANY], E_NOTIMPL),
                (0x7100_0105, &[renderer], S_OK),
                (0x7100_0005, &[source], S_OK),
            ],
        ),
        S_OK
    );
    assert_eq!(
        method_calls(
            &mut m,
            "IMediaControl::Run",
            &[control],
            &[
                // ensure_default probes IReferenceClock, the stop position probes
                // IMediaSeeking, then count_guest_renderers probes IMediaSeeking,
                // IAMFilterMiscFlags, and IMediaPosition on each guest member.
                (0x7100_0000, &[source, ANY, ANY], E_NOINTERFACE),
                (0x7100_0100, &[renderer, ANY, ANY], E_NOINTERFACE),
                (0x7100_0100, &[renderer, ANY, ANY], E_NOINTERFACE),
                (0x7100_0000, &[source, ANY, ANY], E_NOINTERFACE),
                (0x7100_0100, &[renderer, ANY, ANY], E_NOINTERFACE),
                (0x7100_0000, &[source, ANY, ANY], E_NOINTERFACE),
                (0x7100_0000, &[source, ANY, ANY], E_NOINTERFACE),
                (0x7100_0000, &[source, ANY, ANY], E_NOINTERFACE),
                (0x7100_0100, &[renderer, ANY, ANY], E_NOINTERFACE),
                (0x7100_0100, &[renderer, ANY, ANY], E_NOINTERFACE),
                (0x7100_010a, &[renderer, ANY], E_NOTIMPL),
                (0x7100_000a, &[source, ANY], E_NOTIMPL),
                (0x7100_0106, &[renderer, 0, 0], S_OK),
                (0x7100_0006, &[source, 0, 0], S_OK),
            ],
        ),
        S_OK
    );

    // Genuine stream failure (E_FAIL) while running delivers EC_ERRORABORT.
    assert_eq!(
        method(
            &mut m,
            "IMediaEventSink::Notify",
            &[sink, 3, 0x8000_4005, 0]
        ),
        S_OK
    );
    assert_eq!(
        method(
            &mut m,
            "IMediaEventEx::GetEvent",
            &[events, DATA, DATA + 4, DATA + 8, 0]
        ),
        S_OK
    );
    assert_eq!(
        (
            m.cpu.read_u32(DATA).unwrap(),
            m.cpu.read_u32(DATA + 4).unwrap()
        ),
        (3, 0x8000_4005)
    );

    // Decommitting the DMO input allocator suppresses VFW_E_NOT_COMMITTED aborts.
    let mem_input = query(&mut m, input_pin, IID_IMEMINPUTPIN);
    assert_eq!(
        method(&mut m, "IMemInputPin::GetAllocator", &[mem_input, DATA]),
        S_OK
    );
    let in_alloc = m.cpu.read_u32(DATA).unwrap();
    assert_eq!(method(&mut m, "IMemAllocator::Decommit", &[in_alloc]), S_OK);
    assert_eq!(
        method(
            &mut m,
            "IMediaEventSink::Notify",
            &[sink, 3, VFW_E_NOT_COMMITTED, 0]
        ),
        S_OK
    );
    assert_eq!(
        method(
            &mut m,
            "IMediaEventEx::GetEvent",
            &[events, DATA, DATA + 4, DATA + 8, 0]
        ),
        0x8000_4004
    );
    assert_eq!(method(&mut m, "IMemAllocator::Commit", &[in_alloc]), S_OK);

    // A running graph pauses its members first, as quartz does. Stop then
    // flushes the downstream input before waiting for the source, and
    // ends the flush after both guest filters have stopped.
    let s = m.ext.get_mut::<State>();
    let output = s.filters[&dmo_filter].pins[1];
    s.pins.get_mut(&output).unwrap().peer = downstream;
    s.pins.get_mut(&output).unwrap().mem = downstream;
    assert_eq!(
        method_calls(
            &mut m,
            "IMediaControl::Stop",
            &[control],
            &[
                (0x7100_010a, &[renderer, ANY], E_NOTIMPL),
                (0x7100_000a, &[source, ANY], E_NOTIMPL),
                (0x7100_0105, &[renderer], S_OK),
                (0x7100_0005, &[source], S_OK),
                (0x7100_040f, &[downstream], S_OK),
                (0x7100_010a, &[renderer, ANY], E_NOTIMPL),
                (0x7100_000a, &[source, ANY], E_NOTIMPL),
                (0x7100_0004, &[source], S_OK),
                (0x7100_0104, &[renderer], S_OK),
                (0x7100_0410, &[downstream], S_OK),
                // Stopped, the graph re-seeks its streams to the kept position.
                (0x7100_0100, &[renderer, ANY, ANY], E_NOINTERFACE),
                (0x7100_0000, &[source, ANY, ANY], E_NOINTERFACE),
                (0x7100_030e, &[upstream_seek, ANY, 1, ANY, 0], S_OK),
            ],
        ),
        S_OK
    );
    assert_eq!(
        method(
            &mut m,
            "IMediaEventSink::Notify",
            &[sink, 3, VFW_E_NOT_COMMITTED, 0]
        ),
        S_OK
    );
    assert_eq!(
        method(
            &mut m,
            "IMediaEventEx::GetEvent",
            &[events, DATA, DATA + 4, DATA + 8, 0]
        ),
        0x8000_4004
    );
}

#[derive(Default)]
struct DmoFixture {
    calls: Vec<u32>,
    allocate: u32,
    output: u32,
    incomplete: bool,
    received: Vec<sample::Sample>,
    input_flags: u32,
}

fn fixture_dmo(env: &mut Env<'_>) -> Outcome {
    let slot = env.sys.thunks.get(env.thunk).f.data as u32;
    env.ext.get_or_default::<DmoFixture>().calls.push(slot);
    match slot {
        0 => {
            let _ = env.cpu.write_u32(env.arg(2), DATA + 2048);
        }
        8 | 9 => return Outcome::Return(if env.arg(3) == 1 { S_FALSE } else { S_OK }),
        12 => {
            for (arg, value) in [(2, 321), (3, 0), (4, 64)] {
                let _ = env.cpu.write_u32(env.arg(arg), value);
            }
        }
        18 => return Outcome::Return(env.ext.get::<DmoFixture>().allocate),
        21 => {
            env.ext.get_mut::<DmoFixture>().input_flags = env.arg(3);
            let buffer = env.arg(2);
            return guest_call(
                env,
                buffer,
                5,
                vec![DATA + 2400, DATA + 2404],
                move |env, hr| {
                    assert_eq!(hr, S_OK);
                    assert_eq!(env.cpu.read_u32(DATA + 2404).unwrap(), 200);
                    let data = env.cpu.read_u32(DATA + 2400).unwrap();
                    assert_eq!(env.cpu.read_u8(data + 199).unwrap(), 199);
                    guest_call(env, buffer, 4, vec![DATA + 2404], |env, hr| {
                        assert_eq!(hr, S_OK);
                        assert_eq!(env.cpu.read_u32(DATA + 2404).unwrap(), 256);
                        Outcome::Return(S_OK)
                    })
                },
            );
        }
        22 => {
            assert_eq!(env.arg(1), 1);
            assert_eq!(env.arg(2), 2);
            let buffers = env.arg(3);
            let output = env.ext.get::<DmoFixture>().output;
            let incomplete = std::mem::take(&mut env.ext.get_mut::<DmoFixture>().incomplete);
            let flags = 7 | if incomplete { 0x0100_0000 } else { 0 };
            let mut calls = VecDeque::new();
            for index in 0..2 {
                let at = buffers + index * 24;
                let buffer = env.cpu.read_u32(at).unwrap();
                if buffer != 0 {
                    let _ = env.cpu.write_u32(at + 4, flags);
                    write_i64(env, at + 8, 5000);
                    write_i64(env, at + 16, 1000);
                    calls.push_back(Call(buffer, 3, vec![300]));
                }
            }
            return chain(
                env,
                calls,
                Vec::new(),
                Box::new(move |_, _| Outcome::Return(output)),
            );
        }
        100 => {
            let sample = canonical(env, env.arg(1)).unwrap();
            let s = state(env).samples[&sample].clone();
            env.ext.get_mut::<DmoFixture>().received.push(s);
        }
        _ => {}
    }
    Outcome::Return(S_OK)
}

fn fixture_dmo_filter(m: &mut Machine<JitCpu>) -> u32 {
    let filter = activate(m, dmo::CLSID_DMO_WRAPPER, IID_IBASEFILTER);
    let object = DATA + 2048;
    let table = DATA + 2080;
    m.cpu.write_u32(object, table).unwrap();
    for slot in [0, 1, 2, 8, 9, 12, 16, 17, 18, 19, 21, 22] {
        let target = m
            .resolve_import("authored.dmo", &format!("slot{slot}"))
            .unwrap();
        m.cpu.write_u32(table + slot * 4, target).unwrap();
    }
    let dmo = dmo::Dmo::new(&mut env(m), object);
    m.ext
        .get_mut::<State>()
        .filters
        .get_mut(&filter)
        .unwrap()
        .dmo = Some(dmo);
    for (dir, name) in [
        (filter::INPUT, "in0"),
        (filter::OUTPUT, "out0"),
        (filter::OUTPUT, "out1"),
    ] {
        filter::new_pin(&mut env(m), filter, dir, name, GUID_NULL, None).unwrap();
    }
    m.ext.insert(DmoFixture::default());
    filter
}

// spec: MEDIA-DMO-5
#[test]
fn dmo_transitions_propagate_allocation_failure_and_clean_up_resources() {
    let mut m = machine();
    let filter = fixture_dmo_filter(&mut m);
    m.ext.get_mut::<DmoFixture>().allocate = 0xdead_beef;
    let outcome = start_method(&mut m, "IBaseFilter::Pause", &[filter]);
    assert_eq!(graph::source::tests::finish(&mut m, outcome), 0xdead_beef);
    assert_eq!(m.ext.get::<State>().filters[&filter].state, graph::STOPPED);
    m.ext.get_mut::<DmoFixture>().allocate = 0xbeef;
    let outcome = start_method(&mut m, "IBaseFilter::Run", &[filter, 0, 0]);
    assert_eq!(graph::source::tests::finish(&mut m, outcome), S_OK);
    assert_eq!(m.ext.get::<State>().filters[&filter].state, graph::RUNNING);
    let output = m.ext.get::<State>().filters[&filter].pins[1];
    let outcome = filter::connect(&mut env(&mut m), output, DATA + 3000, 0);
    assert_eq!(graph::source::tests::finish(&mut m, outcome), 0x8004_0224);
    let outcome = filter::disconnect(&mut env(&mut m), output);
    assert_eq!(graph::source::tests::finish(&mut m, outcome), 0x8004_0224);
    let outcome = start_method(&mut m, "IBaseFilter::Stop", &[filter]);
    assert_eq!(graph::source::tests::finish(&mut m, outcome), S_OK);
    assert!(m.ext.get::<DmoFixture>().calls.ends_with(&[0, 16, 19, 2]));
}

// spec: MEDIA-DMO-6
#[test]
fn dmo_wraps_samples_drains_incomplete_outputs_and_forwards_stream_events() {
    let mut m = machine();
    let filter = fixture_dmo_filter(&mut m);
    let pins = m.ext.get::<State>().filters[&filter].pins.clone();
    let allocator = sample::create_allocator(&mut env(&mut m), 1, 16384).unwrap();
    let sink = DATA + 2600;
    let table = DATA + 2640;
    m.cpu.write_u32(sink, table).unwrap();
    for (slot, fixture) in [(6, 100), (14, 114), (15, 115), (16, 116), (17, 117)] {
        let target = m
            .resolve_import("authored.dmo", &format!("slot{fixture}"))
            .unwrap();
        m.cpu.write_u32(table + slot * 4, target).unwrap();
    }
    let out = m.ext.get_mut::<State>().pins.get_mut(&pins[1]).unwrap();
    out.allocator = allocator;
    out.mem = sink;
    out.peer = sink;
    let input_allocator = sample::create_allocator(&mut env(&mut m), 1, 256).unwrap();
    let sample = sample::take(&mut env(&mut m), input_allocator).unwrap();
    let buffer = m.ext.get::<State>().samples[&sample].buffer;
    m.cpu
        .write(buffer, &(0..=255).collect::<Vec<u8>>())
        .unwrap();
    method(&mut m, "IMediaSample::SetActualDataLength", &[sample, 200]);
    method(&mut m, "IMediaSample::SetSyncPoint", &[sample, 0]);
    write_i64(&mut env(&mut m), DATA + 16, 20000);
    method(&mut m, "IMediaSample::SetTime", &[sample, DATA + 16, 0]);
    let outcome = start_method(&mut m, "IBaseFilter::Pause", &[filter]);
    assert_eq!(graph::source::tests::finish(&mut m, outcome), S_OK);
    m.ext.get_mut::<DmoFixture>().incomplete = true;
    let input = m.ext.get::<State>().pins[&pins[0]].faces[1];
    let outcome = start_method(&mut m, "IMemInputPin::Receive", &[input, sample]);
    assert_eq!(graph::source::tests::finish(&mut m, outcome), S_OK);
    assert_eq!(m.ext.get::<DmoFixture>().input_flags, 6);
    let received = &m.ext.get::<DmoFixture>().received;
    assert_eq!(received.len(), 2);
    for sample in received {
        assert_eq!(
            (sample.actual, sample.time, sample.sync_point),
            (300, Some((5000, 6000)), true)
        );
    }
    for (event, expected) in [
        ("NewSegment", 117),
        ("EndOfStream", 114),
        ("BeginFlush", 115),
        ("EndFlush", 116),
    ] {
        let outcome = start_method(
            &mut m,
            &format!("IPin::{event}"),
            &[pins[0], 10000, 0, 20000, 0, 0, 0x3ff0_0000],
        );
        assert_eq!(graph::source::tests::finish(&mut m, outcome), S_OK);
        assert!(m.ext.get::<DmoFixture>().calls.contains(&expected));
        if event == "EndOfStream" {
            assert_eq!(
                method(&mut m, "IMemInputPin::Receive", &[input, sample]),
                0x8004_022c
            );
        }
    }
    let outcome = start_method(&mut m, "IBaseFilter::Stop", &[filter]);
    assert_eq!(graph::source::tests::finish(&mut m, outcome), S_OK);
}

// spec: MEDIA-DMO-5
#[test]
fn dmo_wrapper_validates_aggregation_but_keeps_its_own_identity() {
    let mut m = machine();
    let outcome = crate::test_support::activate(
        &mut env(&mut m),
        dmo::CLSID_DMO_WRAPPER,
        adv_os::com::IUNKNOWN,
        DATA + 512,
        DATA,
    );
    assert!(matches!(outcome, Outcome::Return(S_OK)));
    let unknown = m.cpu.read_u32(DATA).unwrap();
    let filter = query(&mut m, unknown, IID_IBASEFILTER);
    assert_eq!(filter, unknown);
    assert_eq!(method(&mut m, "IUnknown::Release", &[unknown]), 1);
    assert_eq!(method(&mut m, "IBaseFilter::Pause", &[filter]), 0x8000_4005);
    assert_eq!(method(&mut m, "IBaseFilter::Stop", &[filter]), 0x8000_4005);
    assert_eq!(method(&mut m, "IBaseFilter::Release", &[filter]), 0);
}

// spec: MEDIA-STREAM-6
#[test]
fn mpeg_pin_counts_and_enumeration_follow_the_filter() {
    let mut m = machine();
    let splitter = activate(&mut m, CLSID_MPEG1_SPLITTER, IID_IBASEFILTER);
    assert_eq!(
        method(&mut m, "IBaseFilter::EnumPins", &[splitter, DATA]),
        S_OK
    );
    let enumerator = m.cpu.read_u32(DATA).unwrap();
    assert_eq!(method(&mut m, "IBaseFilter::AddRef", &[splitter]), 3);
    assert_eq!(method(&mut m, "IBaseFilter::Release", &[splitter]), 2);
    assert_eq!(
        method(&mut m, "IEnumPins::Next", &[enumerator, 2, DATA, 0]),
        E_INVALIDARG
    );
    assert_eq!(method(&mut m, "IEnumPins::Skip", &[enumerator, 2]), S_FALSE);
    assert_eq!(
        method(&mut m, "IEnumPins::Next", &[enumerator, 1, DATA, 0]),
        S_OK
    );
    let pin = m.cpu.read_u32(DATA).unwrap();
    assert_eq!(method(&mut m, "IPin::AddRef", &[pin]), 4);
    assert_eq!(method(&mut m, "IPin::Release", &[pin]), 3);
    filter::new_pin(
        &mut env(&mut m),
        splitter,
        filter::OUTPUT,
        "Audio",
        MEDIATYPE_AUDIO,
        None,
    )
    .unwrap();
    assert_eq!(
        method(&mut m, "IEnumPins::Next", &[enumerator, 1, DATA, 0]),
        S_FALSE
    );
    assert_eq!(method(&mut m, "IPin::Release", &[pin]), 2);
    assert_eq!(method(&mut m, "IEnumPins::Reset", &[enumerator]), S_OK);
    assert_eq!(
        method(&mut m, "IEnumPins::Next", &[enumerator, 2, DATA, DATA + 8]),
        S_OK
    );
    for at in [0, 4] {
        let pin = m.cpu.read_u32(DATA + at).unwrap();
        method(&mut m, "IPin::Release", &[pin]);
    }
    assert_eq!(method(&mut m, "IEnumPins::Release", &[enumerator]), 0);
    assert_eq!(method(&mut m, "IBaseFilter::Release", &[splitter]), 0);
}

// spec: MEDIA-STREAM-5
#[test]
fn mpeg_audio_headers_and_type_admission_match_windows() {
    let mut m = machine();
    let mt = filter::mpeg_audio_type(&[0xff, 0xfb, 0x14, 0xc4]).unwrap();
    let expected: Vec<u8> = [0x50u16, 1]
        .into_iter()
        .flat_map(u16::to_le_bytes)
        .chain([48000u32, 4000].into_iter().flat_map(u32::to_le_bytes))
        .chain([96u16, 0, 22, 4].into_iter().flat_map(u16::to_le_bytes))
        .chain(32000u32.to_le_bytes())
        .chain([8u16, 0, 1, 28].into_iter().flat_map(u16::to_le_bytes))
        .chain([0; 8])
        .collect();
    assert_eq!(mt.format, expected);
    let decoder = activate(&mut m, CLSID_MPEG_AUDIO_CODEC, IID_IBASEFILTER);
    let pin = m.ext.get::<State>().filters[&decoder].pins[0];
    mt.write(&mut env(&mut m), DATA).unwrap();
    assert_eq!(method(&mut m, "IPin::QueryAccept", &[pin, DATA]), S_FALSE);
    let mut layer2 = mt.clone();
    layer2.format[18..20].copy_from_slice(&2u16.to_le_bytes());
    layer2.write(&mut env(&mut m), DATA).unwrap();
    assert_eq!(method(&mut m, "IPin::QueryAccept", &[pin, DATA]), S_OK);
    layer2.sub = MEDIASUBTYPE_PCM;
    layer2.write(&mut env(&mut m), DATA).unwrap();
    assert_eq!(method(&mut m, "IPin::QueryAccept", &[pin, DATA]), S_FALSE);
}

// spec: MEDIA-STREAM-7
#[test]
fn direct_mpeg_filter_transitions_call_guest_allocators() {
    let mut m = machine();
    let decoder = activate(&mut m, CLSID_MPEG_VIDEO_CODEC, IID_IBASEFILTER);
    let output = m.ext.get::<State>().filters[&decoder].pins[1];
    let allocator = DATA + 512;
    let table = DATA + 544;
    m.cpu.write_u32(allocator, table).unwrap();
    m.cpu.write_u32(table + 20, 0x71000005).unwrap();
    m.cpu.write_u32(table + 24, 0x71000006).unwrap();
    m.ext
        .get_mut::<State>()
        .pins
        .get_mut(&output)
        .unwrap()
        .allocator = allocator;
    for (name, target, state) in [
        ("Pause", 0x71000005, graph::PAUSED),
        ("Run", 0x71000005, graph::RUNNING),
        ("Stop", 0x71000006, graph::STOPPED),
    ] {
        assert_eq!(
            method_calls(
                &mut m,
                &format!("IBaseFilter::{name}"),
                &[decoder, 0, 0],
                &[(target, &[allocator], S_OK)]
            ),
            S_OK
        );
        assert_eq!(m.ext.get::<State>().filters[&decoder].state, state);
    }
}

// spec: MEDIA-STREAM-8
#[test]
fn splitter_output_seeking_keeps_positions_and_reports_stream_identity() {
    let mut m = machine();
    let splitter = activate(&mut m, CLSID_MPEG1_SPLITTER, IID_IBASEFILTER);
    let mt = filter::mpeg_audio_type(&[0xff, 0xfb, 0x14, 0xc4]).unwrap();
    let pin = filter::new_pin(
        &mut env(&mut m),
        splitter,
        filter::OUTPUT,
        "Audio",
        MEDIATYPE_AUDIO,
        Some(mt),
    )
    .unwrap();
    let seeking = query(&mut m, pin, IID_IMEDIASEEKING);
    assert_eq!(
        method(&mut m, "IMediaSeeking::GetCapabilities", &[seeking, DATA]),
        S_OK
    );
    assert_eq!(m.cpu.read_u32(DATA).unwrap(), 0x37);
    write_i64(&mut env(&mut m), DATA, 1_000_000);
    write_i64(&mut env(&mut m), DATA + 8, 3_000_000);
    assert_eq!(
        method(
            &mut m,
            "IMediaSeeking::SetPositions",
            &[seeking, DATA, 1, DATA + 8, 1]
        ),
        S_OK
    );
    assert_eq!(
        method(
            &mut m,
            "IMediaSeeking::GetPositions",
            &[seeking, DATA + 16, DATA + 24]
        ),
        S_OK
    );
    assert_eq!(read_i64(&env(&mut m), DATA + 16), Some(1_000_000));
    assert_eq!(read_i64(&env(&mut m), DATA + 24), Some(3_000_000));
    let select = query(&mut m, splitter, IID_IAMSTREAMSELECT);
    assert_eq!(
        method(&mut m, "IAMStreamSelect::Count", &[select, DATA]),
        S_OK
    );
    assert_eq!(m.cpu.read_u32(DATA).unwrap(), 1);
    assert_eq!(
        method(
            &mut m,
            "IAMStreamSelect::Info",
            &[
                select,
                0,
                0,
                DATA,
                DATA + 4,
                DATA + 8,
                DATA + 12,
                DATA + 16,
                DATA + 20
            ]
        ),
        S_OK
    );
    assert_eq!(m.cpu.read_u32(DATA).unwrap(), 1);
    assert_eq!(m.cpu.read_u32(DATA + 16).unwrap(), pin);
    assert_eq!(m.cpu.read_u32(DATA + 20).unwrap(), 0);
}

/// Follow a quartz method's calls into host thunks until it returns.
fn finish_calls(m: &mut Machine<JitCpu>, mut outcome: Outcome) -> u32 {
    loop {
        outcome = match outcome {
            Outcome::Return(hr) => return hr,
            Outcome::Call {
                target, args, then, ..
            } => {
                let thunk = m
                    .sys
                    .thunks
                    .iter()
                    .find(|(index, _)| adv_machine::layout::thunk_addr(*index) == target)
                    .unwrap()
                    .0;
                let handler = m.sys.thunks.get(thunk).f.handler;
                m.cpu.set_reg(Reg::Esp, STACK);
                for (i, arg) in args.iter().enumerate() {
                    m.cpu.write_u32(STACK + 4 + i as u32 * 4, *arg).unwrap();
                }
                let call = handler(&mut Env {
                    context: m.sys.current,
                    cpu: &mut m.cpu,
                    sys: &mut m.sys,
                    ext: &mut m.ext,
                    thunk,
                });
                let hr = finish_calls(m, call);
                then(&mut env(m), adv_machine::Resume::Returned(hr))
            }
            Outcome::Block { then, .. } => {
                std::thread::sleep(std::time::Duration::from_millis(1));
                then(
                    &mut env(m),
                    adv_machine::Resume::Woke(adv_machine::WakeReason::Signaled),
                )
            }
            _ => panic!("unexpected continuation"),
        };
    }
}

// spec: MEDIA-STREAM-7
#[test]
#[cfg(feature = "media")]
fn pushed_mpeg_video_delivers_a_timed_sample_after_a_direct_pause() {
    #[derive(Default)]
    struct Received(u32);
    let mut m = machine();
    m.ext.insert(Received::default());
    m.sys.thunks.resolver_mut::<Registry>().unwrap().add(
        "authored-mpeg.dll",
        "Receive",
        Conv::Stdcall { argc: 2 },
        |env| {
            let sample = &env.ext.get::<State>().samples[&env.arg(1)];
            assert_eq!(sample.size, 16 * 16 * 4);
            assert_eq!(sample.actual, sample.size);
            assert_eq!(sample.time, Some((22222, 22222)));
            env.ext.get_mut::<Received>().0 += 1;
            Outcome::Return(S_OK)
        },
    );
    m.sys.thunks.resolver_mut::<Registry>().unwrap().add(
        "authored-mpeg.dll",
        "EndOfStream",
        Conv::Stdcall { argc: 1 },
        |_| Outcome::Return(S_OK),
    );
    let receive = m.resolve_import("authored-mpeg.dll", "Receive").unwrap();
    let eos = m
        .resolve_import("authored-mpeg.dll", "EndOfStream")
        .unwrap();
    let peer = DATA + 512;
    let mem = DATA + 516;
    let table = DATA + 544;
    m.cpu.write_u32(peer, table).unwrap();
    m.cpu.write_u32(mem, table).unwrap();
    m.cpu.write_u32(table + 24, receive).unwrap();
    m.cpu.write_u32(table + 56, eos).unwrap();
    let bytes = include_bytes!("graph/render/fixtures/guest_movie.mpg");
    let input_mt = filter::mpeg_video_type(bytes).unwrap();
    let media = adv_device::media::demux(bytes.to_vec()).unwrap();
    let packets = media
        .streams
        .iter()
        .find(|s| matches!(s.format, CodecFormat::Video { .. }))
        .unwrap();
    let decoder = activate(&mut m, CLSID_MPEG_VIDEO_CODEC, IID_IBASEFILTER);
    let pins = m.ext.get::<State>().filters[&decoder].pins.clone();
    let output_mt = MediaType::video_rgb(MEDIASUBTYPE_RGB32, 16, 16, 400000);
    let allocator = sample::create_allocator(&mut env(&mut m), 1, output_mt.sample_size).unwrap();
    sample::decommit(&mut env(&mut m), allocator);
    {
        let state = m.ext.get_mut::<State>();
        let input = state.pins.get_mut(&pins[0]).unwrap();
        input.mt = Some(input_mt);
        input.peer = peer;
        let output = state.pins.get_mut(&pins[1]).unwrap();
        output.mt = Some(output_mt);
        output.peer = peer;
        output.mem = mem;
        output.allocator = allocator;
    }
    assert_eq!(method(&mut m, "IBaseFilter::Pause", &[decoder]), S_OK);
    let input = query(&mut m, pins[0], IID_IMEMINPUTPIN);
    let size = packets.packets.iter().map(|p| p.data.len()).sum::<usize>();
    let source_allocator = sample::create_allocator(&mut env(&mut m), 1, size as u32).unwrap();
    let source = sample::take(&mut env(&mut m), source_allocator).unwrap();
    let bytes: Vec<_> = packets
        .packets
        .iter()
        .flat_map(|p| p.data.iter().copied())
        .collect();
    let buffer = m.ext.get::<State>().samples[&source].buffer;
    m.cpu.write(buffer, &bytes).unwrap();
    let sample = m.ext.get_mut::<State>().samples.get_mut(&source).unwrap();
    sample.actual = bytes.len() as u32;
    sample.time = Some((22222, 33333));
    let receive = m
        .resolve_import("quartz.dll", "IMemInputPin::Receive")
        .unwrap();
    let outcome = Outcome::call(receive, vec![input, source], |_, resume| {
        Outcome::Return(resume.value())
    });
    assert_eq!(finish_calls(&mut m, outcome), S_OK);
    let eos = m.resolve_import("quartz.dll", "IPin::EndOfStream").unwrap();
    let outcome = Outcome::call(eos, vec![pins[0]], |_, resume| {
        Outcome::Return(resume.value())
    });
    assert_eq!(finish_calls(&mut m, outcome), S_OK);
    assert!(m.ext.get::<Received>().0 > 0);
}

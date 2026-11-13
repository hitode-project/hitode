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
    assert_eq!(
        method(&mut m, "IMediaSeeking::GetDuration", &[seeking, DATA]),
        S_OK
    );
    assert_eq!(m.cpu.read_u32(DATA).unwrap(), 0);
}

/// Run a quartz method that calls back into the guest exactly once per item
/// of `expected`, checking each call's target and arguments.
fn method_calls(
    m: &mut Machine<JitCpu>,
    name: &str,
    args: &[u32],
    expected: &[(u32, &[u32], u32)],
) -> u32 {
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
    let mut outcome = handler(&mut Env {
        context: m.sys.current,
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk,
    });
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
    // Reset takes the live list, which now holds only the decoder.
    assert_eq!(method(&mut m, "IEnumFilters::Reset", &[filters]), S_OK);
    assert_eq!(method(&mut m, "IEnumFilters::Skip", &[filters, 1]), S_OK);
    assert_eq!(
        method(&mut m, "IEnumFilters::Skip", &[filters, 1]),
        E_INVALIDARG
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
    // connector pin (0x500600) and IMediaSeeking (0x500700).
    let (source, renderer, connector, upstream_seek) =
        (DATA + 0x400, DATA + 0x500, DATA + 0x600, DATA + 0x700);
    for (i, obj) in [source, renderer, connector, upstream_seek]
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
                (0x7100_000d, &[source, graph, 0], S_OK),
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
                (0x7100_010d, &[renderer, graph, 0], S_OK),
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

    assert_eq!(
        method(&mut m, "IMediaSeeking::GetDuration", &[seeking, DATA]),
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
    let outcome = handler(&mut Env {
        context: m.sys.current,
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk: seek_thunk,
    });
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

    // Stop invokes upstream source FIRST and downstream renderer LAST, and
    // ignores any VFW_E_NOT_COMMITTED abort raised while stopping.
    assert_eq!(
        method_calls(
            &mut m,
            "IMediaControl::Stop",
            &[control],
            &[
                (0x7100_0004, &[source], S_OK),
                (0x7100_0104, &[renderer], S_OK),
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

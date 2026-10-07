//! Authored pull source: a guest IPin drives the ordinary splitter connection,
//! whose callbacks query and read the source's separate IAsyncReader face.
use super::*;
use crate::dshow::graph::source::tests::finish;
use crate::dshow::tests::{activate, env, machine};
use adv_cpu::JitCpu;
use adv_cpu::backend::{Guest, Reg};
use adv_machine::{Clock, Conv, Machine};
use std::collections::BTreeMap;

const DATA: u32 = 0x500000;
const STACK: u32 = 0x600000;
const SOURCE: u32 = DATA + 0x800;
const OUTPUT_PIN: u32 = DATA + 0x820;
const READER: u32 = DATA + 0x840;
const FOREIGN: u32 = DATA + 0x860;
const ENUMERATOR: u32 = DATA + 0x880;

struct PullSource {
    refs: BTreeMap<u32, u32>,
    direction: u32,
    owner: u32,
    peer: u32,
    bytes: Vec<u8>,
    fail_reads: bool,
    suppress_outputs: bool,
    connects: u32,
    queries: u32,
    reads: u32,
    disconnects: u32,
    graph: u32,
    controls: Vec<u32>,
    enum_next: bool,
}

fn callback(env: &mut Env<'_>) -> Outcome {
    let op = env.sys.thunks.get(env.thunk).f.data;
    let this = env.arg(0);
    let r = |hr| Outcome::Return(hr);
    match op {
        0 => {
            assert_eq!(this, OUTPUT_PIN);
            assert_eq!(read_guid(env, env.arg(1)).unwrap(), IID_IASYNCREADER);
            let output = env.arg(2);
            let source = env.ext.get_mut::<PullSource>();
            source.queries += 1;
            *source.refs.get_mut(&READER).unwrap() += 1;
            env.cpu.write_u32(output, READER).unwrap();
            r(S_OK)
        }
        1 => {
            let count = env.ext.get_mut::<PullSource>().refs.get_mut(&this).unwrap();
            *count += 1;
            r(*count)
        }
        2 => {
            let count = env.ext.get_mut::<PullSource>().refs.get_mut(&this).unwrap();
            assert!(*count > 0, "reference underflow for {this:#x}");
            *count -= 1;
            r(*count)
        }
        3 => {
            let input = env.arg(1);
            assert_eq!(env.arg(2), 0);
            let pin = filter::host_pin(env, input).unwrap();
            let splitter = state(env).pins[&pin].filter;
            assert_eq!(state(env).filters[&splitter].role, Role::Splitter);
            env.ext.get_mut::<PullSource>().connects += 1;
            MediaType {
                major: MEDIATYPE_STREAM,
                sub: MEDIASUBTYPE_MPEG1SYSTEM,
                fixed_size: true,
                temporal: false,
                sample_size: 1,
                format_type: GUID_NULL,
                format: Vec::new(),
            }
            .write(env, DATA + 0x300)
            .unwrap();
            guest_call(
                env,
                input,
                4,
                vec![OUTPUT_PIN, DATA + 0x300],
                move |env, hr| {
                    if hr == S_OK {
                        add_ref(env, input);
                        env.ext.get_mut::<PullSource>().peer = input;
                        // A fault injection after the accepted pull connection
                        // exercises rollback of reader and connector references.
                        if env.ext.get::<PullSource>().suppress_outputs {
                            let pins = state(env).filters[&splitter].pins.clone();
                            for pin in pins {
                                if state(env).pins[&pin].dir == OUTPUT {
                                    state(env).pins.get_mut(&pin).unwrap().major = GUID_NULL;
                                }
                            }
                        }
                    }
                    Outcome::Return(hr)
                },
            )
        }
        4 => {
            let peer = std::mem::take(&mut env.ext.get_mut::<PullSource>().peer);
            env.ext.get_mut::<PullSource>().disconnects += 1;
            if filter::host_pin(env, peer).is_some() {
                release(env, peer);
            }
            r(S_OK)
        }
        5 => {
            let output = env.arg(1);
            let peer = env.ext.get::<PullSource>().peer;
            if peer == 0 {
                env.cpu.write_u32(output, 0).unwrap();
                return r(VFW_E_NOT_CONNECTED);
            }
            if filter::host_pin(env, peer).is_some() {
                add_ref(env, peer);
            } else {
                *env.ext.get_mut::<PullSource>().refs.get_mut(&peer).unwrap() += 1;
            }
            env.cpu.write_u32(output, peer).unwrap();
            r(S_OK)
        }
        6 => {
            let output = env.arg(1);
            let source = env.ext.get_mut::<PullSource>();
            let (owner, direction) = (source.owner, source.direction);
            *source.refs.get_mut(&owner).unwrap() += 1;
            env.cpu.write_u32(output, owner).unwrap();
            env.cpu.write_u32(output + 4, direction).unwrap();
            r(S_OK)
        }
        7 => {
            let direction = env.ext.get::<PullSource>().direction;
            env.cpu.write_u32(env.arg(1), direction).unwrap();
            r(S_OK)
        }
        8 => {
            let size = env.ext.get::<PullSource>().bytes.len() as i64;
            for output in [env.arg(1), env.arg(2)] {
                env.cpu.write(output, &size.to_le_bytes()).unwrap();
            }
            r(S_OK)
        }
        9 => {
            let position = u64::from(env.arg(1)) | (u64::from(env.arg(2)) << 32);
            let length = env.arg(3) as usize;
            let output = env.arg(4);
            let source = env.ext.get_mut::<PullSource>();
            source.reads += 1;
            if source.fail_reads {
                return r(0x8000_4005);
            }
            let position = position as usize;
            let bytes = source.bytes[position..position + length].to_vec();
            env.cpu.write(output, &bytes).unwrap();
            r(S_OK)
        }
        10 => {
            let graph = env.arg(1);
            env.ext.get_mut::<PullSource>().graph = graph;
            r(S_OK)
        }
        11..=13 => {
            let state = match op {
                11 => STOPPED,
                12 => PAUSED,
                _ => RUNNING,
            };
            env.ext.get_mut::<PullSource>().controls.push(state);
            r(S_OK)
        }
        14 => {
            let output = env.arg(1);
            let source = env.ext.get_mut::<PullSource>();
            source.enum_next = false;
            *source.refs.get_mut(&ENUMERATOR).unwrap() += 1;
            env.cpu.write_u32(output, ENUMERATOR).unwrap();
            r(S_OK)
        }
        15 => {
            assert_eq!(env.arg(1), 1);
            let (output, fetched) = (env.arg(2), env.arg(3));
            let source = env.ext.get_mut::<PullSource>();
            let available = !source.enum_next;
            source.enum_next = true;
            if available {
                *source.refs.get_mut(&OUTPUT_PIN).unwrap() += 1;
            }
            env.cpu
                .write_u32(output, if available { OUTPUT_PIN } else { 0 })
                .unwrap();
            env.cpu.write_u32(fetched, u32::from(available)).unwrap();
            r(if available { S_OK } else { S_FALSE })
        }
        _ => unreachable!(),
    }
}

fn fixture(bytes: Vec<u8>) -> (Machine<JitCpu>, u32) {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    adv_win32::ole32::register(&mut registry);
    register(&mut registry);
    crate::dsound::register(&mut registry);
    for (op, argc) in [
        (0, 3),
        (1, 1),
        (2, 1),
        (3, 3),
        (4, 1),
        (5, 2),
        (6, 2),
        (7, 2),
        (8, 3),
        (9, 5),
        (10, 3),
        (11, 1),
        (12, 1),
        (13, 3),
        (14, 2),
        (15, 4),
    ] {
        registry.add_with_data(
            "authored-pull-source.dll",
            &format!("method{op}"),
            Conv::Stdcall { argc },
            callback,
            op,
        );
    }
    let mut m = Machine::new(
        JitCpu::new(adv_machine::layout::PARK_ADDR, true).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    adv_win32::Win32::install(
        &mut m,
        adv_win32::Win32Config::new(".", "authored.exe", "target/dshow-pull-render-tests"),
    )
    .unwrap();
    install(&mut m);
    m.cpu.map(DATA, 4096, Prot::RW).unwrap();
    m.cpu.map(STACK, 4096, Prot::RW).unwrap();
    m.cpu.set_reg(Reg::Esp, STACK);
    m.ext.insert(PullSource {
        refs: [
            (SOURCE, 1),
            (OUTPUT_PIN, 1),
            (READER, 1),
            (FOREIGN, 1),
            (ENUMERATOR, 0),
        ]
        .into(),
        direction: OUTPUT,
        owner: SOURCE,
        peer: 0,
        bytes,
        fail_reads: false,
        suppress_outputs: false,
        connects: 0,
        queries: 0,
        reads: 0,
        disconnects: 0,
        graph: 0,
        controls: Vec::new(),
        enum_next: false,
    });
    let mut methods = BTreeMap::new();
    for op in 0..=15 {
        methods.insert(
            op,
            m.resolve_import("authored-pull-source.dll", &format!("method{op}"))
                .unwrap(),
        );
    }
    for (object, table, slots) in [
        (
            SOURCE,
            DATA + 0x900,
            vec![
                (1, 1),
                (2, 2),
                (4, 11),
                (5, 12),
                (6, 13),
                (10, 14),
                (13, 10),
            ],
        ),
        (
            OUTPUT_PIN,
            DATA + 0xa00,
            vec![
                (0, 0),
                (1, 1),
                (2, 2),
                (3, 3),
                (5, 4),
                (6, 5),
                (8, 6),
                (9, 7),
            ],
        ),
        (READER, DATA + 0xb00, vec![(1, 1), (2, 2), (7, 9), (8, 8)]),
        (FOREIGN, DATA + 0xc00, vec![(1, 1), (2, 2)]),
        (ENUMERATOR, DATA + 0xd00, vec![(1, 1), (2, 2), (3, 15)]),
    ] {
        m.cpu.write_u32(object, table).unwrap();
        for (slot, op) in slots {
            m.cpu.write_u32(table + slot * 4, methods[&op]).unwrap();
        }
    }
    let graph = activate(&mut m, CLSID_FILTERGRAPH, IID_IGRAPHBUILDER);
    let outcome = add_filter(&mut env(&mut m), graph, SOURCE, 0);
    assert_eq!(finish(&mut m, outcome), S_OK);
    (m, graph)
}

#[test]
fn guest_render_validates_direction_connection_and_graph_owner_before_connecting() {
    for (direction, owner, peer, expected) in [
        (INPUT, SOURCE, 0, VFW_E_INVALID_DIRECTION),
        (OUTPUT, SOURCE, READER, VFW_E_ALREADY_CONNECTED),
        (OUTPUT, FOREIGN, 0, E_INVALIDARG),
    ] {
        let (mut m, graph) = fixture(vec![0x55; 32]);
        let source = m.ext.get_mut::<PullSource>();
        source.direction = direction;
        source.owner = owner;
        source.peer = peer;
        let outcome = pin(&mut env(&mut m), graph, OUTPUT_PIN);
        assert_eq!(finish(&mut m, outcome), expected);
        let source = m.ext.get::<PullSource>();
        assert_eq!(source.refs[&SOURCE], 2);
        assert_eq!(source.refs[&OUTPUT_PIN], 1);
        assert_eq!(source.refs[&READER], 1);
        assert_eq!(source.refs[&FOREIGN], 1);
        assert_eq!(source.connects, 0);
        assert_eq!(m.ext.get::<State>().graphs[&graph].members.len(), 1);
        assert!(m.ext.get::<State>().filters.is_empty());
    }
}

#[test]
fn guest_render_failed_pull_and_probe_release_reader_and_remove_temporary_filters() {
    for fail_reads in [true, false] {
        let (mut m, graph) = fixture(vec![0x55; 32]);
        m.ext.get_mut::<PullSource>().fail_reads = fail_reads;
        let outcome = pin(&mut env(&mut m), graph, OUTPUT_PIN);
        assert_eq!(
            finish(&mut m, outcome),
            if fail_reads {
                VFW_E_TYPE_NOT_ACCEPTED
            } else {
                VFW_E_UNSUPPORTED_STREAM
            }
        );
        let source = m.ext.get::<PullSource>();
        assert_eq!((source.connects, source.queries, source.reads), (1, 1, 1));
        assert_eq!(source.disconnects, 1);
        assert_eq!(source.peer, 0);
        assert_eq!(source.refs[&SOURCE], 2);
        assert_eq!(source.refs[&OUTPUT_PIN], 1);
        assert_eq!(source.refs[&READER], 1);
        assert_eq!(m.ext.get::<State>().graphs[&graph].members.len(), 1);
        assert!(m.ext.get::<State>().filters.is_empty());
        assert!(m.ext.get::<State>().pins.values().all(|p| p.peer == 0));
    }
}

#[test]
fn file_source_render_failure_restores_media_owner_and_disconnects_the_source() {
    let mut m = machine();
    let graph = activate(&mut m, CLSID_FILTERGRAPH, IID_IGRAPHBUILDER);
    let source = member(
        &mut env(&mut m),
        graph,
        Role::FileSource,
        amovie(0xe436_ebb5),
        "Authored empty source",
    )
    .unwrap();
    let output = endpoint(&env(&mut m), source, OUTPUT);
    let state = m.ext.get_mut::<State>();
    state.pins.get_mut(&output).unwrap().mt = Some(MediaType {
        major: MEDIATYPE_STREAM,
        sub: MEDIASUBTYPE_MPEG1SYSTEM,
        fixed_size: true,
        temporal: false,
        sample_size: 1,
        format_type: GUID_NULL,
        format: Vec::new(),
    });
    state.filters.get_mut(&source).unwrap().source = Some(filter::Source {
        file_handle: None,
        decoder: None,
        duration_us: 0,
        info: Some(adv_device::media::StreamInfo {
            container: "authored-empty".into(),
            duration_us: 0,
            video: None,
            audio: None,
        }),
    });
    let outcome = pin(&mut env(&mut m), graph, output);
    assert_eq!(finish(&mut m, outcome), VFW_E_CANNOT_RENDER);
    let state = m.ext.get::<State>();
    assert_eq!(state.graphs[&graph].members.len(), 1);
    assert_eq!(state.filters.len(), 1);
    assert_eq!(state.pins[&output].peer, 0);
    assert!(state.filters[&source].source.is_some());
    assert!(state.pins.values().all(|p| p.peer == 0));
}

#[cfg(feature = "media")]
fn wave() -> Vec<u8> {
    let samples = 800u32;
    let size = samples * 2;
    let mut wave = Vec::new();
    wave.extend_from_slice(b"RIFF");
    wave.extend_from_slice(&(36 + size).to_le_bytes());
    wave.extend_from_slice(b"WAVEfmt ");
    wave.extend_from_slice(&16u32.to_le_bytes());
    wave.extend_from_slice(&1u16.to_le_bytes());
    wave.extend_from_slice(&1u16.to_le_bytes());
    wave.extend_from_slice(&8000u32.to_le_bytes());
    wave.extend_from_slice(&16000u32.to_le_bytes());
    wave.extend_from_slice(&2u16.to_le_bytes());
    wave.extend_from_slice(&16u16.to_le_bytes());
    wave.extend_from_slice(b"data");
    wave.extend_from_slice(&size.to_le_bytes());
    wave.resize(wave.len() + size as usize, 0);
    wave
}

#[cfg(feature = "media")]
#[test]
fn guest_render_builds_the_common_audio_pipeline_and_disconnect_returns_source_references() {
    let (mut m, graph) = fixture(wave());
    let outcome = pin(&mut env(&mut m), graph, OUTPUT_PIN);
    assert_eq!(finish(&mut m, outcome), S_OK);
    let state = m.ext.get::<State>();
    assert_eq!(state.graphs[&graph].members.len(), 4);
    let splitter = *state
        .filters
        .iter()
        .find(|(_, f)| f.role == Role::Splitter)
        .unwrap()
        .0;
    assert!(
        state.filters[&splitter]
            .source
            .as_ref()
            .unwrap()
            .decoder
            .is_some()
    );
    let input = endpoint(&env(&mut m), splitter, INPUT);
    assert_eq!(m.ext.get::<State>().pins[&input].peer, OUTPUT_PIN);
    let source = m.ext.get::<PullSource>();
    assert_eq!((source.connects, source.queries, source.reads), (1, 1, 1));
    assert_eq!(source.refs[&SOURCE], 2);
    assert_eq!(source.refs[&OUTPUT_PIN], 2);
    assert_eq!(source.refs[&READER], 2);
    let outcome = filter::disconnect(&mut env(&mut m), input);
    assert_eq!(finish(&mut m, outcome), S_OK);
    let outcome = filter::disconnect(&mut env(&mut m), OUTPUT_PIN);
    assert_eq!(finish(&mut m, outcome), S_OK);
    let source = m.ext.get::<PullSource>();
    assert_eq!(source.refs[&OUTPUT_PIN], 1);
    assert_eq!(source.refs[&READER], 1);
    let outcome = filter::disconnect(&mut env(&mut m), input);
    assert_eq!(finish(&mut m, outcome), S_FALSE);
}

#[cfg(feature = "media")]
#[test]
fn guest_render_downstream_failure_rolls_back_an_accepted_pull_connection() {
    let (mut m, graph) = fixture(wave());
    m.ext.get_mut::<PullSource>().suppress_outputs = true;
    let outcome = pin(&mut env(&mut m), graph, OUTPUT_PIN);
    assert_eq!(finish(&mut m, outcome), VFW_E_CANNOT_RENDER);
    let source = m.ext.get::<PullSource>();
    assert_eq!(source.disconnects, 1);
    assert_eq!(source.peer, 0);
    assert_eq!(source.refs[&SOURCE], 2);
    assert_eq!(source.refs[&OUTPUT_PIN], 1);
    assert_eq!(source.refs[&READER], 1);
    assert_eq!(m.ext.get::<State>().graphs[&graph].members.len(), 1);
    assert!(m.ext.get::<State>().filters.is_empty());
    assert!(m.ext.get::<State>().pins.values().all(|p| p.peer == 0));
}

#[cfg(feature = "media")]
fn movie() -> Vec<u8> {
    // Same authored source as adv-device/tests/media_decode.rs. Generated once:
    // ffmpeg -f lavfi -i color=c=red:s=16x16:r=25:d=0.24 -f lavfi
    // -i sine=frequency=440:sample_rate=32000:duration=0.24 -c:v mpeg1video
    // -g 1 -b:v 200k -c:a mp2 -b:a 64k -shortest -packetsize 512 -f mpeg guest_movie.mpg
    // Small PES packets expose the tiny clip's end timestamps during probing.
    // The test itself needs no CLI and embeds no game data.
    include_bytes!("fixtures/guest_movie.mpg").to_vec()
}

#[cfg(feature = "media")]
fn invoke(
    m: &mut Machine<JitCpu>,
    pointer: u32,
    interface: &str,
    method: &str,
    args: &[u32],
) -> u32 {
    let slot = adv_abi::signature::com_signatures()
        .iter()
        .find(|signature| signature.module == interface && signature.name == method)
        .unwrap()
        .com_slot
        .unwrap();
    let outcome = guest_call(
        &mut env(m),
        pointer,
        u32::from(slot),
        args.to_vec(),
        |_, hr| Outcome::Return(hr),
    );
    finish(m, outcome)
}

#[cfg(feature = "media")]
#[test]
fn guest_movie_render_configures_system_window_streams_video_and_pcm_and_releases_source_refs() {
    use crate::dshow::{tests::query, video};
    use adv_machine::{Resume, WakeReason};

    #[derive(Default)]
    struct Presentation {
        updates: Vec<Option<adv_device::surface::VideoLayer>>,
        owner_visible: bool,
    }

    let (mut m, graph) = fixture(movie());
    crate::dsound::install(&mut m).unwrap();
    m.ext.get_mut::<crate::dsound::Sound>().enabled = true;
    m.ext.insert(Presentation {
        owner_visible: true,
        ..Presentation::default()
    });
    video::set_video_provider(
        &mut m.ext,
        VideoProvider {
            window: |ext, _, owner| {
                (owner == 42).then_some(video::VideoWindowGeometry {
                    width: 32,
                    height: 24,
                    origin: (1, 2),
                    canvas: (40, 30),
                    visible: ext.get::<Presentation>().owner_visible,
                })
            },
            update: |ext, _, layer| {
                let presentation = ext.get_mut::<Presentation>();
                presentation.updates.push(layer);
                Ok(presentation.updates.len() as u64)
            },
            completed: |_, _| None,
        },
    );
    assert_eq!(
        invoke(&mut m, graph, "IFilterGraph2", "Render", &[OUTPUT_PIN]),
        S_OK
    );
    let state = m.ext.get::<State>();
    assert_eq!(state.graphs[&graph].members.len(), 6);
    for role in [
        Role::Splitter,
        Role::VideoDecoder,
        Role::AudioDecoder,
        Role::VideoRenderer,
        Role::AudioRenderer,
    ] {
        assert_eq!(
            state
                .filters
                .values()
                .filter(|filter| filter.role == role)
                .count(),
            1
        );
    }
    let splitter = *state
        .filters
        .iter()
        .find(|(_, filter)| filter.role == Role::Splitter)
        .unwrap()
        .0;
    let info = state.filters[&splitter]
        .source
        .as_ref()
        .unwrap()
        .info
        .as_ref()
        .unwrap();
    let dimensions = info.video.as_ref().unwrap();
    assert_eq!((dimensions.width, dimensions.height), (16, 16));
    assert_eq!(info.audio.as_ref().unwrap().sample_rate, 32000);
    let audio = *state
        .filters
        .iter()
        .find(|(_, filter)| filter.role == Role::AudioRenderer)
        .unwrap()
        .0;
    let audio_input = endpoint(&env(&mut m), audio, INPUT);
    assert_eq!(
        m.ext.get::<State>().pins[&audio_input]
            .mt
            .as_ref()
            .unwrap()
            .sub,
        MEDIASUBTYPE_PCM
    );

    let window = query(&mut m, graph, IID_IVIDEOWINDOW);
    let control = query(&mut m, graph, IID_IMEDIACONTROL);
    let basic = query(&mut m, graph, IID_IBASICVIDEO);
    assert_eq!(
        invoke(&mut m, window, "IVideoWindow", "put_Owner", &[42]),
        S_OK
    );
    assert_eq!(
        invoke(
            &mut m,
            window,
            "IVideoWindow",
            "put_WindowStyle",
            &[0x5000_0000]
        ),
        S_OK
    );
    assert_eq!(
        invoke(
            &mut m,
            window,
            "IVideoWindow",
            "SetWindowPosition",
            &[3, 4, 8, 6]
        ),
        S_OK
    );
    assert_eq!(
        invoke(&mut m, window, "IVideoWindow", "put_Visible", &[u32::MAX]),
        S_OK
    );
    assert_eq!(
        invoke(
            &mut m,
            window,
            "IVideoWindow",
            "GetWindowPosition",
            &[DATA, DATA + 4, DATA + 8, DATA + 12]
        ),
        S_OK
    );
    let rectangle: Vec<u32> = (0..4)
        .map(|index| m.cpu.read_u32(DATA + index * 4).unwrap())
        .collect();
    assert_eq!(rectangle, [3, 4, 8, 6]);
    assert_eq!(
        invoke(
            &mut m,
            basic,
            "IBasicVideo",
            "GetVideoSize",
            &[DATA, DATA + 4]
        ),
        S_OK
    );
    assert_eq!(
        (
            m.cpu.read_u32(DATA).unwrap(),
            m.cpu.read_u32(DATA + 4).unwrap()
        ),
        (16, 16)
    );

    // Small MPEG-PS files can advertise only their first PES duration. A later
    // decoded picture must still become due rather than stall at that estimate.
    m.ext
        .get_mut::<State>()
        .filters
        .get_mut(&splitter)
        .unwrap()
        .source
        .as_mut()
        .unwrap()
        .duration_us = 20_000;
    assert_eq!(invoke(&mut m, control, "IMediaControl", "Run", &[]), S_OK);
    assert_eq!(m.ext.get::<State>().graphs[&graph].state, RUNNING);
    assert_eq!(m.ext.get::<State>().graphs[&graph].expected, 2);
    let session = *m.ext.get::<State>().sessions.keys().next().unwrap();
    // Drive the authored stream handler directly: no host window, device or
    // real guest program runs while exercising decode -> publish/mixer delivery.
    m.cpu.write_u32(STACK + 4, session).unwrap();
    let outcome = stream::thread_entry(&mut env(&mut m));
    let Outcome::Block { then, .. } = outcome else {
        panic!("stream must wait for media time");
    };
    // MPEG audio establishes the common origin slightly before the first
    // video picture. Advance to the first due picture rather than show it early.
    m.sys.clock.advance_to(m.sys.clock.now_us() + 20_000);
    let outcome = then(&mut env(&mut m), Resume::Woke(WakeReason::Timeout));
    let Outcome::Block { then, .. } = outcome else {
        panic!("stream must wait after its first frame");
    };
    let frame = video::latest(&env(&mut m), graph).unwrap();
    assert!(frame.bgra[0] < 20 && frame.bgra[1] < 20 && frame.bgra[2] > 220);
    m.sys.clock.advance_to(m.sys.clock.now_us() + 40_000);
    let outcome = then(&mut env(&mut m), Resume::Woke(WakeReason::Timeout));
    let Outcome::Block { then, .. } = outcome else {
        panic!("stream must wait after its second frame");
    };
    let frame = video::latest(&env(&mut m), graph).unwrap();
    assert!(frame.bgra[0] < 20 && frame.bgra[1] < 20 && frame.bgra[2] > 220);
    assert!(
        m.ext.get::<State>().filters[&splitter]
            .source
            .as_ref()
            .unwrap()
            .decoder
            .as_ref()
            .unwrap()
            .stats()
            .converted_frames
            >= 2
    );
    let presentation = m
        .ext
        .get::<Presentation>()
        .updates
        .last()
        .unwrap()
        .as_ref()
        .unwrap();
    assert_eq!(
        (
            presentation.destination.x,
            presentation.destination.y,
            presentation.destination.w,
            presentation.destination.h
        ),
        (4, 6, 8, 6)
    );
    assert!(
        m.ext
            .get::<crate::dsound::Sound>()
            .audio
            .stream()
            .metrics()
            .queued_source_frames
            > 0
    );

    assert_eq!(invoke(&mut m, control, "IMediaControl", "Pause", &[]), S_OK);
    assert_eq!(m.ext.get::<State>().graphs[&graph].state, PAUSED);
    let updates = m.ext.get::<Presentation>().updates.len();
    m.sys.clock.advance_to(m.sys.clock.now_us() + 400_000);
    let outcome = then(&mut env(&mut m), Resume::Woke(WakeReason::Signaled));
    let Outcome::Block { then, .. } = outcome else {
        panic!("paused stream must retain its frame and wait");
    };
    assert_eq!(
        video::latest(&env(&mut m), graph).unwrap().bgra.as_ref(),
        frame.bgra.as_ref()
    );
    assert_eq!(m.ext.get::<Presentation>().updates.len(), updates);
    // Owner visibility can change while the paused decoder emits no frames.
    // The existing service boundary must retract that retained movie layer.
    video::pump_video(&mut m).unwrap();
    assert_eq!(m.ext.get::<Presentation>().updates.len(), updates);
    m.ext.get_mut::<Presentation>().owner_visible = false;
    video::pump_video(&mut m).unwrap();
    assert!(
        m.ext
            .get::<Presentation>()
            .updates
            .last()
            .unwrap()
            .is_none()
    );
    assert_eq!(m.ext.get::<Presentation>().updates.len(), updates + 1);
    video::pump_video(&mut m).unwrap();
    assert_eq!(m.ext.get::<Presentation>().updates.len(), updates + 1);
    assert_eq!(invoke(&mut m, control, "IMediaControl", "Stop", &[]), S_OK);
    assert_eq!(m.ext.get::<State>().graphs[&graph].state, STOPPED);
    let outcome = then(&mut env(&mut m), Resume::Woke(WakeReason::Signaled));
    assert_eq!(finish(&mut m, outcome), 0);
    assert!(m.ext.get::<State>().sessions.is_empty());

    assert_eq!(invoke(&mut m, basic, "IBasicVideo", "Release", &[]), 3);
    assert_eq!(invoke(&mut m, control, "IMediaControl", "Release", &[]), 2);
    assert_eq!(invoke(&mut m, window, "IVideoWindow", "Release", &[]), 1);
    assert_eq!(invoke(&mut m, graph, "IFilterGraph2", "Release", &[]), 0);
    assert!(!m.ext.get::<State>().graphs.contains_key(&graph));
    assert!(m.ext.get::<State>().filters.is_empty());
    assert!(
        m.ext
            .get::<Presentation>()
            .updates
            .last()
            .unwrap()
            .is_none()
    );
    let source = m.ext.get::<PullSource>();
    assert_eq!(source.graph, 0);
    assert_eq!(source.peer, 0);
    assert_eq!(source.refs[&SOURCE], 1);
    assert_eq!(source.refs[&OUTPUT_PIN], 1);
    assert_eq!(source.refs[&READER], 1);
    assert_eq!(source.refs[&ENUMERATOR], 0);
    assert_eq!(source.controls, [PAUSED, RUNNING, PAUSED, STOPPED, STOPPED]);
}

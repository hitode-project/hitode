//! Authored guest COM protocol: reject an allocator on the first input, then
//! accept the second. This exercises failed negotiation and reference rollback.
use super::*;
use crate::dshow::graph::source::tests::finish;
use crate::dshow::tests::{activate, env};
use adv_cpu::JitCpu;
use adv_cpu::backend::Guest;
use adv_machine::{Clock, Conv, Machine};
use std::collections::BTreeMap;

const DATA: u32 = 0x500000;
const STACK: u32 = 0x600000;
const FILTER: u32 = DATA + 0x800;
const ENUM: u32 = DATA + 0x810;
const PINS: [u32; 2] = [DATA + 0x820, DATA + 0x830];

#[derive(Default)]
struct Renderer {
    refs: BTreeMap<u32, u32>,
    next: usize,
    connected: BTreeMap<u32, u32>,
    reject_all: bool,
    rejected: u32,
    disconnected: u32,
}

fn callback(env: &mut Env<'_>) -> Outcome {
    let op = env.sys.thunks.get(env.thunk).f.data;
    let pointer = env.arg(0);
    let this = if PINS.iter().any(|pin| pointer == pin + 0x200) {
        pointer - 0x200
    } else {
        pointer
    };
    let r = |hr| Outcome::Return(hr);
    match op {
        0 => {
            let output = env.arg(2);
            assert_eq!(read_guid(env, env.arg(1)).unwrap(), IID_IMEMINPUTPIN);
            *env.ext.get_mut::<Renderer>().refs.entry(this).or_default() += 1;
            env.cpu.write_u32(output, this + 0x200).unwrap();
            r(S_OK)
        }
        1 => {
            let refs = env.ext.get_mut::<Renderer>().refs.entry(this).or_default();
            *refs += 1;
            r(*refs)
        }
        2 => {
            let refs = env.ext.get_mut::<Renderer>().refs.get_mut(&this).unwrap();
            assert!(*refs > 0);
            *refs -= 1;
            r(*refs)
        }
        3 => {
            // EnumPins
            let output = env.arg(1);
            let fake = env.ext.get_mut::<Renderer>();
            fake.next = 0;
            *fake.refs.entry(ENUM).or_default() += 1;
            env.cpu.write_u32(output, ENUM).unwrap();
            r(S_OK)
        }
        4 => {
            // Next(1)
            assert_eq!(env.arg(1), 1);
            let (output, fetched) = (env.arg(2), env.arg(3));
            let fake = env.ext.get_mut::<Renderer>();
            let pin = PINS.get(fake.next).copied();
            fake.next += 1;
            if let Some(pin) = pin {
                *fake.refs.entry(pin).or_default() += 1;
            }
            env.cpu.write_u32(output, pin.unwrap_or(0)).unwrap();
            env.cpu
                .write_u32(fetched, u32::from(pin.is_some()))
                .unwrap();
            r(if pin.is_some() { S_OK } else { S_FALSE })
        }
        5 => {
            env.cpu.write_u32(env.arg(1), INPUT).unwrap();
            r(S_OK)
        }
        6 => {
            env.cpu.write_u32(env.arg(1), 0).unwrap();
            r(VFW_E_NOT_CONNECTED)
        }
        7 => {
            // ReceiveConnection: validate a real negotiated RGB image type.
            let mt = MediaType::read(env, env.arg(2)).unwrap();
            assert_eq!(mt.major, MEDIATYPE_VIDEO);
            assert_eq!(mt.sub, MEDIASUBTYPE_RGB32);
            assert_eq!(mt.video_geometry(), Some((3, 2, 32)));
            let output = env.arg(1);
            env.ext.get_mut::<Renderer>().connected.insert(this, output);
            r(S_OK)
        }
        8 => {
            let fake = env.ext.get_mut::<Renderer>();
            fake.connected.remove(&this);
            fake.disconnected += 1;
            r(S_OK)
        }
        9 => {
            // NotifyAllocator: an actual common allocator was offered.
            assert!(matches!(payload(env, env.arg(1)), Some(Obj::Allocator)));
            let fake = env.ext.get_mut::<Renderer>();
            if this == PINS[0] || fake.reject_all {
                fake.rejected += 1;
                r(0x8000_4005)
            } else {
                r(S_OK)
            }
        }
        10 => r(S_OK), // JoinFilterGraph
        _ => unreachable!(),
    }
}

fn fixture(reject_all: bool) -> (Machine<JitCpu>, u32, u32) {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    adv_win32::ole32::register(&mut registry);
    register(&mut registry);
    for (op, argc) in [
        (0, 3),
        (1, 1),
        (2, 1),
        (3, 2),
        (4, 4),
        (5, 2),
        (6, 2),
        (7, 3),
        (8, 1),
        (9, 3),
        (10, 3),
    ] {
        registry.add_with_data(
            "authored-renderer.dll",
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
        adv_win32::Win32Config::new(".", "authored.exe", "target/dshow-render-tests"),
    )
    .unwrap();
    install(&mut m);
    m.cpu.map(DATA, 4096, Prot::RW).unwrap();
    m.cpu.map(STACK, 4096, Prot::RW).unwrap();
    m.ext.insert(Renderer {
        reject_all,
        refs: [(FILTER, 1), (ENUM, 0), (PINS[0], 1), (PINS[1], 1)].into(),
        ..Renderer::default()
    });
    let mut methods = BTreeMap::new();
    for op in 0..=10 {
        methods.insert(
            op,
            m.resolve_import("authored-renderer.dll", &format!("method{op}"))
                .unwrap(),
        );
    }
    for (object, table, slots) in [
        (
            FILTER,
            DATA + 0x900,
            vec![(1, 1), (2, 2), (10, 3), (13, 10)],
        ),
        (ENUM, DATA + 0xa00, vec![(1, 1), (2, 2), (3, 4)]),
        (
            PINS[0],
            DATA + 0xb00,
            vec![(0, 0), (1, 1), (2, 2), (4, 7), (5, 8), (6, 6), (9, 5)],
        ),
        (
            PINS[1],
            DATA + 0xc00,
            vec![(0, 0), (1, 1), (2, 2), (4, 7), (5, 8), (6, 6), (9, 5)],
        ),
    ] {
        m.cpu.write_u32(object, table).unwrap();
        for (slot, op) in slots {
            m.cpu.write_u32(table + slot * 4, methods[&op]).unwrap();
        }
    }
    // IMemInputPin and IPin have different slot 4 methods. Query returns a
    // separate face sharing the authored renderer's reference identity.
    // Use host callbacks for the two mem faces below.
    for pin in PINS {
        let table = m.cpu.read_u32(pin).unwrap();
        let mem = pin + 0x200;
        m.cpu.write_u32(mem, table + 0x300).unwrap();
        for (slot, op) in [(1, 1), (2, 2), (4, 9)] {
            m.cpu
                .write_u32(table + 0x300 + slot * 4, methods[&op])
                .unwrap();
        }
    }
    let graph = activate(&mut m, CLSID_FILTERGRAPH, IID_IGRAPHBUILDER);
    m.cpu.set_reg(adv_cpu::backend::Reg::Esp, STACK);
    for (i, value) in [graph, FILTER, 0].into_iter().enumerate() {
        m.cpu.write_u32(STACK + 4 + i as u32 * 4, value).unwrap();
    }
    let outcome = super::super::add_filter(&mut env(&mut m), graph, FILTER, 0);
    assert_eq!(finish(&mut m, outcome), S_OK);
    let splitter = member(
        &mut env(&mut m),
        graph,
        Role::Splitter,
        CLSID_MPEG1_SPLITTER,
        "Authored source",
    )
    .unwrap();
    m.ext
        .get_mut::<State>()
        .filters
        .get_mut(&splitter)
        .unwrap()
        .source = Some(filter::Source {
        file_handle: None,
        decoder: None,
        duration_us: 1_000_000,
        info: Some(adv_device::media::StreamInfo {
            container: "authored".into(),
            duration_us: 1_000_000,
            audio: None,
            video: Some(adv_device::media::VideoInfo {
                codec: "authored".into(),
                width: 3,
                height: 2,
                frame_rate: (30, 1),
            }),
        }),
    });
    let output = filter::new_pin(
        &mut env(&mut m),
        splitter,
        OUTPUT,
        "Video",
        MEDIATYPE_VIDEO,
        Some(MediaType {
            major: MEDIATYPE_VIDEO,
            sub: MEDIASUBTYPE_MPEG1PAYLOAD,
            fixed_size: false,
            temporal: true,
            sample_size: 0,
            format_type: GUID_NULL,
            format: Vec::new(),
        }),
    )
    .unwrap();
    let decoder = member(
        &mut env(&mut m),
        graph,
        Role::VideoDecoder,
        CLSID_MPEG_VIDEO_CODEC,
        "Authored decoder",
    )
    .unwrap();
    let input = endpoint(&env(&mut m), decoder, INPUT);
    connect(&mut env(&mut m), output, input).unwrap();
    let out = endpoint(&env(&mut m), decoder, OUTPUT);
    (m, graph, out)
}

#[test]
// spec: MEDIA-GRAPH-3
fn video_render_retries_after_allocator_refusal_and_leaves_only_the_accepted_connection() {
    let (mut m, graph, out) = fixture(false);
    let outcome = pin(&mut env(&mut m), graph, out);
    assert_eq!(finish(&mut m, outcome), S_OK);
    let fake = m.ext.get::<Renderer>();
    assert_eq!(fake.rejected, 1);
    assert_eq!(fake.disconnected, 1);
    assert_eq!(fake.refs[&PINS[0]], 1);
    assert_eq!(fake.refs[&ENUM], 0);
    assert_eq!(fake.connected.len(), 1);
    assert_eq!(fake.connected[&PINS[1]], out);
    assert_eq!(m.ext.get::<State>().pins[&out].peer, PINS[1]);
}

#[test]
fn video_render_refusal_is_failure_and_retires_every_failed_connection() {
    let (mut m, graph, out) = fixture(true);
    let outcome = pin(&mut env(&mut m), graph, out);
    assert_eq!(finish(&mut m, outcome), VFW_E_CANNOT_RENDER);
    let fake = m.ext.get::<Renderer>();
    assert_eq!(fake.rejected, 2);
    assert_eq!(fake.disconnected, 2);
    assert!(fake.connected.is_empty());
    for pointer in PINS {
        assert_eq!(fake.refs[&pointer], 1);
    }
    assert_eq!(fake.refs[&ENUM], 0);
    let output = &m.ext.get::<State>().pins[&out];
    assert_eq!(output.peer, 0);
    assert_eq!(output.mem, 0);
    assert_eq!(output.allocator, 0);
}

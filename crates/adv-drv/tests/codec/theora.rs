use super::*;
use adv_cpu::{
    JitCpu,
    backend::{Prot, asm::Asm},
};
use adv_machine::{Machine, Resolver};
struct Platform;
impl Resolver for Platform {
    fn provides(&self, _: &str) -> bool {
        true
    }
    fn host_fn(&self, module: &str, name: &str) -> Option<HostFn> {
        if module != MODULE {
            return None;
        }
        let (handler, argc) = match name {
            "create" => (create as adv_machine::Handler, 0),
            "malloc" => (allocate as adv_machine::Handler, 1),
            "free" => (free as adv_machine::Handler, 1),
            "done" => (done as adv_machine::Handler, 0),
            "stripe" => (stripe as adv_machine::Handler, 4),
            "predict" => (predict_fixture as adv_machine::Handler, 0),
            _ => return host_fn(module, name),
        };
        Some(HostFn {
            conv: Conv::Cdecl { argc },
            handler,
            data: 0,
            leaf: None,
            trace: false,
            guest: None,
        })
    }
}
fn allocate(e: &mut Env<'_>) -> Outcome {
    assert_eq!(e.cpu.arg(0).unwrap(), 48 * 48 + 2 * 24 * 24);
    let allocator = e.ext.get_mut::<Allocator>();
    allocator.allocations += 1;
    Outcome::Return(if allocator.fail { 0 } else { STORAGE })
}
fn free(e: &mut Env<'_>) -> Outcome {
    assert_eq!(e.cpu.arg(0).unwrap(), STORAGE);
    e.ext.get_mut::<Allocator>().frees += 1;
    Outcome::Return(0)
}
fn done(_: &mut Env<'_>) -> Outcome {
    Outcome::ExitProcess(0)
}

fn predict_fixture(e: &mut Env<'_>) -> Outcome {
    let provider = e.ext.get_mut::<Provider>();
    let decoder = &mut provider.decoders.get_mut(&CONTEXT).unwrap().decoder;
    assert!(decoder.predict(
        initial_control(0),
        RUN_AHEAD_DEPTH,
        |depth| {
            (0..depth)
                .map(|packetno| Packet {
                    bytes: Vec::new(),
                    bos: 0,
                    eos: 0,
                    granulepos: -1,
                    packetno: packetno as i64,
                })
                .collect()
        },
        &mut provider.stats.run_ahead,
    ));
    Outcome::Return(0)
}

use adv_machine::{
    Clock, RunResult,
    layout::{PAGE, PARK_ADDR},
};
const REF_PLANES: u32 = 300;
const CURRENT: u32 = 896;
const GRANPOS: u32 = 904;
const FRAME_TYPE: u32 = 912;
const OUTPUT: u32 = 54104;
const STRIPE: u32 = 54152;
const FIXTURE_LAYOUT: Layout = Layout {
    info_bytes: 64,
    init_bytes: STRIPE + 8,
    info: 0,
    ref_planes: REF_PLANES,
    ref_indices: 276,
    plane_allocation: 588,
    keyframe: 888,
    current: CURRENT,
    granpos: GRANPOS,
    frame_type: FRAME_TYPE,
    bias: FRAME_TYPE + 1,
    pp_level: 53572,
    output: OUTPUT,
    stripe: STRIPE,
};

const CODE: u32 = 0x0040_0000;
const CONTEXT: u32 = 0x0041_0000;
const PACKET: u32 = CONTEXT + 0xf000;
const GRANULE: u32 = PACKET + 32;
const SLOT: u32 = PACKET + 40;
const RESULT: u32 = PACKET + 48;
const STORAGE: u32 = CONTEXT + 0x10000;
struct Allocator {
    fail: bool,
    allocations: u32,
    frees: u32,
}
fn create(e: &mut Env<'_>) -> Outcome {
    let mut info = [0u8; 64];
    info[..3].copy_from_slice(&[3, 2, 1]);
    for (at, value) in [
        (4, 16u32),
        (8, 16),
        (12, 16),
        (16, 16),
        (28, 30),
        (32, 1),
        (60, 6),
    ] {
        info[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }
    let setup = Setup {
        info,
        huffman: std::array::from_fn(|_| vec![1, -271, -312]),
        dc_scale: [100; 64],
        ac_scale: [100; 64],
        loop_filter_limits: [16; 64],
        ranges: std::array::from_fn(|_| (vec![63], vec![16; 128])),
    };
    native_init(e, 0, CONTEXT, Decoder::with_run_ahead(&setup).unwrap())
}
fn fixture(mode: Mode, fail: bool) -> Machine<JitCpu> {
    let mut machine = Machine::new(
        JitCpu::new(PARK_ADDR, false).unwrap(),
        Platform,
        Clock::virtual_time(),
    )
    .unwrap();
    machine.cpu.map(CODE, PAGE, Prot::RX).unwrap();
    machine.cpu.map(CONTEXT, PAGE * 32, Prot::RW).unwrap();
    let malloc = machine.resolve_import(MODULE, "malloc").unwrap();
    let free = machine.resolve_import(MODULE, "free").unwrap();
    machine.cpu.write_u32(SLOT, malloc).unwrap();
    ogg::install(&mut machine.ext);
    let mut original_clear = Asm::new(CODE + 0x300);
    original_clear
        .mov_eax_mem(CONTEXT + 588)
        .push_eax()
        .call(free)
        .add_esp(4)
        .ret();
    machine
        .cpu
        .write(CODE + 0x300, &original_clear.bytes)
        .unwrap();
    machine
        .cpu
        .write(CODE + 0x380, &[0x31, 0xc0, 0xc3])
        .unwrap();
    let entry = || Entry {
        entry: CODE + 0x300,
        original: CODE + 0x300,
        thunk: 0,
        trampoline: Vec::new(),
    };
    machine.ext.insert(Provider {
        mode,
        bindings: vec![Binding {
            layout: FIXTURE_LAYOUT,
            allocator: SLOT,
            live: true,
            init: entry(),
            packet: entry(),
            clear: entry(),
            ctl: Some(Entry {
                entry: CODE + 0x380,
                original: CODE + 0x380,
                ..entry()
            }),
            snapshots: Vec::new(),
        }],
        decoders: BTreeMap::new(),
        stats: Stats::default(),
        file_counts: Arc::default(),
    });
    machine.ext.insert(Allocator {
        fail,
        allocations: 0,
        frees: 0,
    });
    machine
}
// spec: DEV-VIDEO-1, CODEC-CALL-2
#[test]
fn native_lifetime_uses_guest_allocator_and_preserves_first_duplicate_frame() {
    for fail in [false, true] {
        let mut machine = fixture(Mode::Native, fail);
        let ctor = machine.resolve_import(MODULE, "create").unwrap();
        let packet = machine.resolve_import(MODULE, "packet_0").unwrap();
        let clear = machine.resolve_import(MODULE, "clear_0").unwrap();
        let done = machine.resolve_import(MODULE, "done").unwrap();
        let mut caller = Asm::new(CODE);
        caller
            .call(ctor)
            .mov_mem_eax(RESULT)
            .cmp_mem_imm8(RESULT, 0)
            .jnz(done);
        caller
            .mov_mem_imm(CONTEXT + CURRENT, 0)
            .mov_mem_imm(CONTEXT + CURRENT + 4, 0)
            .mov_ecx_imm(CONTEXT);
        caller.bytes.push(0xba);
        caller.bytes.extend_from_slice(&PACKET.to_le_bytes());
        caller
            .push_imm(GRANULE)
            .call(packet)
            .add_esp(4)
            .mov_mem_eax(RESULT + 4);
        caller.mov_ecx_imm(CONTEXT).call(clear).call(done);
        machine.cpu.write(CODE, &caller.bytes).unwrap();
        machine
            .spawn("authored Theora ABI fixture", CODE, &[], 0x10000)
            .unwrap();
        assert!(matches!(
            machine.run_virtual(1_000_000),
            RunResult::Exited(0)
        ));
        assert_eq!(machine.ext.get::<Allocator>().allocations, 1);
        assert_eq!(machine.ext.get::<Allocator>().frees, u32::from(!fail));
        assert!(machine.ext.get::<Provider>().decoders.is_empty());
        assert_eq!(
            machine.cpu.read_u32(RESULT).unwrap(),
            if fail { u32::MAX } else { 0 }
        );
        if fail {
            continue;
        }
        assert_eq!(machine.cpu.read_u32(RESULT + 4).unwrap(), 1);
        assert_eq!(
            i64::from_le_bytes(read(&machine.cpu, GRANULE, 8).unwrap().try_into().unwrap()),
            64
        );
        let descriptors = read(&machine.cpu, CONTEXT + OUTPUT, 48).unwrap();
        for plane in 0..3 {
            let width = word(&descriptors, plane * 16);
            let height = word(&descriptors, plane * 16 + 4);
            let stride = word(&descriptors, plane * 16 + 8) as i32;
            let data = word(&descriptors, plane * 16 + 12);
            for row in 0..height {
                assert!(
                    read(
                        &machine.cpu,
                        data.wrapping_add((row as i32 * stride) as u32),
                        width as usize
                    )
                    .unwrap()
                    .iter()
                    .all(|&b| b == 128)
                );
            }
        }
    }
}

fn family_fixture() -> (Family, Vec<u8>) {
    // The library-signature unit fixture, with all three reviewed hook roles.
    let mut bytes = vec![0; PAGE as usize];
    let init = [
        0x55, 0x8b, 0xec, 0x83, 0xec, 0x10, 0xe8, 0, 0, 0, 0, 0xe8, 0, 0, 0, 0, 0xa1, 0, 0, 0, 0,
        0xa1, 0, 0, 0, 0, 0xc9, 0xc3,
    ];
    bytes[..init.len()].copy_from_slice(&init);
    bytes[7..11].copy_from_slice(&(0x100u32 - 11).to_le_bytes());
    bytes[12..16].copy_from_slice(&(0x200u32 - 16).to_le_bytes());
    bytes[17..21].copy_from_slice(&(CODE + 0x300).to_le_bytes());
    bytes[22..26].copy_from_slice(&(CODE + 0x400).to_le_bytes());
    bytes[0x100..0x10d].copy_from_slice(&[
        0x55, 0x8b, 0xec, 0x83, 0xec, 0x10, 0xb8, 7, 0, 0, 0, 0xc9, 0xc3,
    ]);
    bytes[0x200..0x20d].copy_from_slice(&[
        0x55, 0x8b, 0xec, 0x83, 0xec, 0x10, 0xb8, 8, 0, 0, 0, 0xc9, 0xc3,
    ]);
    bytes[0x400..0x404].copy_from_slice(&42u32.to_le_bytes());
    let text = concat!(
        "root init 6\nhook packet 6\nhook clear 6\ncode init 28 7172a948bdc2bc02c6a0002a3cf85feb18b2659e95825d32fdff7047d1455bf3\nlink init 7 rel packet 0\nlink init 12 rel clear 0\nlink init 17 abs malloc 0\nlink init 22 abs table 0\ncode packet 13 07dd12334a3dbd22a6baa2c8312ca2a02f01d11911d4d45a75d75f82f512143c\ncode clear 13 a8f24299d952d9a343f5d23d104f1acab809a2f524b4a907332d0cca0e222009\nslot malloc\ndata table 4 e8a4b2ee7ede79a3afb332b5b6cc3d952a65fd8cffb897f5d18016577c33d7cc\n",
        "field info_bytes 64\nfield init_bytes 54160\nfield info 0\nfield ref_planes 300\n",
        "field ref_indices 276\nfield plane_allocation 588\nfield keyframe 888\nfield current 896\n",
        "field granpos 904\nfield frame_type 912\nfield bias 913\nfield pp_level 53572\n",
        "field output 54104\nfield stripe 54152\n",
    );
    (Family::parse(text).unwrap(), bytes)
}

// spec: CODEC-MAP-1, CODEC-CALL-2, DEV-VIDEO-1
#[test]
fn complete_family_admission_rejects_changed_code_and_tables() {
    for change in [None, Some(0x107), Some(0x400), Some(7)] {
        let mut m = Machine::new(
            JitCpu::new(PARK_ADDR, false).unwrap(),
            Platform,
            Clock::virtual_time(),
        )
        .unwrap();
        m.cpu.map(CODE, PAGE, Prot::RX).unwrap();
        install(&mut m.ext, Mode::Native);
        let (family, mut bytes) = family_fixture();
        if let Some(offset) = change {
            bytes[offset] ^= 1;
        }
        m.cpu.write(CODE, &bytes).unwrap();
        m.visit_context(None, |e| {
            bind_family(
                e,
                &[ExecutableRange {
                    start: CODE,
                    len: PAGE,
                }],
                &family,
            )
        })
        .unwrap()
        .unwrap();
        assert_eq!(stats(&m.ext).installed, u64::from(change.is_none()));
        assert_eq!(m.sys.redirects.len(), if change.is_none() { 3 } else { 0 });
        assert_eq!(read(&m.cpu, CODE, PAGE as usize).unwrap(), bytes);
    }
}

// spec: CODEC-CALL-2
#[test]
fn unowned_decoder_declines_without_guest_memory_changes() {
    let mut m = Machine::new(
        JitCpu::new(PARK_ADDR, false).unwrap(),
        Platform,
        Clock::virtual_time(),
    )
    .unwrap();
    m.cpu.map(CODE, PAGE * 2, Prot::RX).unwrap();
    m.cpu.map(CONTEXT, PAGE * 16, Prot::RW).unwrap();
    install(&mut m.ext, Mode::Native);
    let (family, bytes) = family_fixture();
    m.cpu.write(CODE, &bytes).unwrap();
    m.visit_context(None, |e| {
        bind_family(
            e,
            &[ExecutableRange {
                start: CODE,
                len: PAGE,
            }],
            &family,
        )
    })
    .unwrap()
    .unwrap();
    let packet = m.resolve_import(MODULE, "packet_0").unwrap();
    let init = m.resolve_import(MODULE, "init_0").unwrap();
    let done = m.resolve_import(MODULE, "done").unwrap();
    let mut caller = Asm::new(CODE + PAGE);
    caller.mov_ecx_imm(CONTEXT);
    caller.bytes.push(0xba);
    caller.bytes.extend_from_slice(&PACKET.to_le_bytes());
    caller.push_imm(0).call(init).add_esp(4).mov_mem_eax(RESULT);
    caller.mov_ecx_imm(CONTEXT);
    caller.bytes.push(0xba);
    caller.bytes.extend_from_slice(&PACKET.to_le_bytes());
    caller
        .push_imm(GRANULE)
        .call(packet)
        .add_esp(4)
        .mov_mem_eax(RESULT + 4)
        .call(done);
    m.cpu.write(CODE + PAGE, &caller.bytes).unwrap();
    m.cpu.write(CONTEXT, &[0xa5; 64]).unwrap();
    m.spawn("Theora fallback fixture", CODE + PAGE, &[], 0x10000)
        .unwrap();
    let result = m.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    assert_eq!(read(&m.cpu, CONTEXT, 64).unwrap(), [0xa5; 64]);
    assert_eq!(m.cpu.read_u32(RESULT).unwrap(), 42);
    assert_eq!(m.cpu.read_u32(RESULT + 4).unwrap(), 7);
    assert!(m.ext.get::<Provider>().decoders.is_empty());
    assert_eq!(stats(&m.ext).native_frames, 0);
}

fn packet_body(result: u32, frame_type: u8) -> Asm {
    // Authored guest oracle for gray all-EOB and first empty packets.
    // Both publish gray references and granule 64 in this ABI fixture.
    let mut code = Asm::new(CODE + 0x500);
    code.bytes.extend_from_slice(&[0x56, 0x57, 0xbe]);
    code.bytes
        .extend_from_slice(&(CONTEXT + REF_PLANES).to_le_bytes());
    code.bytes.push(0xbf);
    code.bytes
        .extend_from_slice(&(CONTEXT + OUTPUT).to_le_bytes());
    code.bytes
        .extend_from_slice(&[0xb9, 12, 0, 0, 0, 0xf3, 0xa5]);
    for (base, width, height, stride, pad) in [
        (STORAGE, 16, 16, 48, 16),
        (STORAGE + 48 * 48, 8, 8, 24, 8),
        (STORAGE + 48 * 48 + 24 * 24, 8, 8, 24, 8),
    ] {
        for row in 0..height {
            for column in (0..width).step_by(4) {
                code.mov_mem_imm(base + (pad + row) * stride + pad + column, 0x80808080);
            }
        }
    }
    code.mov_mem_imm(CONTEXT + CURRENT, 1)
        .mov_mem_imm(CONTEXT + CURRENT + 4, 0)
        .mov_mem_imm(CONTEXT + GRANPOS, 64)
        .mov_mem_imm(CONTEXT + GRANPOS + 4, 0)
        .mov_mem_imm(GRANULE, 64)
        .mov_mem_imm(GRANULE + 4, 0);
    code.bytes.extend_from_slice(&[0xc6, 0x05]);
    code.bytes
        .extend_from_slice(&(CONTEXT + FRAME_TYPE).to_le_bytes());
    code.bytes.push(frame_type);
    code.bytes.extend_from_slice(&[0x5f, 0x5e, 0xb8]);
    code.bytes.extend_from_slice(&result.to_le_bytes());
    code.ret();
    code
}

// spec: CODEC-CALL-2, DEV-VIDEO-1
#[test]
fn packet_pixels_state_and_result_match_guest() {
    // The device fixture's all-EOB keyframe and the ABI fixture's empty packet.
    for (input, result, frame_type) in [(Vec::new(), 1, 1), (vec![0; 5], 0, 0)] {
        let mut observations = Vec::new();
        for mode in [Mode::Native, Mode::Verify] {
            let mut m = fixture(mode, false);
            let guest = packet_body(result, frame_type);
            m.cpu.write(PACKET + 0x100, &input).unwrap();
            m.cpu.write_u32(PACKET, PACKET + 0x100).unwrap();
            m.cpu.write_u32(PACKET + 4, input.len() as u32).unwrap();
            m.cpu.write(CODE + 0x500, &guest.bytes).unwrap();
            m.ext.get_mut::<Provider>().bindings[0].packet.original = CODE + 0x500;
            let ctor = m.resolve_import(MODULE, "create").unwrap();
            let packet = m.resolve_import(MODULE, "packet_0").unwrap();
            let done = m.resolve_import(MODULE, "done").unwrap();
            let mut caller = Asm::new(CODE);
            caller
                .call(ctor)
                .mov_mem_imm(CONTEXT + CURRENT, 0)
                .mov_mem_imm(CONTEXT + CURRENT + 4, 0)
                .mov_ecx_imm(CONTEXT);
            caller.bytes.push(0xba);
            caller.bytes.extend_from_slice(&PACKET.to_le_bytes());
            caller
                .push_imm(GRANULE)
                .call(packet)
                .add_esp(4)
                .mov_mem_eax(RESULT)
                .call(done);
            m.cpu.write(CODE, &caller.bytes).unwrap();
            m.spawn("Theora packet differential", CODE, &[], 0x10000)
                .unwrap();
            let result = m.run_virtual(1_000_000);
            assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
            let descriptors = read(&m.cpu, CONTEXT + OUTPUT, 48).unwrap();
            let mut pixels = Vec::new();
            for plane in 0..3 {
                let width = word(&descriptors, plane * 16);
                let height = word(&descriptors, plane * 16 + 4);
                let stride = word(&descriptors, plane * 16 + 8) as i32;
                let data = word(&descriptors, plane * 16 + 12);
                for row in 0..height {
                    pixels.extend(
                        read(
                            &m.cpu,
                            data.wrapping_add((row as i32 * stride) as u32),
                            width as usize,
                        )
                        .unwrap(),
                    );
                }
            }
            observations.push((
                pixels,
                state_read(&m.cpu, CONTEXT, FIXTURE_LAYOUT).unwrap(),
                read(&m.cpu, GRANULE, 8).unwrap(),
                m.cpu.read_u32(RESULT).unwrap(),
            ));
            let stats = stats(&m.ext);
            if mode == Mode::Verify {
                assert_eq!(stats.compared_bytes, 384);
                assert_eq!((stats.different_bytes, stats.state_mismatches), (0, 0));
                assert!(stats.reason.is_none(), "{:?}", stats.reason);
            } else {
                assert_eq!(stats.native_frames, 1);
            }
        }
        assert_eq!(observations[0], observations[1]);
        assert!(observations[0].0.iter().all(|&byte| byte == 128));
    }
}

fn stripe(e: &mut Env<'_>) -> Outcome {
    assert_eq!(e.arg(0), 0x1234);
    let descriptors = read(e.cpu, e.arg(1), 48).unwrap();
    let (first, end) = (e.arg(2), e.arg(3));
    assert!(first < end && end <= 2);
    for plane in 0..3 {
        let width = word(&descriptors, plane * 16);
        let height = word(&descriptors, plane * 16 + 4);
        let stride = word(&descriptors, plane * 16 + 8) as i32;
        let data = word(&descriptors, plane * 16 + 12);
        for row in first * 8 * height / 16..end * 8 * height / 16 {
            let pixels = read(
                e.cpu,
                data.wrapping_add((row as i32 * stride) as u32),
                width as usize,
            )
            .unwrap();
            assert!(pixels.iter().all(|&byte| byte == 128));
        }
    }
    let count = e.cpu.read_u32(RESULT + 8).unwrap();
    e.cpu.write_u32(RESULT + 8, count + 1).unwrap();
    Outcome::Return(0)
}

// spec: CODEC-CALL-2, DEV-VIDEO-1, WINE-THR-1
#[test]
fn stripe_callbacks_observe_published_rows_before_packet_return() {
    let mut m = fixture(Mode::Native, false);
    let ctor = m.resolve_import(MODULE, "create").unwrap();
    let packet = m.resolve_import(MODULE, "packet_0").unwrap();
    let callback = m.resolve_import(MODULE, "stripe").unwrap();
    let done = m.resolve_import(MODULE, "done").unwrap();
    m.cpu.write_u32(PACKET, PACKET + 0x100).unwrap();
    m.cpu.write_u32(PACKET + 4, 5).unwrap();
    let mut caller = Asm::new(CODE);
    caller
        .call(ctor)
        .mov_mem_imm(CONTEXT + CURRENT, 0)
        .mov_mem_imm(CONTEXT + CURRENT + 4, 0)
        .mov_mem_imm(CONTEXT + STRIPE, 0x1234)
        .mov_mem_imm(CONTEXT + STRIPE + 4, callback)
        .mov_ecx_imm(CONTEXT);
    caller.bytes.push(0xba);
    caller.bytes.extend_from_slice(&PACKET.to_le_bytes());
    caller
        .push_imm(GRANULE)
        .call(packet)
        .add_esp(4)
        .mov_mem_eax(RESULT)
        .call(done);
    m.cpu.write(CODE, &caller.bytes).unwrap();
    m.spawn("Theora stripe fixture", CODE, &[], 0x10000)
        .unwrap();
    let result = m.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    assert_eq!(m.cpu.read_u32(RESULT).unwrap(), 0);
    assert_eq!(m.cpu.read_u32(RESULT + 8).unwrap(), 1);
    assert_eq!(m.cpu.read_u32(GRANULE).unwrap(), 64);
    assert_eq!(stats(&m.ext).native_frames, 1);
}

// spec: CODEC-MAP-1, CODEC-CALL-2
#[test]
fn unmapping_retires_decoder_state_outside_the_image() {
    let mut m = fixture(Mode::Native, false);
    let ctor = m.resolve_import(MODULE, "create").unwrap();
    let done = m.resolve_import(MODULE, "done").unwrap();
    let mut caller = Asm::new(CODE);
    caller.call(ctor).call(done);
    m.cpu.write(CODE, &caller.bytes).unwrap();
    m.spawn("Theora retirement fixture", CODE, &[], 0x10000)
        .unwrap();
    assert!(matches!(m.run_virtual(1_000_000), RunResult::Exited(0)));
    assert!(m.ext.get::<Provider>().decoders.contains_key(&CONTEXT));
    m.ext.insert(super::super::Images {
        mode: Mode::Native,
        mapped: BTreeMap::from([(CODE, PAGE)]),
    });
    super::super::mdct::install(&mut m.ext, Mode::Native);
    m.sys.redirects.insert(CODE + 0x300, (0, Vec::new().into()));
    m.visit_context(None, |e| super::super::forget_image(e, CODE))
        .unwrap();
    assert!(m.ext.get::<Provider>().decoders.is_empty());
    assert!(!m.ext.get::<Provider>().bindings[0].live);
    assert!(m.sys.redirects.is_empty());
}

// Packed decoder tables and packets from the libtheora 1.2.0 encoder.
// The source is twelve generated 320x224 YUV frames with a moving rectangle,
// 8 fps, quality 30 and a four-frame keyframe interval.
fn generated_stream() -> (Setup, Vec<Packet>) {
    fn take<'a>(bytes: &mut &'a [u8], count: usize) -> &'a [u8] {
        let (head, tail) = bytes.split_at(count);
        *bytes = tail;
        head
    }

    let mut bytes = include_bytes!("../data/theora-run-ahead.bin").as_slice();
    let info = take(&mut bytes, 64).try_into().unwrap();
    let huffman = std::array::from_fn(|_| {
        let count = u16::from_le_bytes(take(&mut bytes, 2).try_into().unwrap());
        take(&mut bytes, usize::from(count) * 2)
            .as_chunks::<2>()
            .0
            .iter()
            .copied()
            .map(i16::from_le_bytes)
            .collect()
    });
    let dc_scale =
        std::array::from_fn(|_| u16::from_le_bytes(take(&mut bytes, 2).try_into().unwrap()));
    let ac_scale =
        std::array::from_fn(|_| u16::from_le_bytes(take(&mut bytes, 2).try_into().unwrap()));
    let loop_filter_limits = take(&mut bytes, 64).try_into().unwrap();
    let ranges = std::array::from_fn(|_| {
        let count = word(take(&mut bytes, 4), 0) as usize;
        let sizes = (0..count)
            .map(|_| i32::from_le_bytes(take(&mut bytes, 4).try_into().unwrap()))
            .collect();
        let matrices = take(&mut bytes, (count + 1) * 64).to_vec();
        (sizes, matrices)
    });
    let mut packets = Vec::new();
    while !bytes.is_empty() {
        let header = take(&mut bytes, 28);
        packets.push(Packet {
            bytes: take(&mut bytes, word(header, 0) as usize).to_vec(),
            bos: word(header, 4),
            eos: word(header, 8),
            granulepos: i64::from_le_bytes(header[12..20].try_into().unwrap()),
            packetno: i64::from_le_bytes(header[20..28].try_into().unwrap()),
        });
    }
    (
        Setup {
            info,
            huffman,
            dc_scale,
            ac_scale,
            loop_filter_limits,
            ranges,
        },
        packets,
    )
}

fn initial_control(pp_level: i32) -> Control {
    Control {
        state: State {
            pp_level,
            frame_type: 255,
            ..State::default()
        },
        stripe: [0; 8],
    }
}

fn synchronous_stage(
    ahead: &mut RunAhead<Decoder>,
    packet: &Packet,
    control: &mut Control,
    counters: &mut run_ahead::Stats,
) {
    let stage = ahead.decoder(counters).stage(packet, control);
    ahead.publish(stage);
}

fn compare_frame(
    ahead: &RunAhead<Decoder>,
    sync: &mut Decoder,
    packet: &Packet,
    state: &mut State,
) {
    let result = decode_packet(sync, packet, false, state);
    let stage = ahead.committed();
    assert_eq!(stage.result, result);
    assert_eq!(stage.state, *state);
    assert!(stage.frame.references_match(sync));
    assert_eq!(
        tight_planes(&stage.frame.output().unwrap()).unwrap(),
        tight_planes(&sync.output().unwrap()).unwrap(),
    );
}

// spec: CODEC-CALL-3, DEV-VIDEO-1
#[test]
fn generated_streams_commit_exact_stages_on_shared_workers() {
    let (setup, packets) = generated_stream();
    assert_eq!(packets.len(), 12);
    let start = std::sync::Barrier::new(2);
    std::thread::scope(|scope| {
        for pp_level in [0, 7] {
            let (setup, packets, start) = (&setup, &packets, &start);
            scope.spawn(move || {
                let mut sync = Decoder::new(setup).unwrap();
                let decoder = Decoder::with_run_ahead(setup).unwrap();
                let initial = Stage {
                    result: 0,
                    state: State::default(),
                    frame: decoder.snapshot(),
                };
                let mut ahead = RunAhead::new(decoder, initial);
                let mut control = initial_control(pp_level);
                let mut sync_state = control.state;
                let mut counters = run_ahead::Stats::default();
                start.wait();
                synchronous_stage(&mut ahead, &packets[0], &mut control, &mut counters);
                compare_frame(&ahead, &mut sync, &packets[0], &mut sync_state);
                for batch in packets[1..].chunks(RUN_AHEAD_DEPTH) {
                    ahead.predict(
                        control.clone(),
                        RUN_AHEAD_DEPTH,
                        |_| batch.to_vec(),
                        &mut counters,
                    );
                    for packet in batch {
                        assert!(ahead.try_commit(packet, &control, &mut counters));
                        compare_frame(&ahead, &mut sync, packet, &mut sync_state);
                        control.state = ahead.committed().state;
                    }
                }
                assert_eq!(counters.committed, 11);
                assert_eq!(counters.predicted, counters.committed);
                assert_eq!(counters.mismatched, 0);
            });
        }
    });
}

// spec: CODEC-CALL-3
#[test]
fn substituted_packet_and_changed_controls_restore_committed_references() {
    let (setup, packets) = generated_stream();
    let mut sync = Decoder::new(&setup).unwrap();
    let decoder = Decoder::with_run_ahead(&setup).unwrap();
    let initial = Stage {
        result: 0,
        state: State::default(),
        frame: decoder.snapshot(),
    };
    let mut ahead = RunAhead::new(decoder, initial);
    let mut control = initial_control(7);
    let mut sync_state = control.state;
    let mut stats = run_ahead::Stats::default();
    synchronous_stage(&mut ahead, &packets[0], &mut control, &mut stats);
    compare_frame(&ahead, &mut sync, &packets[0], &mut sync_state);

    ahead.predict(
        control.clone(),
        RUN_AHEAD_DEPTH,
        |_| packets[1..3].to_vec(),
        &mut stats,
    );
    let mut substitute = packets[1].clone();
    substitute.bytes = packets[2].bytes.clone();
    assert!(!ahead.try_commit(&substitute, &control, &mut stats));
    synchronous_stage(&mut ahead, &substitute, &mut control, &mut stats);
    compare_frame(&ahead, &mut sync, &substitute, &mut sync_state);
    assert_eq!(stats.mismatched, 1);
    assert_eq!(stats.discarded, 2);

    ahead.predict(
        control.clone(),
        RUN_AHEAD_DEPTH,
        |_| packets[2..4].to_vec(),
        &mut stats,
    );
    control.state.pp_level = 0;
    sync_state.pp_level = 0;
    assert!(!ahead.try_commit(&packets[2], &control, &mut stats));
    synchronous_stage(&mut ahead, &packets[2], &mut control, &mut stats);
    compare_frame(&ahead, &mut sync, &packets[2], &mut sync_state);
    assert_eq!(stats.mismatched, 2);
    assert_eq!(stats.mismatched_controls, 1);
    assert_eq!(stats.discarded, 4);
}

// spec: CODEC-CALL-3
#[test]
fn callable_control_and_clear_each_discard_pending_packets() {
    let mut machine = fixture(Mode::Native, false);
    let ctor = machine.resolve_import(MODULE, "create").unwrap();
    let predict = machine.resolve_import(MODULE, "predict").unwrap();
    let ctl = machine.resolve_import(MODULE, "ctl_0").unwrap();
    let clear = machine.resolve_import(MODULE, "clear_0").unwrap();
    let done = machine.resolve_import(MODULE, "done").unwrap();
    let mut caller = Asm::new(CODE);
    caller.call(ctor).call(predict);
    caller
        .push_imm(0)
        .push_imm(0)
        .push_imm(1)
        .push_imm(CONTEXT)
        .call(ctl)
        .add_esp(16);
    caller
        .call(predict)
        .mov_ecx_imm(CONTEXT)
        .call(clear)
        .call(done);
    machine.cpu.write(CODE, &caller.bytes).unwrap();
    machine
        .spawn("Theora run-ahead lifetime fixture", CODE, &[], 0x10000)
        .unwrap();
    assert!(matches!(
        machine.run_virtual(1_000_000),
        RunResult::Exited(0)
    ));
    let provider = machine.ext.get::<Provider>();
    assert!(provider.decoders.is_empty());
    let predicted = 2 * RUN_AHEAD_DEPTH as u64;
    assert_eq!(provider.stats.run_ahead.predicted, predicted);
    assert_eq!(provider.stats.run_ahead.discarded, predicted);
    assert_eq!(provider.stats.run_ahead.committed, 0);
}

const SERIAL: u32 = 123456;

fn ogg_page(sequence: u32, flags: u8, granulepos: i64, laces: &[u8], body: &[u8]) -> Vec<u8> {
    use super::super::readahead::crc;

    let mut page = b"OggS\0".to_vec();
    page.push(flags);
    page.extend(granulepos.to_le_bytes());
    page.extend(SERIAL.to_le_bytes());
    page.extend(sequence.to_le_bytes());
    page.extend([0; 4]);
    page.push(laces.len().try_into().unwrap());
    page.extend(laces);
    page.extend(body);
    let checksum = crc(&page);
    page[22..26].copy_from_slice(&checksum.to_le_bytes());
    page
}

fn packet_page(sequence: u32, flags: u8, packet: &Packet, skip: usize) -> Vec<u8> {
    let bytes = &packet.bytes[skip..];
    let mut laces = vec![255; bytes.len() / 255];
    laces.push((bytes.len() % 255) as u8);
    ogg_page(sequence, flags, packet.granulepos, &laces, bytes)
}

fn packet_file(name: &str, bytes: &[u8]) -> std::fs::File {
    use std::io::Write;
    let path = std::env::temp_dir().join(format!("adv-ogg-{name}-{}", std::process::id()));
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    std::fs::remove_file(path).unwrap();
    file.write_all(bytes).unwrap();
    file
}

// spec: CODEC-CALL-3
#[test]
fn file_prediction_checks_crc_and_reassembles_continued_packets() {
    use super::super::readahead::{Packets, Tail};
    use adv_nt::file::ReadSource;
    use std::io::Seek;
    use std::os::unix::fs::FileExt;

    let (_, packets) = generated_stream();
    let mut expected = packets
        .iter()
        .find(|p| p.bytes.len() > 255)
        .unwrap()
        .clone();
    expected.bos = 0x100;
    expected.eos = 0;
    let start = ogg_page(4, 2, -1, &[255], &expected.bytes[..255]);
    let end = packet_page(5, 1, &expected, 255);
    let mut last = packets.last().unwrap().clone();
    last.packetno = expected.packetno + 1;
    last.bos = 0;
    last.eos = 0x200;
    let last_page = packet_page(6, 4, &last, 0);
    let data = [start.as_slice(), &end, &last_page].concat();
    let file = Arc::new(packet_file("pages", &data));
    let tail = Tail {
        source: ReadSource {
            file: Arc::clone(&file),
            offset: 0,
        },
        serial: SERIAL,
        sequence: 4,
        eos: false,
    };
    let mut cursor = Packets::new(tail.clone(), expected.packetno, Vec::new(), 0);
    assert_eq!(cursor.next(), Some(expected.clone()));
    assert_eq!(cursor.next(), Some(last));
    assert!(cursor.next().is_none());
    assert_eq!((&*file).stream_position().unwrap(), data.len() as u64);

    let mut cursor = Packets::new(
        Tail {
            source: ReadSource {
                offset: start.len() as u64,
                ..tail.source.clone()
            },
            sequence: 5,
            ..tail.clone()
        },
        expected.packetno,
        expected.bytes[..255].to_vec(),
        expected.bos,
    );
    assert_eq!(cursor.next(), Some(expected.clone()));

    file.write_all_at(&[end[22] ^ 1], start.len() as u64 + 22)
        .unwrap();
    let mut cursor = Packets::new(tail, expected.packetno, Vec::new(), 0);
    assert!(cursor.next().is_none());
    assert!(cursor.next().is_none());
}

// spec: CODEC-CALL-3
#[test]
fn file_prediction_refills_only_one_slot_per_commit() {
    use super::super::readahead::{Packets, Tail};
    use adv_nt::file::ReadSource;

    let (setup, mut packets) = generated_stream();
    packets.last_mut().unwrap().eos = 0x200;
    let pages: Vec<_> = packets
        .iter()
        .enumerate()
        .flat_map(|(i, packet)| {
            packet_page(
                i as u32,
                if i + 1 == packets.len() { 4 } else { 0 },
                packet,
                0,
            )
        })
        .collect();
    let file = Arc::new(packet_file("rolling", &pages));
    let mut cursor = Packets::new(
        Tail {
            source: ReadSource { file, offset: 0 },
            serial: SERIAL,
            sequence: 0,
            eos: false,
        },
        packets[0].packetno,
        Vec::new(),
        0,
    );
    let mut instance = Instance::new(0, Decoder::with_run_ahead(&setup).unwrap(), None);
    let mut control = initial_control(0);
    let mut sync_state = control.state;
    let mut sync = Decoder::new(&setup).unwrap();
    let mut stats = run_ahead::Stats::default();
    let counts = Arc::new(run_ahead::FileCounts::default());

    assert!(instance.decoder.predict_file(
        control.clone(),
        RUN_AHEAD_DEPTH,
        move || cursor.next().map(|p| (p, true)),
        Arc::clone(&counts),
        &mut stats
    ));
    for packet in &packets {
        assert!(instance.decoder.try_commit(packet, &control, &mut stats));
        compare_frame(&instance.decoder, &mut sync, packet, &mut sync_state);
        control.state = instance.decoder.committed().state;
        assert!(counts.readahead_packets.load(Ordering::Relaxed) <= stats.committed + 2);
        assert!(instance.decoder.refill_file(RUN_AHEAD_DEPTH));
    }
    assert_eq!(stats.committed, packets.len() as u64);
    assert_eq!(
        stats.owner_wait_ns,
        stats.wait_in_flight_ns + stats.wait_not_started_ns
    );
}

// spec: CODEC-CALL-3
#[test]
fn seek_and_file_rewrite_restore_exact_theora_output() {
    use super::super::readahead::Packets;
    use adv_nt::file::ReadSource;
    use std::{os::unix::fs::FileExt, sync::mpsc};

    let (setup, packets) = generated_stream();
    let mut substitute = packets[1].clone();
    substitute.bytes = packets[2].bytes.clone();
    for seek in [true, false] {
        let data = [&[0; 64][..], &packet_page(1, 0, &packets[1], 0)].concat();
        let file = Arc::new(packet_file("rewrite", &data));
        let source_at = |offset| ReadSource {
            file: Arc::clone(&file),
            offset,
        };
        let mut ext = Ext::default();
        ogg::install(&mut ext);
        install(&mut ext, Mode::Native);
        let (source, tail) = ogg::tests::link_fixture(&mut ext, source_at(0), SERIAL, 0);
        let mut cursor = Packets::new(tail.clone(), packets[1].packetno, Vec::new(), 0);
        let provider = ext.get_mut::<Provider>();
        let mut sync = Decoder::new(&setup).unwrap();
        let mut instance = Instance::new(0, Decoder::with_run_ahead(&setup).unwrap(), None);
        instance.source = Some(source);
        let mut control = initial_control(7);
        let mut sync_state = control.state;
        let stats = &mut provider.stats.run_ahead;
        synchronous_stage(&mut instance.decoder, &packets[0], &mut control, stats);
        compare_frame(&instance.decoder, &mut sync, &packets[0], &mut sync_state);

        let (send, read) = mpsc::channel();
        assert!(instance.decoder.predict_file(
            control.clone(),
            RUN_AHEAD_DEPTH,
            move || {
                let packet = cursor.next()?;
                send.send(()).unwrap();
                Some((packet, true))
            },
            Arc::clone(&provider.file_counts),
            stats
        ));
        provider.decoders.insert(0, instance);
        read.recv().unwrap();

        let actual = if seek {
            ogg::tests::link_fixture(&mut ext, source_at(32), SERIAL, 1);
            packets[1].clone()
        } else {
            file.write_all_at(&packet_page(1, 0, &substitute, 0), tail.source.offset)
                .unwrap();
            Packets::new(tail, packets[1].packetno, Vec::new(), 0)
                .next()
                .unwrap()
        };
        let provider = ext.get_mut::<Provider>();
        let instance = provider.decoders.get_mut(&0).unwrap();
        let stats = &mut provider.stats.run_ahead;
        assert_eq!(instance.source.is_none(), seek);
        assert!(!instance.decoder.try_commit(&actual, &control, stats));
        synchronous_stage(&mut instance.decoder, &actual, &mut control, stats);
        compare_frame(&instance.decoder, &mut sync, &actual, &mut sync_state);
        assert_eq!(stats.unlinked_wrote, u64::from(seek));
        assert_eq!(stats.mismatched, u64::from(!seek));
        assert_eq!(stats.committed, 0);
        if !seek {
            assert_eq!(stats.discarded, 1);
            assert_eq!(actual, substitute);
        }
    }
}

// spec: CODEC-CALL-3
#[test]
fn copied_page_links_by_its_header_within_observed_file_reads() {
    use super::super::readahead::Page;
    use adv_nt::file::ReadSource;

    let (_, packets) = generated_stream();
    let page = packet_page(12, 0, &packets[0], 0);
    let marker = Page::new(page[..27].try_into().unwrap(), page.len() as u32).unwrap();
    let offset = 8190;
    let mut bytes = vec![0; offset];
    bytes[..27].copy_from_slice(&page[..27]);
    bytes[22] ^= 1;
    bytes.extend(&page);
    let file = Arc::new(packet_file("copied", &bytes));
    let source = ReadSource { file, offset: 0 };
    assert!(
        marker
            .locate(&[(source.clone(), offset as u32 + 26)])
            .is_none()
    );
    let found = marker.locate(&[(source, bytes.len() as u32)]).unwrap();
    assert_eq!(found.offset, offset as u64);
    let tail = marker.tail(found);
    assert_eq!(tail.source.offset, bytes.len() as u64);
    assert_eq!(tail.sequence, 13);
    assert_eq!(tail.serial, SERIAL);
}

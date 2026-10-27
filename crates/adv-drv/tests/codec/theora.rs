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

use adv_machine::{
    Clock, RunResult,
    layout::{PAGE, PARK_ADDR},
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
    native_init(e, 0, CONTEXT, Decoder::new(&setup).unwrap())
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
    let entry = || Entry {
        entry: CODE + 0x300,
        original: CODE + 0x300,
        thunk: 0,
        trampoline: Vec::new(),
    };
    machine.ext.insert(Provider {
        mode,
        bindings: vec![Binding {
            allocator: SLOT,
            live: true,
            init: entry(),
            packet: entry(),
            clear: entry(),
            snapshots: Vec::new(),
        }],
        decoders: BTreeMap::new(),
        stats: Stats::default(),
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
        assert_eq!(wide(&read(&machine.cpu, GRANULE, 8).unwrap(), 0), 64);
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
    let text = "root init 6\nhook packet 6\nhook clear 6\ncode init 28 7172a948bdc2bc02c6a0002a3cf85feb18b2659e95825d32fdff7047d1455bf3\nlink init 7 rel packet 0\nlink init 12 rel clear 0\nlink init 17 abs malloc 0\nlink init 22 abs table 0\ncode packet 13 07dd12334a3dbd22a6baa2c8312ca2a02f01d11911d4d45a75d75f82f512143c\ncode clear 13 a8f24299d952d9a343f5d23d104f1acab809a2f524b4a907332d0cca0e222009\nslot malloc\ndata table 4 e8a4b2ee7ede79a3afb332b5b6cc3d952a65fd8cffb897f5d18016577c33d7cc\n";
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
                state_read(&m.cpu, CONTEXT).unwrap(),
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

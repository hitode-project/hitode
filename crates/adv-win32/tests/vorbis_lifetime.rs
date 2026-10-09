//! Authored x86 calls with repository-generated tones and PCM reference only.
//! No game, loader, file callback, audio stream, DAC or native DLL is opened.
use adv_abi::Registry;
use adv_cpu::backend::asm::Asm;
use adv_cpu::backend::{Guest, GuestExt, Prot};
use adv_machine::layout::{PAGE, PARK_ADDR};
use adv_machine::{Clock, Conv, Machine, ObjectLimits, Objects, Outcome, RunResult};
use adv_unicorn::UnicornCpu;
use adv_win32::vorbisfile::{self, Policy};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const VF: u32 = DATA + 0x100;
const SECOND: u32 = DATA + 0x500;
const OUT: u32 = DATA + 0x1000;
const CLOSES: u32 = DATA + 0x40;
const ZERO: u32 = DATA + 0x44;
const NEXT_PCM: u32 = DATA + 0x48;
const SOURCE_POSITION: u32 = DATA + 0x80;
const PCM: u32 = 0x420000;
const INPUT: u32 = 0x430000;
const READ_CB: u32 = CODE + 0x8000;
const CLOSE_CB: u32 = CODE + 0x9000;
const ARENA_SIZE: u32 = 131072;
const TONE: &[u8] = include_bytes!("../../adv-runtime/tests/data/tone-mono.ogg");
const REFERENCE: &[u8] = include_bytes!("../../adv-runtime/tests/data/tone-mono.pcm");

struct Test {
    machine: Machine<UnicornCpu>,
    asm: Asm,
    arena: u32,
}

impl Test {
    fn new(limits: ObjectLimits) -> Self {
        let mut registry = Registry::default();
        vorbisfile::register(&mut registry, Policy::Native);
        registry.add("test.dll", "done", Conv::Stdcall { argc: 0 }, |_| {
            Outcome::ExitThread(0)
        });
        let mut machine = Machine::new(
            UnicornCpu::new(PARK_ADDR).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        machine.sys.objects = Objects::with_limits(limits);
        vorbisfile::install(&mut machine, Policy::Native).unwrap();
        for (address, protection) in [
            (CODE, Prot::RX),
            (DATA, Prot::RW),
            (PCM, Prot::RW),
            (INPUT, Prot::RW),
        ] {
            machine.cpu.map(address, PAGE * 16, protection).unwrap();
        }
        machine.cpu.write(INPUT, TONE).unwrap();
        machine.cpu.write_u32(NEXT_PCM, PCM).unwrap();
        machine.spawn("authored codec", CODE, &[], 0x10000).unwrap();
        let arena = machine.sys.vm.reserve(ARENA_SIZE).unwrap();
        machine.sys.vm.free(&mut machine.cpu, arena).unwrap();
        machine.cpu.write(READ_CB, &[0x31, 0xc0, 0xc3]).unwrap();
        let mut close = Asm::new(CLOSE_CB);
        close
            .mov_eax_mem(CLOSES)
            .add_eax_imm(1)
            .mov_mem_eax(CLOSES)
            .mov_eax_mem(ZERO)
            .ret();
        machine.cpu.write(CLOSE_CB, &close.bytes).unwrap();
        Self {
            machine,
            asm: Asm::new(CODE),
            arena,
        }
    }

    fn call(&mut self, name: &str, args: &[u32], save: u32) {
        let thunk = self.machine.resolve_import("vorbisfile.dll", name).unwrap();
        cdecl(&mut self.asm, thunk, args, save);
    }

    fn open(&mut self, vf: u32, initial_len: usize, close: u32, save: u32) {
        self.call(
            "ov_open_callbacks",
            &[
                0x12345678,
                vf,
                INPUT,
                initial_len as u32,
                READ_CB,
                0,
                close,
                0,
            ],
            save,
        );
    }

    fn run(&mut self) {
        let done = self.machine.resolve_import("test.dll", "done").unwrap();
        self.asm.call(done);
        self.machine.cpu.write(CODE, &self.asm.bytes).unwrap();
        let result = self.machine.run_virtual(1_000_000);
        assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    }

    fn word(&self, address: u32) -> u32 {
        self.machine.cpu.read_u32(address).unwrap()
    }

    fn no_arena(&self) {
        assert_eq!(self.machine.sys.vm.region(self.arena), None);
        assert_eq!(self.machine.sys.vm.region(self.arena + ARENA_SIZE), None);
        assert!(!self.machine.cpu.accessible(self.arena, PAGE, Prot::READ));
        assert!(
            !self
                .machine
                .cpu
                .accessible(self.arena + ARENA_SIZE, PAGE, Prot::READ)
        );
        assert_eq!(self.machine.sys.objects.object_count(), 1);
        assert_eq!(vorbisfile::stats(&self.machine).unwrap().active_streams, 0);
    }
}

fn cdecl(asm: &mut Asm, thunk: u32, args: &[u32], save: u32) {
    for &arg in args.iter().rev() {
        asm.push_imm(arg);
    }
    asm.call(thunk)
        .add_esp((args.len() * 4) as u8)
        .mov_mem_eax(save);
}

#[test]
fn object_alias_and_namespace_exhaustion_roll_back_codec_arena_without_guest_close() {
    for limits in [
        ObjectLimits {
            objects: 1,
            ..ObjectLimits::default()
        },
        ObjectLimits {
            aliases: 1,
            ..ObjectLimits::default()
        },
        ObjectLimits {
            namespaces: 1,
            ..ObjectLimits::default()
        },
    ] {
        let mut test = Test::new(limits);
        test.open(VF, TONE.len(), CLOSE_CB, OUT);
        test.call("ov_clear", &[VF], OUT + 4);
        test.run();
        assert_eq!(test.word(OUT) as i32, -129);
        assert_eq!(test.word(OUT + 4), 0);
        assert_eq!(test.word(VF), 0);
        assert_eq!(test.word(CLOSES), 0);
        test.no_arena();
    }
}

#[test]
fn close_returns_capacity_and_same_structure_reopens_without_double_close() {
    let mut test = Test::new(ObjectLimits {
        objects: 2,
        ..ObjectLimits::default()
    });
    test.open(VF, TONE.len(), CLOSE_CB, OUT);
    test.open(VF, TONE.len(), CLOSE_CB, OUT + 24);
    test.open(SECOND, TONE.len(), CLOSE_CB, OUT + 4);
    test.call("ov_clear", &[VF], OUT + 8);
    test.call("ov_clear", &[VF], OUT + 12);
    test.open(VF, TONE.len(), CLOSE_CB, OUT + 16);
    test.call("ov_clear", &[VF], OUT + 20);
    test.run();
    assert_eq!(test.word(OUT + 4) as i32, -129);
    assert_eq!(
        test.word(OUT + 24) as i32,
        -131,
        "a live vf is not replaced"
    );
    for offset in [0, 8, 12, 16, 20] {
        assert_eq!(test.word(OUT + offset), 0);
    }
    assert_eq!(test.word(CLOSES), 2);
    let stats = vorbisfile::stats(&test.machine).unwrap();
    assert_eq!(
        (stats.opens, stats.clears, stats.close_callbacks),
        (2, 2, 2)
    );
    test.no_arena();
}

#[test]
fn failed_open_does_not_take_datasource_ownership() {
    let mut test = Test::new(ObjectLimits::default());
    test.open(VF, 0, CLOSE_CB, OUT);
    test.call("ov_clear", &[VF], OUT + 4);
    test.run();
    assert_eq!(test.word(OUT) as i32, -133);
    assert_eq!(test.word(OUT + 4), 0);
    assert_eq!(test.word(CLOSES), 0);
    let stats = vorbisfile::stats(&test.machine).unwrap();
    assert_eq!(
        (stats.opens, stats.failed_opens, stats.close_callbacks),
        (1, 1, 0)
    );
    test.no_arena();
}

#[test]
fn partial_open_read_rejection_and_test_open_share_one_owned_resource_until_clear() {
    let mut test = Test::new(ObjectLimits {
        objects: 2,
        ..ObjectLimits::default()
    });
    test.call(
        "ov_test_callbacks",
        &[
            0x12345678,
            VF,
            INPUT,
            TONE.len() as u32,
            READ_CB,
            0,
            CLOSE_CB,
            0,
        ],
        OUT,
    );
    test.call("ov_read", &[VF, PCM, 512, 0, 2, 1, OUT + 80], OUT + 4);
    test.call("ov_test_open", &[VF], OUT + 8);
    test.call("ov_info", &[VF, u32::MAX], OUT + 12);
    test.call("ov_clear", &[VF], OUT + 16);
    test.run();
    assert_eq!(test.word(OUT), 0);
    assert_eq!(test.word(OUT + 4) as i32, -131);
    assert_eq!(test.word(OUT + 8), 0);
    assert_ne!(test.word(OUT + 12), 0);
    assert_eq!(test.word(OUT + 16), 0);
    assert_eq!(test.word(CLOSES), 1);
    let stats = vorbisfile::stats(&test.machine).unwrap();
    assert_eq!((stats.opens, stats.clears), (1, 1));
    test.no_arena();
}

#[test]
fn arena_exhaustion_does_not_publish_an_object_or_take_datasource_ownership() {
    let mut test = Test::new(ObjectLimits::default());
    let size = adv_machine::layout::DYN_END - test.arena;
    assert!(test.machine.sys.vm.reserve_at(test.arena, size));
    test.open(VF, TONE.len(), CLOSE_CB, OUT);
    test.run();
    assert_eq!(test.word(OUT) as i32, -129);
    assert_eq!(test.word(CLOSES), 0);
    assert_eq!(test.machine.sys.objects.object_count(), 1);
    assert_eq!(test.machine.sys.vm.region(test.arena), Some(size));
    assert!(!test.machine.cpu.accessible(test.arena, PAGE, Prot::READ));
    let stats = vorbisfile::stats(&test.machine).unwrap();
    assert_eq!(
        (stats.opens, stats.active_streams, stats.close_callbacks),
        (0, 0, 0)
    );
}

#[test]
fn clear_inside_an_active_read_callback_is_rejected_without_losing_the_open_resource() {
    let mut test = Test::new(ObjectLimits::default());
    let clear = test
        .machine
        .resolve_import("vorbisfile.dll", "ov_clear")
        .unwrap();
    let mut callback = Asm::new(READ_CB);
    cdecl(&mut callback, clear, &[VF], OUT + 40);
    callback.mov_eax_mem(ZERO).ret();
    test.machine.cpu.write(READ_CB, &callback.bytes).unwrap();
    test.open(VF, 0, CLOSE_CB, OUT);
    test.run();
    assert_eq!(test.word(OUT + 40) as i32, -131);
    assert_eq!(test.word(OUT) as i32, -133);
    assert_eq!(test.word(CLOSES), 0);
    test.no_arena();
}

#[test]
fn metadata_seek_scan_restores_the_datasource_and_pcm_seek_preserves_identity() {
    let mut test = Test::new(ObjectLimits::default());
    let registry = test.machine.sys.thunks.resolver_mut::<Registry>().unwrap();
    registry.add("test.dll", "read", Conv::Cdecl { argc: 4 }, |env| {
        let (out, size, count) = (env.arg(0), env.arg(1), env.arg(2));
        assert_eq!(env.arg(3), 0x12345678);
        let position = env.cpu.read_u32(SOURCE_POSITION).unwrap() as usize;
        let bytes = (size as usize * count as usize).min(TONE.len() - position);
        env.cpu
            .write(out, &TONE[position..position + bytes])
            .unwrap();
        env.cpu
            .write_u32(SOURCE_POSITION, (position + bytes) as u32)
            .unwrap();
        Outcome::Return(bytes as u32 / size)
    });
    registry.add("test.dll", "seek", Conv::Cdecl { argc: 4 }, |env| {
        assert_eq!(env.arg(0), 0x12345678);
        let offset = (u64::from(env.arg(1)) | (u64::from(env.arg(2)) << 32)) as i64;
        let base = match env.arg(3) {
            0 => 0,
            1 => i64::from(env.cpu.read_u32(SOURCE_POSITION).unwrap()),
            2 => TONE.len() as i64,
            whence => panic!("invalid authored seek whence {whence}"),
        };
        let position = base + offset;
        if position < 0 || position > TONE.len() as i64 {
            return Outcome::Return(u32::MAX);
        }
        env.cpu.write_u32(SOURCE_POSITION, position as u32).unwrap();
        Outcome::Return(0)
    });
    registry.add("test.dll", "tell", Conv::Cdecl { argc: 1 }, |env| {
        assert_eq!(env.arg(0), 0x12345678);
        Outcome::Return(env.cpu.read_u32(SOURCE_POSITION).unwrap())
    });
    let read = test.machine.resolve_import("test.dll", "read").unwrap();
    let seek = test.machine.resolve_import("test.dll", "seek").unwrap();
    let tell = test.machine.resolve_import("test.dll", "tell").unwrap();
    test.call(
        "ov_open_callbacks",
        &[0x12345678, VF, 0, 0, read, seek, CLOSE_CB, tell],
        OUT,
    );
    test.asm.mov_eax_mem(SOURCE_POSITION).mov_mem_eax(OUT + 40);
    test.call("ov_raw_total", &[VF, u32::MAX], OUT + 4);
    test.asm.mov_eax_mem(SOURCE_POSITION).mov_mem_eax(OUT + 44);
    test.call("ov_pcm_total", &[VF, u32::MAX], OUT + 8);
    test.call("ov_pcm_seek", &[VF, 2205, 0], OUT + 12);
    test.call("ov_pcm_tell", &[VF], OUT + 16);
    test.call("ov_read", &[VF, PCM, 512, 0, 2, 1, OUT + 80], OUT + 20);
    test.call("ov_clear", &[VF], OUT + 24);
    test.run();
    assert_eq!(test.word(OUT), 0);
    assert_eq!(test.word(OUT + 4), TONE.len() as u32);
    assert_eq!(test.word(OUT + 8), 11025);
    assert_eq!(test.word(OUT + 12), 0);
    assert_eq!(test.word(OUT + 16), 2205);
    assert_eq!(test.word(OUT + 20), 512);
    assert_eq!(
        test.word(OUT + 40),
        test.word(OUT + 44),
        "metadata scan moved datasource"
    );
    let stats = vorbisfile::stats(&test.machine).unwrap();
    assert_eq!(stats.metadata_scans, 1);
    assert_eq!(test.word(CLOSES), 1);
    test.no_arena();
}

#[test]
fn clear_detaches_old_identity_and_arena_before_reentrant_close_opens_the_same_vf() {
    let mut test = Test::new(ObjectLimits {
        objects: 2,
        ..ObjectLimits::default()
    });
    let mut callback = Asm::new(CLOSE_CB);
    let open = test
        .machine
        .resolve_import("vorbisfile.dll", "ov_open_callbacks")
        .unwrap();
    cdecl(
        &mut callback,
        open,
        &[0x12345678, VF, INPUT, TONE.len() as u32, READ_CB, 0, 0, 0],
        OUT + 40,
    );
    callback.mov_eax_mem(ZERO).ret();
    test.machine.cpu.write(CLOSE_CB, &callback.bytes).unwrap();
    test.open(VF, TONE.len(), CLOSE_CB, OUT);
    test.call("ov_clear", &[VF], OUT + 4);
    test.call("ov_info", &[VF, u32::MAX], OUT + 8);
    test.call("ov_clear", &[VF], OUT + 12);
    test.run();
    for offset in [0, 4, 12, 40] {
        assert_eq!(test.word(OUT + offset), 0);
    }
    assert_ne!(
        test.word(OUT + 8),
        0,
        "callback's reopened stream stays live"
    );
    let stats = vorbisfile::stats(&test.machine).unwrap();
    assert_eq!(
        (stats.opens, stats.clears, stats.close_callbacks),
        (2, 2, 1)
    );
    test.no_arena();
}

#[test]
fn moved_decoder_keeps_pcm_position_and_matches_the_authored_libvorbis_reference() {
    let mut test = Test::new(ObjectLimits::default());
    test.open(VF, TONE.len(), CLOSE_CB, OUT);
    let read = test
        .machine
        .resolve_import("vorbisfile.dll", "ov_read")
        .unwrap();
    for _ in 0..64 {
        for arg in [OUT + 80, 1, 2, 0, 4096] {
            test.asm.push_imm(arg);
        }
        test.asm.mov_eax_mem(NEXT_PCM).push_eax().push_imm(VF);
        test.asm.call(read).add_esp(28);
        // add eax,[NEXT_PCM]: move output only by bytes actually returned.
        test.asm.bytes.extend_from_slice(&[0x03, 0x05]);
        test.asm.bytes.extend_from_slice(&NEXT_PCM.to_le_bytes());
        test.asm.mov_mem_eax(NEXT_PCM);
    }
    test.call("ov_pcm_tell", &[VF], OUT + 4);
    test.call("ov_clear", &[VF], OUT + 8);
    test.run();
    assert_eq!(test.word(OUT), 0);
    // The historical FFmpeg reference is 128 frames shorter than EOS granule.
    // Check the complete 0.25-s EOS output, and compare its shared reference span.
    assert_eq!(test.word(NEXT_PCM) - PCM, 11025 * 2);
    assert_eq!(test.word(OUT + 4), 11025);
    let mut actual = vec![0; REFERENCE.len()];
    test.machine.cpu.read(PCM, &mut actual).unwrap();
    let mut energy = 0.0;
    let mut error = 0.0;
    for (reference, actual) in REFERENCE
        .as_chunks::<2>()
        .0
        .iter()
        .zip(actual.as_chunks::<2>().0)
    {
        let reference = f64::from(i16::from_le_bytes(*reference));
        let actual = f64::from(i16::from_le_bytes(*actual));
        energy += reference * reference;
        error += (reference - actual).powi(2);
    }
    let snr = 10.0 * (energy / error.max(1e-30)).log10();
    assert!(snr > 60.0, "authored reference PCM SNR {snr} dB");
    eprintln!("authored mono EOS: 11025 frames, reference span SNR {snr:.2} dB");
    assert_eq!(test.word(CLOSES), 1);
    test.no_arena();
}

#[test]
fn failed_pcm_publication_can_retry_through_named_and_ordinal_read_entries() {
    let mut test = Test::new(ObjectLimits::default());
    test.open(VF, TONE.len(), CLOSE_CB, OUT);
    test.call("ov_read", &[VF, 0x600000, 512, 0, 2, 1, OUT + 80], OUT + 4);
    test.call("ov_pcm_tell", &[VF], OUT + 8);
    test.call("ov_read", &[VF, PCM, 513, 0, 2, 1, OUT + 80], OUT + 12);
    test.call("ov_pcm_tell", &[VF], OUT + 16);
    test.call(
        "ov_read",
        &[VF, PCM + 1024, 512, 0, 2, 1, 0x600000],
        OUT + 20,
    );
    test.call("ov_pcm_tell", &[VF], OUT + 24);
    test.call("#15", &[VF, PCM + 512, 512, 0, 2, 1, OUT + 80], OUT + 28);
    test.call("ov_pcm_tell", &[VF], OUT + 32);
    test.call("ov_clear", &[VF], OUT + 36);
    test.run();
    for offset in [4, 20] {
        assert_eq!(test.word(OUT + offset) as i32, -129);
    }
    assert_eq!(test.word(OUT + 8), 0);
    assert_eq!(test.word(OUT + 12), 512);
    assert_eq!(test.word(OUT + 16), 256);
    assert_eq!(test.word(OUT + 24), 256);
    assert_eq!(test.word(OUT + 28), 512);
    assert_eq!(test.word(OUT + 32), 512);
    let mut failed_section = [0; 512];
    let mut retry = [0; 512];
    test.machine
        .cpu
        .read(PCM + 1024, &mut failed_section)
        .unwrap();
    test.machine.cpu.read(PCM + 512, &mut retry).unwrap();
    assert_eq!(failed_section, retry);
    assert_eq!(test.word(OUT + 36), 0);
    assert_eq!(test.word(CLOSES), 1);
    test.no_arena();
}

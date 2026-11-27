use super::*;
use iced_x86::code_asm::*;

// spec: DEV-AUDIO-7, DEV-AUDIO-10
#[test]
fn software_buffers_can_exceed_the_old_mixer_source_limit() {
    let mut m = machine();
    let device = device(&mut m);
    let baseline = m.sys.objects.object_count();
    m.cpu.write(DATA + 64, &Format::default().bytes()).unwrap();
    for (offset, value) in [(0, 36), (4, 0x8), (8, 2048), (16, DATA + 64)] {
        m.cpu.write_u32(DATA + offset, value).unwrap();
    }
    let buffers: Vec<_> = (0..1024)
        .map(|index| {
            assert_eq!(
                invoke(
                    &mut m,
                    "IDirectSound::CreateSoundBuffer",
                    &[device, DATA, DATA + 128, 0]
                ),
                0,
                "software buffer {index}"
            );
            m.cpu.read_u32(DATA + 128).unwrap()
        })
        .collect();
    let last = *buffers.last().unwrap();
    let sound = m.ext.get::<Sound>();
    let storage = sound.buffer(&m.sys.objects, last).unwrap().storage_addr();
    m.cpu.write(storage, &[0, 64, 0, 64].repeat(16)).unwrap();
    assert_eq!(
        invoke(&mut m, "IDirectSoundBuffer::Play", &[last, 0, 0, 1]),
        0
    );
    let mut output = [0.0; 32];
    m.ext
        .get::<Sound>()
        .audio
        .stream()
        .read_stereo(&mut output, 44100);
    assert_eq!(output, [0.5; 32]);
    for pointer in buffers {
        assert_eq!(invoke(&mut m, "IDirectSoundBuffer::Release", &[pointer]), 0);
    }
    pump(&mut m).unwrap();
    assert_eq!(m.sys.objects.object_count(), baseline);
    assert!(m.ext.get::<Sound>().storage_records.is_empty());
}

// spec: DEV-AUDIO-1, DEV-AUDIO-2
#[test]
fn retuned_duplicate_waits_for_space_and_needs_only_one_command() {
    let mut m = machine();
    let device = device(&mut m);
    let baseline = m.sys.objects.object_count();
    m.cpu.write(DATA + 64, &Format::default().bytes()).unwrap();
    for (offset, value) in [(0, 36), (4, CTRL_FREQUENCY), (8, 4096), (16, DATA + 64)] {
        m.cpu.write_u32(DATA + offset, value).unwrap();
    }
    let create_args = [device, DATA, DATA + 128, 0];
    assert_eq!(
        invoke(&mut m, "IDirectSound::CreateSoundBuffer", &create_args),
        0
    );
    let original = m.cpu.read_u32(DATA + 128).unwrap();
    assert_eq!(
        invoke(
            &mut m,
            "IDirectSoundBuffer::SetFrequency",
            &[original, 22050]
        ),
        0
    );
    let (audio, source, alias) = {
        let s = m.ext.get::<Sound>();
        let buf = s.buffer(&m.sys.objects, original).unwrap();
        (
            s.audio.clone(),
            buf.audio_id,
            Arc::downgrade(&buf.storage.as_ref().unwrap().read_alias),
        )
    };
    let mut reader = audio.stream().device_reader(44100, 2).unwrap();
    let capacity = (0..).take_while(|_| audio.set_gain(source, 1.0)).count();
    let duplicate = "IDirectSound::DuplicateSoundBuffer";
    let args = [device, original, DATA + 132];
    for (method, args) in [
        ("IDirectSound::CreateSoundBuffer", create_args.as_slice()),
        (duplicate, args.as_slice()),
    ] {
        assert!(matches!(
            with_call(&mut m, method, args, call),
            Outcome::Block { .. }
        ));
        assert_eq!(m.sys.objects.object_count(), baseline + 1);
    }
    // Return the reader's command lane and the producer's backlog to empty.
    while audio.active() {
        reader.read(&mut [], 0);
        audio.maintain();
    }
    for _ in 1..capacity {
        assert!(audio.set_gain(source, 1.0));
    }
    assert!(matches!(
        with_call(&mut m, duplicate, &args, call),
        Outcome::Return(0)
    ));
    let copy = m.cpu.read_u32(DATA + 132).unwrap();
    assert_eq!(
        invoke(
            &mut m,
            "IDirectSoundBuffer::GetFrequency",
            &[copy, DATA + 136]
        ),
        0
    );
    assert_eq!(m.cpu.read_u32(DATA + 136).unwrap(), 22050);
    for buffer in [original, copy] {
        assert_eq!(invoke(&mut m, "IDirectSoundBuffer::Release", &[buffer]), 0);
    }
    while audio.active() || !m.ext.get::<Sound>().pending_release.is_empty() {
        reader.read(&mut [], 0);
        pump(&mut m).unwrap();
    }
    assert_eq!(m.sys.objects.object_count(), baseline);
    assert!(m.ext.get::<Sound>().storage_records.is_empty());
    assert!(alias.upgrade().is_none());
}

// spec: DEV-AUDIO-1, DEV-AUDIO-2
#[test]
fn silent_device_restart_only_plays_new_buffer_samples() {
    let audio = AudioControl::new(true);
    let mut m = machine_for_process(
        adv_machine::ObjectLimits::default(),
        0x1000,
        Some(audio.clone()),
    );
    let stream = audio.stream();
    let format = Format {
        rate: 1000,
        channels: 1,
        bits: 8,
        ..Format::default()
    };
    let mut samples = [0.0; 20];
    for (value, output) in [(192, 0.5), (64, -0.5)] {
        let voice = buffer(&mut m, format, 1000);
        let addr = m
            .ext
            .get::<Sound>()
            .buffer(&m.sys.objects, voice)
            .unwrap()
            .storage_addr();
        m.cpu.write(addr, &[value; 1000]).unwrap();
        assert_eq!(
            invoke(&mut m, "IDirectSoundBuffer8::Play", &[voice, 0, 0, 1]),
            0
        );
        let mut reader = stream.device_reader(1000, 2).unwrap();
        reader.read(&mut samples, 0);
        assert_eq!(samples, [output; 20]);
        assert_eq!(invoke(&mut m, "IDirectSoundBuffer8::Release", &[voice]), 0);
        reader.read(&mut samples, 0);
        assert_eq!(samples, [0.0; 20]);
        pump(&mut m).unwrap();
        assert!(!stream.active());
    }
}

// spec: DEV-AUDIO-1, DEV-AUDIO-2, DEV-AUDIO-10, PERS-SCOPE-13
#[test]
fn guest_sound_buffer_churn_reclaims_mappings_and_objects() {
    const BATCH: usize = 32;
    const BATCHES: usize = 6250;
    const CODE: u32 = 0x400000;
    const FORMAT: u32 = DATA + 256;
    const ORIGINAL: u32 = DATA + 512;
    const COPIES: u32 = DATA + 516;

    let audio = AudioControl::new(true);
    let mut m = machine_for_process(
        adv_machine::ObjectLimits::default(),
        0x1000,
        Some(audio.clone()),
    );
    let device = device(&mut m);
    let format = Format::default();
    m.cpu.write(FORMAT, &format.bytes()).unwrap();
    for (index, (len, flags)) in [
        (2048, 0x4000 | CTRL_VOLUME),
        (12 * 1024, 0x8000 | CTRL_PAN),
        (80 * 1024, 0x2 | 0x8 | CTRL_FREQUENCY),
        (512 * 1024, LOC_DEFER | 0x100 | CTRL_VOLUME),
    ]
    .into_iter()
    .enumerate()
    {
        let desc = DATA + 64 * index as u32;
        m.cpu.write_u32(desc, 36).unwrap();
        m.cpu.write_u32(desc + 4, flags).unwrap();
        m.cpu.write_u32(desc + 8, len).unwrap();
        m.cpu.write_u32(desc + 16, FORMAT).unwrap();
    }
    let sleep = m.resolve_import("kernel32.dll", "Sleep").unwrap();
    let mut a = CodeAssembler::new(32).unwrap();
    let mut batch = a.create_label();
    let mut create = a.create_label();
    let mut release_copies = a.create_label();
    let mut done = a.create_label();
    a.mov(ebx, device).unwrap();
    a.mov(esi, BATCHES as u32).unwrap();
    a.set_label(&mut batch).unwrap();
    a.xor(edi, edi).unwrap();
    a.set_label(&mut create).unwrap();
    // The low two index bits select one of the four guest descriptions.
    a.mov(eax, edi).unwrap();
    a.and(eax, 3).unwrap();
    a.shl(eax, 6).unwrap();
    a.add(eax, DATA).unwrap();
    a.push(0).unwrap();
    a.push(ORIGINAL).unwrap();
    a.push(eax).unwrap();
    com_call(&mut a, ebx, 3);
    check_result(&mut a, done);
    a.mov(edx, dword_ptr(ORIGINAL as i32)).unwrap();
    play(&mut a, edx);
    check_result(&mut a, done);
    a.lea(eax, ptr(edi * 4 + COPIES as i32)).unwrap();
    a.push(eax).unwrap();
    a.push(dword_ptr(ORIGINAL as i32)).unwrap();
    com_call(&mut a, ebx, 5);
    check_result(&mut a, done);
    a.mov(edx, dword_ptr(ORIGINAL as i32)).unwrap();
    com_call(&mut a, edx, 2);
    check_result(&mut a, done);
    a.mov(edx, dword_ptr(edi * 4 + COPIES as i32)).unwrap();
    play(&mut a, edx);
    check_result(&mut a, done);
    a.inc(edi).unwrap();
    a.cmp(edi, BATCH as u32).unwrap();
    a.jb(create).unwrap();
    a.push(1).unwrap();
    a.call(u64::from(sleep)).unwrap();

    a.set_label(&mut release_copies).unwrap();
    a.dec(edi).unwrap();
    a.mov(edx, dword_ptr(edi * 4 + COPIES as i32)).unwrap();
    com_call(&mut a, edx, 2);
    check_result(&mut a, done);
    a.test(edi, edi).unwrap();
    a.jnz(release_copies).unwrap();
    a.push(1).unwrap();
    a.call(u64::from(sleep)).unwrap();
    a.dec(esi).unwrap();
    a.jnz(batch).unwrap();
    a.xor(eax, eax).unwrap();
    a.set_label(&mut done).unwrap();
    a.ret().unwrap();
    m.cpu.map(CODE, 4096, Prot::RX).unwrap();
    m.cpu
        .write(CODE, &a.assemble(u64::from(CODE)).unwrap())
        .unwrap();
    m.spawn("DirectSound churn", CODE, &[], 65536).unwrap();

    let namespace = m.ext.get::<Sound>().com.namespace();
    let baseline = (
        m.sys.objects.object_count(),
        m.sys.objects.alias_count(namespace),
    );
    let mut reader = audio.stream().device_reader(format.rate, 2).unwrap();
    let mut samples = [0.0; 88];
    for batch in 0..BATCHES {
        let next = run_to_sleep(&mut m, batch, "play");
        reader.read(&mut samples, 0);
        pump(&mut m).unwrap();
        let sound = m.ext.get::<Sound>();
        assert_eq!(sound.buffer_ids.len(), BATCH);
        assert_eq!(sound.storage_records.len(), BATCH);
        let views: Vec<_> = sound
            .buffers(&m.sys.objects)
            .map(|buffer| {
                let storage = buffer.storage.as_ref().unwrap();
                assert!(m.cpu.read_u8(storage.addr).is_ok());
                (storage.addr, Arc::downgrade(&storage.read_alias))
            })
            .collect();

        m.sys.clock.advance_to(next);
        let next = run_to_sleep(&mut m, batch, "release");
        reader.read(&mut samples, 0);
        pump(&mut m).unwrap();
        let sound = m.ext.get::<Sound>();
        assert!(sound.buffer_ids.is_empty());
        assert!(sound.storage_records.is_empty());
        assert!(sound.pending_release.is_empty());
        assert_eq!(
            (
                m.sys.objects.object_count(),
                m.sys.objects.alias_count(namespace)
            ),
            baseline
        );
        for (addr, alias) in views {
            assert_eq!(m.sys.vm.region(addr), None);
            assert!(m.cpu.read_u8(addr).is_err());
            assert!(alias.upgrade().is_none());
        }
        m.sys.clock.advance_to(next);
    }
    assert!(matches!(m.run(), adv_machine::RunResult::Exited(0)));
    assert_eq!(
        audio.stream().metrics().removals,
        (2 * BATCH * BATCHES) as u64
    );
}

fn com_call(a: &mut CodeAssembler, this: AsmRegister32, slot: i32) {
    a.push(this).unwrap();
    a.mov(ecx, dword_ptr(this)).unwrap();
    a.call(dword_ptr(ecx + slot * 4)).unwrap();
}

fn play(a: &mut CodeAssembler, this: AsmRegister32) {
    a.push(1).unwrap();
    a.push(0).unwrap();
    a.push(0).unwrap();
    com_call(a, this, 12);
}

fn check_result(a: &mut CodeAssembler, done: CodeLabel) {
    a.test(eax, eax).unwrap();
    a.jnz(done).unwrap();
}

fn run_to_sleep(m: &mut Machine<JitCpu>, batch: usize, phase: &str) -> u64 {
    match m.run() {
        adv_machine::RunResult::Idle {
            next_deadline: Some(next),
        } => next,
        result => panic!("{phase} batch {batch}: {result:?}"),
    }
}

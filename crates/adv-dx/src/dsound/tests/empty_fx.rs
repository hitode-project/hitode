//! Native-oracle empty FX-chain and actual PCM-owner regression.
use super::*;
use adv_cpu::backend::asm::Asm;
use adv_machine::RunResult;

const FLAGS: u32 = 0x182c0;
const BYTES: u32 = 26460;
const ALL_OBJECTS: [u8; 16] = [
    0xe5, 0x4d, 0x11, 0xaa, 0x62, 0xc2, 0x69, 0x41, 0xa1, 0xc8, 0x23, 0xd6, 0x98, 0xcc, 0x73, 0xb5,
];
const I3DL2: [u8; 16] = [
    0x71, 0x5e, 0x98, 0xef, 0xc7, 0xd5, 0xd4, 0x42, 0xba, 0x4d, 0x2d, 0x07, 0x3e, 0x2e, 0x96, 0xf4,
];
const REVERB: [u8; 16] = [
    0x6a, 0x6a, 0x16, 0x4b, 0x66, 0x0d, 0xf3, 0x43, 0x80, 0xe3, 0xee, 0x62, 0x80, 0xde, 0xe1, 0xa4,
];

fn descriptor(m: &mut Machine<JitCpu>, flags: u32, bytes: u32, format: Format) {
    m.cpu.write(DATA, &[0; 36]).unwrap();
    m.cpu.write(DATA + 64, &format.bytes()).unwrap();
    for (index, value) in [36, flags, bytes, 0, DATA + 64].into_iter().enumerate() {
        m.cpu.write_u32(DATA + index as u32 * 4, value).unwrap();
    }
}

enum Arg {
    Value(u32),
    Memory(u32),
}
use Arg::{Memory as M, Value as I};

fn guest_call(
    m: &Machine<JitCpu>,
    code: &mut Asm,
    pointer: u32,
    slot: u32,
    args: &[Arg],
    output: u32,
) {
    let table = m.cpu.read_u32(pointer).unwrap();
    let entry = m.cpu.read_u32(table + slot * 4).unwrap();
    for argument in args.iter().rev() {
        match *argument {
            I(value) => {
                code.push_imm(value);
            }
            M(address) => {
                code.mov_eax_mem(address).push_eax();
            }
        }
    }
    code.push_imm(pointer).call(entry).mov_mem_eax(output);
}

#[test]
fn empty_fx_chain_uses_real_pcm_guest_vtable_and_keeps_failed_dsp_unpublished() {
    let mut m = machine();
    let initial = m.sys.objects.object_count();
    let format = Format::default();
    let legacy = device(&mut m);
    descriptor(&mut m, FLAGS, BYTES, format);
    m.cpu.write_u32(DATA + 128, 0x12345678).unwrap();
    assert_eq!(
        invoke(
            &mut m,
            "IDirectSound8::CreateSoundBuffer",
            &[legacy, DATA, DATA + 128, 0]
        ),
        0x887800be
    );
    assert_eq!(m.cpu.read_u32(DATA + 128).unwrap(), 0x12345678);
    m.cpu.write(DATA + 144, &IID_DEVICE8).unwrap();
    assert_eq!(
        invoke(
            &mut m,
            "IDirectSound8::QueryInterface",
            &[legacy, DATA + 144, DATA + 160]
        ),
        NO_INTERFACE
    );
    assert_eq!(m.cpu.read_u32(DATA + 160).unwrap(), 0);
    assert_eq!(invoke(&mut m, "IDirectSound8::Release", &[legacy]), 0);
    assert_eq!(invoke(&mut m, "DirectSoundCreate8", &[0, DATA + 128, 0]), 0);
    let device = m.cpu.read_u32(DATA + 128).unwrap();
    let objects = m.sys.objects.object_count();
    descriptor(&mut m, FLAGS, BYTES - 4, format);
    m.cpu.write_u32(DATA + 128, 0x12345678).unwrap();
    assert_eq!(
        invoke(
            &mut m,
            "IDirectSound8::CreateSoundBuffer",
            &[device, DATA, DATA + 128, 0]
        ),
        0x887800b4
    );
    assert_eq!(m.cpu.read_u32(DATA + 128).unwrap(), 0x12345678);
    assert_eq!(m.sys.objects.object_count(), objects);
    descriptor(&mut m, FLAGS | 1, 0, format);
    assert_eq!(
        invoke(
            &mut m,
            "IDirectSound8::CreateSoundBuffer",
            &[device, DATA, DATA + 128, 0]
        ),
        INVALID
    );
    assert_eq!(m.sys.objects.object_count(), objects);
    descriptor(&mut m, FLAGS, BYTES, Format { bits: 8, ..format });
    assert_eq!(
        invoke(
            &mut m,
            "IDirectSound8::CreateSoundBuffer",
            &[device, DATA, DATA + 128, 0]
        ),
        UNSUPPORTED
    );
    assert_eq!(m.sys.objects.object_count(), objects);
    descriptor(&mut m, FLAGS, BYTES, format);
    m.cpu.write_u32(DATA + 72, 1).unwrap(); // Incorrect average bytes/sec.
    assert_eq!(
        invoke(
            &mut m,
            "IDirectSound8::CreateSoundBuffer",
            &[device, DATA, DATA + 128, 0]
        ),
        0x88780064
    );
    descriptor(&mut m, FLAGS, BYTES, format);
    assert_eq!(
        invoke(
            &mut m,
            "IDirectSound8::CreateSoundBuffer",
            &[device, DATA, DATA + 128, 0]
        ),
        0
    );
    let buffer = m.cpu.read_u32(DATA + 128).unwrap();
    let storage = m
        .ext
        .get::<Sound>()
        .buffer(&m.sys.objects, buffer)
        .unwrap()
        .storage_addr();
    assert_ne!(storage, 0);
    m.cpu.write_u32(DATA + 132, 0x12345678).unwrap();
    assert_eq!(
        invoke(
            &mut m,
            "IDirectSound8::DuplicateSoundBuffer",
            &[device, buffer, DATA + 132]
        ),
        INVALID_CALL
    );
    assert_eq!(m.cpu.read_u32(DATA + 132).unwrap(), 0);
    assert_eq!(m.sys.objects.object_count(), objects + 1);

    m.cpu.write(DATA + 160, &ALL_OBJECTS).unwrap();
    m.cpu.write(DATA + 176, &REVERB).unwrap();
    m.cpu.write_u32(DATA + 192, 0x12345678).unwrap();
    m.cpu.write_u32(DATA + 256, 20).unwrap();
    m.cpu.write(DATA + 320, &[0; 32]).unwrap();
    m.cpu.write_u32(DATA + 320, 32).unwrap();
    m.cpu.write(DATA + 328, &I3DL2).unwrap();
    m.cpu.write_u32(DATA + 368, 0xaaaaaaaa).unwrap();
    let code_address = 0x800000;
    m.cpu.map(code_address, 4096, Prot::RX).unwrap();
    let mut code = Asm::new(code_address);
    code.bytes.extend_from_slice(&[0x89, 0xe0]); // mov eax,esp
    code.mov_mem_eax(DATA + 896);
    let mut output = DATA + 512;
    let mut expected = Vec::new();
    let mut call = |code: &mut Asm, slot, args: &[Arg], value| {
        guest_call(&m, code, buffer, slot, args, output);
        expected.push((output, value));
        output += 4;
    };
    call(&mut code, 3, &[I(DATA + 256)], 0); // GetCaps
    call(&mut code, 21, &[I(0), I(0), I(0)], 0);
    call(
        &mut code,
        23,
        &[I(DATA + 160), I(0), I(DATA + 176), I(DATA + 192)],
        OBJECT_NOT_FOUND,
    );
    call(
        &mut code,
        11,
        &[
            I(0),
            I(0),
            I(DATA + 200),
            I(DATA + 204),
            I(DATA + 208),
            I(DATA + 212),
            I(2),
        ],
        0,
    );
    code.mov_eax_mem(DATA + 200)
        .bytes
        .extend_from_slice(&[0xc7, 0x00]);
    code.bytes.extend_from_slice(&0x40004000u32.to_le_bytes()); // Real guest writes stereo PCM +0.5.
    call(&mut code, 21, &[I(0), I(0), I(0)], INVALID_CALL);
    call(
        &mut code,
        19,
        &[M(DATA + 200), M(DATA + 204), M(DATA + 208), M(DATA + 212)],
        0,
    );
    call(
        &mut code,
        21,
        &[I(1), I(DATA + 320), I(DATA + 368)],
        CLASS_NOT_REGISTERED,
    );
    call(
        &mut code,
        23,
        &[I(DATA + 160), I(0), I(DATA + 176), I(DATA + 192)],
        OBJECT_NOT_FOUND,
    );
    call(&mut code, 12, &[I(0), I(0), I(1)], 0);
    call(&mut code, 21, &[I(0), I(0), I(0)], INVALID_CALL);
    call(&mut code, 18, &[], 0);
    call(&mut code, 21, &[I(0), I(0), I(0)], 0);
    call(&mut code, 12, &[I(0), I(0), I(1)], 0);
    code.bytes.extend_from_slice(&[0x89, 0xe0]);
    code.mov_mem_eax(DATA + 900);
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    m.cpu.write(code_address, &code.bytes).unwrap();
    let context = m
        .spawn("actual empty-FX PCM guest", code_address, &[], 65536)
        .unwrap();
    let thread = m.sys.contexts[context as usize].thread_handle;
    assert!(matches!(
        m.sys.objects.get(thread),
        Some(WaitObject::Thread { ctx }) if *ctx == context
    ));
    // Sys::spawn publishes the fixture's persistent kernel-thread owner before
    // execution. Compare against this baseline to detect effect-object leaks.
    let guest_objects = m.sys.objects.object_count();
    assert_eq!(guest_objects, objects + 2);
    let run_result = m.run_virtual(1_000_000);
    assert!(matches!(run_result, RunResult::Exited(0)), "{run_result:?}");
    for (address, value) in expected {
        assert_eq!(m.cpu.read_u32(address).unwrap(), value, "at{address:#x}");
    }
    assert_eq!(
        m.cpu.read_u32(DATA + 896).unwrap(),
        m.cpu.read_u32(DATA + 900).unwrap()
    );
    assert_eq!(m.cpu.read_u32(DATA + 260).unwrap(), FLAGS | 8);
    assert_eq!(m.cpu.read_u32(DATA + 264).unwrap(), BYTES);
    assert_eq!(m.cpu.read_u32(DATA + 192).unwrap(), 0x12345678);
    assert_eq!(m.cpu.read_u32(DATA + 368).unwrap(), 5);
    assert_eq!(m.cpu.read_u32(DATA + 200).unwrap(), storage);
    assert_eq!(m.cpu.read_u32(DATA + 204).unwrap(), BYTES);
    assert_eq!(m.sys.objects.object_count(), guest_objects);
    pump(&mut m).unwrap();
    let audio = m.ext.get::<Sound>().audio.clone();
    let stream = audio.stream();
    let mut samples = [0.; 16];
    stream.read_stereo(&mut samples, 44100);
    assert_eq!(&samples[..2], &[0.5, 0.5]);
    assert!(samples[2..].iter().all(|v| *v == 0.));

    assert_eq!(invoke(&mut m, "IDirectSoundBuffer8::Stop", &[buffer]), 0);
    let before = m.sys.objects.object_count();
    for (count, descriptor, result, hr) in
        [(65, u32::MAX, u32::MAX, UNSUPPORTED), (1, 0, 0, INVALID)]
    {
        assert_eq!(
            invoke(
                &mut m,
                "IDirectSoundBuffer8::SetFX",
                &[buffer, count, descriptor, result]
            ),
            hr
        );
        assert_eq!(m.sys.objects.object_count(), before);
    }
    m.cpu.write_u32(DATA + 348, 1).unwrap(); // Nonzero reserved field.
    m.cpu.write_u32(DATA + 368, 0xaaaaaaaa).unwrap();
    assert_eq!(
        invoke(
            &mut m,
            "IDirectSoundBuffer8::SetFX",
            &[buffer, 1, DATA + 320, DATA + 368]
        ),
        INVALID
    );
    assert_eq!(m.cpu.read_u32(DATA + 368).unwrap(), 0xaaaaaaaa);
    m.cpu.write_u32(DATA + 348, 0).unwrap();
    let protected = 0x810000;
    m.cpu.map(protected, 4096, Prot::WRITE).unwrap();
    m.cpu.write(protected, &[0; 32]).unwrap();
    // These are defensive memory failures; neither outputs nor COM ownership
    // change. The existing personality does not claim a native AV return value.
    for (name, args) in [
        (
            "IDirectSoundBuffer8::SetFX",
            vec![buffer, 1, protected, DATA + 368],
        ),
        (
            "IDirectSoundBuffer8::SetFX",
            vec![buffer, 1, DATA + 320, u32::MAX - 1],
        ),
        (
            "IDirectSoundBuffer8::GetObjectInPath",
            vec![buffer, protected, 0, DATA + 176, DATA + 192],
        ),
    ] {
        let thunk = m
            .sys
            .thunks
            .iter()
            .find(|(_, t)| t.module == "dsound.dll" && t.name == name)
            .unwrap()
            .0;
        m.cpu.set_reg(Reg::Esp, STACK);
        for (index, arg) in args.into_iter().enumerate() {
            m.cpu.write_u32(STACK + 4 + index as u32 * 4, arg).unwrap();
        }
        assert!(
            result(&mut Env {
                context: m.sys.current,
                cpu: &mut m.cpu,
                sys: &mut m.sys,
                ext: &mut m.ext,
                thunk
            })
            .is_err()
        );
        assert_eq!(m.cpu.read_u32(DATA + 368).unwrap(), 0xaaaaaaaa);
        assert_eq!(m.cpu.read_u32(DATA + 192).unwrap(), 0x12345678);
        assert_eq!(m.sys.objects.object_count(), before);
    }
    let warning = &m.ext.get::<adv_abi::ApiDiagnostics>().warnings;
    assert!(warning.keys().any(
        |(api, reason)| api == "dsound.dll!IDirectSoundBuffer8::SetFX"
            && reason.contains("bytes=26460")
            && reason.contains("bits=16")
    ));
    assert_eq!(invoke(&mut m, "IDirectSoundBuffer8::Release", &[buffer]), 0);
    assert_eq!(invoke(&mut m, "IDirectSound8::Release", &[device]), 0);
    for _ in 0..3 {
        pump(&mut m).unwrap();
        stream.read_stereo(&mut samples, 44100);
    }
    pump(&mut m).unwrap();
    assert_eq!(m.sys.objects.object_count(), guest_objects - 2);
    assert_eq!(guest_objects - 2, initial + 1); // The known thread owner remains.
    assert!(m.ext.get::<Sound>().storage_records.is_empty());
    assert!(m.ext.get::<Sound>().pending_release.is_empty());
    assert!(m.cpu.read_u8(storage).is_err());
}

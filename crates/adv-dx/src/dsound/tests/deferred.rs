//! Authored Windows native oracle for deferred software allocation and real PCM.
use super::*;

fn status(m: &mut Machine<JitCpu>, buffer: u32) -> u32 {
    assert_eq!(
        invoke(m, "IDirectSoundBuffer8::GetStatus", &[buffer, DATA + 192]),
        0
    );
    m.cpu.read_u32(DATA + 192).unwrap()
}

#[test]
fn deferred_buffers_preserve_caps_and_use_the_shared_software_voice_lifecycle() {
    let mut m = machine();
    let dev = device(&mut m);
    let format = Format {
        rate: 1000,
        channels: 1,
        bits: 8,
    };
    m.cpu.write(DATA + 64, &format.bytes()).unwrap();
    for (flags, expected) in [
        (LOC_DEFER | 8, INVALID),
        (LOC_DEFER | 4, INVALID),
        (LOC_DEFER | 0x1e2, 0),
    ] {
        for (index, value) in [20, flags, 100, 0, DATA + 64].into_iter().enumerate() {
            m.cpu.write_u32(DATA + index as u32 * 4, value).unwrap();
        }
        assert_eq!(
            invoke(
                &mut m,
                "IDirectSound8::CreateSoundBuffer",
                &[dev, DATA, DATA + 128, 0]
            ),
            expected
        );
    }
    let b = m.cpu.read_u32(DATA + 128).unwrap();
    assert_ne!(b, 0);
    m.cpu.write_u32(DATA + 200, 20).unwrap();
    assert_eq!(
        invoke(&mut m, "IDirectSoundBuffer8::GetCaps", &[b, DATA + 200]),
        0
    );
    assert_eq!(m.cpu.read_u32(DATA + 204).unwrap(), LOC_DEFER | 0x1e2);
    for (flags, count, expected) in [
        (0, 0, 0),
        (4, 0, 0),
        (2, 0, INVALID_CALL),
        (6, 0, INVALID),
        (4, 1, INVALID),
    ] {
        assert_eq!(
            invoke(
                &mut m,
                "IDirectSoundBuffer8::AcquireResources",
                &[b, flags, count, 0]
            ),
            expected
        );
        assert_eq!(
            status(&mut m, b),
            0,
            "acquiring does not start PCM playback"
        );
    }
    let bytes = m
        .ext
        .get::<Sound>()
        .buffer(&m.sys.objects, b)
        .unwrap()
        .storage_addr();
    m.cpu.write(bytes, &[192; 100]).unwrap();
    for (priority, flags, expected) in [(0, 2, INVALID_CALL), (0, 6, INVALID), (3, 0, 0), (3, 4, 0)]
    {
        assert_eq!(
            invoke(
                &mut m,
                "IDirectSoundBuffer8::Play",
                &[b, 0, priority, flags]
            ),
            expected
        );
        if expected == 0 {
            pump(&mut m).unwrap();
            let stream = m.ext.get::<Sound>().audio.stream();
            let mut out = [0.0; 40];
            stream.read_stereo(&mut out, 1000);
            assert_eq!(out, [0.5; 40]);
            advance(&mut m, 25_000);
            assert_eq!(position(&mut m, b), 25);
            assert_eq!(invoke(&mut m, "IDirectSoundBuffer8::Stop", &[b]), 0);
            assert_eq!(status(&mut m, b), 0);
            assert_eq!(
                invoke(&mut m, "IDirectSoundBuffer8::SetCurrentPosition", &[b, 0]),
                0
            );
        }
    }
    assert_eq!(invoke(&mut m, "IDirectSoundBuffer8::Release", &[b]), 0);
    assert_eq!(invoke(&mut m, "IDirectSound8::Release", &[dev]), 0);
}

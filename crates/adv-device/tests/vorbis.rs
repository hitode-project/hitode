//! Pure device decoding uses the existing authored sine-tone fixtures. No guest
//! callbacks, Win32 state, audio output or locally installed games are involved.
use adv_device::vorbis::{
    ByteOrder, DecodeError, Next, Packet, PcmFormat, Stream, pcm_bytes, tail_total,
};

const MONO: &[u8] = include_bytes!("../../adv-runtime/tests/data/tone-mono.ogg");
const STEREO: &[u8] = include_bytes!("../../adv-runtime/tests/data/tone-stereo.ogg");
const MONO_PCM: &[u8] = include_bytes!("../../adv-runtime/tests/data/tone-mono.pcm");
const STEREO_PCM: &[u8] = include_bytes!("../../adv-runtime/tests/data/tone-stereo.pcm");
const S16: PcmFormat = PcmFormat {
    bytes_per_sample: 2,
    byte_order: ByteOrder::Little,
    signed: true,
};

fn packet(stream: &mut Stream, input: &[u8], cursor: &mut usize, chunk: usize) -> Option<Packet> {
    loop {
        match stream.next_packet() {
            Next::Packet(packet) => return Some(packet),
            Next::More => {
                stream.compact_input();
                let end = (*cursor + chunk).min(input.len());
                stream.feed(input[*cursor..end].to_vec());
                *cursor = end;
            }
            Next::Eof => return None,
            Next::Error(error) => panic!("Ogg packet read failed: {error:?}"),
        }
    }
}

fn open(stream: &mut Stream, input: &[u8], cursor: &mut usize, chunk: usize) {
    for _ in 0..3 {
        let header = packet(stream, input, cursor, chunk).unwrap();
        stream.accept_header(header).unwrap();
    }
    assert_eq!(stream.header_count(), 3);
    assert_eq!(stream.decoded_frames(), 0, "open must defer audio decoding");
    assert_eq!(stream.pcm_position(), 0);
    assert!(!stream.has_pcm());
}

fn drain(
    stream: &mut Stream,
    input: &[u8],
    cursor: &mut usize,
    chunk: usize,
    target: u64,
) -> Vec<u8> {
    let mut output = Vec::new();
    let mut reached = target == 0;
    while let Some(packet) = packet(stream, input, cursor, chunk) {
        stream.decode_packet(packet).unwrap();
        reached |= stream.skip_to(target);
        if !reached {
            continue;
        }
        // Peek is a prepare operation: failed publication by an API must be
        // able to retry without consuming samples or advancing the position.
        let before = stream.pcm_position();
        let bytes = stream.peek_pcm(usize::MAX, S16);
        assert_eq!(stream.peek_pcm(usize::MAX, S16), bytes);
        assert_eq!(stream.prepare_pcm(usize::MAX, S16), bytes);
        assert_eq!(stream.pcm_position(), before);
        stream.consume_samples(bytes.len() / 2);
        output.extend(bytes);
    }
    output
}

#[test]
fn reusable_pcm_preparation_keeps_frame_alignment_formats_and_retry_position() {
    let mut stream = Stream::new(STEREO.to_vec());
    let mut cursor = 0;
    open(&mut stream, STEREO, &mut cursor, 65_536);
    while !stream.has_pcm() {
        let audio = packet(&mut stream, STEREO, &mut cursor, 65_536).unwrap();
        stream.decode_packet(audio).unwrap();
    }
    for word in [1, 2] {
        for byte_order in [ByteOrder::Little, ByteOrder::Big] {
            for signed in [false, true] {
                let format = PcmFormat {
                    bytes_per_sample: word,
                    byte_order,
                    signed,
                };
                for limit in [0, 1, 7, 513, 4096, 512] {
                    let before = stream.pcm_position();
                    let expected = stream.peek_pcm(limit, format);
                    assert_eq!(stream.prepare_pcm(limit, format), expected);
                    assert_eq!(stream.prepare_pcm(limit, format), expected);
                    assert_eq!(expected.len() % (2 * word as usize), 0);
                    assert_eq!(stream.pcm_position(), before);
                }
            }
        }
    }
    let count = stream.prepare_pcm(512, S16).len();
    stream.consume_samples(count / 2);
    assert_eq!(stream.pcm_position(), (count / 4) as u64);
    stream.reset();
    assert!(stream.prepare_pcm(512, S16).is_empty());
}

fn snr(reference: &[u8], actual: &[u8]) -> f64 {
    let mut energy = 0.0;
    let mut error = 0.0;
    for (r, a) in reference
        .as_chunks::<2>()
        .0
        .iter()
        .zip(actual.as_chunks::<2>().0)
    {
        let r = f64::from(i16::from_le_bytes(*r));
        let a = f64::from(i16::from_le_bytes(*a));
        energy += r * r;
        error += (r - a) * (r - a);
    }
    10.0 * (energy / error.max(1e-30)).log10()
}

#[test]
fn incremental_packets_pcm_commit_and_rewind_keep_reference_samples() {
    for (input, reference, channels) in [(MONO, MONO_PCM, 1), (STEREO, STEREO_PCM, 2)] {
        let mut baseline = None;
        for chunk in [1, 7, 65_536] {
            let mut stream = Stream::new(Vec::new());
            let mut cursor = 0;
            open(&mut stream, input, &mut cursor, chunk);
            assert_eq!(stream.format().channels, channels);
            assert_eq!(stream.format().sample_rate, 44_100);
            let pcm = drain(&mut stream, input, &mut cursor, chunk, 0);
            assert_eq!(pcm.len(), 11_025 * channels * 2, "clip at EOS granule");
            assert_eq!(stream.total_frames(), Some(11_025));
            assert_eq!(stream.pcm_position(), 11_025);
            assert!(
                snr(reference, &pcm) > 60.0,
                "channels={channels} chunk={chunk}"
            );
            if let Some(baseline) = &baseline {
                assert_eq!(&pcm, baseline);
            }
            baseline = Some(pcm.clone());

            let format = stream.format();
            stream.set_totals(11_025, input.len() as u64);
            stream.reset();
            assert_eq!(stream.header_count(), 0);
            assert_eq!(stream.decoded_frames(), 0);
            assert!(!stream.has_pcm());
            assert_eq!(stream.format(), format, "metadata survives rewind");
            assert_eq!(stream.total_frames(), Some(11_025));
            assert_eq!(stream.source_length(), Some(input.len() as u64));
            cursor = 0;
            open(&mut stream, input, &mut cursor, chunk);
            let after_seek = drain(&mut stream, input, &mut cursor, chunk, 2_205);
            assert_eq!(after_seek, pcm[2_205 * channels * 2..]);
        }
    }
}

#[test]
fn metadata_scan_does_not_advance_decoder_and_rejects_corrupt_eos_page() {
    let mut stream = Stream::new(MONO.to_vec());
    let mut cursor = MONO.len();
    open(&mut stream, MONO, &mut cursor, 65_536);
    let before = (
        stream.raw_position(),
        stream.pcm_position(),
        stream.decoded_frames(),
    );
    assert_eq!(tail_total(MONO, stream.format().serial), Some(11_025));
    assert_eq!(
        tail_total(MONO, stream.format().serial.wrapping_add(1)),
        None
    );
    assert_eq!(
        (
            stream.raw_position(),
            stream.pcm_position(),
            stream.decoded_frames()
        ),
        before
    );
    let mut corrupt = MONO.to_vec();
    *corrupt.last_mut().unwrap() ^= 1;
    assert_eq!(tail_total(&corrupt, stream.format().serial), None);

    let audio = packet(&mut stream, MONO, &mut cursor, 65_536).unwrap();
    assert_eq!(stream.accept_header(audio), Err(DecodeError::BadHeader));
    let mut unopened = Stream::new(MONO.to_vec());
    let identification = packet(&mut unopened, MONO, &mut cursor, 65_536).unwrap();
    assert_eq!(
        unopened.decode_packet(identification),
        Err(DecodeError::BadHeader)
    );
    let mut truncated = Stream::new(MONO[..20].to_vec());
    truncated.feed(Vec::new());
    assert!(matches!(
        truncated.next_packet(),
        Next::Error(DecodeError::BadLink)
    ));
}

#[test]
fn pcm_conversion_preserves_width_signedness_endian_rounding_and_clipping() {
    let input = [-2.0, -1.0, -0.5, 0.0, 0.5, 1.0, 2.0];
    assert_eq!(
        pcm_bytes(
            &input,
            PcmFormat {
                bytes_per_sample: 1,
                ..S16
            }
        ),
        [128, 128, 192, 0, 64, 127, 127]
    );
    assert_eq!(
        pcm_bytes(
            &input,
            PcmFormat {
                bytes_per_sample: 1,
                signed: false,
                ..S16
            }
        ),
        [0, 0, 64, 128, 192, 255, 255]
    );
    let expected = [i16::MIN, i16::MIN, -16_384, 0, 16_384, i16::MAX, i16::MAX];
    assert_eq!(
        pcm_bytes(&input, S16),
        expected
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        pcm_bytes(
            &input,
            PcmFormat {
                byte_order: ByteOrder::Big,
                ..S16
            }
        ),
        expected
            .iter()
            .flat_map(|sample| sample.to_be_bytes())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        pcm_bytes(
            &input,
            PcmFormat {
                signed: false,
                ..S16
            }
        ),
        expected
            .iter()
            .flat_map(|sample| (*sample as u16 ^ 0x8000).to_le_bytes())
            .collect::<Vec<_>>()
    );
}

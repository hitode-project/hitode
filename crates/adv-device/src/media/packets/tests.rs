use super::*;
use crate::media::{PacketDecoder, demux};

const WIDTH: u32 = 64;
const HEIGHT: u32 = 48;
const FRAMES: i64 = 10;
const RATE: i32 = 22050;

/// Authored grey ramp plus sine wave, generated outside the decode-only player:
/// ffmpeg -f lavfi -i "nullsrc=size=64x48:rate=10:duration=1,geq=lum='40+16*N':cb=128:cr=128"
/// -f lavfi -i "sine=frequency=440:sample_rate=22050:duration=1"
/// -c:v wmv2 -b:v 400k -c:a wmav2 -b:a 32k -ar 22050 -ac 1 -flags +global_header -f asf ramp.wmv
fn asf_fixture() -> Vec<u8> {
    include_bytes!("ramp.wmv").to_vec()
}

#[test]
fn asf_samples_round_trip_through_the_decoders() {
    let demuxed = demux(asf_fixture()).unwrap();
    assert_eq!(demuxed.container, "asf");
    assert_eq!(demuxed.streams.len(), 2);
    assert!(
        (9_000_000..=12_000_000).contains(&demuxed.duration_100ns),
        "{}",
        demuxed.duration_100ns
    );

    let video = demuxed
        .streams
        .iter()
        .find(|s| matches!(s.format, CodecFormat::Video { .. }))
        .unwrap();
    let CodecFormat::Video {
        fourcc,
        width,
        height,
        ..
    } = &video.format
    else {
        unreachable!()
    };
    assert_eq!(
        (*fourcc, *width, *height),
        (u32::from_le_bytes(*b"WMV2"), WIDTH, HEIGHT)
    );
    assert_eq!(video.packets.len(), FRAMES as usize);
    assert!(video.packets[0].key);
    assert!(
        video
            .packets
            .windows(2)
            .all(|w| w[0].time_100ns < w[1].time_100ns)
    );
    assert!(video.packets.iter().all(|p| p.duration_100ns > 0));
    assert_eq!(
        video.max_packet as usize,
        video.packets.iter().map(|p| p.data.len()).max().unwrap()
    );

    let mut decoder = PacketDecoder::open(&video.format).unwrap();
    let mut frames = Vec::new();
    for packet in &video.packets {
        frames.extend(
            decoder
                .decode(&packet.data, Some(packet.time_100ns))
                .unwrap(),
        );
    }
    frames.extend(decoder.drain().unwrap());
    assert_eq!(frames.len(), FRAMES as usize);
    for (i, (decoded, packet)) in frames.iter().zip(&video.packets).enumerate() {
        let Decoded::Video(frame) = decoded else {
            panic!("audio from a video decoder")
        };
        assert_eq!((frame.width, frame.height), (WIDTH, HEIGHT));
        assert_eq!(frame.pts_us, packet.time_100ns / 10);
        // Limited-range luma: BGRA grey follows the encoded level.
        let expected = (f32::from(40 + i as u8 * 16) - 16.0) * 255.0 / 219.0;
        let centre =
            &frame.bgra[(HEIGHT as usize / 2 * WIDTH as usize + WIDTH as usize / 2) * 4..][..4];
        assert!(
            (f32::from(centre[1]) - expected).abs() < 12.0,
            "frame {i}: {centre:?} vs {expected}"
        );
    }

    let audio = demuxed
        .streams
        .iter()
        .find(|s| matches!(s.format, CodecFormat::Audio { .. }))
        .unwrap();
    let CodecFormat::Audio {
        format_tag,
        rate,
        channels,
        block_align,
        ..
    } = &audio.format
    else {
        unreachable!()
    };
    assert_eq!((*format_tag, *rate, *channels), (0x161, RATE as u32, 1));
    assert!(*block_align > 0);
    let mut decoder = PacketDecoder::open(&audio.format).unwrap();
    let mut samples = 0;
    let mut last_end = None;
    for packet in &audio.packets {
        for decoded in decoder
            .decode(&packet.data, Some(packet.time_100ns))
            .unwrap()
        {
            let Decoded::Audio(frame) = decoded else {
                panic!("video from an audio decoder")
            };
            assert_eq!((frame.sample_rate, frame.channels), (RATE as u32, 1));
            if let Some(end) = last_end {
                // Sample counting keeps decoded blocks contiguous.
                assert!(
                    frame.pts_us.abs_diff(end) <= 1,
                    "{} after {end}",
                    frame.pts_us
                );
            }
            last_end = Some(
                frame.pts_us + frame.samples.len() as u64 * 1_000_000 / u64::from(RATE as u32),
            );
            samples += frame.samples.len();
        }
    }
    assert!(samples >= RATE as usize * 9 / 10, "{samples}");
}

#[test]
fn unknown_codecs_are_refused() {
    let format = CodecFormat::Video {
        fourcc: u32::from_le_bytes(*b"ZZZZ"),
        width: 16,
        height: 16,
        bit_count: 24,
        frame_100ns: 0,
        extradata: Vec::new(),
    };
    assert!(matches!(
        PacketDecoder::open(&format),
        Err(Error::Decode(_))
    ));
}

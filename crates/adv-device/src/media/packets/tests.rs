use super::*;
use crate::media::{PacketDecoder, demux};

const WIDTH: u32 = 64;
const HEIGHT: u32 = 48;
const FRAMES: i64 = 10;
const RATE: i32 = 22050;

/// Encode a small WMV2 + WMA2 ASF file, as Windows Media 8 tools wrote them.
fn asf_fixture() -> Vec<u8> {
    ff::init().unwrap();
    let path = std::env::temp_dir().join(format!("adv-device-packets-{}.wmv", std::process::id()));
    let mut output = format::output_as(&path, "asf").unwrap();

    let video_codec = ff::encoder::find(codec::Id::WMV2).unwrap();
    let mut video = codec::Context::new_with_codec(video_codec)
        .encoder()
        .video()
        .unwrap();
    video.set_width(WIDTH);
    video.set_height(HEIGHT);
    video.set_format(format::Pixel::YUV420P);
    video.set_time_base((1, 10));
    video.set_frame_rate(Some((10, 1)));
    video.set_bit_rate(400_000);
    video.set_flags(codec::flag::Flags::GLOBAL_HEADER);
    let mut video = video.open_as(video_codec).unwrap();

    let audio_codec = ff::encoder::find(codec::Id::WMAV2).unwrap();
    let mut audio = codec::Context::new_with_codec(audio_codec)
        .encoder()
        .audio()
        .unwrap();
    audio.set_rate(RATE);
    audio.set_channel_layout(ff::ChannelLayout::MONO);
    audio.set_format(format::Sample::F32(format::sample::Type::Planar));
    audio.set_bit_rate(32_000);
    audio.set_time_base((1, RATE));
    audio.set_flags(codec::flag::Flags::GLOBAL_HEADER);
    let mut audio = audio.open_as(audio_codec).unwrap();

    output
        .add_stream(video_codec)
        .unwrap()
        .set_parameters(&video);
    output
        .add_stream(audio_codec)
        .unwrap()
        .set_parameters(&audio);
    output.write_header().unwrap();
    let (video_tb, audio_tb) = (
        output.stream(0).unwrap().time_base(),
        output.stream(1).unwrap().time_base(),
    );

    let drain_video = |encoder: &mut ff::encoder::Video, output: &mut format::context::Output| {
        let mut packet = ff::Packet::empty();
        while encoder.receive_packet(&mut packet).is_ok() {
            packet.set_stream(0);
            packet.rescale_ts((1, 10), video_tb);
            packet.write_interleaved(output).unwrap();
        }
    };
    for i in 0..FRAMES {
        let mut picture = frame::Video::new(format::Pixel::YUV420P, WIDTH, HEIGHT);
        let luma = 40 + i as u8 * 16;
        for plane in 0..3 {
            let fill = if plane == 0 { luma } else { 128 };
            picture.data_mut(plane).fill(fill);
        }
        picture.set_pts(Some(i));
        video.send_frame(&picture).unwrap();
        drain_video(&mut video, &mut output);
    }
    video.send_eof().unwrap();
    drain_video(&mut video, &mut output);

    let drain_audio = |encoder: &mut ff::encoder::Audio, output: &mut format::context::Output| {
        let mut packet = ff::Packet::empty();
        while encoder.receive_packet(&mut packet).is_ok() {
            packet.set_stream(1);
            packet.rescale_ts((1, RATE), audio_tb);
            packet.write_interleaved(output).unwrap();
        }
    };
    let chunk = audio.frame_size() as usize;
    let total = RATE as usize; // one second
    let mut at = 0;
    while at < total {
        let mut samples = frame::Audio::new(
            format::Sample::F32(format::sample::Type::Planar),
            chunk,
            ff::ChannelLayout::MONO,
        );
        samples.set_rate(RATE as u32);
        let plane = samples.plane_mut::<f32>(0);
        for (n, value) in plane.iter_mut().enumerate() {
            *value = ((at + n) as f32 * 440.0 * std::f32::consts::TAU / RATE as f32).sin() * 0.5;
        }
        samples.set_pts(Some(at as i64));
        audio.send_frame(&samples).unwrap();
        drain_audio(&mut audio, &mut output);
        at += chunk;
    }
    audio.send_eof().unwrap();
    drain_audio(&mut audio, &mut output);
    output.write_trailer().unwrap();
    drop(output);
    let bytes = std::fs::read(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    bytes
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

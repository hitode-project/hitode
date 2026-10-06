#![cfg(feature = "media")]

use adv_device::media::{Decoder, Error};

fn pcm_wave() -> Vec<u8> {
    let samples = 400u32;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + samples * 2).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&8000u32.to_le_bytes());
    bytes.extend_from_slice(&16000u32.to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&(samples * 2).to_le_bytes());
    for index in 0..samples {
        let value = if index % 2 == 0 { 4096i16 } else { -4096 };
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

#[test]
fn audio_only_media_decodes_once_and_finishes_at_source_time() {
    assert!(Decoder::available());
    assert!(matches!(
        Decoder::open_memory(vec![0; 32]),
        Err(Error::Decode(_))
    ));
    let mut decoder = Decoder::open_memory(pcm_wave()).unwrap();
    let info = decoder.info();
    assert!(info.video.is_none());
    let audio = info.audio.as_ref().unwrap();
    assert_eq!((audio.sample_rate, audio.channels), (8000, 1));
    let first = decoder.frame_at(0).unwrap();
    assert!(first.video.is_none());
    assert!(
        !first.ended,
        "reading EOF must not advance the source clock"
    );
    let pcm: Vec<_> = first
        .audio
        .iter()
        .flat_map(|frame| &frame.samples)
        .copied()
        .collect();
    assert_eq!(pcm.len(), 400);
    assert!(pcm.iter().all(|value| value.is_finite()));
    assert!(pcm.iter().any(|value| value.abs() > 0.1));
    let repeated = decoder.frame_at(0).unwrap();
    assert!(repeated.audio.is_empty());
    assert!(!repeated.ended);
    assert!(decoder.frame_at(50_000).unwrap().ended);
    decoder.seek(0).unwrap();
    let restarted = decoder.frame_at(0).unwrap();
    assert_eq!(
        restarted
            .audio
            .iter()
            .map(|frame| frame.samples.len())
            .sum::<usize>(),
        400
    );
    assert!(!restarted.ended);
}

#[test]
#[ignore = "requires FFmpeg CLI to generate a synthetic MPEG-1 movie"]
fn synthetic_movie_decodes_video_pcm_seek_and_real_eof() {
    let ffmpeg = std::env::var_os("ADV_TEST_FFMPEG").unwrap_or_else(|| "ffmpeg".into());
    let folder = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/media-decoder-tests")
        .join(std::process::id().to_string());
    std::fs::create_dir_all(&folder).unwrap();
    let path = folder.join("synthetic.mpg");
    let output = std::process::Command::new(ffmpeg)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "color=c=red:s=16x16:r=25:d=0.24",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=32000:duration=0.24",
            "-c:v",
            "mpeg1video",
            "-g",
            "1",
            "-b:v",
            "200k",
            "-c:a",
            "mp2",
            "-b:a",
            "64k",
            "-shortest",
            "-f",
            "mpeg",
        ])
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut decoder = Decoder::open_memory(std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(decoder.info().video.as_ref().unwrap().width, 16);
    assert_eq!(decoder.info().video.as_ref().unwrap().height, 16);
    assert!(decoder.info().audio.is_some());
    let mut video_count = 0;
    let mut first_video_pts = None;
    let mut audible = false;
    let mut ended = false;
    for time in (0..=1_000_000).step_by(20_000) {
        let update = decoder.frame_at(time).unwrap();
        for audio in &update.audio {
            assert!(audio.samples.iter().all(|sample| sample.is_finite()));
            audible |= audio.samples.iter().any(|sample| sample.abs() > 0.001);
        }
        if let Some(video) = update.video {
            first_video_pts.get_or_insert(video.pts_us);
            assert_eq!(
                (video.width, video.height, video.bgra.len()),
                (16, 16, 1024)
            );
            assert!(
                video
                    .bgra
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .all(|pixel| pixel[0] < 20 && pixel[1] < 20 && pixel[2] > 220)
            );
            video_count += 1;
        }
        let repeat = decoder.frame_at(time).unwrap();
        assert!(repeat.video.is_none() && repeat.audio.is_empty());
        if update.ended {
            ended = true;
            break;
        }
    }
    assert!(
        video_count >= 2 && audible && ended,
        "video={video_count}, PCM={audible}, EOF={ended}"
    );
    assert!(decoder.stats().converted_frames >= 2);
    decoder.seek(0).unwrap();
    let restarted = decoder.frame_at(first_video_pts.unwrap()).unwrap();
    assert!(
        restarted.video.is_some(),
        "rewind must restore the first due frame"
    );
}

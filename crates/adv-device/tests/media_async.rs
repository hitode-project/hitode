//! Nonblocking movie requests retain the synchronous decoder's answers.
#![cfg(feature = "media")]
use adv_device::media::{Decoder, Update};
use std::time::{Duration, Instant};

fn decoder() -> Decoder {
    Decoder::open_memory(
        include_bytes!("../../adv-dx/src/dshow/graph/render/fixtures/guest_movie.mpg").to_vec(),
    )
    .unwrap()
}

fn frame(decoder: &mut Decoder, time: u64) -> Update {
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(frame) = decoder.poll_frame(time).unwrap() {
            return frame;
        }
        assert!(Instant::now() < until, "decoder did not answer");
        std::thread::yield_now();
    }
}

fn answer(update: Update) -> String {
    format!(
        "{:?} {:?} {}",
        update.video.map(|v| (v.pts_us, v.bgra)),
        update
            .audio
            .into_iter()
            .map(|a| (a.pts_us, a.samples))
            .collect::<Vec<_>>(),
        update.ended
    )
}

// spec: WINE-MEDIA-11
#[test]
fn asynchronous_frames_match_synchronous() {
    let (mut direct, mut asynchronous) = (decoder(), decoder());
    for time in [0, 10_000, 40_000, 80_000, 150_000, 400_000, 1_300_000] {
        assert_eq!(
            answer(frame(&mut asynchronous, time)),
            answer(direct.frame_at(time).unwrap())
        );
    }
}

// spec: WINE-MEDIA-11
#[test]
fn queued_seek_discards_pending_frame() {
    let (mut direct, mut asynchronous) = (decoder(), decoder());
    let _ = asynchronous.poll_frame(0).unwrap();
    asynchronous.request_seek(80_000).unwrap();
    direct.seek(80_000).unwrap();
    assert_eq!(
        answer(frame(&mut asynchronous, 80_000)),
        answer(direct.frame_at(80_000).unwrap())
    );
}

// spec: WINE-MEDIA-9
#[test]
fn drained_windows_media_audio_keeps_sample_time() {
    use adv_device::media::{self, CodecFormat, Decoded, PacketDecoder};
    let media = media::demux(
        include_bytes!("../../adv-runtime/tests/fixtures/media/synthetic.wmv").to_vec(),
    )
    .unwrap();
    let stream = media
        .streams
        .iter()
        .find(|s| matches!(s.format, CodecFormat::Audio { .. }))
        .unwrap();
    let mut decoder = PacketDecoder::open(&stream.format).unwrap();
    let mut frames = Vec::new();
    for packet in &stream.packets {
        frames.extend(
            decoder
                .decode(&packet.data, Some(packet.time_100ns))
                .unwrap(),
        );
    }
    frames.extend(decoder.drain().unwrap());
    let times: Vec<_> = frames
        .into_iter()
        .map(|frame| match frame {
            Decoded::Audio(frame) => frame.pts_us,
            _ => panic!("audio stream produced video"),
        })
        .collect();
    assert_eq!(times.len(), 10);
    assert!(
        times.windows(2).all(|times| times[0] < times[1]),
        "{times:?}"
    );
}

// spec: WINE-MEDIA-11
#[test]
fn program_stream_duration_covers_packet_tail() {
    let decoder = Decoder::open_memory(
        include_bytes!("../../adv-runtime/tests/fixtures/media/synthetic.mpg").to_vec(),
    )
    .unwrap();
    assert!(
        decoder.info().duration_us >= 180_000,
        "duration={}",
        decoder.info().duration_us
    );
}

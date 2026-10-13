use adv_cpu::JitCpu;
use adv_cpu::backend::Guest;
use adv_device::media::{Packet, PacketStream};
use adv_machine::Machine;

use super::*;
use crate::dshow::tests::{DATA, env, handle, machine};

const AUDIO: u16 = 1;
const VIDEO: u16 = 2;
const MS: u64 = 10_000;

fn call(m: &mut Machine<JitCpu>, name: &str, args: &[u32]) -> u32 {
    handle(m, "wmvcore.dll", name, args)
}

fn packets(times_ms: &[u64], key: impl Fn(usize) -> bool, tag: u8) -> Vec<Packet> {
    times_ms
        .iter()
        .enumerate()
        .map(|(i, &time)| Packet {
            time_100ns: time * MS,
            duration_100ns: 100 * MS,
            key: key(i),
            data: vec![tag, i as u8, 0x5a],
        })
        .collect()
}

/// Audio blocks every 250 ms and video frames every 400 ms, keyed every other frame.
fn media() -> Demuxed {
    Demuxed {
        container: "asf".to_owned(),
        duration_100ns: 1_600 * MS,
        streams: vec![
            PacketStream {
                number: AUDIO,
                format: CodecFormat::Audio {
                    format_tag: 0x161,
                    channels: 2,
                    rate: 44_100,
                    bytes_per_second: 16_000,
                    block_align: 2_973,
                    bits: 16,
                    extradata: vec![0; 10],
                },
                bit_rate: 128_000,
                max_packet: 3,
                packets: packets(&[0, 250, 500, 750, 1_000], |_| true, 0xa0),
            },
            PacketStream {
                number: VIDEO,
                format: CodecFormat::Video {
                    fourcc: u32::from_le_bytes(*b"WMV3"),
                    width: 640,
                    height: 480,
                    bit_count: 24,
                    frame_100ns: 400 * MS,
                    extradata: vec![0x4f, 0xd1, 0x1a, 0x01],
                },
                bit_rate: 1_000_000,
                max_packet: 3,
                packets: packets(&[0, 400, 800, 1_200], |i| i % 2 == 0, 0xb0),
            },
        ],
    }
}

fn reader_with(m: &mut Machine<JitCpu>, media: Option<Demuxed>) -> u32 {
    let mut env = env(m);
    let scratch = dshow::alloc(&mut env, 8).unwrap();
    let mut reader = Reader {
        scratch,
        ..Reader::default()
    };
    if let Some(media) = media {
        reader.open(media);
    }
    create(
        &mut env,
        &[Face::SyncReader, Face::Profile, Face::HeaderInfo],
        Obj::Reader(Box::new(reader)),
    )
    .unwrap()
}

#[derive(Debug, PartialEq)]
struct Sample {
    time_ms: u64,
    flags: u32,
    output: u32,
    number: u16,
    data: Vec<u8>,
}

fn next(m: &mut Machine<JitCpu>, reader: u32, number: u16) -> Result<Sample, u32> {
    let hr = call(
        m,
        "IWMSyncReader2::GetNextSample",
        &[
            reader,
            u32::from(number),
            DATA,
            DATA + 8,
            DATA + 16,
            DATA + 24,
            DATA + 28,
            DATA + 32,
        ],
    );
    if hr != S_OK {
        return Err(hr);
    }
    let buffer = m.cpu.read_u32(DATA).unwrap();
    assert_eq!(
        call(
            m,
            "INSSBuffer::GetBufferAndLength",
            &[buffer, DATA + 40, DATA + 44]
        ),
        S_OK
    );
    let mut data = vec![0; m.cpu.read_u32(DATA + 44).unwrap() as usize];
    m.cpu
        .read(m.cpu.read_u32(DATA + 40).unwrap(), &mut data)
        .unwrap();
    assert_eq!(call(m, "INSSBuffer::Release", &[buffer]), 0);
    let mut time = [0; 8];
    m.cpu.read(DATA + 8, &mut time).unwrap();
    Ok(Sample {
        time_ms: u64::from_le_bytes(time) / MS,
        flags: m.cpu.read_u32(DATA + 24).unwrap(),
        output: m.cpu.read_u32(DATA + 28).unwrap(),
        number: m.cpu.read_u16(DATA + 32).unwrap(),
        data,
    })
}

fn read_compressed(m: &mut Machine<JitCpu>, reader: u32) {
    for number in [AUDIO, VIDEO] {
        assert_eq!(
            call(
                m,
                "IWMSyncReader2::SetReadStreamSamples",
                &[reader, u32::from(number), 1]
            ),
            S_OK
        );
    }
}

fn set_range(m: &mut Machine<JitCpu>, reader: u32, start_ms: u64, duration_ms: u64) {
    let (start, duration) = (start_ms * MS, duration_ms * MS);
    assert_eq!(
        call(
            m,
            "IWMSyncReader2::SetRange",
            &[
                reader,
                start as u32,
                (start >> 32) as u32,
                duration as u32,
                (duration >> 32) as u32
            ]
        ),
        S_OK
    );
}

#[test]
fn unopened_reader_refuses_stream_requests() {
    let mut m = machine();
    let reader = reader_with(&mut m, None);
    assert_eq!(next(&mut m, reader, 0), Err(NS_E_INVALID_REQUEST));
    assert_eq!(
        call(&mut m, "IWMProfile::GetStreamCount", &[reader, DATA + 48]),
        NS_E_INVALID_REQUEST
    );
    assert_eq!(
        call(&mut m, "IWMSyncReader2::Close", &[reader]),
        NS_E_INVALID_REQUEST
    );
}

#[test]
// spec: MEDIA-WMV-1
fn stream_samples_arrive_in_time_order_with_clean_point_flags() {
    let mut m = machine();
    let reader = reader_with(&mut m, Some(media()));
    // Decompressed outputs are refused rather than delivered as compressed data.
    assert_eq!(next(&mut m, reader, 0), Err(E_NOTIMPL));
    read_compressed(&mut m, reader);
    let mut order = Vec::new();
    loop {
        match next(&mut m, reader, 0) {
            Ok(sample) => {
                let index = usize::from(sample.number - 1);
                assert_eq!(sample.output, index as u32);
                assert_eq!(sample.data[0], [0xa0, 0xb0][index]);
                order.push((sample.number, sample.time_ms, sample.flags));
            }
            Err(hr) => {
                assert_eq!(hr, NS_E_NO_MORE_SAMPLES);
                break;
            }
        }
    }
    let (clean, start) = (WM_SF_CLEANPOINT, WM_SF_CLEANPOINT | WM_SF_DISCONTINUITY);
    assert_eq!(
        order,
        [
            (AUDIO, 0, start),
            (VIDEO, 0, start),
            (AUDIO, 250, clean),
            (VIDEO, 400, 0),
            (AUDIO, 500, clean),
            (AUDIO, 750, clean),
            (VIDEO, 800, clean),
            (AUDIO, 1_000, clean),
            (VIDEO, 1_200, 0),
        ]
    );
}

#[test]
fn ranges_restart_at_clean_points_and_deselected_streams_stay_silent() {
    let mut m = machine();
    let reader = reader_with(&mut m, Some(media()));
    read_compressed(&mut m, reader);
    m.cpu.write(DATA + 48, &u16::to_le_bytes(AUDIO)).unwrap();
    m.cpu.write_u32(DATA + 52, 0).unwrap();
    assert_eq!(
        call(
            &mut m,
            "IWMSyncReader2::SetStreamsSelected",
            &[reader, 1, DATA + 48, DATA + 52]
        ),
        S_OK
    );
    assert_eq!(
        next(&mut m, reader, AUDIO),
        Err(NS_E_INVALID_REQUEST),
        "a deselected stream has no samples"
    );
    assert_eq!(next(&mut m, reader, 0).unwrap().time_ms, 0);

    // 1000 ms falls on a delta frame: delivery restarts at the clean point before it.
    set_range(&mut m, reader, 1_000, 0);
    let first = next(&mut m, reader, VIDEO).unwrap();
    assert_eq!(
        (first.time_ms, first.flags),
        (800, WM_SF_CLEANPOINT | WM_SF_DISCONTINUITY)
    );
    assert_eq!(next(&mut m, reader, VIDEO).unwrap().time_ms, 1_200);
    assert_eq!(next(&mut m, reader, VIDEO), Err(NS_E_NO_MORE_SAMPLES));

    // A bounded range ends before the first sample at or past its end.
    set_range(&mut m, reader, 0, 500);
    let times: Vec<u64> = std::iter::from_fn(|| next(&mut m, reader, 0).ok())
        .map(|s| s.time_ms)
        .collect();
    assert_eq!(times, [0, 400]);
}

fn attribute_of(m: &mut Machine<JitCpu>, reader: u32, name: &str, capacity: u16) -> u32 {
    let wide: Vec<u8> = name
        .encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect();
    m.cpu.write(DATA + 64, &wide).unwrap();
    m.cpu.write(DATA + 48, &u16::to_le_bytes(0)).unwrap();
    m.cpu
        .write(DATA + 216, &u16::to_le_bytes(capacity))
        .unwrap();
    call(
        m,
        "IWMHeaderInfo::GetAttributeByName",
        &[
            reader,
            DATA + 48,
            DATA + 64,
            DATA + 200,
            DATA + 208,
            DATA + 216,
        ],
    )
}

#[test]
fn header_and_profile_describe_the_opened_streams() {
    let mut m = machine();
    let reader = reader_with(&mut m, Some(media()));
    m.cpu.write(DATA + 128, &IID_IWMHEADERINFO).unwrap();
    assert_eq!(
        call(
            &mut m,
            "IWMSyncReader2::QueryInterface",
            &[reader, DATA + 128, DATA + 120]
        ),
        S_OK
    );
    let header = m.cpu.read_u32(DATA + 120).unwrap();

    assert_eq!(attribute_of(&mut m, header, "Duration", 8), S_OK);
    assert_eq!(m.cpu.read_u32(DATA + 200).unwrap(), WMT_TYPE_QWORD);
    let mut value = [0; 8];
    m.cpu.read(DATA + 208, &mut value).unwrap();
    assert_eq!(u64::from_le_bytes(value), 1_600 * MS);
    assert_eq!(
        attribute_of(&mut m, header, "Duration", 4),
        ASF_E_BUFFERTOOSMALL
    );
    assert_eq!(m.cpu.read_u16(DATA + 216).unwrap(), 8);
    for (name, expected) in [
        ("WM/VideoWidth", 640),
        ("WM/VideoHeight", 480),
        ("WM/VideoFrameRate", 2),
    ] {
        assert_eq!(attribute_of(&mut m, header, name, 4), S_OK, "{name}");
        assert_eq!(m.cpu.read_u32(DATA + 200).unwrap(), WMT_TYPE_DWORD);
        assert_eq!(m.cpu.read_u32(DATA + 208).unwrap(), expected, "{name}");
    }
    assert_eq!(attribute_of(&mut m, header, "NumberOfFrames", 8), S_OK);
    assert_eq!(m.cpu.read_u32(DATA + 208).unwrap(), 4);
    assert_eq!(attribute_of(&mut m, header, "Title", 64), ASF_E_NOTFOUND);
    assert_eq!(call(&mut m, "IWMHeaderInfo::Release", &[header]), 1);

    assert_eq!(
        call(&mut m, "IWMProfile::GetStreamCount", &[reader, DATA + 48]),
        S_OK
    );
    assert_eq!(m.cpu.read_u32(DATA + 48).unwrap(), 2);
    assert_eq!(
        call(
            &mut m,
            "IWMProfile::GetStreamByNumber",
            &[reader, u32::from(VIDEO), DATA + 48]
        ),
        S_OK
    );
    let config = m.cpu.read_u32(DATA + 48).unwrap();
    assert_eq!(
        call(&mut m, "IWMStreamConfig::GetBitrate", &[config, DATA + 52]),
        S_OK
    );
    assert_eq!(m.cpu.read_u32(DATA + 52).unwrap(), 1_000_000);
    m.cpu.write(DATA + 128, &IID_IWMMEDIAPROPS).unwrap();
    assert_eq!(
        call(
            &mut m,
            "IWMStreamConfig::QueryInterface",
            &[config, DATA + 128, DATA + 120]
        ),
        S_OK
    );
    let props = m.cpu.read_u32(DATA + 120).unwrap();
    assert_eq!(
        call(
            &mut m,
            "IWMMediaProps::GetMediaType",
            &[props, 0, DATA + 56]
        ),
        S_OK
    );
    let size = m.cpu.read_u32(DATA + 56).unwrap();
    let block = 0x700000;
    m.cpu.map(block, 4096, Prot::RW).unwrap();
    assert_eq!(
        call(
            &mut m,
            "IWMMediaProps::GetMediaType",
            &[props, block, DATA + 56]
        ),
        S_OK
    );
    let format = m.cpu.read_u32(block + 68).unwrap();
    assert_eq!(m.cpu.read_u32(block + 64).unwrap() + MEDIA_TYPE_SIZE, size);
    // VIDEOINFOHEADER.bmiHeader.biCompression
    assert_eq!(
        m.cpu.read_u32(format + 64).unwrap(),
        u32::from_le_bytes(*b"WMV3")
    );
    assert_eq!(
        call(
            &mut m,
            "IWMMediaProps::GetMediaType",
            &[props, block, DATA + 60]
        ),
        ASF_E_BUFFERTOOSMALL
    );
    assert_eq!(
        call(
            &mut m,
            "IWMProfile::GetStreamByNumber",
            &[reader, 9, DATA + 48]
        ),
        NS_E_INVALID_STREAM
    );
}

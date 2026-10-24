use adv_device::xaudio2::{Buffer, Command, Graph, Send, State, Voice};
use std::sync::{Arc, atomic::Ordering};

// spec: WINE-XAUDIO-1, WINE-XAUDIO-3, WINE-XAUDIO-6
#[test]
fn ranges_loops_operation_sets_and_deferred_callbacks() {
    let (mut control, mut graph) = Graph::new();
    let state = Arc::new(State::default());
    let master = Voice::new(3, 2, 1000, u64::MAX, 0, Arc::new(State::default()));
    let mut submix = Voice::new(2, 2, 1000, 1, 0, Arc::new(State::default()));
    submix.sends = vec![Send {
        voice: 3,
        channels: 2,
        matrix: vec![1.0, 0.0, 0.0, 1.0],
        filter: None,
        filter_state: vec![[0.0; 2]; 2],
    }];
    let mut source = Voice::new(1, 1, 1000, 0, 123, state.clone());
    source.sends = vec![Send {
        voice: 2,
        channels: 2,
        matrix: vec![0.5, 0.25],
        filter: None,
        filter_state: vec![[0.0; 2]; 2],
    }];
    for voice in [master, submix, source] {
        control.send(0, Command::Create(Box::new(voice)));
    }
    control.send(7, Command::Volume(1, 0.5));
    control.send(7, Command::Channels(2, vec![1.0, 2.0]));
    control.send(7, Command::Playing(1, true));
    let mut buffer = Buffer::new(
        vec![0.0, 0.1, 0.2, 0.3, 0.4, 0.5],
        77,
        [1, 6],
        [2, 4, 2],
        true,
    );
    buffer.token = 9;
    state.queued.store(1, Ordering::Release);
    control.send(0, Command::Submit(1, buffer));
    let mut output = [0.0; 20];
    graph.render(&mut output, 2, 1000, 0.0);
    assert_eq!(output, [0.0; 20]);
    assert_eq!(state.queued.load(Ordering::Acquire), 1);
    control.send(0, Command::Commit(7));
    graph.render(&mut output, 2, 1000, 10000.0);
    for (frame, expected) in output
        .as_chunks::<2>()
        .0
        .iter()
        .zip([0.1, 0.2, 0.3, 0.2, 0.3, 0.2, 0.3, 0.4, 0.5, 0.0])
    {
        assert!(
            (frame[0] - expected * 0.25).abs() < 1e-6,
            "{frame:?}/{expected}"
        );
        assert!(
            (frame[1] - expected * 0.25).abs() < 1e-6,
            "{frame:?}/{expected}"
        );
    }
    assert_eq!(state.queued.load(Ordering::Acquire), 0);
    let callbacks: Vec<_> = control
        .events()
        .into_iter()
        .filter(|c| c.object != 0)
        .map(|c| (c.method, c.args))
        .collect();
    assert_eq!(
        callbacks,
        vec![
            (0, vec![0]),
            (3, vec![77]),
            (5, vec![77]),
            (5, vec![77]),
            (4, vec![77]),
            (2, vec![]),
            (1, vec![])
        ]
    );
}

// spec: WINE-XAUDIO-6
#[test]
fn stop_flush_exit_loop_and_frequency_ratio() {
    let (mut control, mut graph) = Graph::new();
    let state = Arc::new(State::default());
    let master = Voice::new(2, 1, 1000, u64::MAX, 0, Arc::new(State::default()));
    let mut source = Voice::new(1, 1, 1000, 0, 0, state.clone());
    source.sends = vec![Send {
        voice: 2,
        channels: 1,
        matrix: vec![1.0],
        filter: None,
        filter_state: vec![[0.0; 2]; 1],
    }];
    control.send(0, Command::Create(Box::new(master)));
    control.send(0, Command::Create(Box::new(source)));
    let buffer = Buffer::new(vec![0.1, 0.2, 0.3, 0.4], 0, [0, 4], [1, 3, 255], false);
    state.queued.store(1, Ordering::Release);
    control.send(0, Command::Submit(1, buffer));
    control.send(0, Command::Playing(1, true));
    let mut output = [0.0; 3];
    graph.render(&mut output, 1, 1000, 0.0);
    assert_eq!(output, [0.1, 0.2, 0.3]);
    control.send(0, Command::Playing(1, false));
    output.fill(0.0);
    graph.render(&mut output, 1, 1000, 3000.0);
    assert_eq!(output, [0.0; 3]);
    control.send(0, Command::ExitLoop(1));
    control.send(0, Command::Playing(1, true));
    control.send(0, Command::Ratio(1, 0.5));
    graph.render(&mut output, 1, 1000, 6000.0);
    assert!((output[0] - 0.4).abs() < 1e-6);
    assert!((output[1] - 0.4).abs() < 1e-6);
    assert_eq!(output[2], 0.0);
    state.queued.store(2, Ordering::Release);
    for context in [1, 2] {
        control.send(
            0,
            Command::Submit(
                1,
                Buffer::new(vec![0.5; 4], context, [0, 4], [0, 4, 0], false),
            ),
        );
    }
    control.send(0, Command::Playing(1, false));
    control.send(0, Command::Flush(1));
    graph.service();
    assert_eq!(state.queued.load(Ordering::Acquire), 0);
}

// spec: WINE-XAUDIO-4, WINE-XAUDIO-7
#[test]
fn reverb_tail_and_meter_precede_voice_gain() {
    use adv_device::xaudio2::Effect;
    let (mut control, mut graph) = Graph::new();
    let state = Arc::new(State::default());
    let master = Voice::new(2, 1, 44100, u64::MAX, 0, Arc::new(State::default()));
    let mut source = Voice::new(1, 1, 44100, 0, 0, state.clone());
    source.sends = vec![Send {
        voice: 2,
        channels: 1,
        matrix: vec![1.0],
        filter: None,
        filter_state: vec![[0.0; 2]; 1],
    }];
    source.effects = vec![
        Effect::Meter(true),
        Effect::Reverb {
            enabled: true,
            channels: 1,
            parameters: Vec::new(),
        },
    ];
    control.send(0, Command::Create(Box::new(master)));
    control.send(0, Command::Create(Box::new(source)));
    let mut samples = vec![0.0; 4410];
    samples[0] = 0.5;
    state.queued.store(1, Ordering::Release);
    control.send(
        0,
        Command::Submit(1, Buffer::new(samples, 0, [0, 4410], [0, 4410, 0], true)),
    );
    control.send(0, Command::Volume(1, 0.25));
    control.send(0, Command::Playing(1, true));
    let mut output = vec![0.0; 4410];
    graph.render(&mut output, 1, 44100, 0.0);
    assert_eq!(f32::from_bits(state.peak[0].load(Ordering::Acquire)), 0.5);
    assert_eq!(output[0], 0.0);
    assert!(output[400..].iter().any(|sample| sample.abs() > 0.00001));
    assert!(output.iter().all(|sample| sample.is_finite()));
    output.fill(0.0);
    graph.render(&mut output, 1, 44100, 100000.0);
    assert!(
        output.iter().any(|sample| sample.abs() > 0.000001),
        "reverb tail after buffer completion"
    );
    control.send(0, Command::Playing(1, false));
    output.fill(0.0);
    graph.render(&mut output, 1, 44100, 200000.0);
    assert!(output.iter().all(|sample| *sample == 0.0));
    control.send(0, Command::Tails(1));
    graph.render(&mut output, 1, 44100, 300000.0);
    assert!(output.iter().any(|sample| sample.abs() > 0.000001));
}

// spec: WINE-XAUDIO-8
#[cfg(feature = "media")]
#[test]
fn adpcm_and_xwma_decode_to_host_pcm() {
    use adv_device::{
        media::{CodecFormat, demux},
        xaudio2::decode,
    };
    let mut format = vec![0u8; 50];
    format[0..2].copy_from_slice(&2u16.to_le_bytes());
    format[2..4].copy_from_slice(&1u16.to_le_bytes());
    format[4..8].copy_from_slice(&22050u32.to_le_bytes());
    format[8..12].copy_from_slice(&44100u32.to_le_bytes());
    format[12..14].copy_from_slice(&8u16.to_le_bytes());
    format[14..16].copy_from_slice(&4u16.to_le_bytes());
    format[16..18].copy_from_slice(&32u16.to_le_bytes());
    format[18..20].copy_from_slice(&4u16.to_le_bytes());
    format[20..22].copy_from_slice(&7u16.to_le_bytes());
    for (i, coefficient) in [
        256i16, 0, 512, -256, 0, 0, 192, 64, 240, 0, 460, -208, 392, -232,
    ]
    .iter()
    .enumerate()
    {
        format[22 + i * 2..24 + i * 2].copy_from_slice(&coefficient.to_le_bytes());
    }
    let pcm = decode(&format, &[0, 16, 0, 0xe8, 3, 0x18, 0xfc, 0]).unwrap();
    assert_eq!(pcm.len(), 4);
    for (sample, expected) in pcm.iter().zip([-1000.0, 1000.0, 1000.0, 1000.0]) {
        assert!((sample - expected / 32768.0).abs() < 1e-6);
    }
    let demuxed = demux(include_bytes!("../src/media/packets/ramp.wmv").to_vec()).unwrap();
    let stream = demuxed
        .streams
        .iter()
        .find(|s| matches!(s.format, CodecFormat::Audio { .. }))
        .unwrap();
    let CodecFormat::Audio {
        format_tag,
        rate,
        channels,
        block_align,
        bits,
        bytes_per_second,
        extradata,
    } = &stream.format
    else {
        unreachable!()
    };
    let mut format = Vec::new();
    format.extend(format_tag.to_le_bytes());
    format.extend(channels.to_le_bytes());
    format.extend(rate.to_le_bytes());
    format.extend(bytes_per_second.to_le_bytes());
    format.extend(block_align.to_le_bytes());
    format.extend(bits.to_le_bytes());
    format.extend((extradata.len() as u16).to_le_bytes());
    format.extend(extradata);
    let data: Vec<_> = stream
        .packets
        .iter()
        .flat_map(|p| p.data.iter().copied())
        .collect();
    let pcm = decode(&format, &data).unwrap();
    assert!(pcm.len() > 19000, "{} decoded frames", pcm.len());
    assert!(pcm.iter().any(|sample| sample.abs() > 0.05));
    assert!(pcm.iter().all(|sample| sample.is_finite()));
}

// spec: WINE-XAUDIO-1
#[test]
fn send_filters_process_the_matrix_output() {
    let (mut control, mut graph) = Graph::new();
    let state = Arc::new(State::default());
    let master = Voice::new(2, 1, 1000, u64::MAX, 0, Arc::new(State::default()));
    let mut source = Voice::new(1, 1, 1000, 0, 0, state.clone());
    source.sends = vec![Send {
        voice: 2,
        channels: 1,
        matrix: vec![0.5],
        filter: Some([0.0, 0.5, 1.0]),
        filter_state: vec![[0.0; 2]],
    }];
    control.send(0, Command::Create(Box::new(master)));
    control.send(0, Command::Create(Box::new(source)));
    state.queued.store(1, Ordering::Release);
    control.send(
        0,
        Command::Submit(1, Buffer::new(vec![1.0; 5], 0, [0, 5], [0, 5, 0], false)),
    );
    control.send(0, Command::Playing(1, true));
    let mut output = [0.0; 5];
    graph.render(&mut output, 1, 1000, 0.0);
    assert_eq!(output, [0.0, 0.125, 0.28125, 0.4140625, 0.5019531]);
}

// spec: WINE-XAUDIO-7
#[test]
#[should_panic(expected = "reverb DSP layout")]
fn unsupported_reverb_layout_does_not_enter_c_dsp() {
    use adv_device::xaudio2::Effect;
    let (mut control, mut graph) = Graph::new();
    let mut master = Voice::new(1, 8, 44100, u64::MAX, 0, Arc::new(State::default()));
    master.effects = vec![Effect::Reverb {
        enabled: true,
        channels: 8,
        parameters: Vec::new(),
    }];
    control.send(0, Command::Create(Box::new(master)));
    graph.render(&mut [0.0; 16], 8, 44100, 0.0);
}

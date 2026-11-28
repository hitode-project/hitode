//! Device-owned XAudio2 voice graph. API commands cross the existing audio
//! ownership boundary; buffer consumption and callback records follow device
//! frames, even while the guest worker cannot execute.
mod reverb;

use adv_sync::spsc::{Consumer, Producer, channel};
use std::collections::{BTreeMap, VecDeque};
use std::sync::{
    Arc,
    atomic::{AtomicU32, AtomicU64, Ordering},
};

pub struct State {
    pub queued: AtomicU32,
    pub context: AtomicU32,
    pub samples: AtomicU64,
    pub peak: [AtomicU32; 64],
    pub rms: [AtomicU32; 64],
}

impl Default for State {
    fn default() -> Self {
        Self {
            queued: AtomicU32::new(0),
            context: AtomicU32::new(0),
            samples: AtomicU64::new(0),
            peak: std::array::from_fn(|_| AtomicU32::new(0)),
            rms: std::array::from_fn(|_| AtomicU32::new(0)),
        }
    }
}

pub struct Buffer {
    pub token: u32,
    pub samples: Vec<f32>,
    pub context: u32,
    pub begin: u32,
    pub end: u32,
    pub loop_begin: u32,
    pub loop_end: u32,
    pub loops: u32,
    pub eos: bool,
    started: bool,
}
impl Buffer {
    pub fn new(
        samples: Vec<f32>,
        context: u32,
        play: [u32; 2],
        region: [u32; 3],
        eos: bool,
    ) -> Self {
        Self {
            token: 0,
            samples,
            context,
            begin: play[0],
            end: play[1],
            loop_begin: region[0],
            loop_end: region[1],
            loops: region[2],
            eos,
            started: false,
        }
    }
}

#[derive(Clone)]
pub struct Send {
    pub voice: u32,
    pub channels: usize,
    pub matrix: Vec<f32>,
    pub filter: Option<[f32; 3]>,
    pub filter_state: Vec<[f32; 2]>,
}
#[derive(Clone)]
pub enum Effect {
    Meter(bool),
    Reverb {
        enabled: bool,
        channels: usize,
        parameters: Vec<u8>,
    },
    FxReverb,
}

pub struct Voice {
    pub id: u32,
    pub channels: usize,
    pub rate: u32,
    /// Sources precede submix stages; the mastering voice is last.
    pub stage: u64,
    pub callback: u32,
    pub generation: u64,
    pub state: Arc<State>,
    pub sends: Vec<Send>,
    pub effects: Vec<Effect>,
    pub filter_enabled: bool,
    volume: f32,
    channel_volumes: Vec<f32>,
    ratio: f32,
    playing: bool,
    tails: bool,
    position: f64,
    buffers: VecDeque<Buffer>,
    reverbs: Vec<Option<reverb::Reverb>>,
    reverb_rate: u32,
    filter: [f32; 3],
    filter_state: Vec<[f32; 2]>,
}
impl Voice {
    pub fn new(
        id: u32,
        channels: usize,
        rate: u32,
        stage: u64,
        callback: u32,
        state: Arc<State>,
    ) -> Self {
        Self {
            id,
            channels,
            rate,
            stage,
            callback,
            generation: 0,
            state,
            sends: Vec::new(),
            effects: Vec::new(),
            filter_enabled: false,
            volume: 1.0,
            channel_volumes: vec![1.0; channels],
            ratio: 1.0,
            playing: stage != 0,
            tails: false,
            position: 0.0,
            buffers: VecDeque::new(),
            reverbs: Vec::new(),
            reverb_rate: 0,
            filter: [0.0, 1.0, 1.0],
            filter_state: vec![[0.0; 2]; channels],
        }
    }
}

pub enum Command {
    Create(Box<Voice>),
    Destroy(u32),
    Submit(u32, Buffer),
    Playing(u32, bool),
    Tails(u32),
    Flush(u32),
    Discontinuity(u32),
    ExitLoop(u32),
    Volume(u32, f32),
    Channels(u32, Vec<f32>),
    Ratio(u32, f32),
    Rate(u32, u32),
    Sends(u32, Vec<Send>),
    Effects(u32, Vec<Effect>),
    Effect(u32, usize, Effect),
    EffectEnabled(u32, usize, bool),
    Matrix(u32, u32, Vec<f32>),
    Filter(u32, [f32; 3]),
    SendFilter(u32, u32, [f32; 3]),
    Engine(bool),
    Callbacks(Vec<u32>),
    Commit(u32),
}
struct Scheduled {
    set: u32,
    command: Command,
}
/// A guest callback, recorded in the same order as the processing pass.
pub struct Callback {
    pub voice: u32,
    pub generation: u64,
    pub object: u32,
    pub method: u32,
    pub args: Vec<u32>,
    pub due_us: u64,
}
pub struct Event {
    pub callbacks: Vec<Callback>,
    // Completed storage is destroyed by the execution owner.
    buffers: Vec<Buffer>,
    voices: Vec<Voice>,
    reverbs: Vec<reverb::Reverb>,
}

pub struct Control {
    commands: Producer<Scheduled>,
    events: Consumer<Event>,
    pending: VecDeque<Scheduled>,
}
impl Control {
    pub fn send(&mut self, set: u32, command: Command) {
        self.pending.push_back(Scheduled { set, command });
        self.maintain();
    }
    pub fn maintain(&mut self) {
        while let Some(command) = self.pending.pop_front() {
            if let Err(command) = self.commands.push(command) {
                self.pending.push_front(command);
                break;
            }
        }
    }
    pub fn events(&mut self) -> Vec<Callback> {
        self.maintain();
        let mut callbacks = Vec::new();
        while let Some(mut event) = self.events.pop() {
            callbacks.append(&mut event.callbacks);
        }
        callbacks
    }
}

pub struct Graph {
    pub owner: u32,
    pub generation: u64,
    commands: Consumer<Scheduled>,
    events: Producer<Event>,
    pending_events: VecDeque<Event>,
    pending: Vec<Scheduled>,
    voices: BTreeMap<u32, Voice>,
    callbacks: Vec<u32>,
    running: bool,
}
impl Graph {
    pub fn new() -> (Control, Box<Self>) {
        let (tx, rx) = channel(2048);
        let (events, notices) = channel(4096);
        (
            Control {
                commands: tx,
                events: notices,
                pending: VecDeque::new(),
            },
            Box::new(Self {
                owner: 0,
                generation: 0,
                commands: rx,
                events,
                pending_events: VecDeque::new(),
                pending: Vec::new(),
                voices: BTreeMap::new(),
                callbacks: Vec::new(),
                running: true,
            }),
        )
    }
    pub fn service(&mut self) {
        let mut retired = Event {
            callbacks: Vec::new(),
            buffers: Vec::new(),
            voices: Vec::new(),
            reverbs: Vec::new(),
        };
        while let Some(scheduled) = self.commands.pop() {
            if let Command::Commit(set) = scheduled.command {
                let mut ready = Vec::new();
                self.pending.retain_mut(|pending| {
                    if set == 0 || pending.set == set {
                        ready.push(std::mem::replace(&mut pending.command, Command::Commit(0)));
                        false
                    } else {
                        true
                    }
                });
                for command in ready {
                    self.apply(command, &mut retired);
                }
            } else if scheduled.set != 0 && self.running {
                self.pending.push(scheduled);
            } else {
                self.apply(scheduled.command, &mut retired);
            }
        }
        if !retired.buffers.is_empty()
            || !retired.voices.is_empty()
            || !retired.callbacks.is_empty()
            || !retired.reverbs.is_empty()
        {
            self.pending_events.push_back(retired);
        }
        self.publish();
    }
    fn apply(&mut self, command: Command, retired: &mut Event) {
        match command {
            Command::Create(mut voice) => {
                let channels = voice
                    .effects
                    .iter()
                    .fold(voice.channels, |channels, effect| match effect {
                        Effect::Reverb { channels, .. } => *channels,
                        _ => channels,
                    });
                voice.channel_volumes.resize(channels, 1.0);
                self.voices.insert(voice.id, *voice);
            }
            Command::Destroy(id) => {
                self.pending.retain(|scheduled| match &scheduled.command {
                    Command::Playing(voice, _)
                    | Command::Tails(voice)
                    | Command::Volume(voice, _)
                    | Command::Channels(voice, _)
                    | Command::Ratio(voice, _)
                    | Command::Matrix(voice, ..)
                    | Command::Effect(voice, ..)
                    | Command::EffectEnabled(voice, ..)
                    | Command::ExitLoop(voice)
                    | Command::Filter(voice, _)
                    | Command::SendFilter(voice, ..) => *voice != id,
                    _ => true,
                });
                if let Some(voice) = self.voices.remove(&id) {
                    retired.voices.push(voice);
                }
            }
            Command::Engine(running) => self.running = running,
            Command::Callbacks(callbacks) => self.callbacks = callbacks,
            command => {
                let id = match &command {
                    Command::Effect(id, ..)
                    | Command::EffectEnabled(id, ..)
                    | Command::Matrix(id, ..)
                    | Command::Submit(id, _)
                    | Command::Playing(id, _)
                    | Command::Tails(id)
                    | Command::Flush(id)
                    | Command::Discontinuity(id)
                    | Command::ExitLoop(id)
                    | Command::Volume(id, _)
                    | Command::Channels(id, _)
                    | Command::Ratio(id, _)
                    | Command::Rate(id, _)
                    | Command::Sends(id, _)
                    | Command::Effects(id, _)
                    | Command::Filter(id, _)
                    | Command::SendFilter(id, ..) => *id,
                    _ => return,
                };
                let Some(voice) = self.voices.get_mut(&id) else {
                    return;
                };
                match command {
                    Command::Submit(_, buffer) => {
                        voice.buffers.push_back(buffer);
                    }
                    Command::Playing(_, playing) => {
                        voice.playing = playing;
                        voice.tails = false;
                    }
                    Command::Tails(_) => {
                        voice.playing = false;
                        voice.tails = true;
                    }
                    Command::Flush(_) => {
                        let keep = usize::from(
                            voice.playing && voice.buffers.front().is_some_and(|b| b.started),
                        );
                        while voice.buffers.len() > keep {
                            let buffer = voice.buffers.remove(keep).unwrap();
                            retired.callbacks.push(Callback {
                                object: 0,
                                voice: id,
                                generation: voice.generation,
                                method: id,
                                args: vec![buffer.token],
                                due_us: 0,
                            });
                            record(
                                &mut retired.callbacks,
                                voice.id,
                                voice.generation,
                                voice.callback,
                                4,
                                vec![buffer.context],
                                0,
                            );
                            voice.state.queued.fetch_sub(1, Ordering::Release);
                            retired.buffers.push(buffer);
                        }
                        if keep == 0 {
                            voice.position = 0.0;
                            voice.state.context.store(0, Ordering::Release);
                        }
                    }
                    Command::Discontinuity(_) => {
                        if let Some(buffer) = voice.buffers.back_mut() {
                            buffer.eos = true;
                        }
                    }
                    Command::ExitLoop(_) => {
                        if let Some(buffer) = voice.buffers.front_mut() {
                            buffer.loops = 0;
                        }
                    }
                    Command::Volume(_, volume) => voice.volume = volume,
                    Command::Channels(_, volumes) => voice.channel_volumes = volumes,
                    Command::Ratio(_, ratio) => voice.ratio = ratio,
                    Command::Rate(_, rate) => voice.rate = rate,
                    Command::Sends(_, sends) => voice.sends = sends,
                    Command::Matrix(_, destination, matrix) => {
                        if let Some(send) = voice.sends.iter_mut().find(|s| s.voice == destination)
                        {
                            send.matrix = matrix;
                        }
                    }
                    Command::EffectEnabled(_, index, enabled) => match &mut voice.effects[index] {
                        Effect::Meter(value) => *value = enabled,
                        Effect::Reverb { enabled: value, .. } => *value = enabled,
                        _ => {}
                    },
                    Command::Effect(_, index, mut effect) => {
                        match (&voice.effects[index], &mut effect) {
                            (
                                Effect::Reverb { enabled, .. },
                                Effect::Reverb { enabled: value, .. },
                            ) => *value = *enabled,
                            (Effect::Meter(enabled), Effect::Meter(value)) => *value = *enabled,
                            _ => {}
                        }
                        voice.effects[index] = effect;
                    }
                    Command::Effects(_, effects) => {
                        retired.reverbs.extend(voice.reverbs.drain(..).flatten());
                        voice.effects = effects;
                    }
                    Command::Filter(_, filter) => {
                        voice.filter = filter;
                        voice.filter_enabled = true;
                    }
                    Command::SendFilter(_, destination, filter) => {
                        if let Some(send) = voice.sends.iter_mut().find(|s| s.voice == destination)
                        {
                            send.filter = Some(filter);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    pub fn active(&self) -> bool {
        true
    }
    fn publish(&mut self) {
        while let Some(event) = self.pending_events.pop_front() {
            if let Err(event) = self.events.push(event) {
                self.pending_events.push_front(event);
                break;
            }
        }
    }
    pub fn render(&mut self, output: &mut [f32], channels: u16, rate: u32, pts: f64) {
        self.service();
        if !self.running {
            return;
        }
        let frames = output.len() / usize::from(channels);
        let mut event = Event {
            callbacks: Vec::new(),
            buffers: Vec::new(),
            voices: Vec::new(),
            reverbs: Vec::new(),
        };
        for &cb in &self.callbacks {
            record(
                &mut event.callbacks,
                self.owner,
                self.generation,
                cb,
                0,
                vec![],
                pts as u64,
            );
        }
        let mut order: Vec<_> = self.voices.values().map(|v| (v.stage, v.id)).collect();
        order.sort_unstable();
        let mut buses: BTreeMap<u32, Vec<f32>> = self
            .voices
            .values()
            .map(|v| (v.id, vec![0.0; frames * v.channels]))
            .collect();
        for (_, id) in order {
            let voice = self.voices.get_mut(&id).unwrap();
            let mut bus = buses.remove(&id).unwrap();
            if voice.stage == 0 && !voice.playing && !voice.tails {
                continue;
            }
            if voice.stage == 0 && voice.playing {
                record(
                    &mut event.callbacks,
                    voice.id,
                    voice.generation,
                    voice.callback,
                    0,
                    vec![0],
                    pts as u64,
                );
                for frame in 0..frames {
                    let due = (pts + frame as f64 * 1_000_000.0 / f64::from(rate)) as u64;
                    while let Some(buffer) = voice.buffers.front_mut() {
                        if !buffer.started {
                            buffer.started = true;
                            voice.position = f64::from(buffer.begin);
                            voice.state.context.store(buffer.context, Ordering::Release);
                            record(
                                &mut event.callbacks,
                                voice.id,
                                voice.generation,
                                voice.callback,
                                3,
                                vec![buffer.context],
                                due,
                            );
                        }
                        if buffer.loops > 0 && voice.position >= f64::from(buffer.loop_end) {
                            voice.position -= f64::from(buffer.loop_end - buffer.loop_begin);
                            if buffer.loops != 255 {
                                buffer.loops -= 1;
                            }
                            record(
                                &mut event.callbacks,
                                voice.id,
                                voice.generation,
                                voice.callback,
                                5,
                                vec![buffer.context],
                                due,
                            );
                            continue;
                        }
                        if voice.position >= f64::from(buffer.end) {
                            let buffer = voice.buffers.pop_front().unwrap();
                            voice.state.queued.fetch_sub(1, Ordering::Release);
                            event.callbacks.push(Callback {
                                object: 0,
                                voice: id,
                                generation: voice.generation,
                                method: id,
                                args: vec![buffer.token],
                                due_us: due,
                            });
                            record(
                                &mut event.callbacks,
                                voice.id,
                                voice.generation,
                                voice.callback,
                                4,
                                vec![buffer.context],
                                due,
                            );
                            if buffer.eos {
                                voice.state.samples.store(0, Ordering::Release);
                                record(
                                    &mut event.callbacks,
                                    voice.id,
                                    voice.generation,
                                    voice.callback,
                                    2,
                                    vec![],
                                    due,
                                );
                            }
                            voice.state.context.store(0, Ordering::Release);
                            event.buffers.push(buffer);
                            continue;
                        }
                        let position = voice.position as usize;
                        let next = (position + 1).min(buffer.end as usize - 1);
                        let fraction = (voice.position - position as f64) as f32;
                        for channel in 0..voice.channels {
                            let a = buffer.samples[position * voice.channels + channel];
                            let b = buffer.samples[next * voice.channels + channel];
                            bus[frame * voice.channels + channel] = a + (b - a) * fraction;
                        }
                        let step = f64::from(voice.rate) * f64::from(voice.ratio) / f64::from(rate);
                        let before = voice.position as u64;
                        voice.position += step;
                        voice
                            .state
                            .samples
                            .fetch_add(voice.position as u64 - before, Ordering::Release);
                        break;
                    }
                }
                record(
                    &mut event.callbacks,
                    voice.id,
                    voice.generation,
                    voice.callback,
                    1,
                    vec![],
                    (pts + frames as f64 * 1_000_000.0 / f64::from(rate)) as u64,
                );
            }
            if voice.filter_enabled {
                for frame in bus.chunks_mut(voice.channels) {
                    for (channel, sample) in frame.iter_mut().enumerate() {
                        *sample =
                            filter_sample(*sample, voice.filter, &mut voice.filter_state[channel]);
                    }
                }
            }
            if voice.reverb_rate != rate {
                event.reverbs.extend(voice.reverbs.drain(..).flatten());
                voice.reverb_rate = rate;
            }
            voice.reverbs.resize_with(voice.effects.len(), || None);
            let mut output_channels = voice.channels;
            for (index, effect) in voice.effects.iter().enumerate() {
                match effect {
                    Effect::Meter(true) => {
                        for channel in 0..output_channels.min(64) {
                            let mut peak = 0.0f32;
                            let mut square = 0.0;
                            for frame in bus.chunks(output_channels) {
                                peak = peak.max(frame[channel].abs());
                                square += frame[channel] * frame[channel];
                            }
                            voice.state.peak[channel].store(peak.to_bits(), Ordering::Release);
                            voice.state.rms[channel].store(
                                (square / frames as f32).sqrt().to_bits(),
                                Ordering::Release,
                            );
                        }
                    }
                    Effect::Reverb {
                        enabled,
                        channels: next,
                        parameters,
                    } => {
                        if *enabled {
                            let dsp = voice.reverbs[index].get_or_insert_with(|| {
                                reverb::Reverb::new(rate, output_channels, *next)
                            });
                            dsp.parameters(parameters);
                            bus = dsp.process(&mut bus, output_channels, *next);
                        } else if output_channels != *next {
                            let mut copy = vec![0.0; frames * next];
                            for (input, output) in
                                bus.chunks(output_channels).zip(copy.chunks_mut(*next))
                            {
                                output[0] = input[0];
                                output[1] = input[output_channels - 1];
                            }
                            bus = copy;
                        }
                        output_channels = *next;
                    }
                    _ => {}
                }
            }
            for frame in bus.chunks_mut(output_channels) {
                for (channel, sample) in frame.iter_mut().enumerate() {
                    *sample *= voice.volume * voice.channel_volumes[channel];
                }
            }
            if voice.stage == u64::MAX {
                for (frame, source) in output
                    .chunks_mut(usize::from(channels))
                    .zip(bus.chunks(output_channels))
                {
                    for (channel, sample) in frame.iter_mut().enumerate() {
                        *sample += source[channel.min(output_channels - 1)];
                    }
                }
            } else {
                for send in &mut voice.sends {
                    if let Some(destination) = buses.get_mut(&send.voice) {
                        let mut mixed = vec![0.0; send.channels];
                        for (source, dest) in bus
                            .chunks(output_channels)
                            .zip(destination.chunks_mut(send.channels))
                        {
                            mixed.fill(0.0);
                            for (d, value) in mixed.iter_mut().enumerate() {
                                for (s, sample) in source.iter().enumerate() {
                                    *value += sample * send.matrix[d * output_channels + s];
                                }
                                if let Some(filter) = send.filter {
                                    *value =
                                        filter_sample(*value, filter, &mut send.filter_state[d]);
                                }
                                dest[d] += *value;
                            }
                        }
                    }
                }
            }
        }
        for &cb in &self.callbacks {
            record(
                &mut event.callbacks,
                self.owner,
                self.generation,
                cb,
                1,
                vec![],
                (pts + frames as f64 * 1_000_000.0 / f64::from(rate)) as u64,
            );
        }
        if !event.callbacks.is_empty() || !event.buffers.is_empty() || !event.reverbs.is_empty() {
            self.pending_events.push_back(event);
        }
        self.publish();
    }
}
fn filter_sample(sample: f32, filter: [f32; 3], state: &mut [f32; 2]) -> f32 {
    state[0] += filter[1] * state[1];
    let high = sample - state[0] - filter[2] * state[1];
    state[1] += filter[1] * high;
    match filter[0] as u32 {
        1 => state[1],
        2 => high,
        3 => high + state[0],
        _ => state[0],
    }
}

fn record(
    events: &mut Vec<Callback>,
    voice: u32,
    generation: u64,
    object: u32,
    method: u32,
    args: Vec<u32>,
    due_us: u64,
) {
    if object != 0 {
        events.push(Callback {
            voice,
            generation,
            object,
            method,
            args,
            due_us,
        });
    }
}

/// Convert a submitted Windows audio buffer into host PCM before it enters
/// the device queue. Compressed packets use the shared media decoder.
pub fn decode(format: &[u8], data: &[u8]) -> Result<Vec<f32>, String> {
    let u16_at = |at| u16::from_le_bytes([format[at], format[at + 1]]);
    let u32_at = |at| u32::from_le_bytes(format[at..at + 4].try_into().unwrap());
    let mut tag = u16_at(0);
    if tag == 0xfffe && format.len() >= 40 {
        tag = u16_at(24);
    }
    let bits = u16_at(14);
    if tag == 3 && bits == 16 {
        tag = 1;
    }
    if tag == 1 || tag == 3 {
        let width = usize::from(bits) / 8;
        if !matches!((tag, bits), (1, 8 | 16 | 24 | 32) | (3, 32)) {
            return Err("unsupported PCM layout".into());
        }
        return Ok(data
            .chunks_exact(width)
            .map(|b| match (tag, bits) {
                (3, 32) => f32::from_le_bytes(b.try_into().unwrap()),
                (_, 8) => (f32::from(b[0]) - 128.0) / 128.0,
                (_, 16) => f32::from(i16::from_le_bytes(b.try_into().unwrap())) / 32768.0,
                (_, 24) => {
                    (((i32::from(b[0]) << 8) | (i32::from(b[1]) << 16) | (i32::from(b[2]) << 24))
                        >> 8) as f32
                        / 8388607.0
                }
                _ => (i32::from_le_bytes(b.try_into().unwrap()) >> 8) as f32 / 8388607.0,
            })
            .collect());
    }
    if tag == 2 {
        let frames = u16_at(18);
        let mut decoder = crate::codec::Decoder::new(crate::codec::DecoderConfig {
            codec: crate::codec::Codec::MicrosoftAdpcm,
            sample_rate: u32_at(4),
            channels: u16_at(2),
            bits_per_sample: bits,
            frames_per_block: frames,
            max_frames_per_packet: usize::from(frames),
        })
        .map_err(|error| error.to_string())?;
        let mut decoded = Vec::new();
        for block in data.chunks_exact(usize::from(u16_at(12))) {
            decoder
                .decode_into(
                    block,
                    u64::from(frames),
                    crate::codec::PcmEncoding::S16Le,
                    &mut decoded,
                )
                .map_err(|error| error.to_string())?;
        }
        return Ok(decoded
            .as_chunks::<2>()
            .0
            .iter()
            .map(|bytes| f32::from(i16::from_le_bytes(*bytes)) / 32768.0)
            .collect());
    }
    let codec = crate::media::CodecFormat::Audio {
        format_tag: tag,
        rate: u32_at(4),
        channels: u16_at(2),
        block_align: u16_at(12),
        bits,
        bytes_per_second: u32_at(8),
        extradata: format[18..].to_vec(),
    };
    let mut decoder = crate::media::PacketDecoder::open(&codec).map_err(|e| e.to_string())?;
    let align = usize::from(u16_at(12));
    let mut samples = Vec::new();
    for packet in data.chunks(align) {
        for frame in decoder.decode(packet, None).map_err(|e| e.to_string())? {
            if let crate::media::Decoded::Audio(frame) = frame {
                samples.extend_from_slice(&frame.samples);
            }
        }
    }
    for frame in decoder.drain().map_err(|e| e.to_string())? {
        if let crate::media::Decoded::Audio(frame) = frame {
            samples.extend_from_slice(&frame.samples);
        }
    }
    Ok(samples)
}

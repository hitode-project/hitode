//! Execution/audio ownership boundary. One SPSC producer belongs to the execution
//! thread; the callback exclusively owns Engine. Resource retirement goes back
//! through a second SPSC queue, so the callback never destroys heap buffers.
use crate::{
    clock::PlaybackClock,
    mixer::{PcmBlock, PcmMetrics},
    owned::OwnedVoice,
    resample::Converter,
    ring::{RingConfig, RingVoice},
};
use adv_sync::{
    WakeEvent,
    spsc::{Consumer, Producer, channel},
};
use std::{
    cell::RefCell,
    collections::{BTreeMap, VecDeque},
    rc::Rc,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU64, Ordering},
    },
};

const SOURCES: usize = 256;
#[derive(Default)]
pub(crate) struct SourceState {
    pub queued_us: AtomicU64,
    pub queued_blocks: AtomicU64,
}
#[derive(Default)]
pub struct RingState {
    sequence: AtomicU64,
    position: AtomicU64,
    minimum_position: AtomicU64,
    pts: AtomicU64,
    frequency: AtomicU32,
    bytes: AtomicU32,
    align: AtomicU32,
    playing: AtomicBool,
    looping: AtomicBool,
    generation: AtomicU64,
}
impl RingState {
    pub fn cursor(&self, now: u64, generation: u64) -> Option<u32> {
        let (epoch, bytes, align, base, minimum, playing, looping, pts, frequency) = loop {
            let sequence = self.sequence.load(Ordering::Acquire);
            if sequence & 1 != 0 {
                std::hint::spin_loop();
                continue;
            }
            let snapshot = (
                self.generation.load(Ordering::Relaxed),
                self.bytes.load(Ordering::Relaxed),
                self.align.load(Ordering::Relaxed),
                self.position.load(Ordering::Relaxed),
                self.minimum_position.load(Ordering::Relaxed),
                self.playing.load(Ordering::Relaxed),
                self.looping.load(Ordering::Relaxed),
                self.pts.load(Ordering::Relaxed),
                self.frequency.load(Ordering::Relaxed),
            );
            std::sync::atomic::fence(Ordering::Acquire);
            if self.sequence.load(Ordering::Relaxed) == sequence {
                break snapshot;
            }
        };
        if epoch != generation {
            return None;
        }
        if bytes == 0 {
            return Some(0);
        }
        let align = align.max(1);
        let mut position = i128::from(base);
        if playing || (!looping && base >= u64::from(bytes) && now < pts) {
            let delta = i128::from(now) - i128::from(pts);
            position += delta * i128::from(frequency) * i128::from(align) / 1_000_000;
        }
        if looping {
            Some(
                position
                    .max(i128::from(minimum))
                    .rem_euclid(i128::from(bytes)) as u32
                    / align
                    * align,
            )
        } else {
            Some(if position >= i128::from(bytes) {
                0
            } else {
                position.max(0) as u32 / align * align
            })
        }
    }
}

pub enum AudioEvent {
    Notification {
        source: u64,
        generation: u64,
        event: u32,
        due_us: u64,
    },
    Released {
        source: u64,
    },
}
pub(crate) enum Retired {
    Ring(Box<RingVoice>, Arc<RingState>),
    Owned(Box<OwnedVoice>),
    Pcm(PcmBlock),
    Converter(Converter),
    Notices(Vec<(u32, u32)>),
}
enum Command {
    Ring(u64, Box<RingVoice>, Arc<RingState>),
    Owned(u64, Box<OwnedVoice>),
    Pcm(PcmBlock),
    Play(u64, bool, u64, u64),
    Epoch(u64, u64),
    Stop(u64),
    Position(u64, u32, u64),
    Written(u64, u32, u32),
    Gain(u64, [f32; 2]),
    Retune(u64, u32, Converter),
    Notices(u64, Vec<(u32, u32)>),
    Pause(u64, bool),
    End(u64),
    Remove(u64),
    Suspend(bool),
}
enum Voice {
    Ring {
        source: u64,
        voice: Box<RingVoice>,
        state: Arc<RingState>,
        generation: u64,
    },
    Owned {
        source: u64,
        voice: Box<OwnedVoice>,
    },
}
impl Voice {
    fn source(&self) -> u64 {
        match self {
            Self::Ring { source, .. } | Self::Owned { source, .. } => *source,
        }
    }
    fn active(&self) -> bool {
        match self {
            Self::Ring { voice, .. } => voice.active(),
            Self::Owned { voice, .. } => voice.active(),
        }
    }
    fn retire(self) -> Retired {
        match self {
            Self::Ring { voice, state, .. } => Retired::Ring(voice, state),
            Self::Owned { voice, .. } => Retired::Owned(voice),
        }
    }
}
#[derive(Default)]
struct Counters {
    queued_blocks: AtomicU64,
    queued_source_frames: AtomicU64,
    playback_starts: AtomicU64,
    flushes: AtomicU64,
    ends: AtomicU64,
    removals: AtomicU64,
    max_stream_queue_us: AtomicU64,
    missing_stream_frames: AtomicU64,
    missing_output_frames: AtomicU64,
    active_sources: AtomicU64,
    audible_sources: AtomicU64,
    output_samples: AtomicU64,
    nonzero_samples: AtomicU64,
}
pub(crate) struct Shared {
    engine: AtomicPtr<Engine>,
    clock: Arc<PlaybackClock>,
    rate: AtomicU32,
    suspended: AtomicBool,
    desired_active: AtomicU64,
    pending_commands: AtomicU64,
    active_until_us: AtomicU64,
    connected: AtomicBool,
    waker: OnceLock<WakeEvent>,
    counters: Counters,
}
impl Drop for Shared {
    fn drop(&mut self) {
        let ptr = *self.engine.get_mut();
        if !ptr.is_null() {
            unsafe {
                drop(Box::from_raw(ptr));
            }
        }
    }
}
struct Control {
    commands: Producer<Command>,
    events: Consumer<AudioEvent>,
    retired: Consumer<Retired>,
    backlog: VecDeque<Command>,
    formats: BTreeMap<u64, (u32, Arc<SourceState>)>,
    rings: BTreeMap<u64, (u32, u32)>,
    pending_events: VecDeque<AudioEvent>,
    guest_us: u64,
}
/// Clone only within the execution thread. The SPSC endpoint itself is never cloned.
#[derive(Clone)]
pub struct AudioControl {
    local: Rc<RefCell<Control>>,
    stream: crate::AudioStream,
}
impl AudioControl {
    pub fn new(device_allowed: bool) -> Self {
        let (commands_tx, commands_rx) = channel(2048);
        let (events_tx, events_rx) = channel(4096);
        let (retired_tx, retired_rx) = channel(4096);
        let clock = Arc::new(PlaybackClock::new(device_allowed));
        let shared = Arc::new(Shared {
            engine: AtomicPtr::new(std::ptr::null_mut()),
            clock,
            rate: AtomicU32::new(44100),
            suspended: AtomicBool::new(false),
            desired_active: AtomicU64::new(0),
            pending_commands: AtomicU64::new(0),
            active_until_us: AtomicU64::new(0),
            connected: AtomicBool::new(false),
            waker: OnceLock::new(),
            counters: Counters::default(),
        });
        let engine = Box::new(Engine {
            commands: commands_rx,
            events: events_tx,
            retired: retired_tx,
            voices: (0..SOURCES).map(|_| None).collect(),
            suspended: false,
            cursor_us: 0.0,
            virtual_us: 0,
            rate: 44100,
            channels: 2,
            notification_retry: None,
        });
        shared
            .engine
            .store(Box::into_raw(engine), Ordering::Release);
        let stream = crate::AudioStream { shared };
        Self {
            local: Rc::new(RefCell::new(Control {
                commands: commands_tx,
                events: events_rx,
                retired: retired_rx,
                backlog: VecDeque::with_capacity(4096),
                formats: BTreeMap::new(),
                rings: BTreeMap::new(),
                pending_events: VecDeque::new(),
                guest_us: 0,
            })),
            stream,
        }
    }
    pub fn stream(&self) -> crate::AudioStream {
        self.stream.clone()
    }
    fn send(&self, command: Command) -> bool {
        self.try_send(command).is_ok()
    }
    #[allow(clippy::result_large_err)] // SPSC transfers inline commands without allocating an envelope.
    fn try_send(&self, command: Command) -> Result<(), Command> {
        self.maintain();
        {
            let mut local = self.local.borrow_mut();
            if !local.backlog.is_empty() {
                if local.backlog.len() == local.backlog.capacity() {
                    return Err(command);
                }
                self.stream
                    .shared
                    .pending_commands
                    .fetch_add(1, Ordering::Release);
                local.backlog.push_back(command);
            } else {
                self.stream
                    .shared
                    .pending_commands
                    .fetch_add(1, Ordering::Release);
                if let Err(command) = local.commands.push(command) {
                    local.backlog.push_back(command);
                }
            }
        }
        if let Some(mut reader) = self.stream.claim() {
            reader.engine.service(&reader.shared);
        }
        Ok(())
    }
    pub fn maintain(&self) {
        let mut local = self.local.borrow_mut();
        while let Some(retired) = local.retired.pop() {
            // Destruction happens here, on the producer/execution thread.
            match retired {
                Retired::Ring(v, s) => {
                    drop(v);
                    drop(s);
                }
                Retired::Owned(v) => drop(v),
                Retired::Pcm(v) => drop(v),
                Retired::Converter(v) => drop(v),
                Retired::Notices(v) => drop(v),
            }
        }
        while let Some(event) = local.events.pop() {
            local.pending_events.push_back(event);
        }
        while let Some(command) = local.backlog.pop_front() {
            if let Err(command) = local.commands.push(command) {
                local.backlog.push_front(command);
                break;
            }
        }
    }
    pub fn take_events(&self) -> Vec<AudioEvent> {
        self.maintain();
        self.local.borrow_mut().pending_events.drain(..).collect()
    }
    pub fn playback_clock(&self) -> Arc<PlaybackClock> {
        self.stream.shared.clock.clone()
    }
    pub fn sync_clock(&self, guest_us: u64) {
        self.local.borrow_mut().guest_us = guest_us;
        self.maintain();
        if let Some(mut reader) = self.stream.claim() {
            if !reader.shared.clock.uses_device_time() {
                reader.engine.advance_virtual(guest_us, &reader.shared);
            }
            reader.engine.service(&reader.shared);
        }
    }
    pub fn active(&self) -> bool {
        self.stream.active()
    }
    pub fn suspend(&self, suspended: bool) -> bool {
        self.stream.shared.clock.suspend_device(suspended);
        self.stream
            .shared
            .suspended
            .store(suspended, Ordering::Release);
        self.send(Command::Suspend(suspended))
    }
    pub fn add_ring(&self, source: u64, mut config: RingConfig) -> Result<Arc<RingState>, String> {
        let local = self.local.borrow();
        if !local.rings.contains_key(&source) && local.rings.len() + local.formats.len() >= SOURCES
        {
            return Err("audio source limit".into());
        }
        drop(local);
        config.output_rate = self.stream.shared.rate.load(Ordering::Acquire);
        let frequency = config.frequency;
        let align = u32::from(config.channels) * u32::from(config.bits) / 8;
        let voice = RingVoice::new(config)?;
        let state = Arc::new(RingState::default());
        if !self.send(Command::Ring(source, voice, state.clone())) {
            return Err("audio command limit".into());
        }
        self.local
            .borrow_mut()
            .rings
            .insert(source, (frequency, align));
        Ok(state)
    }
    pub fn play_ring(&self, source: u64, looping: bool, generation: u64) -> bool {
        let now = self
            .stream
            .shared
            .clock
            .now_us(self.local.borrow().guest_us);
        self.send(Command::Play(source, looping, generation, now))
    }
    pub fn change_generation(&self, source: u64, generation: u64) -> bool {
        self.send(Command::Epoch(source, generation))
    }
    pub fn stop_ring(&self, source: u64) -> bool {
        self.send(Command::Stop(source))
    }
    pub fn set_position(&self, source: u64, position: u32) -> bool {
        let now = self
            .stream
            .shared
            .clock
            .now_us(self.local.borrow().guest_us);
        self.send(Command::Position(source, position, now))
    }
    pub fn written(&self, source: u64, offset: u32, len: u32) -> bool {
        if let Some(&(_, align)) = self.local.borrow().rings.get(&source) {
            self.stream
                .shared
                .counters
                .queued_blocks
                .fetch_add(1, Ordering::Relaxed);
            self.stream
                .shared
                .counters
                .queued_source_frames
                .fetch_add(u64::from(len / align), Ordering::Relaxed);
        }
        self.send(Command::Written(source, offset, len))
    }
    pub fn set_notifications(&self, source: u64, entries: Vec<(u32, u32)>) -> bool {
        self.send(Command::Notices(source, entries))
    }
    pub fn set_frequency(&self, source: u64, frequency: u32) -> Result<(), String> {
        let converter = Converter::new(frequency, self.stream.shared.rate.load(Ordering::Acquire))?;
        if !self.send(Command::Retune(source, frequency, converter)) {
            return Err("audio command limit".into());
        }
        if let Some(rate) = self.local.borrow_mut().rings.get_mut(&source) {
            rate.0 = frequency;
        }
        Ok(())
    }
    pub fn set_gain(&self, source: u64, gain: f32) -> bool {
        self.set_stereo_gain(source, [gain; 2])
    }
    pub fn set_stereo_gain(&self, source: u64, gain: [f32; 2]) -> bool {
        self.send(Command::Gain(source, gain))
    }
    pub fn pause_pcm(&self, source: u64, paused: bool, _guest_us: u64) -> bool {
        self.send(Command::Pause(source, paused))
    }
    pub fn end_pcm(&self, source: u64) -> bool {
        self.send(Command::End(source))
    }
    pub fn flush_pcm(&self, source: u64) -> bool {
        if !self.send(Command::Remove(source)) {
            return false;
        }
        self.local.borrow_mut().formats.remove(&source);
        self.stream
            .shared
            .counters
            .flushes
            .fetch_add(1, Ordering::Relaxed);
        true
    }
    pub fn remove_pcm(&self, source: u64) -> bool {
        if !self.send(Command::Remove(source)) {
            return false;
        }
        self.local.borrow_mut().formats.remove(&source);
        self.local.borrow_mut().rings.remove(&source);
        true
    }
    pub fn push_pcm(&self, mut block: PcmBlock) -> Result<(), PcmBlock> {
        if block.sample_rate == 0
            || block.channels == 0
            || block.channels > 8
            || block.samples.is_empty()
            || !block
                .samples
                .len()
                .is_multiple_of(usize::from(block.channels))
        {
            return Err(block);
        }
        let duration = OwnedVoice::duration(&block);
        let frames = block.samples.len() as u64 / u64::from(block.channels);
        if duration > 2_000_000 {
            return Err(block);
        }
        let guest_now = self.local.borrow().guest_us;
        if self.stream.shared.clock.uses_device_time() {
            block.guest_pts_us = self
                .stream
                .shared
                .clock
                .now_us(guest_now)
                .saturating_add(block.guest_pts_us.saturating_sub(guest_now));
        }
        let source = block.stream_id;
        let existing = self.local.borrow().formats.get(&source).cloned();
        let command = if let Some((frequency, state)) = existing {
            if frequency != block.sample_rate
                || state.queued_blocks.load(Ordering::Acquire) >= 128
                || state.queued_us.load(Ordering::Relaxed) + duration > 2_000_000
            {
                return Err(block);
            }
            state.queued_us.fetch_add(duration, Ordering::Relaxed);
            state.queued_blocks.fetch_add(1, Ordering::Release);
            Command::Pcm(block)
        } else {
            if self.local.borrow().formats.len() + self.local.borrow().rings.len() >= SOURCES {
                return Err(block);
            }
            let frequency = block.sample_rate;
            let state = Arc::new(SourceState::default());
            state.queued_us.store(duration, Ordering::Relaxed);
            state.queued_blocks.store(1, Ordering::Relaxed);
            let voice = match OwnedVoice::new(
                state.clone(),
                self.stream.shared.rate.load(Ordering::Acquire),
                block,
            ) {
                Ok(voice) => voice,
                Err((_, block)) => return Err(block),
            };
            self.local
                .borrow_mut()
                .formats
                .insert(source, (frequency, state));
            Command::Owned(source, voice)
        };
        let new_source = matches!(&command, Command::Owned(..));
        if let Err(command) = self.try_send(command) {
            return match command {
                Command::Pcm(block) => {
                    if let Some((_, state)) = self.local.borrow().formats.get(&source) {
                        state.queued_us.fetch_sub(duration, Ordering::Relaxed);
                        state.queued_blocks.fetch_sub(1, Ordering::Release);
                    }
                    Err(block)
                }
                Command::Owned(_, mut voice) => {
                    self.local.borrow_mut().formats.remove(&source);
                    Err(voice.take_initial())
                }
                _ => unreachable!("PCM command"),
            };
        }
        if new_source {
            self.stream
                .shared
                .counters
                .playback_starts
                .fetch_add(1, Ordering::Relaxed);
        }
        self.stream
            .shared
            .counters
            .queued_blocks
            .fetch_add(1, Ordering::Relaxed);
        self.stream
            .shared
            .counters
            .queued_source_frames
            .fetch_add(frames, Ordering::Relaxed);
        self.stream
            .shared
            .counters
            .max_stream_queue_us
            .fetch_max(duration, Ordering::Relaxed);
        Ok(())
    }
}

pub(crate) struct Engine {
    commands: Consumer<Command>,
    events: Producer<AudioEvent>,
    retired: Producer<Retired>,
    voices: Vec<Option<Voice>>,
    suspended: bool,
    cursor_us: f64,
    virtual_us: u64,
    rate: u32,
    channels: u16,
    notification_retry: Option<AudioEvent>,
}
impl Engine {
    fn service(&mut self, shared: &Shared) {
        if let Some(event) = self.notification_retry.take() {
            if let Err(event) = self.events.push(event) {
                self.notification_retry = Some(event);
                return;
            }
            if let Some(waker) = shared.waker.get() {
                waker.signal();
            }
        }
        let mut processed = 0;
        while !self.retired.is_full() && self.notification_retry.is_none() {
            let Some(command) = self.commands.pop() else {
                break;
            };
            processed += 1;
            let source = match &command {
                Command::Ring(id, ..)
                | Command::Owned(id, ..)
                | Command::Play(id, ..)
                | Command::Epoch(id, ..)
                | Command::Stop(id)
                | Command::Position(id, ..)
                | Command::Written(id, ..)
                | Command::Gain(id, ..)
                | Command::Retune(id, ..)
                | Command::Notices(id, ..)
                | Command::Pause(id, ..)
                | Command::End(id)
                | Command::Remove(id) => Some(*id),
                Command::Pcm(b) => Some(b.stream_id),
                _ => None,
            };
            let at = source.and_then(|id| {
                self.voices
                    .iter()
                    .position(|v| v.as_ref().is_some_and(|v| v.source() == id))
            });
            match command {
                Command::Ring(source, voice, state) => self.insert(
                    Voice::Ring {
                        source,
                        voice,
                        state,
                        generation: 0,
                    },
                    at,
                ),
                Command::Owned(source, voice) => self.insert(Voice::Owned { source, voice }, at),
                Command::Pcm(block) => {
                    if let Some(Voice::Owned { voice, .. }) =
                        at.and_then(|i| self.voices[i].as_mut())
                    {
                        if let Err(block) = voice.push(block) {
                            voice.return_credit(&block);
                            self.retire(Retired::Pcm(block));
                        }
                    } else {
                        self.retire(Retired::Pcm(block));
                    }
                }
                Command::Remove(source) => {
                    if let Some(i) = at {
                        let old = self.voices[i].take().unwrap().retire();
                        self.retire(old);
                        shared.counters.removals.fetch_add(1, Ordering::Relaxed);
                    }
                    let event = AudioEvent::Released { source };
                    if let Err(event) = self.events.push(event) {
                        self.notification_retry = Some(event);
                    }
                    if let Some(waker) = shared.waker.get() {
                        waker.signal();
                    }
                }
                Command::Suspend(suspended) => self.suspended = suspended,
                command => {
                    if let Some(voice) = at.and_then(|i| self.voices[i].as_mut()) {
                        match (voice, command) {
                            (
                                Voice::Ring {
                                    voice, generation, ..
                                },
                                Command::Play(_, looping, epoch, issued),
                            ) => {
                                *generation = epoch;
                                voice.play(looping);
                                if shared.clock.uses_device_time()
                                    && !shared.connected.load(Ordering::Acquire)
                                {
                                    voice.detach(issued as f64);
                                }
                                shared
                                    .counters
                                    .playback_starts
                                    .fetch_add(1, Ordering::Relaxed);
                            }
                            (Voice::Ring { generation, .. }, Command::Epoch(_, epoch)) => {
                                *generation = epoch
                            }
                            (Voice::Ring { voice, .. }, Command::Stop(_)) => voice.stop(),
                            (Voice::Ring { voice, .. }, Command::Position(_, position, issued)) => {
                                voice.set_position(position);
                                if shared.clock.uses_device_time()
                                    && !shared.connected.load(Ordering::Acquire)
                                {
                                    voice.detach(issued as f64);
                                }
                            }
                            (Voice::Ring { voice, .. }, Command::Written(_, offset, len)) => {
                                voice.written(offset, len)
                            }
                            (Voice::Ring { voice, .. }, Command::Notices(_, entries)) => {
                                let old = voice.set_notifications(entries);
                                self.retire(Retired::Notices(old));
                            }
                            (Voice::Ring { voice, .. }, Command::Gain(_, gain)) => {
                                voice.set_gain(gain)
                            }
                            (Voice::Owned { voice, .. }, Command::Gain(_, gain)) => {
                                voice.set_gain(gain)
                            }
                            (Voice::Owned { voice, .. }, Command::Pause(_, paused)) => {
                                voice.pause(paused, self.cursor_us)
                            }
                            (Voice::Owned { voice, .. }, Command::End(_)) => {
                                voice.ended = true;
                                shared.counters.ends.fetch_add(1, Ordering::Relaxed);
                            }
                            (
                                Voice::Ring { voice, .. },
                                Command::Retune(_, frequency, converter),
                            ) => {
                                let old = voice.retune(frequency, converter);
                                self.retire(Retired::Converter(old));
                            }
                            (
                                Voice::Owned { voice, .. },
                                Command::Retune(_, frequency, converter),
                            ) => {
                                let old = voice.retune(frequency, converter);
                                self.retire(Retired::Converter(old));
                            }
                            (_, Command::Notices(_, entries)) => {
                                self.retire(Retired::Notices(entries))
                            }
                            _ => {}
                        }
                    } else {
                        match command {
                            Command::Retune(_, _, converter) => {
                                self.retire(Retired::Converter(converter))
                            }
                            Command::Notices(_, entries) => self.retire(Retired::Notices(entries)),
                            _ => {}
                        }
                    }
                }
            }
        }
        let active = self.voices.iter().flatten().filter(|v| v.active()).count() as u64;
        shared.desired_active.store(active, Ordering::Release);
        shared
            .counters
            .active_sources
            .store(active, Ordering::Relaxed);
        shared
            .pending_commands
            .fetch_sub(processed, Ordering::Release);
    }
    fn retire(&mut self, value: Retired) {
        let result = self.retired.push(value);
        debug_assert!(result.is_ok());
    }
    fn insert(&mut self, voice: Voice, at: Option<usize>) {
        let at = at.or_else(|| self.voices.iter().position(Option::is_none));
        if let Some(i) = at {
            if let Some(old) = self.voices[i].replace(voice) {
                self.retire(old.retire());
            }
        } else {
            self.retire(voice.retire());
        }
    }
    fn prepare(&mut self, rate: u32, channels: u16, new_device_epoch: bool) -> Result<(), String> {
        // The physical backend owns one reader per stream, so reacquiring after
        // device time starts means a new consumed-frame epoch. Reuse its kernel
        // and buffers, but reset history just as a fresh converter did. Virtual
        // pulls retain history only while the output configuration is unchanged.
        let reset = new_device_epoch || channels != self.channels;
        for voice in self.voices.iter_mut().flatten() {
            match voice {
                Voice::Ring { voice, .. } => voice.prepare(rate, reset)?,
                Voice::Owned { voice, .. } => voice.prepare(rate, reset)?,
            }
        }
        self.rate = rate;
        self.channels = channels;
        Ok(())
    }
    fn advance_virtual(&mut self, now: u64, shared: &Shared) {
        let delta = now.saturating_sub(self.cursor_us as u64);
        self.virtual_us = now;
        if delta == 0 {
            return;
        }
        if !self.voices.iter().flatten().any(Voice::active) {
            self.cursor_us = now as f64;
            return;
        }
        let mut scratch = [0.0f32; 512];
        let mut frames = delta * u64::from(self.rate) / 1_000_000;
        while frames > 0 {
            let count = frames.min(256) as usize;
            self.render(&mut scratch[..count * 2], 2, shared);
            frames -= count as u64;
        }
        self.cursor_us = now as f64;
    }
    fn render(&mut self, out: &mut [f32], channels: u16, shared: &Shared) {
        out.fill(0.0);
        if self.suspended {
            return;
        }
        let first_pts = self.cursor_us;
        let active_before = self.voices.iter().flatten().any(Voice::active);
        let step = 1_000_000.0 / f64::from(self.rate);
        let mut audible = 0;
        for (chunk, output) in out.chunks_mut(256 * usize::from(channels)).enumerate() {
            let pts = first_pts + (chunk * 256) as f64 * step;
            let mut missing = [0u64; 4];
            for voice in self.voices.iter_mut().flatten() {
                match voice {
                    Voice::Ring {
                        source,
                        voice,
                        state,
                        generation,
                    } => {
                        let before = voice.missing_frames;
                        voice.render(output, channels, self.rate, &mut missing);
                        shared
                            .counters
                            .missing_stream_frames
                            .fetch_add(voice.missing_frames - before, Ordering::Relaxed);
                        audible += u64::from(voice.audible);
                        let due = pts + output.len() as f64 / f64::from(channels) * step;
                        state.sequence.fetch_add(1, Ordering::AcqRel);
                        std::sync::atomic::fence(Ordering::Release);
                        state
                            .position
                            .store(voice.total_position(), Ordering::Relaxed);
                        state
                            .minimum_position
                            .store(voice.minimum_position(), Ordering::Relaxed);
                        state.pts.store(due as u64, Ordering::Relaxed);
                        state.frequency.store(voice.frequency(), Ordering::Relaxed);
                        state.bytes.store(voice.bytes, Ordering::Relaxed);
                        state.align.store(voice.alignment(), Ordering::Relaxed);
                        state.playing.store(voice.playing, Ordering::Relaxed);
                        state.looping.store(voice.looping, Ordering::Relaxed);
                        state.generation.store(*generation, Ordering::Relaxed);
                        state.sequence.fetch_add(1, Ordering::Release);
                        let mut sent = false;
                        voice.notifications(|event| {
                            let success = self
                                .events
                                .push(AudioEvent::Notification {
                                    source: *source,
                                    generation: *generation,
                                    event,
                                    due_us: due as u64,
                                })
                                .is_ok();
                            sent |= success;
                            success
                        });
                        if sent && let Some(waker) = shared.waker.get() {
                            waker.signal();
                        }
                    }
                    Voice::Owned { voice, .. } => {
                        let count = voice.render(
                            output,
                            self.rate,
                            channels,
                            pts,
                            &mut self.retired,
                            &mut missing,
                        );
                        shared
                            .counters
                            .missing_stream_frames
                            .fetch_add(count, Ordering::Relaxed);
                    }
                }
            }
            shared.counters.missing_output_frames.fetch_add(
                missing
                    .iter()
                    .map(|b| u64::from(b.count_ones()))
                    .sum::<u64>(),
                Ordering::Relaxed,
            );
        }
        for sample in out.iter_mut() {
            *sample = if sample.is_finite() {
                sample.clamp(-1.0, 1.0)
            } else {
                0.0
            };
        }
        shared
            .counters
            .output_samples
            .fetch_add(out.len() as u64, Ordering::Relaxed);
        shared.counters.nonzero_samples.fetch_add(
            out.iter().filter(|value| value.abs() > 0.00001).count() as u64,
            Ordering::Relaxed,
        );
        self.cursor_us += out.len() as f64 / f64::from(channels) * step;
        let active = self
            .voices
            .iter()
            .flatten()
            .filter(|voice| voice.active())
            .count() as u64;
        shared.desired_active.store(active, Ordering::Release);
        shared
            .counters
            .active_sources
            .store(active, Ordering::Relaxed);
        if active_before && shared.clock.uses_device_time() {
            shared
                .active_until_us
                .store(self.cursor_us as u64, Ordering::Release);
        }
        shared
            .counters
            .audible_sources
            .store(audible, Ordering::Relaxed);
    }
}

impl crate::AudioStream {
    pub fn rendered_samples(&self) -> (u64, u64) {
        (
            self.shared.counters.output_samples.load(Ordering::Relaxed),
            self.shared.counters.nonzero_samples.load(Ordering::Relaxed),
        )
    }
    pub fn set_waker(&self, waker: WakeEvent) -> Result<(), WakeEvent> {
        self.shared.waker.set(waker)
    }
    pub fn metrics(&self) -> PcmMetrics {
        let c = &self.shared.counters;
        PcmMetrics {
            queued_blocks: c.queued_blocks.load(Ordering::Relaxed),
            queued_source_frames: c.queued_source_frames.load(Ordering::Relaxed),
            playback_starts: c.playback_starts.load(Ordering::Relaxed),
            flushes: c.flushes.load(Ordering::Relaxed),
            ends: c.ends.load(Ordering::Relaxed),
            removals: c.removals.load(Ordering::Relaxed),
            max_stream_queue_us: c.max_stream_queue_us.load(Ordering::Relaxed),
            missing_stream_frames: c.missing_stream_frames.load(Ordering::Relaxed),
            missing_output_frames: c.missing_output_frames.load(Ordering::Relaxed),
        }
    }
    pub fn active(&self) -> bool {
        !self.shared.suspended.load(Ordering::Acquire)
            && (self.shared.pending_commands.load(Ordering::Acquire) != 0
                || self.shared.desired_active.load(Ordering::Acquire) != 0
                || self.shared.clock.now_us(0)
                    < self.shared.active_until_us.load(Ordering::Acquire))
    }
    fn claim(&self) -> Option<crate::DeviceReader> {
        let ptr = self
            .shared
            .engine
            .swap(std::ptr::null_mut(), Ordering::AcqRel);
        if ptr.is_null() {
            return None;
        }
        Some(crate::DeviceReader {
            engine: std::mem::ManuallyDrop::new(unsafe { Box::from_raw(ptr) }),
            shared: self.shared.clone(),
            channels: 2,
            device_time: false,
            device_origin_us: 0.0,
            native_clock: false,
        })
    }
    pub fn device_reader(&self, rate: u32, channels: u16) -> Result<crate::DeviceReader, String> {
        if rate == 0 || channels == 0 || channels > 8 {
            return Err("invalid device format".into());
        }
        let mut reader = self.claim().ok_or("audio consumer already owned")?;
        if self.shared.clock.uses_device_time() {
            let now = self.shared.clock.now_us(reader.engine.cursor_us as u64) as f64;
            let delta = (now - reader.engine.cursor_us).max(0.0);
            for voice in reader.engine.voices.iter_mut().flatten() {
                match voice {
                    Voice::Ring { voice, .. } => voice.reanchor(now),
                    Voice::Owned { voice, .. } => {
                        if voice.paused {
                            voice.shift_timeline(delta);
                        }
                    }
                }
            }
            reader.engine.cursor_us = now;
        }
        reader.engine.service(&self.shared);
        reader
            .engine
            .prepare(rate, channels, self.shared.clock.uses_device_time())?;
        self.shared.rate.store(rate, Ordering::Release);
        reader.channels = channels;
        reader.device_origin_us = reader.engine.cursor_us;
        reader.device_time = self
            .shared
            .clock
            .start_device(reader.engine.cursor_us as u64);
        if reader.device_time {
            self.shared.connected.store(true, Ordering::Release);
        }
        Ok(reader)
    }
    pub fn read_stereo(&self, out: &mut [f32], rate: u32) {
        self.read_interleaved(out, rate, 2);
    }
    pub fn read_interleaved(&self, out: &mut [f32], rate: u32, channels: u16) {
        out.fill(0.0);
        if let Ok(mut reader) = self.device_reader(rate, channels) {
            reader.read(out, 0);
        }
    }
}
impl crate::DeviceReader {
    /// Native DAC consumption and mixer write position are separate. Padding
    /// is the existing queue before this callback, not its writable frame count.
    pub fn read_with_clock(
        &mut self,
        out: &mut [f32],
        delay_us: u64,
        clock: Option<crate::DeviceClock>,
    ) -> crate::DeviceRead {
        if let Some(clock) = clock {
            let consumed = self.device_origin_us
                + clock.consumed_frames as f64 * 1_000_000.0 / f64::from(self.engine.rate);
            if !self.native_clock {
                self.engine.cursor_us = consumed
                    + f64::from(clock.queued_frames) * 1_000_000.0 / f64::from(self.engine.rate);
                self.native_clock = true;
            }
            if self.device_time {
                self.shared.clock.publish_device(consumed as u64, 0);
            }
        }
        self.read(out, delay_us)
    }
    pub fn read(&mut self, out: &mut [f32], delay_us: u64) -> crate::DeviceRead {
        let first = self.engine.cursor_us as u64;
        if !out.len().is_multiple_of(usize::from(self.channels)) {
            out.fill(0.0);
            return crate::DeviceRead {
                first_pts_us: first,
                lock_miss: false,
                missing_output_frames: self
                    .shared
                    .counters
                    .missing_output_frames
                    .load(Ordering::Relaxed),
                missing_stream_frames: self
                    .shared
                    .counters
                    .missing_stream_frames
                    .load(Ordering::Relaxed),
            };
        }
        if self.device_time && !self.native_clock {
            self.shared.clock.publish_device(first, delay_us);
        }
        self.engine.service(&self.shared);
        self.engine.render(out, self.channels, &self.shared);
        crate::DeviceRead {
            first_pts_us: first,
            lock_miss: false,
            missing_output_frames: self
                .shared
                .counters
                .missing_output_frames
                .load(Ordering::Relaxed),
            missing_stream_frames: self
                .shared
                .counters
                .missing_stream_frames
                .load(Ordering::Relaxed),
        }
    }
}
impl Drop for crate::DeviceReader {
    fn drop(&mut self) {
        if self.device_time {
            let pts = self.engine.cursor_us;
            for voice in self.engine.voices.iter_mut().flatten() {
                if let Voice::Ring { voice, .. } = voice {
                    voice.detach(pts);
                }
            }
            self.shared.connected.store(false, Ordering::Release);
        }
        // Move the existing allocation, without allocating or dropping Engine.
        let engine = unsafe { std::mem::ManuallyDrop::take(&mut self.engine) };
        self.shared
            .engine
            .store(Box::into_raw(engine), Ordering::Release);
    }
}

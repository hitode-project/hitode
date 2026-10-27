//! Audio-owned DMA voice state. The only shared data is a byte-atomic read view.
use crate::resample::Converter;
use adv_sync::shared::SharedReadView;
use std::sync::Arc;

pub struct RingConfig {
    pub view: Arc<dyn SharedReadView>,
    pub bytes: u32,
    pub channels: u16,
    pub bits: u16,
    pub frequency: u32,
    pub output_rate: u32,
    pub notifications: Vec<(u32, u32)>,
}
#[derive(Clone, Copy, Default)]
struct Written {
    generation: u32,
    turn: u32,
    mask: u64,
}
#[derive(Clone, Copy, Default)]
struct Coverage {
    epochs: [Written; 2],
    pending: u64,
}
impl Coverage {
    fn mark(&mut self, generation: u32, turn: u32, mask: u64) {
        if let Some(epoch) = self
            .epochs
            .iter_mut()
            .find(|epoch| epoch.generation == generation && epoch.turn == turn)
        {
            epoch.mask |= mask;
            return;
        }
        let at = if self.epochs[0].generation != generation {
            0
        } else if self.epochs[1].generation != generation {
            1
        } else if self.epochs[0].turn <= self.epochs[1].turn {
            0
        } else {
            1
        };
        self.epochs[at] = Written {
            generation,
            turn,
            mask,
        };
    }
    fn covers(&self, generation: u32, turn: u32, bit: u32) -> bool {
        self.epochs.iter().any(|epoch| {
            epoch.generation == generation && epoch.turn >= turn && epoch.mask & (1 << bit) != 0
        })
    }
}
pub struct RingVoice {
    pub(crate) lead: crate::actor::Lead,
    view: Arc<dyn SharedReadView>,
    pub bytes: u32,
    channels: u16,
    bits: u16,
    frequency: u32,
    converter: Converter,
    phase: u64,
    minimum_position: u64,
    detached_at: Option<f64>,
    read_frame: u64,
    pub playing: bool,
    draining: u32,
    pub looping: bool,
    generation: u64,
    written: Vec<Coverage>,
    streaming: bool,
    initial_partial: bool,
    notifications: Vec<(u32, u32)>,
    pending_notices: [u64; 16],
    gain: [f32; 2],
    target_gain: [f32; 2],
    gain_frames: u32,
    pub missing_frames: u64,
    pub audible: bool,
}

impl RingVoice {
    pub fn new(config: RingConfig) -> Result<Box<Self>, String> {
        let align = u32::from(config.channels) * u32::from(config.bits) / 8;
        if !matches!(config.channels, 1 | 2)
            || !matches!(config.bits, 8 | 16)
            || config.bytes == 0
            || config.bytes as usize > config.view.len()
            || !config.bytes.is_multiple_of(align)
            || config.notifications.len() > 1024
            || config
                .notifications
                .iter()
                .any(|&(offset, _)| offset != u32::MAX && offset >= config.bytes)
        {
            return Err("invalid shared PCM voice".into());
        }
        let frames = config.bytes / align;
        Ok(Box::new(Self {
            lead: Default::default(),
            view: config.view,
            bytes: config.bytes,
            channels: config.channels,
            bits: config.bits,
            frequency: config.frequency,
            converter: Converter::new(config.frequency, config.output_rate)?,
            phase: 0,
            minimum_position: 0,
            detached_at: None,
            read_frame: 0,
            playing: false,
            draining: 0,
            looping: false,
            generation: 0,
            written: vec![Coverage::default(); frames.div_ceil(64) as usize],
            streaming: false,
            initial_partial: false,
            notifications: config.notifications,
            pending_notices: [0; 16],
            gain: [1.0; 2],
            target_gain: [1.0; 2],
            gain_frames: 0,
            missing_frames: 0,
            audible: false,
        }))
    }
    fn align(&self) -> u32 {
        u32::from(self.channels) * u32::from(self.bits) / 8
    }
    fn frames(&self) -> u64 {
        u64::from(self.bytes / self.align())
    }
    pub fn position(&self) -> u32 {
        (((self.phase >> 32) % self.frames()) * u64::from(self.align())) as u32
    }
    pub fn total_position(&self) -> u64 {
        (self.phase >> 32) * u64::from(self.align())
    }
    pub fn minimum_position(&self) -> u64 {
        self.minimum_position
    }
    pub(crate) fn detach(&mut self, pts: f64) {
        if self.playing && self.detached_at.is_none() {
            self.detached_at = Some(pts);
        }
    }
    pub(crate) fn reanchor(&mut self, now: f64) {
        if let Some(at) = self.detached_at.take() {
            let first = self.phase >> 32;
            let delta = (now - at) * f64::from(self.frequency) * 4294967296.0 / 1_000_000.0;
            let minimum = (self.minimum_position / u64::from(self.align())) << 32;
            self.phase = ((self.phase as f64 + delta).max(minimum as f64)) as u64;
            self.crossed_notifications(first, self.phase >> 32);
            self.read_frame = self.phase >> 32;
            self.converter.reset();
        }
    }
    /// Contiguous coverage from the playback phase, bounded to one ring traversal.
    pub(crate) fn lead_us(&self) -> Option<f64> {
        if !self.streaming || !self.playing {
            return None;
        }
        let total = self.frames();
        let first = self.phase >> 32;
        let limit = if self.looping {
            total
        } else {
            total.saturating_sub(first)
        };
        let mut covered = 0;
        while covered < limit {
            let frame = first + covered;
            let position = frame % total;
            let bit = (position % 64) as u32;
            let run = (64 - u64::from(bit))
                .min(total - position)
                .min(limit - covered);
            let mask = self.written[position as usize / 64]
                .epochs
                .iter()
                .filter(|e| {
                    e.generation == self.generation as u32 && e.turn >= (frame / total) as u32
                })
                .fold(0, |mask, e| mask | e.mask);
            let available = u64::from((mask >> bit).trailing_ones()).min(run);
            covered += available;
            if available < run {
                break;
            }
        }
        let fraction = (self.phase & 0xffff_ffff) as f64 / 4294967296.0;
        Some((covered as f64 - fraction).max(0.0) * 1_000_000.0 / f64::from(self.frequency))
    }
    pub fn output_rate(&self) -> u32 {
        self.converter.rate
    }
    pub fn frequency(&self) -> u32 {
        self.frequency
    }
    pub(crate) fn prepare(&mut self, rate: u32, reset: bool) -> Result<(), String> {
        if !self.converter.matches(self.frequency, rate) {
            let converter = Converter::new(self.frequency, rate)?;
            let _ = self.retune(self.frequency, converter);
        } else if reset {
            self.read_frame = self.phase >> 32;
            self.converter.reset();
        }
        Ok(())
    }
    pub fn alignment(&self) -> u32 {
        self.align()
    }
    pub fn active(&self) -> bool {
        self.playing || self.draining != 0
    }
    pub fn set_notifications(&mut self, entries: Vec<(u32, u32)>) -> Vec<(u32, u32)> {
        self.pending_notices.fill(0);
        std::mem::replace(&mut self.notifications, entries)
    }
    pub(crate) fn retune(&mut self, frequency: u32, converter: Converter) -> Converter {
        self.frequency = frequency;
        self.read_frame = self.phase >> 32;
        std::mem::replace(&mut self.converter, converter)
    }
    pub fn set_position(&mut self, bytes: u32) {
        self.detached_at = None;
        self.draining = 0;
        self.pending_notices.fill(0);
        let frame = u64::from(bytes / self.align());
        self.phase = frame << 32;
        self.minimum_position = frame * u64::from(self.align());
        self.read_frame = frame;
        self.converter.reset();
    }
    pub fn play(&mut self, looping: bool) {
        if !self.playing {
            self.minimum_position = self.total_position();
        }
        self.draining = 0;
        if !self.playing && self.written.iter().any(|cell| cell.pending != 0) {
            self.generation = (self.generation + 1) % u64::from(u32::MAX);
            let frames = self.frames();
            self.initial_partial = self.written.iter().enumerate().any(|(index, cell)| {
                let length = (frames - index as u64 * 64).min(64);
                let full = if length == 64 {
                    u64::MAX
                } else {
                    (1 << length) - 1
                };
                cell.pending != full
            });
            for cell in &mut self.written {
                if cell.pending != 0 {
                    cell.mark(self.generation as u32, 0, cell.pending);
                    cell.pending = 0;
                }
            }
        }
        self.looping = looping;
        self.playing = true;
    }
    pub fn stop(&mut self) {
        self.detached_at = None;
        self.playing = false;
        self.draining = 0;
        self.audible = false;
    }
    pub fn set_gain(&mut self, gain: [f32; 2]) {
        self.target_gain = gain;
        self.gain_frames = (self.converter.rate / 200).max(1);
    }
    /// Called at a successful Lock, including outstanding locks whose contents
    /// remain hardware-visible. Full preload stays a static loop; streaming is
    /// learned from partial preload, refill notifications and an active refill.
    pub fn written(&mut self, offset: u32, len: u32) {
        if len == 0 {
            return;
        }
        if self.playing && self.initial_partial && self.notifications.len() >= 2 && len < self.bytes
        {
            self.streaming = true;
        }
        let mut frame = u64::from(offset / self.align());
        let mut count = u64::from(len / self.align());
        let total = self.frames();
        let current = self.phase >> 32;
        while count != 0 {
            frame %= total;
            let bit = (frame % 64) as u32;
            let turn = current / total + u64::from(frame < current % total);
            let mut run = (64 - u64::from(bit)).min(count).min(total - frame);
            if self.playing && frame < current % total {
                run = run.min(current % total - frame);
            }
            let mask = if run == 64 {
                u64::MAX
            } else {
                ((1 << run) - 1) << bit
            };
            let cell = &mut self.written[frame as usize / 64];
            if self.playing {
                cell.mark(self.generation as u32, turn as u32, mask);
            } else {
                cell.pending |= mask;
            }
            frame += run;
            count -= run;
        }
    }
    #[allow(clippy::too_many_arguments)]
    fn read(
        view: &dyn SharedReadView,
        bytes: u32,
        channels: u16,
        bits: u16,
        looping: bool,
        streaming: bool,
        generation: u64,
        written: &[Coverage],
        frame: u64,
    ) -> [f32; 2] {
        let align = u32::from(channels) * u32::from(bits) / 8;
        let frames = u64::from(bytes / align);
        if !looping && frame >= frames {
            return [0.0; 2];
        }
        let position = frame % frames;
        let cell = written[position as usize / 64];
        if streaming
            && !cell.covers(
                generation as u32,
                (frame / frames) as u32,
                (position % 64) as u32,
            )
        {
            return [0.0; 2];
        }
        let at = position as usize * align as usize;
        let sample = |channel: usize| {
            let at = at + channel.min(usize::from(channels) - 1) * usize::from(bits) / 8;
            if bits == 8 {
                (f32::from(view.load_u8(at)) - 128.0) / 128.0
            } else {
                f32::from(i16::from_le_bytes([view.load_u8(at), view.load_u8(at + 1)])) / 32768.0
            }
        };
        [sample(0), sample(1)]
    }
    pub fn render(&mut self, out: &mut [f32], channels: u16, rate: u32, missing: &mut [u64]) {
        self.audible = false;
        if !self.active() || channels == 0 || rate != self.converter.rate {
            return;
        }
        let first = self.phase >> 32;
        let step = (u64::from(self.frequency) << 32) / u64::from(rate);
        for (index, output) in out.chunks_exact_mut(usize::from(channels)).enumerate() {
            let frame = self.phase >> 32;
            if self.playing && !self.looping && frame >= self.frames() {
                self.playing = false;
                self.draining = self.converter.tail_frames();
                for (i, &(offset, _)) in self.notifications.iter().enumerate() {
                    if offset == u32::MAX {
                        self.pending_notices[i / 64] |= 1 << (i % 64);
                    }
                }
            }
            if !self.active() {
                break;
            }
            let position = frame % self.frames();
            let cell = self.written[position as usize / 64];
            let absent = self.playing
                && self.streaming
                && !cell.covers(
                    self.generation as u32,
                    (frame / self.frames()) as u32,
                    (position % 64) as u32,
                );
            self.missing_frames += u64::from(absent);
            if absent {
                missing[index / 64] |= 1 << (index % 64);
            }
            let sample = self.converter.next(|| {
                let sample = Self::read(
                    self.view.as_ref(),
                    self.bytes,
                    self.channels,
                    self.bits,
                    self.looping,
                    self.streaming,
                    self.generation,
                    &self.written,
                    self.read_frame,
                );
                self.read_frame += 1;
                sample
            });
            for (gain, target) in self.gain.iter_mut().zip(self.target_gain) {
                if self.gain_frames != 0 {
                    *gain += (target - *gain) / self.gain_frames as f32;
                }
            }
            self.gain_frames = self.gain_frames.saturating_sub(1);
            let sample = [sample[0] * self.gain[0], sample[1] * self.gain[1]];
            self.audible |= sample.iter().any(|value| value.abs() > 0.00001);
            if channels == 1 {
                output[0] += (sample[0] + sample[1]) * 0.5;
            } else {
                output[0] += sample[0];
                output[1] += sample[1];
            }
            self.phase = self.phase.wrapping_add(step);
            if !self.playing {
                self.draining = self.draining.saturating_sub(1);
            }
        }
        self.crossed_notifications(first, self.phase >> 32);
    }
    fn crossed_notifications(&mut self, first: u64, last: u64) {
        let total = self.frames();
        for (i, &(offset, _)) in self.notifications.iter().enumerate() {
            if offset == u32::MAX || (!self.looping && first >= total) {
                continue;
            }
            let offset = u64::from(offset / self.align());
            let occurrence = first / total * total + offset;
            let occurrence = if occurrence < first {
                occurrence + total
            } else {
                occurrence
            };
            if occurrence < last {
                self.pending_notices[i / 64] |= 1 << (i % 64);
            }
        }
    }
    /// Returns pending notifications without allocation. A saturated event queue
    /// leaves the bit set so a later callback can retry instead of losing it.
    pub fn notifications(&mut self, mut send: impl FnMut(u32) -> bool) {
        for (word, bits) in self.pending_notices.iter_mut().enumerate() {
            while *bits != 0 {
                let bit = bits.trailing_zeros() as usize;
                if !send(self.notifications[word * 64 + bit].1) {
                    return;
                }
                *bits &= !(1 << bit);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Silence;
    impl SharedReadView for Silence {
        fn len(&self) -> usize {
            131
        }
        fn load_u8(&self, _: usize) -> u8 {
            128
        }
    }

    // spec: DEV-AUDIO-2
    #[test]
    fn device_reanchoring_preserves_start_and_crossed_refill_notifications() {
        let mut voice = RingVoice::new(RingConfig {
            view: Arc::new(Silence),
            bytes: 131,
            channels: 1,
            bits: 8,
            frequency: 1000,
            output_rate: 1000,
            notifications: vec![(0, 1), (64, 2), (130, 3)],
        })
        .unwrap();
        voice.written(0, 131);
        voice.play(true);
        voice.detach(1_000_000.0);
        voice.reanchor(1_120_000.0);
        let mut sent = Vec::new();
        voice.notifications(|event| {
            sent.push(event);
            true
        });
        assert_eq!(sent, [1, 2]);
        sent.clear();
        voice.render(&mut [0.0; 20], 1, 1000, &mut [0]);
        voice.notifications(|event| {
            sent.push(event);
            true
        });
        assert_eq!(sent, [1, 3]);
    }

    // spec: MEDIA-AUDIO-4
    #[test]
    fn lead_matches_frame_coverage_across_wrap_generations_and_partial_words() {
        let mut voice = RingVoice::new(RingConfig {
            view: Arc::new(Silence),
            bytes: 131,
            channels: 1,
            bits: 8,
            frequency: 1000,
            output_rate: 1000,
            notifications: vec![],
        })
        .unwrap();
        voice.playing = true;
        voice.streaming = true;
        voice.generation = 7;
        for (cell, mask) in voice
            .written
            .iter_mut()
            .zip([u64::MAX, u64::MAX ^ (1 << 20), 7])
        {
            cell.epochs = [
                Written {
                    generation: 7,
                    turn: 2,
                    mask,
                },
                Written {
                    generation: 7,
                    turn: 3,
                    mask: 0xffff,
                },
            ];
        }
        for looping in [false, true] {
            voice.looping = looping;
            for first in 0..524 {
                voice.phase = (first << 32) | 0x4000_0000;
                let limit = if looping {
                    131
                } else {
                    131u64.saturating_sub(first)
                };
                let count = (0..limit)
                    .take_while(|n| {
                        let frame = first + n;
                        let pos = frame % 131;
                        voice.written[pos as usize / 64].covers(
                            7,
                            (frame / 131) as u32,
                            (pos % 64) as u32,
                        )
                    })
                    .count();
                assert_eq!(
                    voice.lead_us(),
                    Some((count as f64 - 0.25).max(0.0) * 1000.0)
                );
            }
        }
        voice.generation = 8;
        assert_eq!(voice.lead_us(), Some(0.0));
        voice.playing = false;
        assert_eq!(voice.lead_us(), None);
    }
}

//! Audio-owned DMA voice state. The only shared data is a byte-atomic read view.
//! Playing rings stage up to 30 ms (capped at half the ring) of source-rate PCM
//! ahead of playback so the write cursor leads the play cursor without exposing
//! uncommitted guest bytes or rereading guest storage across `SetFrequency`.
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
    staged: Vec<[f32; 2]>,
    staged_valid: Vec<u64>,
    staged_missing: Vec<u64>,
    staged_start: u64,
    staged_end: u64,
    converter_frame: u64,
    pub playing: bool,
    draining: u32,
    pub looping: bool,
    generation: u64,
    written: Vec<Coverage>,
    committed: Vec<Coverage>,
    partial_commits: bool,
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
        Self::with_staging(config, true)
    }
    pub(crate) fn with_staging(config: RingConfig, stage_lead: bool) -> Result<Box<Self>, String> {
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
        let max_lead = if stage_lead {
            Self::staged_lead_frames(u64::from(frames), config.frequency.max(200_000)) as usize
        } else {
            0
        };
        let staged_cap = if max_lead == 0 {
            0
        } else {
            max_lead.next_power_of_two()
        };
        let coverage_words = frames.div_ceil(64) as usize;
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
            staged: vec![[0.0; 2]; staged_cap],
            staged_valid: vec![0; staged_cap.div_ceil(64)],
            staged_missing: vec![0; staged_cap.div_ceil(64)],
            staged_start: 0,
            staged_end: 0,
            converter_frame: 0,
            playing: false,
            draining: 0,
            looping: false,
            generation: 0,
            written: vec![Coverage::default(); coverage_words],
            committed: vec![Coverage::default(); coverage_words],
            partial_commits: false,
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
    /// Source frames staged ahead of playback: 30 ms capped at half the ring.
    pub const fn staged_lead_frames(frames: u64, frequency: u32) -> u64 {
        let half = frames / 2;
        if half == 0 {
            return 0;
        }
        let wanted = frequency as u64 * 30 / 1_000;
        let wanted = if wanted == 0 { 1 } else { wanted };
        if wanted < half { wanted } else { half }
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
    /// Monotonic source byte frontier committed into host staging and never reread.
    pub(crate) fn total_write_position(&self) -> u64 {
        if self.playing && !self.staged.is_empty() {
            self.staged_end.max(self.phase >> 32) * u64::from(self.align())
        } else {
            self.total_position()
        }
    }
    pub fn minimum_position(&self) -> u64 {
        self.minimum_position
    }
    fn clear_staged(&mut self, frame: u64) {
        self.staged_start = frame;
        self.staged_end = frame;
        self.converter_frame = frame;
        self.staged_valid.fill(0);
        self.staged_missing.fill(0);
    }
    fn is_staged_valid(&self, frame: u64) -> bool {
        if self.staged.is_empty() || frame < self.staged_start || frame >= self.staged_end {
            return false;
        }
        let idx = (frame as usize) & (self.staged.len() - 1);
        (self.staged_valid[idx / 64] >> (idx % 64)) & 1 != 0
    }
    fn is_staged_missing(&self, frame: u64) -> bool {
        if !self.is_staged_valid(frame) {
            return false;
        }
        let idx = (frame as usize) & (self.staged.len() - 1);
        (self.staged_missing[idx / 64] >> (idx % 64)) & 1 != 0
    }
    fn can_freeze(&self, frame: u64) -> bool {
        if !self.partial_commits {
            return true;
        }
        let total = self.frames();
        let pos = frame % total;
        self.committed[pos as usize / 64].covers(
            self.generation as u32,
            (frame / total) as u32,
            (pos % 64) as u32,
        )
    }
    fn try_freeze_frame(&mut self, frame: u64) {
        let idx = (frame as usize) & (self.staged.len() - 1);
        let word = idx / 64;
        let bit = 1u64 << (idx % 64);
        if self.staged_valid[word] & bit != 0 || !self.can_freeze(frame) {
            return;
        }
        let (sample, absent) = Self::read_checked(
            self.view.as_ref(),
            self.bytes,
            self.channels,
            self.bits,
            self.looping,
            self.streaming,
            self.generation,
            &self.written,
            frame,
        );
        self.staged[idx] = sample;
        self.staged_valid[word] |= bit;
        if absent {
            self.staged_missing[word] |= bit;
        } else {
            self.staged_missing[word] &= !bit;
        }
    }
    fn refresh_staged(&mut self) {
        if !self.playing || self.staged.is_empty() {
            return;
        }
        for frame in self.staged_start..self.staged_end {
            self.try_freeze_frame(frame);
        }
    }
    fn stage_ahead(&mut self) {
        let play_frame = self.phase >> 32;
        if !self.staged.is_empty() {
            let mask = self.staged.len() - 1;
            while self.staged_start < play_frame && self.staged_start < self.staged_end {
                let idx = (self.staged_start as usize) & mask;
                let bit = 1u64 << (idx % 64);
                self.staged_valid[idx / 64] &= !bit;
                self.staged_missing[idx / 64] &= !bit;
                self.staged_start += 1;
            }
        }
        if self.staged_start < play_frame {
            self.staged_start = play_frame;
        }
        if self.staged_end < self.staged_start {
            self.staged_end = self.staged_start;
        }
        if self.converter_frame < self.staged_start {
            self.converter_frame = self.staged_start;
        }
        if !self.playing || self.staged.is_empty() || (!self.looping && play_frame >= self.frames())
        {
            return;
        }
        let lead = Self::staged_lead_frames(self.frames(), self.frequency);
        let mut target = (play_frame + lead).min(self.staged_start + self.staged.len() as u64);
        if !self.looping {
            target = target.min(self.frames());
        }
        while self.staged_end < target {
            let frame = self.staged_end;
            self.staged_end += 1;
            self.try_freeze_frame(frame);
        }
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
            self.clear_staged(self.phase >> 32);
            self.converter.reset();
            self.stage_ahead();
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
            self.converter_frame = self.phase >> 32;
            self.converter.reset();
            self.stage_ahead();
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
        self.converter_frame = self.phase >> 32;
        let old = std::mem::replace(&mut self.converter, converter);
        self.stage_ahead();
        old
    }
    pub fn set_position(&mut self, bytes: u32) {
        self.detached_at = None;
        self.draining = 0;
        self.pending_notices.fill(0);
        let frame = u64::from(bytes / self.align());
        self.phase = frame << 32;
        self.minimum_position = frame * u64::from(self.align());
        self.clear_staged(frame);
        self.converter.reset();
        self.stage_ahead();
    }
    pub fn play(&mut self, looping: bool) {
        let was_playing = self.playing;
        if !was_playing {
            self.minimum_position = self.total_position();
        }
        self.draining = 0;
        if !was_playing && self.written.iter().any(|cell| cell.pending != 0) {
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
            let committed_full = self.committed.iter().enumerate().all(|(index, cell)| {
                let length = (frames - index as u64 * 64).min(64);
                let full = if length == 64 {
                    u64::MAX
                } else {
                    (1 << length) - 1
                };
                cell.pending == full
            });
            self.partial_commits = !committed_full;
            let generation = self.generation as u32;
            for cell in &mut self.written {
                if cell.pending != 0 {
                    cell.mark(generation, 0, cell.pending);
                    cell.pending = 0;
                }
            }
            for cell in &mut self.committed {
                if cell.pending != 0 {
                    cell.mark(generation, 0, cell.pending);
                    cell.pending = 0;
                }
            }
        }
        if was_playing && self.looping != looping && self.staged_end > self.frames() {
            let boundary = self.frames().max(self.staged_start);
            if looping {
                if !self.staged.is_empty() {
                    let mask = self.staged.len() - 1;
                    for frame in boundary..self.staged_end {
                        let idx = (frame as usize) & mask;
                        let bit = 1u64 << (idx % 64);
                        self.staged_valid[idx / 64] &= !bit;
                        self.staged_missing[idx / 64] &= !bit;
                    }
                }
                self.staged_end = self.staged_end.min(boundary);
                self.converter_frame = self.converter_frame.min(self.staged_end);
            } else if !self.staged.is_empty() {
                let mask = self.staged.len() - 1;
                for frame in boundary..self.staged_end {
                    let idx = (frame as usize) & mask;
                    let bit = 1u64 << (idx % 64);
                    self.staged[idx] = [0.0; 2];
                    self.staged_valid[idx / 64] |= bit;
                    self.staged_missing[idx / 64] &= !bit;
                }
            }
        }
        self.looping = looping;
        self.playing = true;
        if !was_playing {
            self.clear_staged(self.phase >> 32);
        }
        self.stage_ahead();
    }
    pub fn stop(&mut self) {
        self.detached_at = None;
        self.playing = false;
        self.draining = 0;
        self.audible = false;
        self.clear_staged(self.phase >> 32);
        self.converter.reset();
    }
    pub fn set_gain(&mut self, gain: [f32; 2]) {
        self.target_gain = gain;
        self.gain_frames = (self.converter.rate / 200).max(1);
    }
    #[allow(clippy::too_many_arguments)]
    fn mark_coverage(
        cells: &mut [Coverage],
        staged_valid: &[u64],
        staged_len: usize,
        staged_start: u64,
        staged_end: u64,
        playing: bool,
        generation: u32,
        current: u64,
        total: u64,
        mut frame: u64,
        mut count: u64,
        check_cells: bool,
    ) {
        let cur_turn = current / total;
        let cur_rem = current % total;
        while count != 0 {
            frame %= total;
            let bit = (frame % 64) as u32;
            if !playing {
                let run = (64 - u64::from(bit)).min(count).min(total - frame);
                let mask = if run == 64 {
                    u64::MAX
                } else {
                    ((1 << run) - 1) << bit
                };
                cells[frame as usize / 64].pending |= mask;
                frame += run;
                count -= run;
                continue;
            }
            let base_frame = cur_turn * total + frame;
            if frame < cur_rem {
                let run = (64 - u64::from(bit))
                    .min(count)
                    .min(total - frame)
                    .min(cur_rem - frame);
                let mask = if run == 64 {
                    u64::MAX
                } else {
                    ((1 << run) - 1) << bit
                };
                cells[frame as usize / 64].mark(generation, (cur_turn + 1) as u32, mask);
                frame += run;
                count -= run;
            } else if base_frame < staged_end {
                let is_valid = staged_len != 0
                    && base_frame >= staged_start
                    && ((staged_valid[((base_frame as usize) & (staged_len - 1)) / 64]
                        >> (((base_frame as usize) & (staged_len - 1)) % 64))
                        & 1
                        != 0);
                let is_covered = check_cells
                    && cells[frame as usize / 64].covers(generation, cur_turn as u32, bit);
                let turn = cur_turn + u64::from(is_valid || is_covered);
                cells[frame as usize / 64].mark(generation, turn as u32, 1u64 << bit);
                frame += 1;
                count -= 1;
            } else {
                let run = (64 - u64::from(bit)).min(count).min(total - frame);
                let mask = if run == 64 {
                    u64::MAX
                } else {
                    ((1 << run) - 1) << bit
                };
                cells[frame as usize / 64].mark(generation, cur_turn as u32, mask);
                frame += run;
                count -= run;
            }
        }
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
        let frame = u64::from(offset / self.align());
        let count = u64::from(len / self.align());
        let total = self.frames();
        Self::mark_coverage(
            &mut self.written,
            &self.staged_valid,
            self.staged.len(),
            self.staged_start,
            self.staged_end,
            self.playing,
            self.generation as u32,
            self.phase >> 32,
            total,
            frame,
            count,
            true,
        );
    }
    /// Called at Unlock once guest PCM bytes are written into shared storage,
    /// freezing newly committed frames inside the active staging window.
    pub fn committed(&mut self, offset: u32, len: u32) {
        if len == 0 {
            return;
        }
        if self.playing && len < self.bytes {
            self.partial_commits = true;
        }
        let frame = u64::from(offset / self.align());
        let count = u64::from(len / self.align());
        let total = self.frames();
        Self::mark_coverage(
            &mut self.committed,
            &self.staged_valid,
            self.staged.len(),
            self.staged_start,
            self.staged_end,
            self.playing,
            self.generation as u32,
            self.phase >> 32,
            total,
            frame,
            count,
            false,
        );
        self.refresh_staged();
    }
    #[allow(clippy::too_many_arguments)]
    fn read_checked(
        view: &dyn SharedReadView,
        bytes: u32,
        channels: u16,
        bits: u16,
        looping: bool,
        streaming: bool,
        generation: u64,
        written: &[Coverage],
        frame: u64,
    ) -> ([f32; 2], bool) {
        let align = u32::from(channels) * u32::from(bits) / 8;
        let frames = u64::from(bytes / align);
        if !looping && frame >= frames {
            return ([0.0; 2], false);
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
            return ([0.0; 2], true);
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
        ([sample(0), sample(1)], false)
    }
    pub fn render(
        &mut self,
        out: &mut [f32],
        channels: u16,
        rate: u32,
        missing: &mut [u64],
    ) -> usize {
        self.audible = false;
        if !self.active() || channels == 0 || rate != self.converter.rate {
            return 0;
        }
        self.stage_ahead();
        let first = self.phase >> 32;
        let step = (u64::from(self.frequency) << 32) / u64::from(rate);
        let mut played_frames = 0;
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
            self.stage_ahead();
            let position = frame % self.frames();
            let cell = self.written[position as usize / 64];
            let covered = cell.covers(
                self.generation as u32,
                (frame / self.frames()) as u32,
                (position % 64) as u32,
            );
            let absent =
                self.playing && self.streaming && (!covered || self.is_staged_missing(frame));
            self.missing_frames += u64::from(absent);
            if absent {
                missing[index / 64] |= 1 << (index % 64);
            }
            let total_frames = self.frames();
            let sample = self.converter.next(|| {
                let source_frame = self.converter_frame;
                self.converter_frame += 1;
                let staged_valid = !self.staged.is_empty()
                    && source_frame >= self.staged_start
                    && source_frame < self.staged_end
                    && {
                        let idx = (source_frame as usize) & (self.staged.len() - 1);
                        (self.staged_valid[idx / 64] >> (idx % 64)) & 1 != 0
                    };
                if staged_valid {
                    if self.streaming {
                        let pos = source_frame % total_frames;
                        let ok = self.written[pos as usize / 64].covers(
                            self.generation as u32,
                            (source_frame / total_frames) as u32,
                            (pos % 64) as u32,
                        );
                        if !ok {
                            return [0.0; 2];
                        }
                    }
                    let idx = (source_frame as usize) & (self.staged.len() - 1);
                    self.staged[idx]
                } else {
                    Self::read_checked(
                        self.view.as_ref(),
                        self.bytes,
                        self.channels,
                        self.bits,
                        self.looping,
                        self.streaming,
                        self.generation,
                        &self.written,
                        source_frame,
                    )
                    .0
                }
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
            if self.playing {
                played_frames += 1;
                self.phase = self.phase.wrapping_add(step);
            } else {
                self.draining = self.draining.saturating_sub(1);
            }
        }
        self.stage_ahead();
        self.crossed_notifications(first, self.phase >> 32);
        played_frames
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

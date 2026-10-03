//! Bounded owned PCM source for host decoders and movies. Buffers are retired to
//! the execution thread; their allocation and destruction never run in a callback.
use crate::{
    actor::{Retired, SourceState},
    mixer::PcmBlock,
    resample::Converter,
};
use adv_sync::spsc::Producer;
use std::{
    collections::VecDeque,
    sync::{Arc, atomic::Ordering},
};

// Keep timeline precision separately from the public integer guest timestamp.
// Overlap only changes this view; callback code never shifts or destroys PCM.
struct Queued {
    pcm: PcmBlock,
    pts: f64,
    first_frame: usize,
}
impl Queued {
    fn frames(&self) -> usize {
        self.pcm.samples.len() / usize::from(self.pcm.channels) - self.first_frame
    }
    fn end(&self) -> f64 {
        self.pts + self.frames() as f64 * 1_000_000.0 / f64::from(self.pcm.sample_rate)
    }
}
pub(crate) struct OwnedVoice {
    queue: VecDeque<Queued>,
    pub state: Arc<SourceState>,
    converter: Converter,
    frequency: u32,
    read_pts: f64,
    initialized: bool,
    pub paused: bool,
    paused_at: Option<f64>,
    pub ended: bool,
    gain: [f32; 2],
    target: [f32; 2],
    gain_frames: u32,
    first_pts: Option<f64>,
    timeline_end: f64,
    rendered_until: f64,
}
impl OwnedVoice {
    pub fn new(
        state: Arc<SourceState>,
        rate: u32,
        block: PcmBlock,
    ) -> Result<Box<Self>, (String, PcmBlock)> {
        let frequency = block.sample_rate;
        let converter = match Converter::new(frequency, rate) {
            Ok(converter) => converter,
            Err(error) => return Err((error, block)),
        };
        let mut queue = VecDeque::with_capacity(128);
        let first_pts = Some(block.guest_pts_us as f64);
        let queued = Queued {
            pts: block.guest_pts_us as f64,
            first_frame: 0,
            pcm: block,
        };
        let timeline_end = queued.end();
        queue.push_back(queued);
        Ok(Box::new(Self {
            queue,
            state,
            converter,
            frequency,
            read_pts: 0.0,
            initialized: false,
            paused: false,
            paused_at: None,
            ended: false,
            gain: [1.0; 2],
            target: [1.0; 2],
            gain_frames: 0,
            first_pts,
            timeline_end,
            rendered_until: 0.0,
        }))
    }
    pub fn frequency(&self) -> u32 {
        self.frequency
    }
    pub fn retune(&mut self, frequency: u32, converter: Converter) -> Converter {
        self.frequency = frequency;
        self.initialized = false;
        std::mem::replace(&mut self.converter, converter)
    }
    pub fn push(&mut self, block: PcmBlock) -> Result<(), PcmBlock> {
        if self.queue.len() == self.queue.capacity() {
            return Err(block);
        }
        let mut pts = block.guest_pts_us as f64;
        let mut first_frame = 0;
        // Host media decoders commonly expose millisecond PTS. Preserve the
        // historical two-ms rounding tolerance without accumulating per-packet
        // rounding drift. Larger overlaps keep only the unqueued suffix.
        if (pts - self.timeline_end).abs() <= 2_000.0 {
            pts = self.timeline_end;
        } else if pts < self.timeline_end {
            first_frame = ((self.timeline_end - pts) * f64::from(block.sample_rate) / 1_000_000.0)
                .round() as usize;
            if first_frame >= block.samples.len() / usize::from(block.channels) {
                return Err(block);
            }
            pts = self.timeline_end;
        }
        self.first_pts.get_or_insert(pts);
        self.ended = false;
        let queued = Queued {
            pcm: block,
            pts,
            first_frame,
        };
        self.timeline_end = queued.end();
        self.queue.push_back(queued);
        Ok(())
    }
    pub fn set_gain(&mut self, gain: [f32; 2]) {
        self.target = gain;
        self.gain_frames = (self.converter.rate / 200).max(1);
    }
    pub fn active(&self) -> bool {
        !self.paused
            && self.first_pts.is_some()
            && (!self.queue.is_empty()
                || !self.ended
                || self.rendered_until
                    < self.timeline_end
                        + f64::from(self.converter.tail_frames()) * 1_000_000.0
                            / f64::from(self.converter.rate))
    }
    pub fn shift_timeline(&mut self, delta: f64) {
        for block in &mut self.queue {
            block.pts += delta;
        }
        if let Some(first) = self.first_pts.as_mut() {
            *first += delta;
        }
        self.timeline_end += delta;
        self.rendered_until += delta;
        self.read_pts += delta;
        if let Some(at) = self.paused_at.as_mut() {
            *at += delta;
        }
    }
    pub fn pause(&mut self, paused: bool, now: f64) {
        if paused && !self.paused {
            self.paused_at = Some(now);
        } else if !paused && let Some(at) = self.paused_at.take() {
            self.shift_timeline((now - at).max(0.0));
        }
        self.paused = paused;
    }
    pub(crate) fn take_initial(&mut self) -> PcmBlock {
        self.queue.pop_front().expect("prepared initial PCM").pcm
    }
    pub(crate) fn return_credit(&self, block: &PcmBlock) {
        self.state
            .queued_us
            .fetch_sub(Self::duration(block), Ordering::Relaxed);
        self.state.queued_blocks.fetch_sub(1, Ordering::Release);
    }
    fn sample(queue: &VecDeque<Queued>, pts: f64) -> [f32; 2] {
        let Some(block) = queue
            .iter()
            .find(|block| pts + 0.000001 >= block.pts && pts + 0.000001 < block.end())
        else {
            return [0.0; 2];
        };
        let frame = (((pts - block.pts).max(0.0) * f64::from(block.pcm.sample_rate) / 1_000_000.0)
            .round() as usize)
            .min(block.frames() - 1)
            + block.first_frame;
        let channels = usize::from(block.pcm.channels);
        let at = frame * channels;
        let sample = |channel: usize| {
            let value = block.pcm.samples[at + channel.min(channels - 1)];
            if value.is_finite() { value } else { 0.0 }
        };
        [sample(0), sample(1)]
    }
    pub fn duration(block: &PcmBlock) -> u64 {
        block.samples.len() as u64 / u64::from(block.channels) * 1_000_000
            / u64::from(block.sample_rate)
    }
    pub fn render(
        &mut self,
        out: &mut [f32],
        rate: u32,
        channels: u16,
        pts: f64,
        retired: &mut Producer<Retired>,
        missing: &mut [u64],
    ) -> u64 {
        if self.paused || rate != self.converter.rate {
            return 0;
        }
        if !self.initialized {
            self.read_pts = pts;
            self.initialized = true;
        }
        let mut missing_frames = 0;
        for (index, output) in out.chunks_exact_mut(usize::from(channels)).enumerate() {
            let time = pts + index as f64 * 1_000_000.0 / f64::from(rate);
            // Sinc input prefetch may pass an entire block before its output
            // interval is presented. Retire only after presentation passes it;
            // otherwise coverage metrics would report legitimate history missing.
            while self
                .queue
                .front()
                .is_some_and(|block| block.end() <= time + 0.000001)
                && !retired.is_full()
            {
                let block = self.queue.pop_front().unwrap().pcm;
                self.return_credit(&block);
                let result = retired.push(Retired::Pcm(block));
                debug_assert!(result.is_ok());
            }
            let covered = self
                .queue
                .iter()
                .any(|block| time + 0.000001 >= block.pts && time + 0.000001 < block.end());
            if !covered
                && self.first_pts.is_some_and(|first| time + 0.000001 >= first)
                && (!self.ended || time + 0.000001 < self.timeline_end)
            {
                missing_frames += 1;
                missing[index / 64] |= 1 << (index % 64);
            }
            let sample = self.converter.next(|| {
                let sample = Self::sample(&self.queue, self.read_pts);
                self.read_pts += 1_000_000.0 / f64::from(self.frequency);
                sample
            });
            for (gain, target) in self.gain.iter_mut().zip(self.target) {
                if self.gain_frames != 0 {
                    *gain += (target - *gain) / self.gain_frames as f32;
                }
            }
            self.gain_frames = self.gain_frames.saturating_sub(1);
            if channels == 1 {
                output[0] += (sample[0] * self.gain[0] + sample[1] * self.gain[1]) * 0.5;
            } else {
                output[0] += sample[0] * self.gain[0];
                output[1] += sample[1] * self.gain[1];
            }
        }
        self.rendered_until =
            pts + out.len() as f64 / f64::from(channels) * 1_000_000.0 / f64::from(rate);
        missing_frames
    }
}

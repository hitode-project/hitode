//! PCM transfer values and counters. The former queue mixer is retained
//! solely as a test reference; the production engine is the audio-owned actor.
/// Owned interleaved PCM, timestamped at its first frame in guest microseconds.
pub struct PcmBlock {
    pub stream_id: u64,
    pub guest_pts_us: u64,
    pub sample_rate: u32,
    pub channels: u16,
    pub samples: Vec<f32>,
}

/// Counters distinguish queued digital silence from absent PCM. Missing frames
/// use the requested output rate; queued frames use each block's source rate.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PcmMetrics {
    pub low_lead_events: u64,
    /// Lifetime minimum lead/refill interval, scaled by 1000; zero before observation.
    pub min_lead_ratio_permille: u64,
    pub queued_blocks: u64,
    pub queued_source_frames: u64,
    /// First accepted block of a new stream or after an explicit flush.
    pub playback_starts: u64,
    pub flushes: u64,
    pub ends: u64,
    pub removals: u64,
    /// Maximum total queued PCM duration for one owned stream.
    pub max_stream_queue_us: u64,
    /// Sum across streams lacking coverage after their first PTS. Pauses,
    /// flushed streams and the drained tail of ended streams are excluded.
    pub missing_stream_frames: u64,
    /// Output frames with at least one stream lacking PCM, even if another
    /// stream (for example background music) still provides samples.
    pub missing_output_frames: u64,
}

#[cfg(test)]
mod reference {
    use super::{PcmBlock, PcmMetrics};
    use std::collections::{BTreeMap, VecDeque};
    use std::sync::Arc;
    impl PcmBlock {
        fn duration(&self) -> u64 {
            self.samples.len() as u64 * 1_000_000
                / u64::from(self.channels)
                / u64::from(self.sample_rate)
        }
    }

    struct Stream {
        queue: VecDeque<Queued>,
        paused: Option<u64>,
        ended: bool,
        gain: f32,
        stereo_gain: [f32; 2],
        first_pts_us: Option<f64>,
    }
    struct Queued {
        pcm: PcmBlock,
        pts_us: f64,
    }
    impl std::ops::Deref for Queued {
        type Target = PcmBlock;
        fn deref(&self) -> &PcmBlock {
            &self.pcm
        }
    }
    impl Queued {
        fn end_us(&self) -> f64 {
            self.pts_us
                + self.samples.len() as f64 / f64::from(self.channels) * 1_000_000.0
                    / f64::from(self.sample_rate)
        }
    }
    impl Default for Stream {
        fn default() -> Self {
            Self {
                queue: VecDeque::new(),
                paused: None,
                ended: false,
                gain: 1.0,
                stereo_gain: [1.0; 2],
                first_pts_us: None,
            }
        }
    }

    /// Blocks this close to the previous block's end continue it: container timestamps are
    /// rounded (ASF/WMA to milliseconds), so consecutive packets meet only approximately.
    const CONTIGUOUS_US: f64 = 2_000.0;

    /// How long after its end queued PCM is kept (well beyond the 100 ms resync threshold).
    const STALE_US: f64 = 500_000.0;

    // Callback buffering and host scheduling introduce tens of milliseconds of phase
    // jitter. Correct sustained clock drift outside this window, without jumping PCM.
    const CLOCK_JITTER_US: f64 = 30_000.0;
    const CLOCK_FILTER_US: f64 = 500_000.0;
    const MAX_RATE_ADJUSTMENT: f64 = 0.002;

    #[derive(Default)]
    pub struct Mixer {
        clock: Arc<crate::clock::PlaybackClock>,
        streams: BTreeMap<u64, Stream>,
        guest_us: u64,
        device_us: f64,
        device_origin_us: f64,
        device_frames: u64,
        device_rate: u32,
        filtered_skew_us: f64,
        rate_adjustment: f64,
        initialized: bool,
        suspended: bool,
        /// Large resynchronizations only; normal callback jitter does not move the timeline.
        pub resyncs: u64,
        pub callbacks: u64,
        metrics: PcmMetrics,
    }

    impl Mixer {
        pub fn metrics(&self) -> PcmMetrics {
            self.metrics
        }
        /// Freeze device consumption while the owning session is paused.
        pub fn suspend(&mut self, suspended: bool) {
            self.suspended = suspended;
        }
        /// PAL gain changes affect queued PCM without flushing the device's latency window.
        pub fn set_gain(&mut self, id: u64, gain: f32) {
            if let Some(s) = self.streams.get_mut(&id) {
                s.gain = gain;
            }
        }
        /// Change channel gain on already queued PCM without discarding device-latency data.
        pub fn set_stereo_gain(&mut self, id: u64, gain: [f32; 2]) {
            if let Some(s) = self.streams.get_mut(&id) {
                s.stereo_gain = gain;
            }
        }
        pub fn sync_clock(&mut self, guest_us: u64) {
            if self.clock.uses_device_time() {
                self.guest_us = self.clock.now_us(guest_us);
                self.expire(self.guest_us);
                return;
            }
            self.expire(guest_us);
            let skew = self.device_us - guest_us as f64;
            if !self.initialized || skew.abs() > 100_000.0 {
                self.device_us = guest_us as f64;
                self.device_origin_us = self.device_us;
                self.device_frames = 0;
                self.filtered_skew_us = 0.0;
                self.rate_adjustment = 0.0;
                self.resyncs += u64::from(self.initialized);
                self.initialized = true;
            } else {
                let elapsed = guest_us.saturating_sub(self.guest_us) as f64;
                self.filtered_skew_us +=
                    (skew - self.filtered_skew_us) * elapsed / (CLOCK_FILTER_US + elapsed);
                let excess = (self.filtered_skew_us.abs() - CLOCK_JITTER_US).max(0.0)
                    * self.filtered_skew_us.signum();
                let adjustment =
                    (-excess / 10_000_000.0).clamp(-MAX_RATE_ADJUSTMENT, MAX_RATE_ADJUSTMENT);
                if adjustment != self.rate_adjustment {
                    // Rebase at the next sample, never at the guest clock: already consumed
                    // PCM cannot be replayed. Only the subsequent interpolation step changes.
                    self.device_origin_us = self.device_us;
                    self.device_frames = 0;
                    self.rate_adjustment = adjustment;
                }
            }
            self.guest_us = guest_us;
        }

        /// Device-only clock correction. It never changes guest-visible time or sound cursors.
        pub fn clock_adjustment_ppm(&self) -> f64 {
            self.rate_adjustment * 1_000_000.0
        }

        /// Drop PCM that ended [`STALE_US`] before `guest_us`: it can no longer be played, since a
        /// device that far behind resynchronizes to the guest clock, and with no device (headless
        /// runs) nothing else would drain it and producers would stall on backpressure.
        fn expire(&mut self, guest_us: u64) {
            let limit = guest_us as f64 - STALE_US;
            for s in self.streams.values_mut().filter(|s| s.paused.is_none()) {
                while s.queue.front().is_some_and(|b| b.end_us() < limit) {
                    s.queue.pop_front();
                }
            }
            self.streams.retain(|_, s| !s.ended || !s.queue.is_empty());
        }

        /// At most two seconds per stream and 256 streams. Rejection returns ownership. A block's
        /// timestamp is a guest time that has been reached, so older queued PCM expires first.
        pub fn push_pcm(&mut self, block: PcmBlock) -> Result<(), PcmBlock> {
            self.expire(block.guest_pts_us);
            if block.sample_rate == 0
                || block.channels == 0
                || block.channels > 8
                || !block.samples.len().is_multiple_of(block.channels as usize)
                || block.samples.is_empty()
                || block.duration() > 2_000_000
                || (!self.streams.contains_key(&block.stream_id) && self.streams.len() >= 256)
            {
                return Err(block);
            }
            let mut block = block;
            let s = self.streams.entry(block.stream_id).or_default();
            let mut pts_us = block.guest_pts_us as f64;
            if let Some(end) = s.queue.back().map(Queued::end_us) {
                if (pts_us - end).abs() <= CONTIGUOUS_US {
                    pts_us = end;
                } else if pts_us < end {
                    // Overlap beyond timestamp rounding: keep only what is not queued yet. Returning
                    // the block would not be backpressure; the producer would retry it forever.
                    let ch = usize::from(block.channels);
                    let frames = ((end - pts_us) * f64::from(block.sample_rate) / 1_000_000.0)
                        .round() as usize;
                    if frames * ch >= block.samples.len() {
                        return Ok(());
                    }
                    block.samples.drain(..frames * ch);
                    pts_us = end;
                }
            }
            let queue_us = s.queue.iter().map(|b| b.duration()).sum::<u64>() + block.duration();
            if queue_us > 2_000_000 {
                return Err(block);
            }
            self.metrics.queued_blocks += 1;
            self.metrics.queued_source_frames +=
                block.samples.len() as u64 / u64::from(block.channels);
            self.metrics.max_stream_queue_us = self.metrics.max_stream_queue_us.max(queue_us);
            if s.first_pts_us.is_none() {
                s.first_pts_us = Some(pts_us);
                self.metrics.playback_starts += 1;
            }
            s.ended = false;
            s.queue.push_back(Queued { pcm: block, pts_us });
            Ok(())
        }

        pub fn pause_pcm(&mut self, id: u64, paused: bool, guest_us: u64) {
            let s = self.streams.entry(id).or_default();
            if paused {
                s.paused.get_or_insert(guest_us);
            } else if let Some(at) = s.paused.take() {
                let elapsed = guest_us.saturating_sub(at);
                if let Some(pts) = s.first_pts_us.as_mut() {
                    *pts += elapsed as f64;
                }
                for b in &mut s.queue {
                    b.pcm.guest_pts_us = b.pcm.guest_pts_us.saturating_add(elapsed);
                    b.pts_us += elapsed as f64;
                }
            }
        }
        pub fn end_pcm(&mut self, id: u64) {
            if let Some(s) = self.streams.get_mut(&id) {
                self.metrics.ends += u64::from(!s.ended);
                s.ended = true;
            }
        }
        pub fn flush_pcm(&mut self, id: u64) {
            if let Some(s) = self.streams.get_mut(&id) {
                self.metrics.flushes += 1;
                s.queue.clear();
                s.ended = false;
                s.first_pts_us = None;
            }
        }
        pub fn remove_pcm(&mut self, id: u64) {
            self.metrics.removals += u64::from(self.streams.remove(&id).is_some());
        }
        pub fn active(&self) -> bool {
            !self.suspended
                && self
                    .streams
                    .values()
                    .any(|s| s.paused.is_none() && !s.queue.is_empty())
        }

        /// Linear interpolation; mono duplicates, stereo maps to L/R, extra output channels are silent.
        /// Caller uses try_lock: a producer can never block the real-time callback.
        pub fn mix(&mut self, out: &mut [f32], rate: u32, channels: u16) {
            out.fill(0.0);
            if self.suspended || rate == 0 || channels == 0 {
                return;
            }
            self.callbacks += 1;
            if self.device_rate != rate {
                self.device_origin_us = self.device_us;
                self.device_frames = 0;
                self.device_rate = rate;
            }
            let step_us = 1_000_000.0 / f64::from(rate) * (1.0 + self.rate_adjustment);
            const FRAMES: usize = 256;
            for (chunk, output) in out.chunks_mut(FRAMES * channels as usize).enumerate() {
                let first = self.device_frames + (chunk * FRAMES) as u64;
                let mut missing = [0u64; FRAMES / 64];
                for s in self.streams.values_mut().filter(|s| s.paused.is_none()) {
                    self.metrics.missing_stream_frames += mix_stream(
                        s,
                        output,
                        channels,
                        self.device_origin_us,
                        first,
                        step_us,
                        &mut missing,
                    );
                }
                self.metrics.missing_output_frames += missing
                    .iter()
                    .map(|word| u64::from(word.count_ones()))
                    .sum::<u64>();
            }
            for sample in out.iter_mut() {
                *sample = sample.clamp(-1.0, 1.0);
            }
            self.device_frames += out.len() as u64 / u64::from(channels);
            self.device_us = self.device_origin_us + self.device_frames as f64 * step_us;
            self.streams.retain(|_, s| !s.ended || !s.queue.is_empty());
        }
    }

    /// Work on a contiguous packet run. Source-rate division, queue traversal and
    /// gain setup occur at packet boundaries; the inner loop only advances phase.
    fn mix_stream(
        s: &mut Stream,
        out: &mut [f32],
        channels: u16,
        origin_us: f64,
        first_frame: u64,
        step_us: f64,
        missing: &mut [u64],
    ) -> u64 {
        const EPS: f64 = 0.000001;
        let channels = usize::from(channels);
        let frames = out.len() / channels;
        let gains = [s.gain * s.stereo_gain[0], s.gain * s.stereo_gain[1]];
        let mut at = 0;
        let mut missing_count = 0;
        while at < frames {
            let time = origin_us + (first_frame + at as u64) as f64 * step_us;
            while s.queue.front().is_some_and(|b| b.end_us() <= time + EPS) {
                s.queue.pop_front();
            }
            let unavailable = s.queue.front().is_none_or(|b| time + EPS < b.pts_us);
            if unavailable {
                let end = s.queue.front().map_or(frames, |b| {
                    (at + ((b.pts_us - time - EPS) / step_us).ceil().max(1.0) as usize).min(frames)
                });
                if let Some(first) = s.first_pts_us
                    && (!s.ended || !s.queue.is_empty())
                {
                    let begin =
                        (at + ((first - time - EPS) / step_us).ceil().max(0.0) as usize).min(end);
                    for i in begin..end {
                        missing[i / 64] |= 1 << (i % 64);
                    }
                    missing_count += (end - begin) as u64;
                }
                at = end;
                continue;
            }
            let b = s.queue.front().unwrap();
            let end =
                (at + ((b.end_us() - time - EPS) / step_us).ceil().max(1.0) as usize).min(frames);
            let source_channels = usize::from(b.channels);
            let count = b.samples.len() / source_channels;
            let mut phase = (time - b.pts_us).max(0.0) * f64::from(b.sample_rate) / 1_000_000.0;
            let phase_step = step_us * f64::from(b.sample_rate) / 1_000_000.0;
            let next = s.queue.get(1).filter(|next| {
                next.channels == b.channels
                    && next.sample_rate == b.sample_rate
                    && (next.pts_us - b.end_us()).abs() <= 1.0
            });
            for (i, frame) in out[at * channels..end * channels]
                .chunks_exact_mut(channels)
                .enumerate()
            {
                // Rebase near integral source positions, where an ulp can select
                // the neighboring sample. This also preserves non-finite handling
                // at packet boundaries without scanning PCM on the callback.
                let fraction = phase.fract();
                if !(0.00000001..=0.99999999).contains(&fraction) {
                    let time = origin_us + (first_frame + (at + i) as u64) as f64 * step_us;
                    phase = (time - b.pts_us).max(0.0) * f64::from(b.sample_rate) / 1_000_000.0;
                }
                // Repeated phase addition can round the final position a few ulps
                // past the boundary; packet selection already bounded this run.
                let n = (phase as usize).min(count - 1);
                let fraction = (phase - n as f64).clamp(0.0, 1.0) as f32;
                {
                    let sample = |channel: usize| {
                        let source = channel.min(source_channels - 1);
                        let a = b.samples[n * source_channels + source];
                        let z = if n + 1 < count {
                            b.samples[(n + 1) * source_channels + source]
                        } else {
                            next.map_or(a, |next| next.samples[source])
                        };
                        let value = a + (z - a) * fraction;
                        if value.is_finite() {
                            value * gains[channel]
                        } else {
                            0.0
                        }
                    };
                    if channels == 1 {
                        frame[0] += (sample(0) + sample(1)) * 0.5;
                    } else {
                        frame[0] += sample(0);
                        frame[1] += sample(1);
                    }
                }
                phase += phase_step;
            }
            at = end;
        }
        missing_count
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        impl Mixer {
            fn reference_mix(&mut self, out: &mut [f32], rate: u32, channels: u16) {
                out.fill(0.0);
                if self.suspended || rate == 0 || channels == 0 {
                    return;
                }
                self.callbacks += 1;
                if self.device_rate != rate {
                    self.device_origin_us = self.device_us;
                    self.device_frames = 0;
                    self.device_rate = rate;
                }
                let step_us = 1_000_000.0 / f64::from(rate) * (1.0 + self.rate_adjustment);
                for (i, frame) in out.chunks_exact_mut(channels as usize).enumerate() {
                    let time =
                        self.device_origin_us + (self.device_frames + i as u64) as f64 * step_us;
                    let mut missing = false;
                    for s in self.streams.values_mut().filter(|s| s.paused.is_none()) {
                        while s
                            .queue
                            .front()
                            .is_some_and(|b| b.end_us() <= time + 0.000001)
                        {
                            s.queue.pop_front();
                        }
                        let unavailable =
                            s.queue.front().is_none_or(|b| time + 0.000001 < b.pts_us);
                        if unavailable {
                            if s.first_pts_us.is_some_and(|pts| time + 0.000001 >= pts)
                                && (!s.ended || !s.queue.is_empty())
                            {
                                self.metrics.missing_stream_frames += 1;
                                missing = true;
                            }
                            continue;
                        }
                        let b = s.queue.front().unwrap();
                        let pos =
                            (time - b.pts_us).max(0.0) * f64::from(b.sample_rate) / 1_000_000.0;
                        let n = pos as usize;
                        let count = b.samples.len() / b.channels as usize;
                        if n >= count {
                            continue;
                        }
                        let sample = |c: usize| {
                            let channel_gain = s.stereo_gain[c.min(1)];
                            let c = c.min(b.channels as usize - 1);
                            let a = b.samples[n * b.channels as usize + c];
                            let z = if n + 1 < count {
                                b.samples[(n + 1) * b.channels as usize + c]
                            } else {
                                s.queue
                                    .get(1)
                                    .filter(|next| {
                                        next.channels == b.channels
                                            && next.sample_rate == b.sample_rate
                                            && (next.pts_us - b.end_us()).abs() <= 1.0
                                    })
                                    .and_then(|next| next.samples.get(c))
                                    .copied()
                                    .unwrap_or(a)
                            };
                            let v = a + (z - a) * pos.fract() as f32;
                            if v.is_finite() {
                                v * s.gain * channel_gain
                            } else {
                                0.0
                            }
                        };
                        if channels == 1 {
                            frame[0] += (sample(0) + sample(1)) * 0.5;
                        } else {
                            frame[0] += sample(0);
                            frame[1] += sample(1);
                        }
                    }
                    self.metrics.missing_output_frames += u64::from(missing);
                    for sample in frame {
                        *sample = sample.clamp(-1.0, 1.0);
                    }
                }
                self.device_frames += out.len() as u64 / u64::from(channels);
                self.device_us = self.device_origin_us + self.device_frames as f64 * step_us;
                self.streams.retain(|_, s| !s.ended || !s.queue.is_empty());
            }
        }

        fn block(id: u64, pts: u64, samples: Vec<f32>) -> PcmBlock {
            PcmBlock {
                stream_id: id,
                guest_pts_us: pts,
                sample_rate: 1000,
                channels: 1,
                samples,
            }
        }
        #[test]
        fn block_mix_matches_prior_frame_algorithm_across_packet_and_rate_boundaries() {
            let fixture = |selected: Option<u64>| {
                let mut m = Mixer::default();
                m.sync_clock(0);
                for id in 0..7u64 {
                    if selected.is_some_and(|selected| selected != id) {
                        continue;
                    }
                    let rate = [22050, 44100, 48000][id as usize % 3];
                    let channels = 1 + (id % 2) as u16;
                    for packet in 0..18u64 {
                        let frames = rate as usize / 100;
                        let mut samples: Vec<_> = (0..frames * channels as usize)
                            .map(|i| {
                                ((i + packet as usize * frames) as f32 * (id as f32 + 0.5) * 0.017)
                                    .sin()
                                    * 0.3
                            })
                            .collect();
                        if packet == 2 {
                            samples[0] = f32::NAN;
                        }
                        let pts = packet * 10_000
                            + if id % 3 == 0 && packet >= 6 {
                                8000
                            } else {
                                id * 997
                            };
                        assert!(
                            m.push_pcm(PcmBlock {
                                stream_id: id,
                                guest_pts_us: pts,
                                sample_rate: rate,
                                channels,
                                samples
                            })
                            .is_ok()
                        );
                    }
                    m.set_gain(id, 0.7);
                    m.set_stereo_gain(id, [0.9, 0.3]);
                }
                m.pause_pcm(5, true, 0);
                m.end_pcm(6);
                m
            };
            for selected in (0..7).map(Some).chain(std::iter::once(None)) {
                let (mut block, mut reference) = (fixture(selected), fixture(selected));
                let mut maximum = 0.0f32;
                for callback in 0..80 {
                    let rate = [48000, 44100, 32000][callback / 27 % 3];
                    let channels = 1 + callback % 3;
                    let frames = 93 + callback * 13 % 601;
                    let mut actual = vec![0.0; frames * channels];
                    let mut expected = actual.clone();
                    block.mix(&mut actual, rate, channels as u16);
                    reference.reference_mix(&mut expected, rate, channels as u16);
                    for (sample, (a, b)) in actual.iter().zip(expected).enumerate() {
                        assert!(
                            (a - b).abs() < 0.000002,
                            "selected={selected:?} callback={callback} sample={sample} rate={rate} channels={channels} actual={a} expected={b}"
                        );
                        maximum = maximum.max((a - b).abs());
                    }
                    assert_eq!(
                        block.metrics(),
                        reference.metrics(),
                        "coverage at callback {callback}"
                    );
                    if callback == 5 {
                        for m in [&mut block, &mut reference] {
                            m.pause_pcm(5, false, 30_000);
                        }
                    }
                    if callback == 12 {
                        for m in [&mut block, &mut reference] {
                            m.flush_pcm(3);
                        }
                    }
                }
                assert!(
                    maximum < 0.000002,
                    "packet/block phase differs from prior timeline: {maximum}"
                );
            }
        }
        #[test]
        fn missing_pcm_metrics_distinguish_digital_silence_and_future_start() {
            let mut m = Mixer::default();
            m.sync_clock(0);
            assert!(m.push_pcm(block(1, 20_000, vec![0.0; 10])).is_ok());
            m.mix(&mut [1.0; 30], 1000, 1);
            assert_eq!(
                m.metrics().missing_stream_frames,
                0,
                "pre-start silence and PCM zeros are covered"
            );
            m.mix(&mut [1.0; 5], 1000, 1);
            assert_eq!(m.metrics().missing_stream_frames, 5);
            assert_eq!(m.metrics().missing_output_frames, 5);
        }

        #[test]
        fn background_music_cannot_hide_missing_voice_pcm_metrics() {
            let mut m = Mixer::default();
            m.sync_clock(0);
            for id in [1, 2] {
                assert!(m.push_pcm(block(id, 0, vec![0.0; 10])).is_ok());
            }
            assert!(m.push_pcm(block(3, 0, vec![0.25; 30])).is_ok());
            let mut out = [0.0; 15];
            m.mix(&mut out, 1000, 1);
            assert_eq!(out, [0.25; 15]);
            assert_eq!(
                m.metrics().missing_stream_frames,
                10,
                "two streams missing five frames each"
            );
            assert_eq!(m.metrics().missing_output_frames, 5);
        }

        #[test]
        fn stop_end_pause_and_resume_are_not_missing_pcm() {
            for end in [true, false] {
                let mut m = Mixer::default();
                m.sync_clock(0);
                assert!(m.push_pcm(block(1, 0, vec![0.0; 10])).is_ok());
                if end {
                    m.end_pcm(1);
                } else {
                    m.flush_pcm(1);
                }
                m.mix(&mut [1.0; 30], 1000, 1);
                assert_eq!(
                    m.metrics().missing_stream_frames,
                    0,
                    "explicit stop and natural end are intentional"
                );
            }
            let mut m = Mixer::default();
            m.sync_clock(0);
            assert!(m.push_pcm(block(1, 10_000, vec![0.0; 10])).is_ok());
            m.pause_pcm(1, true, 0);
            m.mix(&mut [1.0; 50], 1000, 1);
            m.pause_pcm(1, false, 50_000);
            m.mix(&mut [1.0; 20], 1000, 1);
            assert_eq!(
                m.metrics().missing_stream_frames,
                0,
                "resume moves the scheduled first PTS too"
            );
        }

        #[test]
        fn pcm_lifecycle_and_queue_high_water_metrics_count_only_queued_data() {
            let mut m = Mixer::default();
            assert!(m.push_pcm(block(1, 0, vec![0.0; 10])).is_ok());
            assert!(m.push_pcm(block(1, 10_000, vec![0.0; 10])).is_ok());
            assert!(
                m.push_pcm(block(1, 0, vec![0.0; 5])).is_ok(),
                "covered overlap accepted but not queued"
            );
            assert!(m.push_pcm(block(1, 0, vec![0.0; 2001])).is_err());
            m.flush_pcm(1);
            assert!(m.push_pcm(block(1, 30_000, vec![0.0; 5])).is_ok());
            m.end_pcm(1);
            m.end_pcm(1);
            m.remove_pcm(1);
            let stats = m.metrics();
            assert_eq!(stats.queued_blocks, 3);
            assert_eq!(stats.queued_source_frames, 25);
            assert_eq!(stats.playback_starts, 2);
            assert_eq!(stats.flushes, 1);
            assert_eq!(stats.ends, 1);
            assert_eq!(stats.removals, 1);
            assert_eq!(stats.max_stream_queue_us, 20_000);
        }
        #[test]
        fn sustained_output_tolerates_device_clock_drift() {
            // A 200 ppm clock difference accumulates 100 ms in ~8 min. Exercise both
            // signs, resampling, callback jitter and a live device-rate change over
            // twenty virtual minutes. A nonzero triangle reveals holes AND jumps.
            for ppm in [-1000i64, -200, 200, 1000] {
                check_sustained_output(ppm);
            }
        }

        fn check_sustained_output(ppm: i64) {
            const SOURCE_RATE: u64 = 44_100;
            let mut m = Mixer::default();
            let mut produced = 0u64;
            let mut missing = 0;
            let mut max_step = 0.0f32;
            let mut previous = 0.25;
            for callback in 0..120_000u64 {
                let now =
                    callback * 10_000 * 1_000_000 / (1_000_000 + ppm) as u64 + callback % 7 * 137;
                m.sync_clock(now);
                let end = (now + 100_000) * SOURCE_RATE / 1_000_000;
                if end > produced {
                    assert!(
                        m.push_pcm(PcmBlock {
                            stream_id: 1,
                            guest_pts_us: produced * 1_000_000 / SOURCE_RATE,
                            sample_rate: SOURCE_RATE as u32,
                            channels: 1,
                            samples: (produced..end)
                                .map(|frame| {
                                    let phase = (frame % 441) as f32 / 441.0;
                                    0.25 + 0.25 * (1.0 - (2.0 * phase - 1.0).abs())
                                })
                                .collect(),
                        })
                        .is_ok()
                    );
                    produced = end;
                }
                let rate = if callback < 60_000 { 48_000 } else { 44_100 };
                let mut out = vec![0.0; rate as usize / 100 * 2];
                m.mix(&mut out, rate, 2);
                for frame in out.as_chunks::<2>().0 {
                    missing += usize::from(!(0.249..=0.501).contains(&frame[0]));
                    assert_eq!(frame[0], frame[1]);
                    max_step = max_step.max((frame[0] - previous).abs());
                    previous = frame[0];
                }
            }
            assert_eq!(missing, 0, "clock drift {ppm} ppm must not introduce holes");
            assert!(
                max_step < 0.0012,
                "clock drift {ppm} ppm introduced a discontinuity: {max_step}"
            );
            assert_eq!(
                m.resyncs, 0,
                "ordinary drift must not jump the PCM timeline"
            );
            assert!((m.clock_adjustment_ppm() + ppm as f64).abs() < 30.0);
        }

        #[test]
        fn clock_jump_recovers_without_retaining_drift_correction() {
            let mut m = Mixer::default();
            m.sync_clock(0);
            // Many buffered callbacks ahead of the guest start gradual correction.
            m.mix(&mut [0.0; 60], 1000, 1);
            for n in 1..100 {
                m.sync_clock(n * 10_000);
                m.mix(&mut [0.0; 10], 1000, 1);
            }
            assert!(m.clock_adjustment_ppm() < 0.0);
            // A real discontinuity (device stopped / no callbacks) still catches up.
            m.sync_clock(2_000_000);
            assert_eq!(m.resyncs, 1);
            assert_eq!(m.clock_adjustment_ppm(), 0.0);
            assert!(m.push_pcm(block(1, 2_000_000, vec![0.25; 100])).is_ok());
            let mut out = [0.0; 10];
            m.mix(&mut out, 1000, 1);
            assert_eq!(out, [0.25; 10]);
        }
        #[test]
        fn session_suspend_freezes_pcm_and_device_cursor() {
            let mut m = Mixer::default();
            m.sync_clock(0);
            assert!(m.push_pcm(block(1, 0, vec![0.25; 100])).is_ok());
            let mut out = [1.0; 10];
            m.suspend(true);
            assert!(!m.active());
            m.mix(&mut out, 1000, 1);
            assert_eq!(out, [0.0; 10]);
            assert_eq!(m.device_frames, 0);
            m.suspend(false);
            assert!(m.active());
            m.mix(&mut out, 1000, 1);
            assert_eq!(out, [0.25; 10]);
            assert_eq!(m.device_frames, 10);
        }

        #[test]
        fn rounded_timestamps_join_and_overlaps_are_trimmed() {
            let mut m = Mixer::default();
            assert!(m.push_pcm(block(3, 0, vec![0.25; 100])).is_ok());
            // Millisecond rounding: 0.6 ms early still continues at 100 ms.
            assert!(m.push_pcm(block(3, 99_400, vec![0.5; 100])).is_ok());
            let s = &m.streams[&3];
            assert_eq!(s.queue[1].pts_us, 100_000.0);
            // 50 ms of real overlap: the first 50 samples are dropped, never the whole block.
            assert!(m.push_pcm(block(3, 150_000, vec![1.0; 100])).is_ok());
            let s = &m.streams[&3];
            assert_eq!(
                (s.queue[2].pts_us, s.queue[2].samples.len()),
                (200_000.0, 50)
            );
            // Entirely covered: accepted and discarded.
            assert!(m.push_pcm(block(3, 100_000, vec![1.0; 10])).is_ok());
            assert_eq!(m.streams[&3].queue.len(), 3);
        }

        #[test]
        fn unconsumed_pcm_expires_with_the_guest_clock() {
            // No device: nothing mixes. A producer must not stall once its PCM is in the past.
            let mut m = Mixer::default();
            assert!(m.push_pcm(block(1 << 63, 0, vec![0.5; 2000])).is_ok());
            assert!(
                m.push_pcm(block(1 << 63, 2_000_000, vec![0.5; 1000]))
                    .is_err(),
                "two-second bound"
            );
            assert!(
                m.push_pcm(block(1 << 63, 2_600_000, vec![0.5; 1000]))
                    .is_ok(),
                "first block expired"
            );
            m.end_pcm(1 << 63);
            m.sync_clock(4_200_000);
            assert!(!m.active());
            assert!(m.streams.is_empty(), "ended and drained");
            // Paused streams keep their queue; unpausing rebases it.
            assert!(m.push_pcm(block(9, 5_000_000, vec![0.5; 100])).is_ok());
            m.pause_pcm(9, true, 5_000_000);
            m.sync_clock(9_000_000);
            m.pause_pcm(9, false, 9_000_000);
            assert!(m.active());
        }

        #[test]
        fn timestamp_pause_flush_backpressure_and_end() {
            let mut m = Mixer::default();
            m.sync_clock(1000);
            assert!(m.push_pcm(block(7, 2000, vec![0.25; 2000])).is_ok());
            assert!(m.push_pcm(block(7, 2_002_000, vec![1.0])).is_err());
            let mut out = [0.0; 6];
            m.mix(&mut out, 1000, 2);
            assert_eq!(out, [0.0, 0.0, 0.25, 0.25, 0.25, 0.25]);
            m.pause_pcm(7, true, 4000);
            assert!(!m.active());
            m.pause_pcm(7, false, 5000);
            assert!(m.active());
            m.flush_pcm(7);
            assert!(!m.active());
            assert!(m.push_pcm(block(7, 4000, vec![0.5])).is_ok());
            m.end_pcm(7);
            m.mix(&mut out, 1000, 2);
            assert!(!m.active());
        }
    }
}

//! Shared virtual raster scanline timing for DirectDraw and D3D9.
use std::time::Duration;

/// Compute `(in_vertical_blank, scanline)` for one refresh cycle with `height`
/// visible rows and one blanking row.
pub(crate) fn scanline(
    height: u32,
    hz: u32,
    now_us: u64,
    vsync: Option<(Duration, Duration)>,
) -> (bool, u32) {
    let rows = u64::from(height) + 1;
    let line = if let Some((since, period)) = vsync
        && !period.is_zero()
    {
        let phase = (since.as_secs_f64() / period.as_secs_f64()).fract();
        (phase * rows as f64) as u32
    } else {
        // Integer phase avoids drift and overflow at long runtimes.
        let phase = (now_us % 1_000_000) * u64::from(hz) % 1_000_000;
        (phase * rows / 1_000_000) as u32
    };
    (line >= height, line)
}

#[derive(Default)]
pub(crate) struct PresentQueue {
    deadlines: std::collections::VecDeque<u64>,
    pub count: u32,
    pub sync: u64,
}

impl PresentQueue {
    pub fn available(&mut self, now: u64, limit: u32) -> Option<u64> {
        while self
            .deadlines
            .front()
            .is_some_and(|&deadline| deadline <= now)
        {
            self.deadlines.pop_front();
        }
        self.deadlines
            .get(self.deadlines.len().wrapping_sub(limit as usize))
            .copied()
    }

    pub fn submit(&mut self, now: u64, interval: u32, hz: u32) {
        self.count = self.count.wrapping_add(1);
        let first = now.max(self.deadlines.back().copied().unwrap_or(0));
        if interval == 0 {
            self.sync = first;
        } else {
            let tick = u128::from(first) * u128::from(hz) / 1_000_000 + u128::from(interval);
            self.sync = (tick * 1_000_000).div_ceil(u128::from(hz)) as u64;
            self.deadlines.push_back(self.sync);
        }
    }
}

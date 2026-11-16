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

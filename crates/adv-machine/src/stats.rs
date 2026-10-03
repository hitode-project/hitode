//! Opt-in host-side cost accounting for the runtime and its frontend.
//!
//! Counters are always kept (plain integer adds); wall-clock timing only runs when
//! [`Stats::enabled`] is set, so a normal run pays one branch per host call. Per-frame rows
//! are closed whenever the graphics front advances [`crate::Sys::frame`].
//!
//! Terms, all host wall time:
//! * `run`: inside `Backend::run` (guest x86 execution plus in-hook host calls).
//! * `hook`: host handlers and guest-return continuations inside `run`.
//! * `resume`: continuations of woken contexts, outside `run`.
//! * guest execution ≈ `run − hook`.
//!
//! `sched` counts [`crate::Machine::run`] entries and `polls` the virtual-time jumps over
//! contexts that only yield. On the real clock each of them is a host wake-up (a poll waits at
//! most 1 ms), so on virtual time their sum is what a real-time frontend would wake for.

use std::io::{self, Write};
use std::time::Instant;

use crate::thunk::ThunkTable;

/// One finished guest frame (or the stretch before the first one, frame `u32::MAX`).
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameRow {
    pub frame: u32,
    /// Guest clock when the frame was closed.
    pub guest_us: u64,
    pub run_ns: u64,
    pub hook_ns: u64,
    pub resume_ns: u64,
    pub frontend_render_ns: u64,
    pub frontend_audio_ns: u64,
    pub frontend_sleep_ns: u64,
    pub cpu_runs: u64,
    pub host_calls: u64,
    pub sched: u64,
    pub polls: u64,
    pub frontend_wakes: u64,
    /// Executed translation blocks and their guest code bytes (block counting only).
    pub blocks: u64,
    pub block_bytes: u64,
}

impl FrameRow {
    fn sub(&self, o: &Self) -> Self {
        Self {
            frame: self.frame,
            guest_us: self.guest_us,
            run_ns: self.run_ns - o.run_ns,
            hook_ns: self.hook_ns - o.hook_ns,
            resume_ns: self.resume_ns - o.resume_ns,
            frontend_render_ns: self.frontend_render_ns - o.frontend_render_ns,
            frontend_audio_ns: self.frontend_audio_ns - o.frontend_audio_ns,
            frontend_sleep_ns: self.frontend_sleep_ns - o.frontend_sleep_ns,
            cpu_runs: self.cpu_runs - o.cpu_runs,
            host_calls: self.host_calls - o.host_calls,
            sched: self.sched - o.sched,
            polls: self.polls - o.polls,
            frontend_wakes: self.frontend_wakes - o.frontend_wakes,
            blocks: self.blocks - o.blocks,
            block_bytes: self.block_bytes - o.block_bytes,
        }
    }

    const CSV_HEADER: &'static str = "frame,guest_us,run_ns,hook_ns,resume_ns,guest_exec_ns,render_ns,audio_ns,sleep_ns,\
cpu_runs,host_calls,sched,polls,frontend_wakes,blocks,block_bytes";

    fn write_csv(&self, w: &mut dyn Write) -> io::Result<()> {
        writeln!(
            w,
            "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
            if self.frame == u32::MAX {
                -1
            } else {
                i64::from(self.frame)
            },
            self.guest_us,
            self.run_ns,
            self.hook_ns,
            self.resume_ns,
            self.run_ns.saturating_sub(self.hook_ns),
            self.frontend_render_ns,
            self.frontend_audio_ns,
            self.frontend_sleep_ns,
            self.cpu_runs,
            self.host_calls,
            self.sched,
            self.polls,
            self.frontend_wakes,
            self.blocks,
            self.block_bytes,
        )
    }
}

#[derive(Default)]
pub struct Stats {
    /// Take wall-clock timings and keep per-frame rows.
    pub enabled: bool,
    /// Running totals; `total.frame`/`guest_us` are those of the frame being accumulated.
    pub total: FrameRow,
    /// Totals when the current frame started.
    frame_start: FrameRow,
    pub frames: Vec<FrameRow>,
    /// Per thunk index: (host wall ns, calls).
    per_thunk: Vec<(u64, u64)>,
}

impl Stats {
    pub fn new() -> Self {
        Self {
            total: FrameRow {
                frame: u32::MAX,
                ..FrameRow::default()
            },
            ..Self::default()
        }
    }

    /// Start a timing, if enabled.
    #[inline]
    pub fn start(&self) -> Option<Instant> {
        self.enabled.then(Instant::now)
    }

    /// Host handler or continuation time for `thunk`, inside or outside `Backend::run`.
    #[inline]
    pub(crate) fn host(&mut self, t0: Option<Instant>, thunk: u32, in_run: bool) {
        let Some(t0) = t0 else { return };
        let ns = t0.elapsed().as_nanos() as u64;
        if in_run {
            self.total.hook_ns += ns;
        } else {
            self.total.resume_ns += ns;
        }
        let i = thunk as usize;
        if self.per_thunk.len() <= i {
            self.per_thunk.resize(i + 1, (0, 0));
        }
        self.per_thunk[i].0 += ns;
        self.per_thunk[i].1 += 1;
    }

    /// Frontend work on the frame being accumulated.
    pub fn add_render(&mut self, t0: Option<Instant>) {
        if let Some(t0) = t0 {
            self.total.frontend_render_ns += t0.elapsed().as_nanos() as u64;
        }
    }

    pub fn add_audio(&mut self, t0: Option<Instant>) {
        if let Some(t0) = t0 {
            self.total.frontend_audio_ns += t0.elapsed().as_nanos() as u64;
        }
    }

    /// A frontend wait: `t0` is when it started sleeping.
    pub fn add_sleep(&mut self, t0: Option<Instant>) {
        self.total.frontend_wakes += 1;
        if let Some(t0) = t0 {
            self.total.frontend_sleep_ns += t0.elapsed().as_nanos() as u64;
        }
    }

    /// Close the current row if the presentation sequence moved on.
    pub(crate) fn note_frame(&mut self, frame: u32, guest_us: u64, blocks: Option<(u64, u64)>) {
        if let Some((b, bytes)) = blocks {
            self.total.blocks = b;
            self.total.block_bytes = bytes;
        }
        if frame == self.total.frame {
            return;
        }
        if self.enabled {
            self.total.guest_us = guest_us;
            self.frames.push(self.total.sub(&self.frame_start));
        }
        self.total.frame = frame;
        self.frame_start = self.total;
    }

    pub fn write_csv(&self, w: &mut dyn Write) -> io::Result<()> {
        writeln!(w, "{}", FrameRow::CSV_HEADER)?;
        for r in &self.frames {
            r.write_csv(w)?;
        }
        Ok(())
    }

    /// Human-readable totals over `guest_us` of guest time and `wall` of host time, with the
    /// `top` most expensive host functions.
    pub fn summary(&self, thunks: &ThunkTable, guest_us: u64, wall_ns: u64, top: usize) -> String {
        let t = &self.total;
        let secs = (guest_us as f64 / 1e6).max(1e-9);
        let ms = |ns: u64| ns as f64 / 1e6;
        let pct = |ns: u64| 100.0 * ns as f64 / wall_ns.max(1) as f64;
        let exec = t.run_ns.saturating_sub(t.hook_ns);
        let host = t.hook_ns + t.resume_ns;
        let mut s = format!(
            "stats: {:.1} s guest time, {:.1} ms host wall ({:.2}% of real time)\n\
             \x20 guest x86 exec {:9.1} ms ({:5.2}% of wall)\n\
             \x20 host calls     {:9.1} ms ({:5.2}%)  {} calls, {:.0}/s\n\
             \x20 frontend render{:9.1} ms ({:5.2}%)\n\
             \x20 frontend audio {:9.1} ms ({:5.2}%)\n\
             \x20 frontend sleep {:9.1} ms ({:5.2}%)\n\
             \x20 frames {}  ({:.1}/s)\n\
             \x20 cpu entries {:.0}/s, scheduler wakes {:.0}/s, poll steps {:.0}/s, frontend wakes {:.0}/s\n",
            secs,
            ms(wall_ns),
            100.0 * wall_ns as f64 / (secs * 1e9),
            ms(exec),
            pct(exec),
            ms(host),
            pct(host),
            t.host_calls,
            t.host_calls as f64 / secs,
            ms(t.frontend_render_ns),
            pct(t.frontend_render_ns),
            ms(t.frontend_audio_ns),
            pct(t.frontend_audio_ns),
            ms(t.frontend_sleep_ns),
            pct(t.frontend_sleep_ns),
            self.frames.len(),
            self.frames.len() as f64 / secs,
            t.cpu_runs as f64 / secs,
            t.sched as f64 / secs,
            t.polls as f64 / secs,
            t.frontend_wakes as f64 / secs,
        );
        // Host work per frame: what a real-time frontend must fit into one frame interval.
        let mut work: Vec<u64> = self
            .frames
            .iter()
            .map(|r| r.run_ns + r.resume_ns + r.frontend_render_ns + r.frontend_audio_ns)
            .collect();
        if !work.is_empty() {
            let over = |ms: u64| work.iter().filter(|&&w| w > ms * 1_000_000).count();
            let (o16, o33) = (over(16), over(33));
            work.sort_unstable();
            let q = |p: f64| ms(work[((work.len() - 1) as f64 * p) as usize]);
            s += &format!(
                "  host work per frame: median {:.2} ms, p99 {:.2} ms, max {:.2} ms; over 16 ms: {o16}, over 33 ms: {o33}\n",
                q(0.5),
                q(0.99),
                q(1.0),
            );
        }
        if t.blocks > 0 {
            s += &format!(
                "  blocks {:.0}/s, guest code {:.1} KB/s ({:.0} blocks/frame)\n",
                t.blocks as f64 / secs,
                t.block_bytes as f64 / secs / 1024.0,
                t.blocks as f64 / self.frames.len().max(1) as f64
            );
        }
        let mut by_cost: Vec<(usize, (u64, u64))> = self
            .per_thunk
            .iter()
            .copied()
            .enumerate()
            .filter(|(_, c)| c.1 > 0)
            .collect();
        by_cost.sort_by_key(|&(_, (ns, _))| std::cmp::Reverse(ns));
        if !by_cost.is_empty() {
            s += "  host functions by time:\n";
        }
        for (i, (ns, n)) in by_cost.into_iter().take(top) {
            let f = thunks.get(i as u32);
            s += &format!(
                "    {:9.2} ms {:5.2}% {:8} calls {:7.2} us/call  {}!{}\n",
                ms(ns),
                pct(ns),
                n,
                ns as f64 / 1e3 / n as f64,
                f.module,
                f.name
            );
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_close_when_the_frame_moves_and_hold_deltas() {
        let mut s = Stats::new();
        s.enabled = true;
        s.total.host_calls = 5;
        s.note_frame(0, 1_000, Some((10, 100)));
        s.total.host_calls = 12;
        s.total.run_ns = 3_000;
        s.note_frame(0, 2_000, Some((15, 160)));
        s.note_frame(1, 16_000, Some((40, 400)));
        assert_eq!(s.frames.len(), 2);
        let (pre, f0) = (s.frames[0], s.frames[1]);
        assert_eq!((pre.frame, pre.host_calls, pre.blocks), (u32::MAX, 5, 10));
        assert_eq!(
            (
                f0.frame,
                f0.guest_us,
                f0.host_calls,
                f0.run_ns,
                f0.blocks,
                f0.block_bytes
            ),
            (0, 16_000, 7, 3_000, 30, 300)
        );
        let mut csv = Vec::new();
        s.write_csv(&mut csv).unwrap();
        let csv = String::from_utf8(csv).unwrap();
        assert!(csv.lines().nth(1).unwrap().starts_with("-1,"));
        assert_eq!(csv.lines().count(), 3);
    }

    #[test]
    fn disabled_stats_keep_counters_but_no_rows() {
        let mut s = Stats::new();
        assert!(s.start().is_none());
        s.note_frame(0, 1, None);
        s.note_frame(1, 2, None);
        assert!(s.frames.is_empty());
        assert_eq!(s.total.frame, 1);
    }
}

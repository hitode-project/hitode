//! Opt-in controller boundary observations; no instruction hook or JIT policy change.
//! TSV: host ns since enable, event, context, three event-specific numeric fields.
//! run_start: PC; run_end: PC, owner wall ns, inline host-service ns.
//! pick: yielded; lease/collect: token. block: kind (1 timer/2 object/3 set),
//! deadline or handle, thunk. wake: thunk. CS: address, owner TID/recursion, event.
//! Lease-to-collect includes queueing and completion delivery, not worker CPU time.
//! frame: completed guest-frame serial (not a shell observation or scanout).
use std::io::{self, Write};
use std::time::Instant;

#[derive(Default)]
pub struct ScheduleTrace {
    sink: Option<Box<dyn Write>>,
    start: Option<Instant>,
    remaining: usize,
    dropped: u64,
    error: Option<io::Error>,
}

impl ScheduleTrace {
    pub fn to_writer(mut sink: Box<dyn Write>) -> io::Result<Self> {
        writeln!(sink, "time_ns\tevent\tcontext\ta\tb\tc")?;
        Ok(Self {
            sink: Some(sink),
            start: Some(Instant::now()),
            remaining: 1_000_000,
            ..Self::default()
        })
    }

    pub fn enabled(&self) -> bool {
        self.sink.is_some()
    }

    pub fn event(&mut self, kind: &str, context: u32, a: u64, b: u64, c: u64) {
        let Some(sink) = self.sink.as_mut() else {
            return;
        };
        if self.error.is_some() {
            return;
        }
        if self.remaining == 0 {
            self.dropped += 1;
            return;
        }
        self.remaining -= 1;
        if let Err(error) = writeln!(
            sink,
            "{}\t{kind}\t{context}\t{a}\t{b}\t{c}",
            self.start.unwrap().elapsed().as_nanos()
        ) {
            self.error = Some(error);
        }
    }

    pub fn flush(&mut self) -> io::Result<()> {
        if let Some(error) = self.error.take() {
            return Err(error);
        }
        if let Some(sink) = self.sink.as_mut() {
            if self.dropped != 0 {
                writeln!(
                    sink,
                    "{}\ttruncated\t0\t{}\t0\t0",
                    self.start.unwrap().elapsed().as_nanos(),
                    self.dropped
                )?;
                self.dropped = 0;
            }
            sink.flush()?;
        }
        Ok(())
    }
}

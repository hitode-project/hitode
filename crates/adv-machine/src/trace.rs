//! Host-boundary call trace.
//!
//! One record per completed host call, with a stable textual format suitable for
//! comparing runtime execution with native Windows oracle traces:
//!
//! `<ctx>\t<frame>\t<module>!<name>\t<arg0,arg1,...>\t<ret>` (numbers in hex)

use adv_cpu::backend::{Guest, GuestExt, Reg};
use std::collections::HashSet;
use std::io::Write;
use std::{cell::RefCell, rc::Rc};

#[derive(Clone)]
struct SharedSink(Rc<RefCell<Box<dyn Write>>>);
impl Write for SharedSink {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().write(bytes)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.borrow_mut().flush()
    }
}

pub struct TraceRecord<'a> {
    pub ctx: u32,
    pub frame: u32,
    pub module: &'a str,
    pub name: &'a str,
    pub args: &'a [u32],
    pub ret: u64,
}

impl TraceRecord<'_> {
    pub fn to_line(&self) -> String {
        let args: Vec<String> = self.args.iter().map(|a| format!("{a:x}")).collect();
        format!(
            "{}\t{}\t{}!{}\t{}\t{:x}",
            self.ctx,
            self.frame,
            self.module,
            self.name,
            args.join(","),
            self.ret
        )
    }
}

#[derive(Default, Clone)]
pub struct Trace {
    sink: Option<SharedSink>,
    /// Trace every host call, not only those registered with `trace: true`.
    pub all: bool,
    variadic_noted: HashSet<(String, String)>,
}

impl Trace {
    pub fn to_writer(w: Box<dyn Write>) -> Self {
        Self {
            sink: Some(SharedSink(Rc::new(RefCell::new(w)))),
            all: false,
            variadic_noted: HashSet::new(),
        }
    }

    /// Process switch comment; ordinary single-process trace rows stay unchanged.
    pub fn process(&mut self, pid: u32) {
        if let Some(w) = &mut self.sink {
            let _ = writeln!(w, "# process\t{pid:08x}");
        }
    }

    pub fn enabled(&self) -> bool {
        self.sink.is_some()
    }

    /// Architectural trap input before OS-personality handling. Valid host
    /// transport interrupts are excluded; comments do not become API records.
    pub fn raw_interrupt(
        &mut self,
        context: u32,
        frame: u32,
        vector: u8,
        cpu: &dyn Guest,
        candidate: Option<(&str, &str)>,
    ) {
        if let Some(w) = &mut self.sink {
            let _ = write!(w, "# raw_interrupt\t{context}\t{frame}\t{vector:02x}");
            for reg in [Reg::Eax, Reg::Edx, Reg::Eip, Reg::Esp, Reg::Eflags] {
                let _ = write!(w, "\t{reg:?}={:08x}", cpu.reg(reg));
            }
            if let Ok(word) = cpu.read_u32(cpu.reg(Reg::Esp)) {
                let _ = write!(w, "\tStackTop={word:08x}");
            }
            // A coincidental stack word is only a diagnostic candidate. It
            // never validates transport or authorizes a host API dispatch.
            if let Some((module, name)) = candidate {
                let _ = write!(w, "\tCandidate={module:?}!{name:?}");
            }
            let _ = writeln!(w);
        }
    }

    /// Diagnostic-only dynamic import metadata. Comment records leave completed
    /// calls in their existing five-column format. Disabled tracing allocates nothing.
    pub fn resolution(
        &mut self,
        caller: u32,
        module: &str,
        symbol: &dyn std::fmt::Display,
        address: u32,
    ) {
        if let Some(w) = &mut self.sink {
            let _ = writeln!(
                w,
                "# resolve\t{caller:08x}\t{module}!{symbol}\t{address:08x}"
            );
        }
    }

    pub fn record(&mut self, r: &TraceRecord<'_>) {
        if let Some(w) = &mut self.sink {
            let _ = writeln!(w, "{}", r.to_line());
        }
    }

    /// A terminal outcome never produces a completed-call row. Retain its
    /// original owner and continuation separately so clean exits are observable.
    pub fn terminal(
        &mut self,
        record: &TraceRecord<'_>,
        kind: &str,
        esp: u32,
        resume: u32,
        caller: Option<u32>,
    ) {
        if let Some(w) = &mut self.sink {
            let _ = writeln!(
                w,
                "# terminal\t{kind}\t{}\tOriginEsp={esp:08x}\tResume={resume:08x}\tCaller={caller:08x?}",
                record.to_line(),
            );
        }
    }

    /// A bounded diagnostic comment, independent of per-function trace flags.
    /// Escaped text cannot introduce extra call records or raw control lines.
    pub fn warning(
        &mut self,
        context: u32,
        frame: u32,
        caller: u32,
        api: &str,
        reason: &str,
        sample: Option<&str>,
    ) {
        if let Some(w) = &mut self.sink {
            let _ = writeln!(
                w,
                "# warning\t{context}\t{frame}\t{caller:08x}\t{api}\t{reason:?}\t{}",
                sample.unwrap_or("")
            );
        }
    }

    /// A comment preserves the five-column oracle format and makes truncation explicit.
    pub fn note_variadic(&mut self, module: &str, name: &str, fixed: u8) {
        if let Some(w) = &mut self.sink
            && self
                .variadic_noted
                .insert((module.to_owned(), name.to_owned()))
        {
            let _ = writeln!(
                w,
                "# signature\t{module}!{name}\tfixed_prefix={fixed}\tvariadic=true"
            );
        }
    }

    pub fn flush(&mut self) {
        if let Some(w) = &mut self.sink {
            let _ = w.flush();
        }
    }
}

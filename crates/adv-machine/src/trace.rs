//! Host-boundary call trace.
//!
//! One record per completed host call, in the same shape the M2 oracle (a PAL.dll proxy
//! running under real Windows) will emit, so the two traces can be diffed line by line:
//!
//! `<ctx>\t<frame>\t<module>!<name>\t<arg0,arg1,...>\t<ret>` (numbers in hex)

use std::collections::HashSet;
use std::io::Write;

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

#[derive(Default)]
pub struct Trace {
    sink: Option<Box<dyn Write>>,
    /// Trace every host call, not only those registered with `trace: true`.
    pub all: bool,
    variadic_noted: HashSet<(String, String)>,
}

impl Trace {
    pub fn to_writer(w: Box<dyn Write>) -> Self {
        Self {
            sink: Some(w),
            all: false,
            variadic_noted: HashSet::new(),
        }
    }

    pub fn enabled(&self) -> bool {
        self.sink.is_some()
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

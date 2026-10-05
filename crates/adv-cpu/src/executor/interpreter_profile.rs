//! Opt-in interpreter hotspot table for performance work.
//!
//! With `ADV_INTERP_PROFILE=<file>`, the executor rewrites `<file>` every five
//! seconds with the interpreted blocks that ran the most guest instructions in
//! that window, why each ran in the interpreter, and the instructions within
//! the block's average run that native code does not support. The file is a local
//! diagnostic: it names guest addresses and instructions, never bytes.
use std::collections::HashMap;
use std::fmt::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use iced_x86::{Decoder, DecoderOptions};

use crate::Memory;

const WINDOW: Duration = Duration::from_secs(5);
const ROWS: usize = 60;
/// Instructions decoded from a block start in search of an unsupported one.
const SCAN: usize = 64;

/// Why a block ran in the interpreter.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) enum Reason {
    /// No region owns the address.
    #[default]
    Uncovered,
    /// Discovery tried the address and compiled nothing for it.
    Rejected,
    /// A region owns the address but has no native code yet.
    Pending,
    /// Native code owns the address but has no entry there.
    Interior,
    /// The native code just requested an interpreted instruction.
    Fallback,
    /// Single-stepping, a pending x87 exception, or an interpreter-only run.
    Forced,
}

#[derive(Default)]
struct Block {
    entries: u64,
    units: u64,
    reason: Reason,
}

pub(crate) struct Profile {
    path: PathBuf,
    blocks: HashMap<u32, Block>,
    written: Instant,
}

impl Profile {
    pub(crate) fn from_env() -> Option<Box<Self>> {
        let path = std::env::var_os("ADV_INTERP_PROFILE")?;
        Some(Box::new(Self {
            path: path.into(),
            blocks: HashMap::new(),
            written: Instant::now(),
        }))
    }

    pub(crate) fn record(&mut self, pc: u32, units: u32, reason: Reason, memory: &Memory) {
        let block = self.blocks.entry(pc).or_default();
        block.entries += 1;
        block.units += u64::from(units);
        block.reason = reason;
        if self.written.elapsed() >= WINDOW {
            self.write(memory);
        }
    }

    fn write(&mut self, memory: &Memory) {
        self.written = Instant::now();
        let mut rows: Vec<_> = self.blocks.drain().collect();
        rows.sort_unstable_by_key(|(_, block)| std::cmp::Reverse(block.units));
        let mut text = String::from("units\tpc\tentries\treason\tunsupported\n");
        for (pc, block) in rows.into_iter().take(ROWS) {
            let run = block.units.div_ceil(block.entries.max(1)).max(1) as usize;
            let unsupported = unsupported(memory, pc, run);
            let unsupported = if unsupported.is_empty() {
                "-".into()
            } else {
                unsupported.join(" ")
            };
            let _ = writeln!(
                text,
                "{}\t{pc:#x}\t{}\t{:?}\t{unsupported}",
                block.units, block.entries, block.reason
            );
        }
        let _ = std::fs::write(&self.path, text);
    }
}

/// The unsupported instructions among the first `run` from `pc`, at most
/// [`SCAN`], as `address:code`.
fn unsupported(memory: &Memory, pc: u32, run: usize) -> Vec<String> {
    let Some(available) = (0..=SCAN * 15)
        .rev()
        .step_by(16)
        .find(|&len| memory.read_data(pc, len).is_ok())
    else {
        return Vec::new();
    };
    let Ok(bytes) = memory.read_data(pc, available) else {
        return Vec::new();
    };
    let mut decoder = Decoder::with_ip(32, &bytes, u64::from(pc), DecoderOptions::NONE);
    decoder
        .iter()
        .take(run.min(SCAN))
        .filter(|i| !i.is_invalid() && !crate::compile::supported(i))
        .map(|i| format!("{:#x}:{:?}", i.ip32(), i.code()))
        .collect()
}

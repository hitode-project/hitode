//! The translation front end samples guest code and plans decoded units.
//!
//! Cold and loop layouts share instruction snapshots until their code is
//! invalidated. Resident preparation derives access summaries from those
//! snapshots. Tier-1 lowering takes its checked sites and helper indices
//! separately, so a rebuild can reuse the layout and its watches.

pub(crate) mod decode;
pub(crate) mod flags;
pub(crate) mod forms;
pub(super) mod plans;
mod quick_form;
pub(crate) mod summary;
pub(crate) mod tables;
pub(crate) mod trace;

pub(crate) mod tags {
    pub use iced_x86::{Code, ConditionCode, Mnemonic, OpAccess, Register, RflagsBits};
}

use std::sync::{Arc, OnceLock};

use adv_cpu_core::{Memory, native_region::NativeRegion};
use decode::Helper;
pub(crate) use decode::{DecodeCounts, Decodes};
use trace::{Redirect, Trace};

/// The inputs lowerings share for one trace shape (A64A-ROLE-2).
#[derive(Clone, Default)]
pub struct Unit {
    /// The executable images, whose read-only tables may be indexed.
    pub(crate) images: Vec<(u32, u32)>,
    pub(crate) tables: Vec<tables::Table>,
    pub(crate) trace: Trace,
    pub(super) regions: Vec<(usize, NativeRegion)>,
    resident_links: OnceLock<Trace>,
    pub(crate) table_target: bool,
}

impl Unit {
    /// Traces the unit at `eip`, ending before any of `stops`.
    /// The code store registers the resulting dependencies before placement.
    pub fn build(
        &mut self,
        memory: &mut adv_cpu_core::memory::Memory,
        eip: u32,
        stops: &[u32],
        host: &dyn Fn(u32, u32) -> Option<u32>,
        redirect: &dyn Fn(u32) -> Option<Redirect>,
        loops: bool,
    ) {
        let mut decodes = Decodes::default();
        self.build_cached(
            memory,
            eip,
            stops,
            host,
            redirect,
            (loops, None),
            &mut decodes,
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn build_cached(
        &mut self,
        memory: &mut Memory,
        eip: u32,
        stops: &[u32],
        host: &dyn Fn(u32, u32) -> Option<u32>,
        redirect: &dyn Fn(u32) -> Option<Redirect>,
        layout: (bool, Option<&[u32; 8]>),
        decodes: &mut Decodes,
    ) {
        self.regions.clear();
        self.resident_links.take();
        self.trace
            .build_cached(memory, eip, stops, host, redirect, layout, decodes);
        tables::prepare(self, memory);
        self.table_target = tables::image(&self.images, eip, 1)
            && memory.committed(eip, 15).is_ok()
            && !memory.shared_range(eip, 15)
            && decodes.table_target(eip);
    }

    pub(crate) fn summarize(&mut self, decodes: &mut Decodes) {
        self.trace.x86 = decodes.summaries(&self.trace);
    }

    pub(super) fn jit_trace(&self, resident_links: bool) -> &Trace {
        if resident_links {
            self.resident_links.get_or_init(|| {
                let mut trace = self.trace.clone();
                trace.observe_resident_links();
                trace
            })
        } else {
            &self.trace
        }
    }

    /// Guest bytes the unit depends on, as sorted
    /// (address, length) spans.
    pub fn code(&self) -> &[(u32, u32)] {
        &self.trace.code
    }

    /// What the unit's `interpret` calls run, from the base index its
    /// back end was given.
    pub(crate) fn helpers(&self) -> &[Helper] {
        &self.trace.helpers
    }

    pub(crate) fn cut(&self, cut: &[bool]) -> Self {
        Self {
            trace: self.trace.cut(cut),
            ..Self::default()
        }
    }
}

/// Instruction forms for reports reflect the bytes currently mapped at the site.
pub(crate) fn instruction_form(memory: &adv_cpu_core::Memory, eip: u32) -> Option<tags::Code> {
    let mut bytes = [0; 15];
    let page = (0x1000 - (eip & 0xfff) as usize).min(bytes.len());
    if memory.read_into(eip, &mut bytes).is_err() {
        memory.read_into(eip, &mut bytes[..page]).ok()?;
    }
    let mut decoder =
        iced_x86::Decoder::with_ip(32, &bytes, u64::from(eip), iced_x86::DecoderOptions::NONE);
    Some(decoder.decode().code())
}

/// The site's cold and extended layouts share the front end's instruction
/// snapshots. Both layouts stay available while their code is valid.
#[derive(Default)]
pub(crate) struct Site {
    pub cold: Option<Arc<Unit>>,
    pub hot: Option<Arc<Unit>>,
    pub guarded: Option<Arc<Unit>>,
}

use trace::{Edge, Mode, Next};

impl Unit {
    pub(crate) fn prepare_regions(
        &mut self,
        memory: &adv_cpu_core::Memory,
        admit: &dyn Fn(u32, &[u8]) -> Vec<NativeRegion>,
    ) {
        for start in 0..self.trace.primary {
            if !self.trace.merge[start] {
                continue;
            }
            let Some(end) = (start..self.trace.primary)
                .find(|&n| self.trace.insts[n].taken == Some(Edge::Internal(start)))
            else {
                continue;
            };
            let body = &self.trace.insts[start..=end];
            if body.iter().enumerate().any(|(n, i)| {
                i.mode != Mode::Plain
                    || i.call.is_some()
                    || matches!(i.op, decode::Op::Value(_) | decode::Op::Parts(_))
                    || (n != 0 && (self.trace.head(start + n) || self.trace.merge[start + n]))
                    || (n + 1 != body.len()
                        && (i.next != Next::Fall
                            || i.taken.is_some()
                            || i.next_pc != body[n + 1].pc))
            }) {
                continue;
            }
            let pc = body[0].pc;
            let Some(len) = body.last().unwrap().next_pc.checked_sub(pc) else {
                continue;
            };
            let Ok(bytes) = memory.read(pc, len as usize) else {
                continue;
            };
            for region in admit(pc, bytes) {
                if region.instructions() as usize == body.len() {
                    self.regions.push((start, region));
                }
            }
        }
    }
}

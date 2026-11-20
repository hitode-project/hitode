//! Resident entry sensors and canonical event tails (A64-OPT-8).

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(u32)]
pub(crate) enum ResidentCold {
    Other,
    #[cfg(native)]
    HostFault,
    Validation,
    BlockedAccess,
    PageCross,
    FrameDisjoint,
    Division,
}

#[cfg(native)]
impl ResidentCold {
    pub(crate) fn decode(detail: u64) -> Option<(u32, Self)> {
        let reason = match detail >> 32 {
            0 => return None,
            n if n == ResidentCold::Validation as u64 => ResidentCold::Validation,
            n if n == ResidentCold::BlockedAccess as u64 => ResidentCold::BlockedAccess,
            n if n == ResidentCold::PageCross as u64 => ResidentCold::PageCross,
            n if n == ResidentCold::FrameDisjoint as u64 => ResidentCold::FrameDisjoint,
            n if n == ResidentCold::Division as u64 => ResidentCold::Division,
            _ => unreachable!("resident cold reason"),
        };
        Some((detail as u32, reason))
    }
}

/// One invariant indirect target carried by a prepared resident row form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RowTarget {
    pub base: u8,
    pub offset: u32,
    pub value: u32,
}

/// Entry registers whose low bits select between two row forms.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RowAlignment {
    pub mask: u32,
    pub bases: Vec<(u8, u32)>,
}

impl RowAlignment {
    pub(crate) fn nonzero(&self, regs: &[u32; 8]) -> bool {
        self.bases
            .iter()
            .any(|&(reg, offset)| regs[usize::from(reg)].wrapping_add(offset) & self.mask != 0)
    }

    pub(crate) fn words(&self) -> u32 {
        // OR the bases, adding each nonzero bias, then AND and branch.
        1 + self.bases.len() as u32
            + self
                .bases
                .iter()
                .filter(|&&(_, offset)| offset != 0)
                .count() as u32
    }
}

/// Row entry failures precede guest effects. A short profitable-work
/// check is ordinary routing and never contributes to withdrawal.
#[derive(Clone, Copy, Debug)]
#[repr(u8)]
pub(crate) enum RowFailure {
    Target,
    Range,
    Alias,
    Parameter,
    Count,
    Span,
}

/// Slots for opt-in per-head counters. Conditional cases use ordered arms.
#[derive(Clone, Copy)]
pub(crate) enum Counter {
    Entry,
    MmxMiss,
    SpanMiss,
    Batch,
    Class(u8),
    RowEntry,
    RowArrival,
    RowShort,
}

pub(crate) const COUNTERS: usize = 25;

impl Counter {
    pub(crate) fn index(self) -> usize {
        match self {
            Self::Entry => 0,
            Self::MmxMiss => 1,
            Self::SpanMiss => 2,
            Self::Batch => 3,
            Self::Class(class) => 4 + usize::from(class),
            Self::RowEntry => 20,
            Self::RowArrival => 23,
            Self::RowShort => 24,
        }
    }
}

#[cfg(native)]
mod entry;
#[cfg(native)]
pub(crate) use entry::*;

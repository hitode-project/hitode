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

#[cfg(native)]
mod entry;
#[cfg(native)]
pub(crate) use entry::*;

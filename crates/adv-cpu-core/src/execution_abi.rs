//! Versioned state and recovery contract shared by experimental native lanes.
//! No guest memory is read while reconstructing an interrupted register prefix.

/// Increment when a native entry or recovery description changes its meaning.
pub const VERSION: u32 = 2;
/// Owner-pinned request word shared by native lanes. Producers use Release
/// stores; generated code uses atomic loads. Only the owner consumes requests.
pub type PollWord = std::sync::atomic::AtomicBool;
/// EAX, ECX, EDX, EBX, ESP, EBP, ESI and EDI in the ARM64 native ABI.
pub const ARM64_GPRS: [u8; 8] = [19, 20, 21, 22, 23, 24, 25, 26];
pub const ARM64_FLAGS: u8 = 8;
pub const ARM64_CONTEXT: u8 = 27;
pub const ARM64_WINDOW: u8 = 28;

/// Complete fixed-size host snapshot. `flags` is ARM64 PSTATE/NZCV;
/// it is not silently interpreted as guest EFLAGS. Unsupported extended state
/// must be rejected by the platform adapter before claiming the exception.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct HostContextSnapshot {
    pub gpr: [u64; 32],
    pub sp: u64,
    pub pc: u64,
    pub flags: u64,
    pub simd: [[u8; 16]; 32],
    pub fpcr: u32,
    pub fpsr: u32,
    pub gpr_count: u32,
    pub simd_count: u32,
    /// 2 = aarch64. Zero is not a valid captured context.
    pub architecture: u32,
}

/// A recoverable value, never an instruction to reread mutable guest memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryValue {
    /// Canonical guest GPR at the last explicitly materialized checkpoint.
    GuestRegister(u8),
    /// Low 32 bits of a register in the captured host context.
    HostRegister(u8),
    Constant(u32),
    /// Canonical guest EFLAGS at the materialized checkpoint.
    GuestFlags,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryAccess {
    Read,
    Write,
}

/// A fault cannot be repaired by replaying an already visible store prefix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreCommitProof {
    /// Reads only: the faulting instruction has not committed its destination.
    LoadBeforeCommit,
    /// A naturally aligned scalar store, wholly inside one stable host page.
    /// The publisher must prove private pinned backing and all-or-none hardware
    /// behavior for the advertised architecture, width and instruction form.
    AlignedPrivateScalar,
}

/// Immutable, exact-machine-PC recovery metadata, owned alongside native code.
/// The next PC is used only after the one faulting access succeeds. Prefix
/// stores and arithmetic are recovered once, never replayed as an entire block.
#[derive(Clone, Copy, Debug)]
pub struct NativeRecoveryPoint {
    pub host_pc: usize,
    pub recovery_pc: usize,
    pub guest_pc: u32,
    pub next_pc: u32,
    pub width: u8,
    pub access: RecoveryAccess,
    pub proof: StoreCommitProof,
    pub registers: [RecoveryValue; 8],
    pub flags: RecoveryValue,
    pub address: RecoveryValue,
    pub value: RecoveryValue,
    /// Loads replace this GPR only after the checked read succeeds.
    pub destination: Option<u8>,
    pub committed_units: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::{offset_of, size_of};

    // spec: A64-MEM-9
    #[test]
    fn entry_frame_and_signal_snapshot_have_fixed_c_layouts() {
        assert_eq!(offset_of!(HostContextSnapshot, sp), 256);
        assert_eq!(offset_of!(HostContextSnapshot, simd), 280);
        assert_eq!(offset_of!(HostContextSnapshot, fpcr), 792);
        assert_eq!(offset_of!(HostContextSnapshot, architecture), 808);
        assert_eq!(size_of::<HostContextSnapshot>(), 816);
        assert!(!std::mem::needs_drop::<HostContextSnapshot>());
    }
}

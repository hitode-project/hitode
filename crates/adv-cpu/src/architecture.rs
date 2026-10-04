//! Guest architectural state, independent of scheduling and OS context formats.
use crate::{CpuState, X87State};
use thiserror::Error;

/// A paused x86-32 architectural snapshot. Register order follows x86 encoding;
/// selectors are ES, CS, SS, DS, FS, GS. x87 registers/tags are physical R0..R7.
/// MMX aliases share the canonical x87 payload, rather than a second register set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArchitecturalState {
    pub regs: [u32; 8],
    pub eip: u32,
    pub eflags: u32,
    pub selectors: [u16; 6],
    pub fs_base: u32,
    pub x87: X87State,
    pub xmm: [[u8; 16]; 8],
    pub mxcsr: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum StateError {
    #[error("backend does not support architectural state access")]
    Unsupported,
    #[error("architectural restore cannot change flat segment selectors")]
    UnsupportedSegments,
    #[error("architectural restore cannot change the FS base")]
    UnsupportedFsBase,
    #[error("architectural restore changes unsupported EFLAGS bits: {0:#x}")]
    UnsupportedFlags(u32),
    #[error("architectural restore changes unsupported x87 control bits: {0:#x}")]
    UnsupportedX87Control(u16),
    #[error("MXCSR contains reserved bits: {0:#x}")]
    InvalidMxcsr(u32),
    #[error("x87 opcode contains reserved bits: {0:#x}")]
    InvalidX87Opcode(u16),
    #[error("backend architectural state access failed: {0}")]
    Backend(String),
}

impl ArchitecturalState {
    // Arithmetic flags, TF, IF, DF and ID. Other bits remain opaque at this
    // boundary; a roundtrip preserves them, but cannot switch CPU modes.
    pub const WRITABLE_EFLAGS: u32 = 0x0020_0fd5;
    // Exception masks, precision, rounding and legacy infinity control. Reserved
    // bits stay as captured; full restore is not FLDCW's status-mask operation.
    pub const WRITABLE_X87_CONTROL: u16 = 0x1f3f;

    /// Validate the entire candidate before any register/cache is changed.
    /// Descriptor loading and hidden FS-base transactions are not implemented.
    pub fn validate_restore(&self, current: &Self) -> Result<(), StateError> {
        if self.mxcsr & !0xffff != 0 {
            return Err(StateError::InvalidMxcsr(self.mxcsr));
        }
        if self.x87.opcode & !0x7ff != 0 {
            return Err(StateError::InvalidX87Opcode(self.x87.opcode));
        }
        if self.selectors != current.selectors {
            return Err(StateError::UnsupportedSegments);
        }
        if self.fs_base != current.fs_base {
            return Err(StateError::UnsupportedFsBase);
        }
        let flags = (self.eflags ^ current.eflags) & !Self::WRITABLE_EFLAGS;
        if flags != 0 {
            return Err(StateError::UnsupportedFlags(flags));
        }
        let control = (self.x87.control ^ current.x87.control) & !Self::WRITABLE_X87_CONTROL;
        if control != 0 {
            return Err(StateError::UnsupportedX87Control(control));
        }
        Ok(())
    }
}

impl CpuState {
    /// Materialize floating aliases only at an explicit architectural boundary.
    pub fn capture_architecture(&mut self) -> ArchitecturalState {
        self.commit_mmx();
        ArchitecturalState {
            regs: self.regs,
            eip: self.eip,
            eflags: self.eflags,
            selectors: self.segments.selectors,
            fs_base: self.fs_base,
            x87: self.x87.clone(),
            xmm: self.xmm,
            mxcsr: self.mxcsr,
        }
    }

    /// Restore only architecture. Fuel, REP continuation, fault/provenance,
    /// descriptor identity and every executor control stay with their owner.
    pub fn restore_architecture(&mut self, state: &ArchitecturalState) -> Result<(), StateError> {
        // Validate against a canonical clone: failure must not even materialize
        // a pending MMX cache in the live CpuState.
        let current = self.clone().capture_architecture();
        state.validate_restore(&current)?;
        self.regs = state.regs;
        self.eip = state.eip;
        self.eflags = state.eflags;
        self.x87 = state.x87.clone();
        self.xmm = state.xmm;
        self.mxcsr = state.mxcsr;
        self.mmx_mode = 0;
        self.mmx = self.x87.regs.map(crate::X87Register::significand);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{IndirectBranch, IndirectBranchKind, X87Register};

    fn pending() -> CpuState {
        let mut cpu = CpuState {
            regs: [1, 2, 3, 4, 5, 6, 7, 8],
            eip: 0x12345,
            eflags: 0x202,
            fuel: 0xabcdef,
            fault_addr: 0x12345678,
            fs_base: 0x70000,
            ..CpuState::default()
        };
        cpu.segments.gdt_base = 0x50000;
        cpu.segments.gdt_limit = 127;
        cpu.last_indirect_branch = IndirectBranch {
            source: 0x1234,
            target: 0x9876,
            kind: IndirectBranchKind::Return,
        };
        crate::interpreter::RepContinuation::commit_native_compare(&mut cpu, 6, true);
        cpu.mmx[3] = 0x1234567890abcdef;
        cpu.mmx_mode = 1 | 4 | (1 << 11);
        cpu
    }
    #[test]
    fn capture_materializes_only_architectural_mmx_aliases() {
        let mut cpu = pending();
        let before = cpu.clone();
        let snapshot = cpu.capture_architecture();
        assert_eq!(
            snapshot.x87.regs[3],
            X87Register::from_parts(before.mmx[3], 0xffff)
        );
        assert_eq!(snapshot.x87.tag, 0);
        assert_eq!(snapshot.x87.top(), 0);
        assert_eq!(cpu.mmx_mode, 0);
        assert_eq!(cpu.fuel, before.fuel);
        assert_eq!(cpu.rep, before.rep);
        assert_eq!(cpu.fault_addr, before.fault_addr);
        assert_eq!(cpu.last_indirect_branch, before.last_indirect_branch);
    }
    #[test]
    fn complete_restore_preserves_raw_payload_independent_tags_status_and_metadata() {
        let mut cpu = pending();
        let before = cpu.clone();
        let mut snapshot = cpu.clone().capture_architecture();
        snapshot.regs = [11, 12, 13, 14, 15, 16, 17, 18];
        snapshot.eip = 0x54321;
        snapshot.eflags = 0x00200ad7;
        snapshot.x87.control = 0x077f;
        snapshot.x87.status = 0x8581;
        snapshot.x87.tag = 0xaaff;
        snapshot.x87.opcode = 0x7ff;
        snapshot.x87.ip = 0x12345678;
        snapshot.x87.dp = 0x88776655;
        snapshot.x87.cs = 0x23;
        snapshot.x87.ds = 0x2b;
        snapshot.x87.regs =
            std::array::from_fn(|i| X87Register::from_parts(0xa000000000000000 | i as u64, 0x7fff));
        snapshot.xmm = std::array::from_fn(|i| std::array::from_fn(|j| (i * 16 + j) as u8));
        snapshot.mxcsr = 0x5f80;
        cpu.restore_architecture(&snapshot).unwrap();
        assert_eq!(cpu.clone().capture_architecture(), snapshot);
        assert_eq!(cpu.mmx_mode, 0);
        assert_eq!(cpu.mmx, snapshot.x87.regs.map(X87Register::significand));
        assert_eq!(cpu.fuel, before.fuel);
        assert_eq!(cpu.rep, before.rep);
        assert_eq!(cpu.segments, before.segments);
        assert_eq!(cpu.fault_addr, before.fault_addr);
        assert_eq!(cpu.last_indirect_branch, before.last_indirect_branch);
        assert_eq!(
            cpu.x87.status, 0x8581,
            "full restore is not FLDCW mask recomputation"
        );
    }
    #[test]
    fn every_invalid_field_rejects_the_entire_candidate_without_committing_aliases() {
        for field in 0..6 {
            let mut cpu = pending();
            let before = cpu.clone();
            let mut snapshot = cpu.clone().capture_architecture();
            snapshot.regs[0] = 999;
            snapshot.eip = 999;
            match field {
                0 => snapshot.mxcsr |= 0x10000,
                1 => snapshot.x87.opcode |= 0x800,
                2 => snapshot.selectors[1] ^= 8,
                3 => snapshot.fs_base ^= 0x1000,
                4 => snapshot.eflags ^= 1 << 17,
                5 => snapshot.x87.control ^= 1 << 15,
                _ => unreachable!(),
            }
            assert!(cpu.restore_architecture(&snapshot).is_err());
            assert_eq!(cpu, before);
        }
    }
    #[test]
    fn opaque_bits_already_present_can_roundtrip_without_a_mode_switch() {
        let mut cpu = CpuState::default();
        cpu.eflags |= 0x80003000;
        cpu.x87.control |= 0x8080;
        let snapshot = cpu.capture_architecture();
        cpu.restore_architecture(&snapshot).unwrap();
        assert_eq!(cpu.capture_architecture(), snapshot);
    }
}

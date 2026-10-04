//! Architectural access through the reference engine. ST registers are logical
//! in Unicorn; the public snapshot stores physical x87 registers and full tags.
use super::*;
use adv_cpu::{ArchitecturalState, StateError, X87Register, X87State};

fn error(e: uc_error) -> StateError {
    StateError::Backend(e.to_string())
}

impl Engine<'_> {
    pub(crate) fn capture_state(&mut self) -> Result<ArchitecturalState, StateError> {
        let mut state = ArchitecturalState {
            regs: [0; 8],
            eip: self.reg_read(RegisterX86::EIP).map_err(error)? as u32,
            eflags: self.reg_read(RegisterX86::EFLAGS).map_err(error)? as u32,
            selectors: [0; 6],
            fs_base: self.reg_read(RegisterX86::FS_BASE).map_err(error)? as u32,
            x87: X87State::default(),
            xmm: [[0; 16]; 8],
            mxcsr: self.reg_read(RegisterX86::MXCSR).map_err(error)? as u32,
        };
        for (out, reg) in state.regs.iter_mut().zip([
            RegisterX86::EAX,
            RegisterX86::ECX,
            RegisterX86::EDX,
            RegisterX86::EBX,
            RegisterX86::ESP,
            RegisterX86::EBP,
            RegisterX86::ESI,
            RegisterX86::EDI,
        ]) {
            *out = self.reg_read(reg).map_err(error)? as u32;
        }
        for (out, reg) in state.selectors.iter_mut().zip([
            RegisterX86::ES,
            RegisterX86::CS,
            RegisterX86::SS,
            RegisterX86::DS,
            RegisterX86::FS,
            RegisterX86::GS,
        ]) {
            *out = self.reg_read(reg).map_err(error)? as u16;
        }
        state.x87.control = self.reg_read(RegisterX86::FPCW).map_err(error)? as u16;
        state.x87.status = self.reg_read(RegisterX86::FPSW).map_err(error)? as u16;
        state.x87.tag = self.reg_read(RegisterX86::FPTAG).map_err(error)? as u16;
        state.x87.opcode = self.reg_read(RegisterX86::FOP).map_err(error)? as u16;
        state.x87.ip = u32::try_from(self.reg_read(RegisterX86::FIP).map_err(error)?)
            .map_err(|_| StateError::Backend("x87 IP is outside x86-32".into()))?;
        state.x87.dp = u32::try_from(self.reg_read(RegisterX86::FDP).map_err(error)?)
            .map_err(|_| StateError::Backend("x87 DP is outside x86-32".into()))?;
        state.x87.cs = self.reg_read(RegisterX86::FCS).map_err(error)? as u16;
        state.x87.ds = self.reg_read(RegisterX86::FDS).map_err(error)? as u16;
        for logical in 0..8 {
            let bytes = self
                .reg_read_long(RegisterX86::ST0 as i32 + logical as i32)
                .map_err(error)?;
            let physical = state.x87.physical_index(logical);
            state.x87.regs[physical] = X87Register(
                bytes
                    .as_ref()
                    .try_into()
                    .map_err(|_| StateError::Backend("bad x87 register width".into()))?,
            );
            let bytes = self
                .reg_read_long(RegisterX86::XMM0 as i32 + logical as i32)
                .map_err(error)?;
            state.xmm[logical].copy_from_slice(&bytes);
        }
        Ok(state)
    }

    pub(crate) fn restore_state(&mut self, state: &ArchitecturalState) -> Result<(), StateError> {
        state.validate_restore(&self.capture_state()?)?;
        // QEMU stores only empty/nonempty tag bits and reclassifies nonempty
        // payloads on reads. It cannot preserve an intentionally inconsistent
        // full tag. Reject that reference limitation before touching a register.
        for physical in 0..8 {
            let tag = state.x87.physical_tag(physical);
            if tag != 3 && tag != state.x87.regs[physical].tag() {
                return Err(StateError::Backend(
                    "reference engine cannot preserve payload-independent x87 tags".into(),
                ));
            }
        }
        let previous = self.context_init().map_err(error)?;
        if let Err(failure) = self.write_state(state) {
            self.context_restore(&previous).map_err(|rollback| {
                StateError::Backend(format!(
                    "{failure}; architectural rollback failed: {rollback}"
                ))
            })?;
            return Err(failure);
        }
        Ok(())
    }
    fn write_state(&mut self, state: &ArchitecturalState) -> Result<(), StateError> {
        for (value, reg) in state.regs.iter().zip([
            RegisterX86::EAX,
            RegisterX86::ECX,
            RegisterX86::EDX,
            RegisterX86::EBX,
            RegisterX86::ESP,
            RegisterX86::EBP,
            RegisterX86::ESI,
            RegisterX86::EDI,
        ]) {
            self.reg_write(reg, u64::from(*value)).map_err(error)?;
        }
        self.reg_write(RegisterX86::EIP, u64::from(state.eip))
            .map_err(error)?;
        self.reg_write(RegisterX86::EFLAGS, u64::from(state.eflags))
            .map_err(error)?;
        self.reg_write(RegisterX86::FPCW, u64::from(state.x87.control))
            .map_err(error)?;
        self.reg_write(RegisterX86::FPSW, u64::from(state.x87.status))
            .map_err(error)?;
        self.reg_write(RegisterX86::FOP, u64::from(state.x87.opcode))
            .map_err(error)?;
        self.reg_write(RegisterX86::FIP, u64::from(state.x87.ip))
            .map_err(error)?;
        self.reg_write(RegisterX86::FDP, u64::from(state.x87.dp))
            .map_err(error)?;
        self.reg_write(RegisterX86::FCS, u64::from(state.x87.cs))
            .map_err(error)?;
        self.reg_write(RegisterX86::FDS, u64::from(state.x87.ds))
            .map_err(error)?;
        for logical in 0..8 {
            self.reg_write_long(
                RegisterX86::ST0 as i32 + logical as i32,
                &state.x87.regs[state.x87.physical_index(logical)].0,
            )
            .map_err(error)?;
            self.reg_write_long(
                RegisterX86::XMM0 as i32 + logical as i32,
                &state.xmm[logical],
            )
            .map_err(error)?;
        }
        self.reg_write(RegisterX86::FPTAG, u64::from(state.x87.tag))
            .map_err(error)?;
        self.reg_write(RegisterX86::MXCSR, u64::from(state.mxcsr))
            .map_err(error)?;
        Ok(())
    }
}

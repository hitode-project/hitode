//! Explicit Unicorn adapter for executor-independent architectural state.
//! Unicorn's general reg_read_long allocates a Box for each register read.
use adv_cpu::{CpuState, X87Register, X87State};
use unicorn_engine::{
    RegisterX86 as R, Unicorn,
    unicorn_const::{Arch, uc_error},
};

#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct RegisterBytes([u8; 16]);

#[cfg(test)]
fn read<D>(uc: &Unicorn<'_, D>, register: R) -> Result<[u8; 16], uc_error> {
    if uc.get_arch() != Arch::X86 {
        return Err(uc_error::ARCH);
    }
    // This helper never accepts wider registers. In x86 mode, ST is 10 bytes
    // and XMM is 16 bytes; all other register layouts stay with the public API.
    if !matches!(
        register,
        R::ST0
            | R::ST1
            | R::ST2
            | R::ST3
            | R::ST4
            | R::ST5
            | R::ST6
            | R::ST7
            | R::FP0
            | R::FP1
            | R::FP2
            | R::FP3
            | R::FP4
            | R::FP5
            | R::FP6
            | R::FP7
            | R::XMM0
            | R::XMM1
            | R::XMM2
            | R::XMM3
            | R::XMM4
            | R::XMM5
            | R::XMM6
            | R::XMM7
    ) {
        return Err(uc_error::ARG);
    }
    let mut bytes = RegisterBytes([0; 16]);
    // SAFETY: uc owns a live x86 engine; the validated register writes at most
    // 16 bytes into aligned storage. The engine does not retain the pointer.
    let error = unsafe {
        unicorn_engine::uc_reg_read(
            uc.get_handle(),
            register as i32,
            bytes.0.as_mut_ptr().cast(),
        )
    };
    if error == uc_error::OK {
        Ok(bytes.0)
    } else {
        Err(error)
    }
}

#[cfg(test)]
fn read_st<D>(uc: &Unicorn<'_, D>, register: R) -> Result<[u8; 10], uc_error> {
    let bytes = read(uc, register)?;
    Ok(bytes[..10].try_into().unwrap())
}

#[cfg(test)]
fn read_xmm<D>(uc: &Unicorn<'_, D>, register: R) -> Result<[u8; 16], uc_error> {
    read(uc, register)
}

const FP: [R; 8] = [
    R::FP0,
    R::FP1,
    R::FP2,
    R::FP3,
    R::FP4,
    R::FP5,
    R::FP6,
    R::FP7,
];

const FPU_REGISTERS: [R; 25] = [
    R::FP0,
    R::FP1,
    R::FP2,
    R::FP3,
    R::FP4,
    R::FP5,
    R::FP6,
    R::FP7,
    R::FPCW,
    R::FPSW,
    R::FPTAG,
    R::FOP,
    R::FIP,
    R::FDP,
    R::FCS,
    R::FDS,
    R::MXCSR,
    R::XMM0,
    R::XMM1,
    R::XMM2,
    R::XMM3,
    R::XMM4,
    R::XMM5,
    R::XMM6,
    R::XMM7,
];

fn read_fpu<D>(uc: &Unicorn<'_, D>) -> Result<[RegisterBytes; 25], uc_error> {
    if uc.get_arch() != Arch::X86 {
        return Err(uc_error::ARCH);
    }
    let mut register_ids = FPU_REGISTERS.map(|register| register as i32);
    let mut bytes = [RegisterBytes([0; 16]); 25];
    let mut addresses: [*mut std::ffi::c_void; 25] =
        std::array::from_fn(|slot| bytes[slot].0.as_mut_ptr().cast());
    // SAFETY: the fixed list contains only x86 registers at most sixteen bytes
    // wide. Each independent aligned stack slot remains alive for this call;
    // neither the engine nor the C batch API retains these pointers. Unlike the
    // public Rust batch helper this does not allocate vectors or boxes.
    let error = unsafe {
        unicorn_engine::uc_reg_read_batch(
            uc.get_handle(),
            register_ids.as_mut_ptr(),
            addresses.as_mut_ptr(),
            register_ids.len() as i32,
        )
    };
    if error == uc_error::OK {
        Ok(bytes)
    } else {
        Err(error)
    }
}

impl RegisterBytes {
    fn u16(self) -> u16 {
        u16::from_ne_bytes(self.0[..2].try_into().unwrap())
    }
    fn u32(self) -> u32 {
        u32::from_ne_bytes(self.0[..4].try_into().unwrap())
    }
    fn u64(self) -> u64 {
        u64::from_ne_bytes(self.0[..8].try_into().unwrap())
    }
}

/// Last architectural state explicitly synchronized with the transition engine.
/// This is a disposable adapter cache, never part of a guest thread context.
#[derive(Default)]
pub(super) struct FpuAdapter {
    x87: Option<X87State>,
    mxcsr: Option<u32>,
}

impl FpuAdapter {
    pub(super) fn export<D>(
        &mut self,
        uc: &mut Unicorn<'_, D>,
        state: &mut CpuState,
    ) -> Result<(), uc_error> {
        state.commit_mmx();
        let current = &state.x87;
        let previous = self.x87.as_ref();
        // FPn addresses physical slots, so TOP changes cannot rotate stored bits.
        for (slot, reg) in FP.into_iter().enumerate() {
            if previous.is_none_or(|p| p.regs[slot] != current.regs[slot]) {
                let mut bytes = RegisterBytes([0; 16]);
                bytes.0[..10].copy_from_slice(&current.regs[slot].0);
                uc.reg_write_long(reg, &bytes.0)?;
            }
        }
        let fields = [
            (
                R::FPCW,
                u64::from(current.control),
                previous.map(|p| u64::from(p.control)),
            ),
            (
                R::FPSW,
                u64::from(current.status),
                previous.map(|p| u64::from(p.status)),
            ),
            (
                R::FPTAG,
                u64::from(current.tag),
                previous.map(|p| u64::from(p.tag)),
            ),
            (
                R::FOP,
                u64::from(current.opcode),
                previous.map(|p| u64::from(p.opcode)),
            ),
            (
                R::FIP,
                u64::from(current.ip),
                previous.map(|p| u64::from(p.ip)),
            ),
            (
                R::FDP,
                u64::from(current.dp),
                previous.map(|p| u64::from(p.dp)),
            ),
            (
                R::FCS,
                u64::from(current.cs),
                previous.map(|p| u64::from(p.cs)),
            ),
            (
                R::FDS,
                u64::from(current.ds),
                previous.map(|p| u64::from(p.ds)),
            ),
        ];
        for (reg, value, old) in fields {
            if old != Some(value) {
                uc.reg_write(reg, value)?;
            }
        }
        if self.mxcsr != Some(state.mxcsr) {
            uc.reg_write(R::MXCSR, u64::from(state.mxcsr))?;
        }
        self.x87 = Some(current.clone());
        self.mxcsr = Some(state.mxcsr);
        Ok(())
    }

    pub(super) fn import<D>(
        &mut self,
        uc: &Unicorn<'_, D>,
        state: &mut CpuState,
    ) -> Result<(), uc_error> {
        let bytes = read_fpu(uc)?;
        let mut next = X87State {
            control: bytes[8].u16(),
            status: bytes[9].u16(),
            tag: bytes[10].u16(),
            opcode: bytes[11].u16(),
            ip: bytes[12].u64() as u32,
            dp: bytes[13].u64() as u32,
            cs: bytes[14].u16(),
            ds: bytes[15].u16(),
            ..X87State::default()
        };
        for (slot, raw) in bytes[..8].iter().enumerate() {
            next.regs[slot] = X87Register(raw.0[..10].try_into().unwrap());
            // Unicorn stores only empty/nonempty tags and reconstructs numeric
            // classification on read. Preserve exact explicit tags of untouched
            // slots (notably MMX's all-valid tag) across an integer handoff.
            if let Some(previous) = self.x87.as_ref()
                && next.regs[slot] == previous.regs[slot]
                && (next.physical_tag(slot) == 3) == (previous.physical_tag(slot) == 3)
            {
                next.set_physical_tag(slot, previous.physical_tag(slot));
            }
        }
        let mxcsr = bytes[16].u32();
        state.x87 = next.clone();
        state.mxcsr = mxcsr;
        for (value, raw) in state.xmm.iter_mut().zip(&bytes[17..]) {
            *value = raw.0;
        }
        state.mmx_mode = 0;
        state.refresh_mmx();
        self.x87 = Some(next);
        self.mxcsr = Some(mxcsr);
        Ok(())
    }
}

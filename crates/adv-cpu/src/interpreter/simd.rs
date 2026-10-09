//! Predecoded SIMD moves and integer kernels with checked, narrow commits.
use super::*;
use crate::floating::packed::{self, Kernel};

#[derive(Clone, Copy)]
enum Location {
    Xmm(u8),
    Mmx(u8),
    Gpr(u8),
    Memory,
    Immediate(u8),
}

impl Location {
    fn decode(i: &Instruction, operand: u32) -> Option<Self> {
        if i.op_kind(operand) == OpKind::Memory {
            return Some(Self::Memory);
        }
        if i.op_kind(operand) == OpKind::Immediate8 {
            return Some(Self::Immediate(i.immediate8()));
        }
        let r = i.op_register(operand);
        let n = r as u32;
        if (R::XMM0 as u32..=R::XMM7 as u32).contains(&n) {
            Some(Self::Xmm((n - R::XMM0 as u32) as u8))
        } else if (R::MM0 as u32..=R::MM7 as u32).contains(&n) {
            Some(Self::Mmx((n - R::MM0 as u32) as u8))
        } else {
            Reg::decode(r)
                .filter(|r| r.width == 4)
                .map(|r| Self::Gpr(r.index))
        }
    }

    fn vector(self) -> bool {
        matches!(self, Self::Xmm(_) | Self::Mmx(_))
    }
}

#[derive(Clone, Copy)]
pub(super) struct Plan {
    kernel: Option<Kernel>,
    address: Address,
    destination: Location,
    source: Location,
    pub(super) next: u32,
    read_width: u8,
    write_width: u8,
    aligned: bool,
    mmx: bool,
}

impl Plan {
    pub(super) fn decode(i: &Instruction) -> Option<Self> {
        if i.has_lock_prefix() || i.segment_prefix() == R::GS {
            return None;
        }
        let m = i.mnemonic();
        let kernel = packed::kernel(m);
        if kernel.is_none()
            && !matches!(
                m,
                M::Movaps
                    | M::Movapd
                    | M::Movups
                    | M::Movupd
                    | M::Movdqa
                    | M::Movdqu
                    | M::Movntdq
                    | M::Movntps
                    | M::Movntpd
                    | M::Movntq
                    | M::Movd
                    | M::Movq
            )
        {
            return None;
        }
        let destination = Location::decode(i, 0)?;
        let source = Location::decode(i, 1)?;
        let mmx = matches!(destination, Location::Mmx(_)) || matches!(source, Location::Mmx(_));
        let width = if mmx { 8 } else { 16 };
        if kernel.is_some() && !destination.vector() {
            return None;
        }
        if kernel.is_none() && matches!(source, Location::Immediate(_)) {
            return None;
        }
        if matches!(destination, Location::Immediate(_)) {
            return None;
        }
        let read_width = match m {
            M::Movd => 4,
            M::Movq => 8,
            _ => width,
        };
        let write_width = if destination.vector() {
            width
        } else {
            read_width
        };
        let has_memory =
            matches!(source, Location::Memory) || matches!(destination, Location::Memory);
        let address = if has_memory {
            Address::decode(i, false)?
        } else {
            Address {
                base: None,
                index: None,
                scale: 1,
                displacement: 0,
                word: false,
                fs: false,
            }
        };
        Some(Self {
            kernel,
            address,
            destination,
            source,
            next: i.next_ip32(),
            read_width,
            write_width,
            aligned: has_memory
                && i.memory_size().size() == 16
                && !matches!(m, M::Movups | M::Movupd | M::Movdqu),
            mmx,
        })
    }

    fn read(
        self,
        from: Location,
        width: u8,
        address: u32,
        state: &CpuState,
        memory: &Memory,
    ) -> Result<[u8; 16], Step> {
        let mut bytes = [0; 16];
        match from {
            Location::Xmm(n) => {
                bytes[..width as usize].copy_from_slice(&state.xmm[n as usize][..width as usize])
            }
            Location::Mmx(n) => bytes[..8].copy_from_slice(&state.mmx[n as usize].to_le_bytes()),
            Location::Gpr(n) => bytes[..4].copy_from_slice(&state.regs[n as usize].to_le_bytes()),
            Location::Immediate(n) => bytes[0] = n,
            Location::Memory => {
                let len = width as usize;
                if let Some(pointer) = memory.direct(address, len, false) {
                    // SAFETY: direct() validated an ordinary readable range;
                    // the shared Memory borrow excludes writes and remapping.
                    unsafe { std::ptr::copy_nonoverlapping(pointer, bytes.as_mut_ptr(), len) };
                } else {
                    crate::floating::check_access(memory, state.eip, address, len, false)?;
                    memory
                        .read_into(address, &mut bytes[..len])
                        .map_err(|_| Step::ReadFault {
                            pc: state.eip,
                            address,
                        })?;
                }
            }
        }
        Ok(bytes)
    }

    pub(super) fn execute(self, state: &mut CpuState, memory: &mut Memory) -> Step {
        let pc = state.eip;
        // MMX checks the pending x87 exception before alignment or memory.
        if self.mmx && state.x87.status & 0x80 != 0 {
            return Step::Trap { pc, vector: 16 };
        }
        let address = self.address.resolve_regs(&state.regs, state.fs_base);
        if self.aligned && address & 15 != 0 {
            return Step::Trap { pc, vector: 13 };
        }
        let right = match self.read(self.source, self.read_width, address, state, memory) {
            Ok(value) => value,
            Err(step) => return step,
        };
        let value = if let Some(kernel) = self.kernel {
            let left = match self.read(self.destination, self.write_width, address, state, memory) {
                Ok(value) => value,
                Err(step) => return step,
            };
            kernel(left, right)
        } else {
            right
        };
        if matches!(self.destination, Location::Memory) {
            let len = self.write_width as usize;
            if let Some(pointer) = memory.direct(address, len, true) {
                // SAFETY: the complete ordinary writable range was checked;
                // the exclusive borrow excludes aliases, revocation and code.
                unsafe { std::ptr::copy_nonoverlapping(value.as_ptr(), pointer, len) };
            } else {
                if let Err(step) = crate::floating::check_access(memory, pc, address, len, true) {
                    return step;
                }
                if memory.write(address, &value[..len]).is_err() {
                    return Step::WriteFault { pc, address };
                }
            }
        }
        // No operation below can fault. Canonicalize imported native MMX state
        // only after memory commits, so failures preserve the whole CPU image.
        state.commit_mmx();
        match self.destination {
            Location::Xmm(n) => state.xmm[n as usize] = value,
            Location::Mmx(n) => {
                let bits = u64::from_le_bytes(value[..8].try_into().unwrap());
                state.mmx[n as usize] = bits;
                state.x87.regs[n as usize] = crate::X87Register::from_parts(bits, 0xffff);
            }
            Location::Gpr(n) => {
                state.regs[n as usize] = u32::from_le_bytes(value[..4].try_into().unwrap())
            }
            Location::Memory => {}
            Location::Immediate(_) => unreachable!("validated writable SIMD destination"),
        }
        if self.mmx {
            state.x87.set_top(0);
            state.x87.tag = 0;
        }
        state.refresh_mmx();
        state.eip = self.next;
        Step::Executed
    }
}

#[cfg(test)]
#[path = "simd_tests.rs"]
mod tests;

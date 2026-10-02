//! Transactional x87, SSE and packed-integer execution on canonical CPU state.
use crate::{CpuState, Memory, PAGE_SIZE, READ, WRITE, interpreter::Step};
use iced_x86::{Instruction, Register as R};
mod simd;
mod softfloat;
mod transcendental;
mod x87;

pub(crate) fn execute(i: &Instruction, state: &mut CpuState, memory: &mut Memory) -> Option<Step> {
    if i.has_lock_prefix() || i.segment_prefix() == R::GS {
        return None;
    }
    if !x87::supported(i) && !simd::supported(i) {
        return None;
    }
    let pc = state.eip;
    let mut tx = Transaction {
        state: state.clone(),
        pc,
        write: None,
        x87_exception: false,
        sse_exception: false,
    };
    tx.state.commit_mmx();
    let result = if x87::supported(i) {
        x87::execute(i, &mut tx, memory)
    } else {
        simd::execute(i, &mut tx, memory)
    };
    if let Err(result) = result {
        return Some(result);
    }
    if tx.sse_exception {
        state.mxcsr = tx.state.mxcsr;
        return Some(Step::Trap { pc, vector: 19 });
    }
    if tx.x87_exception {
        // x87 arithmetic records an unmasked exception and suppresses the
        // result/pop/store. The next waiting instruction delivers #MF.
        state.commit_mmx();
        state.x87.status = (state.x87.status & !0x82ff) | (tx.state.x87.status & 0x82ff);
        state.x87.ip = tx.state.x87.ip;
        state.x87.dp = tx.state.x87.dp;
        state.x87.cs = tx.state.x87.cs;
        state.x87.ds = tx.state.x87.ds;
        state.x87.opcode = tx.state.x87.opcode;
        state.eip = i.next_ip32();
        return Some(Step::Executed);
    }
    if let Some(write) = tx.write {
        let bytes = if let Some(bytes) = write.large.as_deref() {
            bytes
        } else {
            &write.small[..write.len]
        };
        if let Some(mask) = write.mask {
            for (offset, byte) in bytes.iter().enumerate() {
                if mask & (1 << offset) != 0 {
                    // Every selected byte was checked before any store. Masked
                    // bytes have no memory access, including across page edges.
                    if memory
                        .write(write.address + offset as u32, &[*byte])
                        .is_err()
                    {
                        return Some(Step::WriteFault {
                            pc,
                            address: write.address + offset as u32,
                        });
                    }
                }
            }
        } else if memory.write(write.address, bytes).is_err() {
            return Some(Step::WriteFault {
                pc,
                address: write.address,
            });
        }
    }
    tx.state.eip = i.next_ip32();
    tx.state.refresh_mmx();
    *state = tx.state;
    Some(Step::Executed)
}

struct PendingWrite {
    address: u32,
    len: usize,
    small: [u8; 16],
    large: Option<Vec<u8>>,
    mask: Option<u16>,
}
struct Transaction {
    state: CpuState,
    pc: u32,
    write: Option<PendingWrite>,
    x87_exception: bool,
    sse_exception: bool,
}

impl Transaction {
    fn address(&self, i: &Instruction) -> Result<u32, Step> {
        let register = |r| {
            reg_index(r)
                .map(|slot| self.state.regs[slot] & if r.size() == 2 { 0xffff } else { u32::MAX })
        };
        let mut address = i.memory_displacement32();
        if i.memory_base() != R::None {
            address = address.wrapping_add(register(i.memory_base()).ok_or(Step::Unsupported {
                pc: self.pc,
                code: i.code(),
            })?);
        }
        if i.memory_index() != R::None {
            address = address.wrapping_add(
                register(i.memory_index())
                    .ok_or(Step::Unsupported {
                        pc: self.pc,
                        code: i.code(),
                    })?
                    .wrapping_mul(i.memory_index_scale()),
            );
        }
        if i.memory_base().size() == 2 || i.memory_index().size() == 2 {
            address &= 0xffff;
        }
        if i.segment_prefix() == R::FS {
            address = address.wrapping_add(self.state.fs_base);
        }
        Ok(address)
    }
    fn check(&self, memory: &Memory, address: u32, len: usize, write: bool) -> Result<(), Step> {
        let fault = |address| {
            if write {
                Step::WriteFault {
                    pc: self.pc,
                    address,
                }
            } else {
                Step::ReadFault {
                    pc: self.pc,
                    address,
                }
            }
        };
        if u64::from(address) + len as u64 > 1 << 32 {
            return Err(fault(address));
        }
        let mut offset = 0;
        while offset < len {
            let at = address + offset as u32;
            let range = memory.committed(at, 1).map_err(|_| fault(at))?;
            if memory.perms[range.start / PAGE_SIZE] & if write { WRITE } else { READ } == 0 {
                return Err(fault(at));
            }
            offset += (PAGE_SIZE - at as usize % PAGE_SIZE).min(len - offset);
        }
        Ok(())
    }
    fn read<'a>(&self, memory: &'a Memory, address: u32, len: usize) -> Result<&'a [u8], Step> {
        self.check(memory, address, len, false)?;
        memory.read(address, len).map_err(|_| Step::ReadFault {
            pc: self.pc,
            address,
        })
    }
    fn store(&mut self, memory: &Memory, address: u32, bytes: &[u8]) -> Result<(), Step> {
        self.check(memory, address, bytes.len(), true)?;
        let mut small = [0; 16];
        let large = if bytes.len() <= 16 {
            small[..bytes.len()].copy_from_slice(bytes);
            None
        } else {
            Some(bytes.to_vec())
        };
        self.write = Some(PendingWrite {
            address,
            len: bytes.len(),
            small,
            large,
            mask: None,
        });
        Ok(())
    }
    fn masked_store(
        &mut self,
        memory: &Memory,
        address: u32,
        bytes: &[u8],
        mask: u16,
    ) -> Result<(), Step> {
        for offset in 0..bytes.len() {
            if mask & (1 << offset) != 0 {
                let at = address.checked_add(offset as u32).ok_or(Step::WriteFault {
                    pc: self.pc,
                    address,
                })?;
                self.check(memory, at, 1, true)?;
            }
        }
        let mut small = [0; 16];
        small[..bytes.len()].copy_from_slice(bytes);
        self.write = Some(PendingWrite {
            address,
            len: bytes.len(),
            small,
            large: None,
            mask: Some(mask),
        });
        Ok(())
    }
    fn unsupported(&self, i: &Instruction) -> Step {
        Step::Unsupported {
            pc: self.pc,
            code: i.code(),
        }
    }
}
fn reg_index(r: R) -> Option<usize> {
    Some(match r {
        R::EAX | R::AX | R::AL | R::AH => 0,
        R::ECX | R::CX | R::CL | R::CH => 1,
        R::EDX | R::DX | R::DL | R::DH => 2,
        R::EBX | R::BX | R::BL | R::BH => 3,
        R::ESP | R::SP => 4,
        R::EBP | R::BP => 5,
        R::ESI | R::SI => 6,
        R::EDI | R::DI => 7,
        _ => return None,
    })
}

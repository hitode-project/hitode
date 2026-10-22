//! Maps a PE image into the guest the way NT maps an image section: the
//! headers read-only, each section with the protection its flags ask for.

use adv_cpu::backend::{Guest, MemFault, Prot};
use adv_machine::layout::PAGE;
use adv_machine::vm::{Vm, align_up};
use adv_pe::Pe;

const SCN_MEM_EXECUTE: u32 = 0x2000_0000;
const SCN_MEM_READ: u32 = 0x4000_0000;
const SCN_MEM_WRITE: u32 = 0x8000_0000;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("the range of the image at {0:#010x} is taken")]
    Occupied(u32),
    #[error(transparent)]
    Memory(#[from] MemFault),
}

/// Maps `pe`, already relocated for `base`, at `base`.
pub fn map(cpu: &mut dyn Guest, vm: &mut Vm, pe: &Pe, base: u32) -> Result<(), Error> {
    let size = align_up(pe.size_of_image, PAGE);
    if !vm.reserve_at(base, size) {
        return Err(Error::Occupied(base));
    }
    cpu.map(base, size, Prot::RW)?;
    cpu.write(base, &pe.image)?;
    cpu.protect(base, align_up(pe.size_of_headers, PAGE), Prot::READ)?;
    for section in &pe.sections {
        let size = align_up(section.mem_size(), PAGE);
        if size != 0 {
            cpu.protect(
                base + section.virtual_address,
                size,
                protection(section.characteristics),
            )?;
        }
    }
    Ok(())
}

fn protection(characteristics: u32) -> Prot {
    [
        (SCN_MEM_READ, Prot::READ),
        (SCN_MEM_WRITE, Prot::WRITE),
        (SCN_MEM_EXECUTE, Prot::EXEC),
    ]
    .into_iter()
    .filter(|&(flag, _)| characteristics & flag != 0)
    .fold(Prot(0), |prot, (_, flag)| prot | flag)
}

//! Maps a PE image into the guest the way NT maps an image section: the
//! headers read-only, each section with the protection its flags ask for.

use adv_cpu::backend::{Guest, MemFault, Prot};
use adv_machine::Vm;
use adv_machine::layout::PAGE;
use adv_machine::vm::align_up;
use adv_pe::Pe;

use crate::memory::{self, Kind, Memory, PAGE_EXECUTE_WRITECOPY, PAGE_READONLY};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("the range of the image at {0:#010x} is taken")]
    Occupied(u32),
    #[error(transparent)]
    Memory(#[from] MemFault),
}

/// Maps `pe`, already relocated for `base`, at `base`.
pub fn map(
    cpu: &mut dyn Guest,
    vm: &mut Vm,
    memory: &mut Memory,
    pe: &Pe,
    base: u32,
) -> Result<(), Error> {
    let size = align_up(pe.size_of_image, PAGE);
    if !vm.reserve_at(base, size) {
        return Err(Error::Occupied(base));
    }
    let mut pages = vec![Some(PAGE_READONLY); (size / PAGE) as usize];
    for section in &pe.sections {
        let first = (section.virtual_address / PAGE) as usize;
        let count = (align_up(section.mem_size(), PAGE) / PAGE) as usize;
        let protection = memory::section_protection(section.characteristics);
        for page in pages.iter_mut().skip(first).take(count) {
            *page = Some(protection);
        }
    }
    cpu.map(base, size, Prot::RW)?;
    cpu.write(base, &pe.image)?;
    let mut start = 0;
    while start < pages.len() {
        let protection = pages[start];
        let run = pages[start..]
            .iter()
            .take_while(|&&p| p == protection)
            .count();
        cpu.protect(
            base + start as u32 * PAGE,
            run as u32 * PAGE,
            memory::guest_protection(memory, protection.expect("committed")),
        )?;
        start += run;
    }
    memory.insert(base, Kind::Image, PAGE_EXECUTE_WRITECOPY, pages);
    Ok(())
}

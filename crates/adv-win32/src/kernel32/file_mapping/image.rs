//! SEC_IMAGE sections. A view lays out the file as the loader would, relocated
//! when its preferred base is taken, without binding imports or running code.
//! Pages stay private copies; synthetic system entries reach their host
//! implementations from any address.
use std::io::{Read, Seek, SeekFrom};
use std::rc::Rc;

use adv_cpu::backend::Prot;
use adv_machine::{Env, Outcome};
use adv_pe::{Pe, file_flags, scn};

use super::{
    ALL, COPY, FileObj, GENERIC_READ, GRANULE, PAGE, READ, Section, View, WRITE, fail, page_prot,
    reserve_view_at, state,
};

const ERROR_BAD_EXE_FORMAT: u32 = 193;

pub(super) fn parse(env: &mut Env<'_>, handle: u32) -> Result<Pe, u32> {
    let source = env.sys.objects.host_mut::<FileObj>(handle).ok_or(6u32)?;
    if source.access & (GENERIC_READ | 0x10000000 | 1) == 0 {
        return Err(5);
    }
    let bytes = match source.file.memory_bytes() {
        Some(bytes) => bytes.to_vec(),
        None => {
            // Reading the image leaves the handle's file pointer where it was.
            let file = &mut source.file;
            let position = file.stream_position().map_err(|_| 5u32)?;
            let mut bytes = Vec::new();
            let read = file
                .seek(SeekFrom::Start(0))
                .and_then(|_| file.read_to_end(&mut bytes));
            file.seek(SeekFrom::Start(position)).map_err(|_| 5u32)?;
            read.map_err(|_| 5u32)?;
            bytes
        }
    };
    Pe::parse(&bytes).map_err(|_| ERROR_BAD_EXE_FORMAT)
}

/// The page protection Windows gives a section: writable pages copy on write.
fn section_protection(characteristics: u32) -> u32 {
    let read = characteristics & scn::MEM_READ != 0;
    match (
        characteristics & scn::MEM_WRITE != 0,
        characteristics & scn::MEM_EXECUTE != 0,
    ) {
        (true, true) => 0x80,
        (true, false) => 8,
        (false, true) if read => 0x20,
        (false, true) => 0x10,
        (false, false) => 2,
    }
}

pub(super) fn map(
    env: &mut Env<'_>,
    section: Rc<Section>,
    rights: u32,
    access: u32,
    offset: u64,
    count: u64,
    base: u32,
) -> Outcome {
    // FILE_MAP_EXECUTE is meaningful for image views; writes always copy.
    let access = access & !0x20;
    if access == ALL || access & WRITE != 0 {
        return fail(env, 5);
    }
    if access & !(READ | COPY) != 0 || access == 0 {
        return fail(env, 87);
    }
    if rights & READ == 0 {
        return fail(env, 5);
    }
    let image = section.image.as_ref().expect("image section");
    let span = section.len.div_ceil(PAGE) * PAGE;
    if offset != 0 || count > span {
        return fail(env, 87);
    }
    if base != 0 && u64::from(base) % GRANULE != 0 {
        return fail(env, 1132);
    }
    let preferred = image.image_base;
    let address = if base != 0 {
        if !reserve_view_at(env, base, span) {
            return fail(env, 487);
        }
        base
    } else if u64::from(preferred) % GRANULE == 0 && reserve_view_at(env, preferred, span) {
        preferred
    } else {
        match env.sys.vm.reserve_exact(span as u32) {
            Some(address) => address,
            None => return fail(env, 8),
        }
    };
    let mut pe = (**image).clone();
    if address != preferred {
        let rebased = if pe.characteristics & file_flags::RELOCS_STRIPPED != 0 {
            Err(487)
        } else {
            pe.rebase(address).map_err(|_| ERROR_BAD_EXE_FORMAT)
        };
        if let Err(error) = rebased {
            let _ = env.sys.vm.free(env.cpu, address);
            return fail(env, error);
        }
    }
    let pages = (span / PAGE) as usize;
    let mut protections = vec![2; pages];
    if pe.section_alignment < PAGE as u32 {
        // Low-alignment images map as one writable, executable range.
        protections.fill(0x80);
    } else {
        for s in &pe.sections {
            let first = (s.virtual_address / PAGE as u32) as usize;
            let count = s.mem_size().div_ceil(PAGE as u32) as usize;
            let value = section_protection(s.characteristics);
            if let Some(range) = protections.get_mut(first..(first + count).min(pages)) {
                range.fill(value);
            }
        }
    }
    pe.image.resize(span as usize, 0);
    if env
        .cpu
        .map_private_copy_pages(address, &pe.image, Prot::RW)
        .is_err()
    {
        let _ = env.sys.vm.free(env.cpu, address);
        return fail(env, 8);
    }
    for (index, &value) in protections.iter().enumerate() {
        let page = address + index as u32 * PAGE as u32;
        let prot = page_prot(env, value).unwrap_or(Prot::READ);
        if env.cpu.protect(page, PAGE as u32, prot).is_err() {
            let _ = env.sys.vm.free(env.cpu, address);
            return fail(env, 8);
        }
    }
    state(env).views.insert(
        address,
        View {
            section,
            offset: 0,
            len: span as u32,
            copy: true,
            maximum: 0x80,
            protections,
        },
    );
    Outcome::Return(address)
}

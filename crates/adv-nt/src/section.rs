//! Sections, after Wine's `NtCreateSection` and `virtual_map_image`: the
//! server keeps the section and parses the image headers; the platform maps
//! the image itself and tells the server where the view went.

use adv_machine::layout::PAGE;
use adv_machine::vm::align_up;
use adv_machine::{Env, Outcome};
use adv_pe::Pe;

use crate::image;
use crate::memory::{self, CURRENT_PROCESS, Memory};
use crate::object::{Attributes, read_u32, write_u32};
use crate::protocol::{CreateMappingRequest, GetMappingInfoRequest, MapImageViewRequest};
use crate::server;
use crate::status::{self, Error, Status};

const SEC_IMAGE: u32 = 0x100_0000;
const SECTION_MAP_READ: u32 = 0x0004;
const FILE_READ_DATA: u32 = 0x0001;
const FILE_WRITE_DATA: u32 = 0x0002;
const IMAGE_NOT_AT_BASE: Status = 0x4000_0003;
const IMAGE_MACHINE_TYPE_MISMATCH: Status = 0x4000_002e;
const MACHINE_I386: u16 = 0x014c;

/// The parts of `struct pe_image_info` the platform reads.
pub struct ImageInfo {
    pub map_addr: u32,
    pub entry_point: u32,
    pub map_size: u32,
    pub subsystem: u32,
    pub subsystem_minor: u16,
    pub subsystem_major: u16,
    pub image_charact: u16,
    pub dll_charact: u16,
    pub machine: u16,
}

const IMAGE_INFO_SIZE: usize = 96;

impl ImageInfo {
    fn parse(bytes: &[u8]) -> ImageInfo {
        let u32_at =
            |offset: usize| u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        let u16_at = |offset: usize| u16::from_le_bytes([bytes[offset], bytes[offset + 1]]);
        ImageInfo {
            map_addr: u32_at(8),
            entry_point: u32_at(32),
            map_size: u32_at(36),
            subsystem: u32_at(48),
            subsystem_minor: u16_at(52),
            subsystem_major: u16_at(54),
            image_charact: u16_at(60),
            dll_charact: u16_at(62),
            machine: u16_at(64),
        }
    }
}

/// A mapped image view.
pub struct View {
    pub base: u32,
    pub size: u32,
    pub info: ImageInfo,
    /// The image is not at its preferred base.
    pub relocated: bool,
}

/// NtCreateSection(*handle, access, *attributes, *size, protection, flags, file)
pub fn create(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[handle, access, attributes, size, protection, flags, file] = args else {
        unreachable!()
    };
    status::finish((|| {
        write_u32(env, handle, 0)?;
        let size = match size {
            0 => 0,
            size => u64::from(read_u32(env, size)?) | u64::from(read_u32(env, size + 4)?) << 32,
        };
        let created = create_mapping(env, access, attributes, size, protection, flags, file)?;
        write_u32(env, handle, created)?;
        Ok(())
    })())
}

/// Creates a section in the server and returns its handle.
pub fn create_mapping(
    env: &mut Env<'_>,
    access: u32,
    attributes: u32,
    size: u64,
    protection: u32,
    flags: u32,
    file: u32,
) -> Result<u32, Error> {
    use crate::memory::*;
    let file_access = match protection & 0xff {
        PAGE_READONLY | PAGE_EXECUTE_READ | PAGE_WRITECOPY | PAGE_EXECUTE_WRITECOPY => {
            FILE_READ_DATA
        }
        PAGE_READWRITE | PAGE_EXECUTE_READWRITE if flags & SEC_IMAGE != 0 => FILE_READ_DATA,
        PAGE_READWRITE | PAGE_EXECUTE_READWRITE => FILE_READ_DATA | FILE_WRITE_DATA,
        PAGE_EXECUTE | PAGE_NOACCESS => 0,
        _ => return Err(status::INVALID_PAGE_PROTECTION.into()),
    };
    let objattr = match attributes {
        0 => Vec::new(),
        attributes => Attributes::read(env, attributes)?.wire(),
    };
    let mut request = CreateMappingRequest {
        access,
        flags,
        file_access,
        size,
        file_handle: file,
        ..Default::default()
    };
    let (reply, _) = server::request(env, &mut request, &[&objattr], &mut [])?;
    status::check(reply.header.error)?;
    Ok(reply.handle)
}

/// NtMapViewOfSection(handle, process, *base, zero_bits, commit_size,
/// *offset, *size, inherit, allocation_type, protection)
pub fn map_view(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[handle, process, base_out, _, _, offset, size_out, _, _, _] = args else {
        unreachable!()
    };
    status::finish((|| {
        if process != CURRENT_PROCESS {
            return Err(Error::Unsupported(
                "mapping a view into another process".into(),
            ));
        }
        if read_u32(env, base_out)? != 0 {
            return Err(Error::Unsupported(
                "mapping a view at a given address".into(),
            ));
        }
        if offset != 0 && (read_u32(env, offset)? != 0 || read_u32(env, offset + 4)? != 0) {
            return Err(Error::Unsupported("mapping a view at an offset".into()));
        }
        let view = map_image(env, handle)?;
        write_u32(env, base_out, view.base)?;
        write_u32(env, size_out, view.size)?;
        if view.relocated {
            return Err(IMAGE_NOT_AT_BASE.into());
        }
        if view.info.machine != MACHINE_I386 {
            return Err(IMAGE_MACHINE_TYPE_MISMATCH.into());
        }
        Ok(())
    })())
}

/// Maps the image of section `mapping`, as Wine's `virtual_map_image`.
pub fn map_image(env: &mut Env<'_>, mapping: u32) -> Result<View, Error> {
    let mut request = GetMappingInfoRequest {
        handle: mapping,
        access: SECTION_MAP_READ,
        ..Default::default()
    };
    let mut data = vec![0u8; 1024];
    let (reply, size) = server::request(env, &mut request, &[], &mut data)?;
    status::check(reply.header.error)?;
    if reply.shared_file != 0 {
        return Err(Error::Unsupported("images with shared sections".into()));
    }
    if size < IMAGE_INFO_SIZE {
        return Err(Error::Unsupported(
            "mapping a view of a data section".into(),
        ));
    }
    let info = ImageInfo::parse(&data);

    let fd = server::handle_fd(env, mapping)?;
    let file =
        read_file(fd.fd).map_err(|error| Error::Unsupported(format!("image file: {error}")))?;
    drop(fd);
    let mut pe = Pe::parse(&file).map_err(|_| status::INVALID_IMAGE_FORMAT)?;
    let preferred = if info.map_addr != 0 {
        info.map_addr
    } else {
        pe.image_base
    };
    let size = align_up(pe.size_of_image, PAGE);
    let base = if env.sys.vm.containing(preferred).is_none()
        && env
            .sys
            .vm
            .next_region(preferred)
            .is_none_or(|next| next - preferred >= size)
    {
        preferred
    } else {
        memory::find_free(&env.sys.vm, size, false).ok_or(status::NO_MEMORY)?
    };
    if base != pe.image_base {
        pe.rebase(base).map_err(|_| status::INVALID_IMAGE_FORMAT)?;
    }
    let memory = env.ext.get_or_default::<Memory>();
    image::map(env.cpu, &mut env.sys.vm, memory, &pe, base)
        .map_err(|error| Error::Unsupported(format!("mapping image: {error}")))?;

    let mut request = MapImageViewRequest {
        mapping,
        base: u64::from(base),
        size: u64::from(info.map_size),
        entry: info.entry_point,
        machine: info.machine,
        ..Default::default()
    };
    let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
    if reply.header.error != 0 && reply.header.error != IMAGE_NOT_AT_BASE {
        return Err(reply.header.error.into());
    }
    Ok(View {
        base,
        size: align_up(info.map_size, PAGE),
        relocated: base != preferred,
        info,
    })
}

fn read_file(fd: std::os::fd::RawFd) -> std::io::Result<Vec<u8>> {
    use std::os::fd::FromRawFd;
    // SAFETY: a descriptor the server gave the process, open for this call;
    // ManuallyDrop leaves it open.
    let file = std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(fd) });
    let size = file.metadata()?.len();
    let mut bytes = vec![0u8; size as usize];
    std::os::unix::fs::FileExt::read_exact_at(&*file, &mut bytes, 0)?;
    Ok(bytes)
}

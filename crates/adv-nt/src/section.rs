//! Sections, after Wine's `NtCreateSection` and `virtual_map_image`: the
//! server keeps the section and parses the image headers; the platform maps
//! the image itself and tells the server where the view went.

use adv_machine::layout::PAGE;
use adv_machine::vm::align_up;
use adv_machine::{Env, Outcome};
use adv_pe::Pe;

use crate::image;
use crate::memory::{self, CURRENT_PROCESS, Memory};
use crate::object::{Attributes, read_u32, write_bytes, write_u32};
use crate::protocol::{
    CreateMappingRequest, GetMappingInfoRequest, IsSameMappingRequest, MapImageViewRequest,
    MapViewRequest, OpenMappingRequest, UnmapViewRequest,
};
use crate::server;
use crate::status::{self, Error, Status};
use crate::unixlib;

const SEC_IMAGE: u32 = 0x100_0000;
const SECTION_QUERY: u32 = 0x0001;
const SECTION_MAP_WRITE: u32 = 0x0002;
const SECTION_MAP_READ: u32 = 0x0004;
const SECTION_MAP_EXECUTE: u32 = 0x0008;
const FILE_READ_DATA: u32 = 0x0001;
const FILE_WRITE_DATA: u32 = 0x0002;
const IMAGE_NOT_AT_BASE: Status = 0x4000_0003;
const IMAGE_MACHINE_TYPE_MISMATCH: Status = 0x4000_002e;
const SECTION_NOT_IMAGE: Status = 0xc000_0049;
const MAPPED_ALIGNMENT: Status = 0xc000_0220;
/// Views of data sections start at multiples of this.
const ALLOCATION_GRANULARITY: u32 = 0x1_0000;
const MACHINE_I386: u16 = 0x014c;
const MACHINE_AMD64: u16 = 0x8664;
const MACHINE_ARM64: u16 = 0xaa64;

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

/// `struct pe_image_info`.
pub const PE_IMAGE_INFO_SIZE: usize = 96;

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

/// SECTION_IMAGE_INFORMATION for `struct pe_image_info`, as Wine's
/// `virtual_fill_image_information`.
pub fn image_information(pe_info: &[u8]) -> [u8; IMAGE_INFORMATION_SIZE] {
    let u32_at =
        |offset: usize| u32::from_le_bytes(pe_info[offset..offset + 4].try_into().unwrap());
    let info = ImageInfo::parse(pe_info);
    let (mut transfer, mut stack_size, mut stack_commit) = (
        u32_at(0).wrapping_add(info.entry_point),
        u32_at(16),
        u32_at(24),
    );
    // A 32-bit process is not told 64-bit values.
    if matches!(info.machine, MACHINE_AMD64 | MACHINE_ARM64) {
        (transfer, stack_size, stack_commit) = (0x8123_1234, 0x10_0000, 0x1_0000);
    }
    let mut out = [0u8; IMAGE_INFORMATION_SIZE];
    let words = [
        (0x00, transfer),
        (0x04, u32_at(44)),
        (0x08, stack_size),
        (0x0c, stack_commit),
        (0x10, info.subsystem),
        (0x24, u32_at(68)),
        (0x28, u32_at(80)),
        (0x2c, u32_at(84)),
    ];
    for (offset, value) in words {
        out[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    // Subsystem and OS versions, characteristics and machine are laid out
    // alike in both; then the code flag and the image flags.
    out[0x14..0x22].copy_from_slice(&pe_info[52..66]);
    out[0x22] = pe_info[66] & 1;
    out[0x23] = pe_info[67];
    out
}

pub const IMAGE_INFORMATION_SIZE: usize = 0x30;

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
        let attributes = match attributes {
            0 => None,
            attributes => Some(Attributes::read(env, attributes)?),
        };
        let (created, status) = create_mapping(
            env,
            access,
            attributes.as_ref(),
            size,
            protection,
            flags,
            file,
        )?;
        write_u32(env, handle, created)?;
        status::check(status)
    })())
}

/// NtOpenSection(*handle, access, *attributes)
pub fn open(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[handle, access, attributes] = args else {
        unreachable!()
    };
    status::finish((|| {
        write_u32(env, handle, 0)?;
        let attributes = Attributes::read(env, attributes)?;
        let opened = open_mapping(env, access, &attributes)?;
        write_u32(env, handle, opened)?;
        Ok(())
    })())
}

/// Opens a named section and returns its handle.
pub fn open_mapping(env: &mut Env<'_>, access: u32, attributes: &Attributes) -> Result<u32, Error> {
    let mut request = OpenMappingRequest {
        access,
        attributes: attributes.attributes,
        rootdir: attributes.root,
        ..Default::default()
    };
    let (reply, _) = server::request(env, &mut request, &[&attributes.name_bytes()], &mut [])?;
    status::check(reply.header.error)?;
    Ok(reply.handle)
}

/// Maps a read-only view of all of data section `mapping`; returns its base
/// and size.
pub fn map_read_only(env: &mut Env<'_>, mapping: u32) -> Result<(u32, u32), Error> {
    let mut request = GetMappingInfoRequest {
        handle: mapping,
        access: SECTION_MAP_READ,
        ..Default::default()
    };
    let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
    status::check(reply.header.error)?;
    map_data(env, mapping, reply.size, 0, 0, crate::memory::PAGE_READONLY)
}

const SECTION_BASIC_INFORMATION: u32 = 0;
const SECTION_IMAGE_INFORMATION: u32 = 1;
const BASIC_INFORMATION_SIZE: u32 = 16;

/// NtQuerySection(handle, class, *info, size, *result_size)
pub fn query(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[handle, class, info, size, result_size] = args else {
        unreachable!()
    };
    status::finish((|| {
        let needed = match class {
            SECTION_BASIC_INFORMATION => BASIC_INFORMATION_SIZE,
            SECTION_IMAGE_INFORMATION => IMAGE_INFORMATION_SIZE as u32,
            class => {
                return Err(Error::Unsupported(format!(
                    "section information class {class}"
                )));
            }
        };
        if size < needed {
            return Err(status::INFO_LENGTH_MISMATCH.into());
        }
        let mut request = GetMappingInfoRequest {
            handle,
            access: SECTION_QUERY,
            ..Default::default()
        };
        let mut pe_info = vec![0u8; PE_IMAGE_INFO_SIZE];
        let (reply, _) = server::request(env, &mut request, &[], &mut pe_info)?;
        status::check(reply.header.error)?;
        if class == SECTION_BASIC_INFORMATION {
            let mut out = [0u8; BASIC_INFORMATION_SIZE as usize];
            out[4..8].copy_from_slice(&reply.flags.to_le_bytes());
            out[8..16].copy_from_slice(&reply.size.to_le_bytes());
            write_bytes(env, info, &out)?;
        } else if reply.flags & SEC_IMAGE != 0 {
            write_bytes(env, info, &image_information(&pe_info))?;
        } else {
            return Err(SECTION_NOT_IMAGE.into());
        }
        if result_size != 0 {
            write_u32(env, result_size, needed)?;
        }
        Ok(())
    })())
}

/// Creates a section in the server and returns its handle, with
/// OBJECT_NAME_EXISTS when a named section was opened instead.
pub fn create_mapping(
    env: &mut Env<'_>,
    access: u32,
    attributes: Option<&Attributes>,
    size: u64,
    protection: u32,
    flags: u32,
    file: u32,
) -> Result<(u32, Status), Error> {
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
    let objattr = attributes.map(Attributes::wire).unwrap_or_default();
    let mut request = CreateMappingRequest {
        access,
        flags,
        file_access,
        size,
        file_handle: file,
        ..Default::default()
    };
    let (reply, _) = server::request(env, &mut request, &[&objattr], &mut [])?;
    match reply.header.error {
        status::SUCCESS | status::OBJECT_NAME_EXISTS => Ok((reply.handle, reply.header.error)),
        error => Err(error.into()),
    }
}

/// NtMapViewOfSection(handle, process, *base, zero_bits, commit_size,
/// *offset, *size, inherit, allocation_type, protection)
pub fn map_view(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[
        handle,
        process,
        base_out,
        _,
        _,
        offset,
        size_out,
        _,
        _,
        protection,
    ] = args
    else {
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
        let offset = match offset {
            0 => 0,
            offset => {
                u64::from(read_u32(env, offset)?) | u64::from(read_u32(env, offset + 4)?) << 32
            }
        };
        let mut request = GetMappingInfoRequest {
            handle,
            access: view_access(protection)?,
            ..Default::default()
        };
        let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
        status::check(reply.header.error)?;
        if reply.flags & SEC_IMAGE == 0 {
            let requested = read_u32(env, size_out)?;
            let (base, size) = map_data(env, handle, reply.size, offset, requested, protection)?;
            write_u32(env, base_out, base)?;
            write_u32(env, size_out, size)?;
            return Ok(());
        }
        if offset != 0 {
            return Err(Error::Unsupported(
                "mapping an image view at an offset".into(),
            ));
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

/// The section access a view with `protection` needs.
fn view_access(protection: u32) -> Result<u32, Error> {
    use crate::memory::*;
    Ok(match protection & 0xff {
        PAGE_NOACCESS => 0,
        PAGE_READONLY | PAGE_WRITECOPY => SECTION_MAP_READ,
        PAGE_READWRITE => SECTION_MAP_READ | SECTION_MAP_WRITE,
        PAGE_EXECUTE | PAGE_EXECUTE_READ | PAGE_EXECUTE_WRITECOPY => {
            SECTION_MAP_READ | SECTION_MAP_EXECUTE
        }
        PAGE_EXECUTE_READWRITE => SECTION_MAP_READ | SECTION_MAP_WRITE | SECTION_MAP_EXECUTE,
        _ => return Err(status::INVALID_PAGE_PROTECTION.into()),
    })
}

/// Maps a view of `requested` bytes, or the rest of the section, from
/// `offset` in data section `mapping`. A read-only or read-write view
/// shares the file's pages; a copy-on-write or executable view is a copy
/// of the file's contents.
fn map_data(
    env: &mut Env<'_>,
    mapping: u32,
    section_size: u64,
    offset: u64,
    requested: u32,
    protection: u32,
) -> Result<(u32, u32), Error> {
    use crate::memory::*;
    if !offset.is_multiple_of(u64::from(ALLOCATION_GRANULARITY)) {
        return Err(MAPPED_ALIGNMENT.into());
    }
    let available = section_size
        .checked_sub(offset)
        .filter(|&available| available > 0)
        .ok_or(status::INVALID_VIEW_SIZE)?;
    let size = match requested {
        0 => u32::try_from(available).map_err(|_| status::NO_MEMORY)?,
        requested if u64::from(requested) > available => {
            return Err(status::INVALID_VIEW_SIZE.into());
        }
        requested => requested,
    };
    let fd = server::handle_fd(env, mapping)?;
    let base = match protection & 0xff {
        PAGE_READONLY | PAGE_READWRITE => {
            // SAFETY: the server's descriptor stays open while fd lives.
            let file = unsafe { std::os::fd::BorrowedFd::borrow_raw(fd.fd) };
            let memory = env.ext.get_or_default::<Memory>();
            memory::map_shared_view(
                env.cpu,
                &mut env.sys.vm,
                memory,
                file,
                offset,
                size,
                protection,
            )?
        }
        PAGE_EXECUTE_READWRITE => {
            return Err(Error::Unsupported(
                "mapping a writable executable view of a data section".into(),
            ));
        }
        _ => {
            let bytes = read_file(fd.fd)
                .map_err(|error| Error::Unsupported(format!("section file: {error}")))?;
            let start = (offset as usize).min(bytes.len());
            let end = (start + size as usize).min(bytes.len());
            let memory = env.ext.get_or_default::<Memory>();
            memory::map_view(
                env.cpu,
                &mut env.sys.vm,
                memory,
                &bytes[start..end],
                size,
                protection,
            )?
        }
    };
    drop(fd);
    let mut request = MapViewRequest {
        mapping,
        access: view_access(protection)?,
        base: u64::from(base),
        size: u64::from(align_up(size, PAGE)),
        start: offset,
        ..Default::default()
    };
    let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
    if let Err(error) = status::check(reply.header.error) {
        let memory = env.ext.get_or_default::<Memory>();
        memory::remove_view(env.cpu, &mut env.sys.vm, memory, base);
        return Err(error);
    }
    Ok((base, align_up(size, PAGE)))
}

/// NtUnmapViewOfSection(process, address)
pub fn unmap_view(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[process, address] = args else {
        unreachable!()
    };
    status::finish((|| {
        if process != CURRENT_PROCESS {
            return Err(Error::Unsupported(
                "unmapping a view of another process".into(),
            ));
        }
        let memory = env.ext.get_or_default::<Memory>();
        let base = memory::view_base(memory, address).ok_or(status::NOT_MAPPED_VIEW)?;
        let mut request = UnmapViewRequest {
            base: u64::from(base),
            ..Default::default()
        };
        let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
        status::check(reply.header.error)?;
        let memory = env.ext.get_or_default::<Memory>();
        memory::remove_view(env.cpu, &mut env.sys.vm, memory, base);
        unixlib::forget_image(env, base);
        Ok(())
    })())
}

/// Maps the image of section `mapping`, as Wine's `virtual_map_image`.
/// NtAreMappedFilesTheSame(address1, address2), as Wine's: success when both
/// lie in one view or in image views of one file.
pub fn are_same(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[first, second] = args else {
        unreachable!()
    };
    status::finish((|| {
        let memory = env.ext.get_or_default::<Memory>();
        let (Some(first), Some(second)) = (
            memory::region(memory, first),
            memory::region(memory, second),
        ) else {
            return Err(status::INVALID_ADDRESS.into());
        };
        if first.1 == memory::Kind::Private || second.1 == memory::Kind::Private {
            return Err(status::CONFLICTING_ADDRESSES.into());
        }
        if first.0 == second.0 {
            return Ok(());
        }
        let mut request = IsSameMappingRequest {
            base1: u64::from(first.0),
            base2: u64::from(second.0),
            ..Default::default()
        };
        let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
        status::check(reply.header.error)
    })())
}

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
    if size < PE_IMAGE_INFO_SIZE {
        return Err(Error::Unsupported(
            "mapping a view of a data section".into(),
        ));
    }
    let info = ImageInfo::parse(&data);

    let fd = server::handle_fd(env, mapping)?;
    let file =
        read_file(fd.fd).map_err(|error| Error::Unsupported(format!("image file: {error}")))?;
    let path = file_path(fd.fd);
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
    unixlib::note_image(env, base, path);
    Ok(View {
        base,
        size: align_up(info.map_size, PAGE),
        relocated: base != preferred,
        info,
    })
}

/// The host path of an open file, for finding a builtin image's Unix
/// library next to it.
pub(crate) fn file_path(fd: std::os::fd::RawFd) -> Option<std::path::PathBuf> {
    #[cfg(target_vendor = "apple")]
    {
        use std::os::unix::ffi::OsStrExt;
        let mut path = [0u8; libc::PATH_MAX as usize];
        // SAFETY: F_GETPATH fills at most PATH_MAX bytes of the buffer.
        if unsafe { libc::fcntl(fd, libc::F_GETPATH, path.as_mut_ptr()) } == -1 {
            return None;
        }
        let length = path.iter().position(|&byte| byte == 0)?;
        Some(std::ffi::OsStr::from_bytes(&path[..length]).into())
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        std::fs::read_link(format!("/proc/self/fd/{fd}")).ok()
    }
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

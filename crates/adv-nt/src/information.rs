//! Process and thread information, after Wine's `NtQueryInformationProcess`
//! and `NtQueryInformationThread`.

use adv_machine::{Env, Outcome};

use crate::memory::CURRENT_PROCESS;
use crate::object::{write_bytes, write_u32};
use crate::process::PROCESSORS;
use crate::protocol::{GetNextThreadRequest, GetProcessInfoRequest, GetThreadInfoRequest};
use crate::section::{IMAGE_INFORMATION_SIZE, PE_IMAGE_INFO_SIZE, image_information};
use crate::server;
use crate::status::{self, Error};

const PROCESS_DEBUG_PORT: u32 = 7;
const PROCESS_DEFAULT_HARD_ERROR_MODE: u32 = 12;
const PROCESS_WOW64_INFORMATION: u32 = 26;
const PROCESS_IMAGE_INFORMATION: u32 = 37;
const PROCESS_THREAD_STACK_ALLOCATION: u32 = 41;

/// NtQueryInformationProcess(process, class, *info, size, *result_size)
pub fn query_process(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[process, class, info, size, result_size] = args else {
        unreachable!()
    };
    status::finish((|| {
        if matches!(class, 27 | 43) {
            let mut bytes = vec![0; size.saturating_sub(10) as usize];
            let mut request = crate::protocol::GetProcessImageNameRequest {
                handle: process,
                win32: i32::from(class == 43),
                ..Default::default()
            };
            let (reply, count) = server::request(env, &mut request, &[], &mut bytes)?;
            if result_size != 0 {
                write_u32(env, result_size, 10 + reply.len)?;
            }
            if reply.header.error == 0xc000_0023 {
                return Err(status::INFO_LENGTH_MISMATCH.into());
            }
            status::check(reply.header.error)?;
            write_u32(env, info, reply.len | ((reply.len + 2) << 16))?;
            write_u32(env, info + 4, info + 8)?;
            write_bytes(env, info + 8, &bytes[..count])?;
            write_bytes(env, info + 8 + count as u32, &[0; 2])?;
            return Ok(());
        }
        if class == 30 {
            if size != 0 && info & 3 != 0 {
                return Err(0x8000_0002.into());
            }
            if result_size != 0 {
                let value = crate::object::read_u32(env, result_size)?;
                write_u32(env, result_size, value)?;
            }
            if size != 4 {
                return Err(status::INFO_LENGTH_MISMATCH.into());
            }
            let mut request = crate::protocol::GetProcessDebugInfoRequest {
                handle: process,
                ..Default::default()
            };
            let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
            write_u32(env, info, reply.debug)?;
            if result_size != 0 {
                write_u32(env, result_size, 4)?;
            }
            return status::check(reply.header.error);
        }
        if class == 20 {
            if result_size != 0 {
                write_u32(env, result_size, 4)?;
            }
            if size < 4 {
                return Err(status::INFO_LENGTH_MISMATCH.into());
            }
            if info == 0 {
                return Err(status::ACCESS_VIOLATION.into());
            }
            current_process(env, process)?;
            // Wine leaves the handle-count field unset.
            write_u32(env, info, 0)?;
            if size > 4 {
                return Err(status::INFO_LENGTH_MISMATCH.into());
            }
            return Ok(());
        }
        if class == 18 {
            if result_size != 0 {
                write_u32(env, result_size, 2)?;
            }
            if size != 2 {
                return Err(status::INFO_LENGTH_MISMATCH.into());
            }
            if info == 0 {
                return Err(status::ACCESS_VIOLATION.into());
            }
            let mut request = GetProcessInfoRequest {
                handle: process,
                ..Default::default()
            };
            let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
            status::check(reply.header.error)?;
            write_bytes(env, info, &[0, reply.priority as u8])?;
            return Ok(());
        }
        if class == u32::MAX {
            return Err(status::INVALID_INFO_CLASS.into());
        }
        if matches!(class, 0 | 2 | 4) {
            let length = if class == 0 {
                24
            } else if class == 2 {
                48
            } else {
                32
            };
            if result_size != 0 {
                write_u32(env, result_size, length)?;
            }
            if size < length {
                return Err(status::INFO_LENGTH_MISMATCH.into());
            }
            if info == 0 {
                return Err(status::ACCESS_VIOLATION.into());
            }
            let mut request = GetProcessInfoRequest {
                handle: process,
                ..Default::default()
            };
            let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
            let result = status::check(reply.header.error);
            if result.is_err() && size > length {
                return Err(status::INFO_LENGTH_MISMATCH.into());
            }
            result?;
            let mut bytes = vec![0; length as usize];
            if class == 0 {
                for (index, value) in [
                    reply.exit_code as u32,
                    reply.peb as u32,
                    reply.affinity as u32 & ((1 << PROCESSORS) - 1),
                    u32::from(reply.base_priority),
                    reply.pid,
                    reply.ppid,
                ]
                .into_iter()
                .enumerate()
                {
                    bytes[index * 4..index * 4 + 4].copy_from_slice(&value.to_le_bytes());
                }
            } else if class == 4 {
                bytes[..8].copy_from_slice(&reply.start_time.to_le_bytes());
                bytes[8..16].copy_from_slice(&reply.end_time.to_le_bytes());
            }
            write_bytes(env, info, &bytes)?;
            if size > length {
                return Err(status::INFO_LENGTH_MISMATCH.into());
            }
            return Ok(());
        }
        if matches!(class, 7 | 12 | 21 | 22 | 26 | 31 | 34) && size != 4 {
            return Err(status::INFO_LENGTH_MISMATCH.into());
        }
        current_process(env, process)?;
        if class == 3 {
            let length = if size == 44 { 44 } else { 48 };
            if result_size != 0 {
                write_u32(env, result_size, length)?;
            }
            if size < 44 {
                return Err(status::INFO_LENGTH_MISMATCH.into());
            }
            let (reserved, committed) = crate::memory::usage(env);
            let mut bytes = [0u8; 48];
            for (offset, value) in [
                (0, reserved),
                (4, reserved),
                (12, committed),
                (16, committed),
                (36, committed),
                (40, committed),
                (44, committed),
            ] {
                bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
            }
            write_bytes(env, info, &bytes[..(size as usize).min(48)])?;
            if size != 44 && size != 48 {
                return Err(status::INFO_LENGTH_MISMATCH.into());
            }
            return Ok(());
        }
        let bytes = match class {
            // Nothing debugs a Hitode process, and an i386 process is
            // native, not WoW64.
            PROCESS_DEBUG_PORT | PROCESS_WOW64_INFORMATION => vec![0; 4],
            21 => {
                let mut request = GetProcessInfoRequest {
                    handle: process,
                    ..Default::default()
                };
                let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
                status::check(reply.header.error)?;
                (reply.affinity as u32 & ((1 << PROCESSORS) - 1))
                    .to_le_bytes()
                    .to_vec()
            }
            34 => {
                let value = match env.ext.try_get::<ExecuteFlags>() {
                    Some(flags) => flags.0,
                    None => {
                        if env
                            .ext
                            .get_or_default::<crate::memory::Memory>()
                            .execute_readable
                        {
                            2u32
                        } else {
                            1u32
                        }
                    }
                };
                value.to_le_bytes().to_vec()
            }
            22 => {
                let mut request = GetProcessInfoRequest {
                    handle: process,
                    ..Default::default()
                };
                let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
                status::check(reply.header.error)?;
                u32::from(reply.disable_boost).to_le_bytes().to_vec()
            }
            31 => {
                let mut request = crate::protocol::GetProcessDebugInfoRequest {
                    handle: process,
                    ..Default::default()
                };
                let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
                if reply.header.error != 0xc000_0353 {
                    status::check(reply.header.error)?;
                }
                (reply.debug_children as u32).to_le_bytes().to_vec()
            }
            PROCESS_DEFAULT_HARD_ERROR_MODE => {
                let mode = env.ext.try_get::<ErrorMode>().map_or(0, |mode| mode.0);
                mode.to_le_bytes().to_vec()
            }
            PROCESS_IMAGE_INFORMATION => {
                if size as usize != IMAGE_INFORMATION_SIZE {
                    return Err(status::INFO_LENGTH_MISMATCH.into());
                }
                let mut request = GetProcessInfoRequest {
                    handle: process,
                    ..Default::default()
                };
                let mut pe_info = vec![0u8; PE_IMAGE_INFO_SIZE];
                let (reply, _) = server::request(env, &mut request, &[], &mut pe_info)?;
                status::check(reply.header.error)?;
                image_information(&pe_info).to_vec()
            }
            class => {
                return Err(Error::Unsupported(format!(
                    "process information class {class}"
                )));
            }
        };
        if size as usize != bytes.len() {
            return Err(status::INFO_LENGTH_MISMATCH.into());
        }
        write_bytes(env, info, &bytes)?;
        if result_size != 0 {
            write_u32(env, result_size, bytes.len() as u32)?;
        }
        Ok(())
    })())
}

/// The process's hard error mode, which Wine's ntdll.so keeps in a global.
struct ErrorMode(u32);

/// NtSetInformationProcess(process, class, *info, size)
pub fn set_process(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[process, class, info, size] = args else {
        unreachable!()
    };
    status::finish((|| match class {
        18 => {
            if size != 2 {
                return Err(status::INVALID_PARAMETER.into());
            }
            let mut bytes = [0; 2];
            env.cpu
                .read(info, &mut bytes)
                .map_err(|_| status::ACCESS_VIOLATION)?;
            let mut request = crate::protocol::SetProcessInfoRequest {
                handle: process,
                priority: i32::from(bytes[1]),
                mask: 1,
                ..Default::default()
            };
            let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
            status::check(reply.header.error)
        }
        34 => {
            if size != 4 {
                return Err(status::INVALID_PARAMETER.into());
            }
            if env
                .ext
                .try_get::<ExecuteFlags>()
                .is_some_and(|flags| flags.0 & 8 != 0)
            {
                return Err(0xc000_0022.into());
            }
            let flags = crate::object::read_u32(env, info)?;
            if !matches!(flags & 3, 1 | 2) {
                return Err(status::INVALID_PARAMETER.into());
            }
            crate::memory::set_execute(env, flags & 2 != 0)?;
            env.ext.insert(ExecuteFlags(flags));
            Ok(())
        }
        22 => {
            if size != 4 {
                return Err(status::INVALID_PARAMETER.into());
            }
            let mut request = crate::protocol::SetProcessInfoRequest {
                handle: process,
                disable_boost: crate::object::read_u32(env, info)? as i32,
                mask: 4,
                ..Default::default()
            };
            let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
            status::check(reply.header.error)
        }
        PROCESS_DEFAULT_HARD_ERROR_MODE => {
            if size != 4 {
                return Err(status::INVALID_PARAMETER.into());
            }
            let mode = crate::object::read_u32(env, info)?;
            env.ext.insert(ErrorMode(mode));
            Ok(())
        }
        PROCESS_THREAD_STACK_ALLOCATION => {
            let stack = match size {
                12 => info,
                28 => info + 16,
                _ => return Err(status::INFO_LENGTH_MISMATCH.into()),
            };
            let reserve = crate::object::read_u32(env, stack)?;
            let zero_bits = crate::object::read_u32(env, stack + 4)?;
            let scratch = env
                .sys
                .guest_words
                .allocate(env.cpu, &mut env.sys.vm)
                .ok_or(status::NO_MEMORY)?;
            scratch.set_limit(reserve);
            let result = crate::memory::allocate(
                env,
                &[
                    CURRENT_PROCESS,
                    scratch.address,
                    zero_bits,
                    scratch.address + 4,
                    crate::memory::MEM_RESERVE,
                    crate::memory::PAGE_READWRITE,
                ],
            );
            match result {
                Outcome::Return(status::SUCCESS) => {
                    write_u32(env, stack + 8, scratch.load())?;
                    Ok(())
                }
                Outcome::Return(code) => Err(code.into()),
                _ => unreachable!(),
            }
        }
        class => Err(Error::Unsupported(format!(
            "setting process information class {class}"
        ))),
    })())
}

const THREAD_BASIC_INFORMATION: u32 = 0;
const THREAD_TIMES: u32 = 1;
const THREAD_PRIORITY: u32 = 2;
const THREAD_BASE_PRIORITY: u32 = 3;
const THREAD_AFFINITY_MASK: u32 = 4;
const THREAD_IMPERSONATION_TOKEN: u32 = 5;
const THREAD_BASIC_INFORMATION_SIZE: usize = 28;
const THREAD_AM_I_LAST_THREAD: u32 = 12;
const GET_THREAD_INFO_FLAG_LAST: u32 = 0x04;

/// NtQueryInformationThread(thread, class, *info, size, *result_size)
pub fn query_thread(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[thread, class, info, size, result_size] = args else {
        unreachable!()
    };
    status::finish((|| {
        if class == 30 {
            let mut request = GetThreadInfoRequest {
                handle: thread,
                ..Default::default()
            };
            let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
            status::check(reply.header.error)?;
            let mut bytes = [0; 12];
            bytes[..4]
                .copy_from_slice(&(reply.affinity as u32 & ((1 << PROCESSORS) - 1)).to_le_bytes());
            let count = size.min(12);
            if info != 0 {
                write_bytes(env, info, &bytes[..count as usize])?;
            }
            if result_size != 0 {
                write_u32(env, result_size, count)?;
            }
            return Ok(());
        }
        if class == 38 {
            let mut bytes = vec![0; size.saturating_sub(8) as usize];
            let mut request = GetThreadInfoRequest {
                handle: thread,
                ..Default::default()
            };
            let (reply, count) = server::request(env, &mut request, &[], &mut bytes)?;
            let code = if info == 0 {
                0xc000_0023
            } else {
                reply.header.error
            };
            if result_size != 0 && matches!(code, 0 | 0xc000_0023) {
                write_u32(env, result_size, 8 + reply.desc_len)?;
            }
            status::check(code)?;
            write_u32(env, info, reply.desc_len | (reply.desc_len << 16))?;
            write_u32(env, info + 4, info + 8)?;
            write_bytes(env, info + 8, &bytes[..count])?;
            return Ok(());
        }
        if class == 6 {
            if size != 12 {
                return Err(status::INFO_LENGTH_MISMATCH.into());
            }
            let selector = crate::object::read_u32(env, info)?;
            if selector >> 16 != 0 {
                return Err(0xc000_0001.into());
            }
            let mut request = GetThreadInfoRequest {
                handle: thread,
                ..Default::default()
            };
            let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
            status::check(reply.header.error)?;
            let state = env
                .cpu
                .capture_architecture()
                .map_err(|error| Error::Unsupported(error.to_string()))?;
            let selector = selector as u16 | 3;
            let (base, limit, access, granularity) = if selector == 3 {
                (0u32, 0u32, 0u32, 0u32)
            } else if selector == state.selectors[1] | 3 {
                (0, 0xfffff, 0xfa, 0xc)
            } else if selector == state.selectors[3] | 3 {
                (0, 0xfffff, 0xf2, 0xc)
            } else if selector == state.selectors[4] | 3 {
                (reply.teb as u32, 0xfff, 0xf2, 0x4)
            } else {
                return Err(0xc000_0001.into());
            };
            let low = (limit & 0xffff) | (base << 16);
            let high = ((base >> 16) & 0xff)
                | (access << 8)
                | (limit & 0xf0000)
                | (granularity << 20)
                | (base & 0xff000000);
            write_u32(env, info + 4, low)?;
            write_u32(env, info + 8, high)?;
            if result_size != 0 {
                write_u32(env, result_size, 8)?;
            }
            return Ok(());
        }
        if matches!(class, 9 | 14 | 16 | 17) {
            if class == 14 && size != 4 || class == 16 && size != 4 || class == 17 && size != 1 {
                return Err(status::INFO_LENGTH_MISMATCH.into());
            }
            if class == 16 {
                if info == 0 {
                    return Err(0xc000_0022.into());
                }
                write_u32(env, info, 0)?;
            } else {
                let mut request = GetThreadInfoRequest {
                    handle: thread,
                    access: if class == 9 || class == 17 { 0x40 } else { 0 },
                    ..Default::default()
                };
                let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
                status::check(reply.header.error)?;
                let value = match class {
                    9 => reply.entry_point as u32,
                    14 => u32::from(reply.flags & 8 != 0),
                    _ => u32::from(reply.flags & 1 != 0),
                };
                let count = size.min(if class == 17 { 1 } else { 4 });
                if info != 0 {
                    write_bytes(env, info, &value.to_le_bytes()[..count as usize])?;
                }
            }
            if result_size != 0 {
                write_u32(env, result_size, size.min(if class == 17 { 1 } else { 4 }))?;
            }
            return Ok(());
        }
        if class == THREAD_TIMES {
            let mut request = crate::protocol::GetThreadTimesRequest {
                handle: thread,
                ..Default::default()
            };
            let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
            status::check(reply.header.error)?;
            // Guest threads have no Unix thread whose CPU times Wine can query.
            let mut bytes = [0; 32];
            bytes[..8].copy_from_slice(&reply.creation_time.to_le_bytes());
            bytes[8..16].copy_from_slice(&reply.exit_time.to_le_bytes());
            let count = (size as usize).min(bytes.len());
            if info != 0 {
                write_bytes(env, info, &bytes[..count])?;
            }
            if result_size != 0 {
                write_u32(env, result_size, count as u32)?;
            }
            return Ok(());
        }
        match class {
            7 | 8 | 10 | 11 | 15 => return Err(status::NOT_IMPLEMENTED.into()),
            13 => return Err(status::INVALID_INFO_CLASS.into()),
            THREAD_PRIORITY | THREAD_BASE_PRIORITY | THREAD_IMPERSONATION_TOKEN => {
                return Err(status::NOT_IMPLEMENTED.into());
            }
            THREAD_BASIC_INFORMATION | THREAD_AFFINITY_MASK | THREAD_AM_I_LAST_THREAD => {}
            class => {
                return Err(Error::Unsupported(format!(
                    "thread information class {class}"
                )));
            }
        }
        let mut request = GetThreadInfoRequest {
            handle: thread,
            access: if class == THREAD_AFFINITY_MASK {
                0x40
            } else {
                0
            },
            ..Default::default()
        };
        let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
        status::check(reply.header.error)?;
        if class == THREAD_AFFINITY_MASK {
            let affinity = reply.affinity as u32 & ((1 << PROCESSORS) - 1);
            let count = size.min(4);
            if info != 0 {
                write_bytes(env, info, &affinity.to_le_bytes()[..count as usize])?;
            }
            if result_size != 0 {
                write_u32(env, result_size, count)?;
            }
            return Ok(());
        }
        if class == THREAD_AM_I_LAST_THREAD {
            if size != 4 {
                return Err(status::INFO_LENGTH_MISMATCH.into());
            }
            let last = u32::from(reply.flags & GET_THREAD_INFO_FLAG_LAST != 0);
            write_u32(env, info, last)?;
            if result_size != 0 {
                write_u32(env, result_size, 4)?;
            }
            return Ok(());
        }
        // ExitStatus, TebBaseAddress, ClientId, AffinityMask, Priority and
        // BasePriority.
        let fields = [
            reply.exit_code as u32,
            reply.teb as u32,
            reply.pid,
            reply.tid,
            reply.affinity as u32 & ((1 << PROCESSORS) - 1),
            reply.priority as u32,
            reply.base_priority as u32,
        ];
        let bytes: Vec<u8> = fields
            .iter()
            .flat_map(|field| field.to_le_bytes())
            .collect();
        let count = (size as usize).min(THREAD_BASIC_INFORMATION_SIZE);
        if info != 0 {
            write_bytes(env, info, &bytes[..count])?;
        }
        if result_size != 0 {
            write_u32(env, result_size, count as u32)?;
        }
        Ok(())
    })())
}

/// NtGetNextThread(process, thread, access, attributes, flags, *handle)
pub fn next_thread(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[process, last, access, attributes, flags, handle] = args else {
        unreachable!()
    };
    status::finish((|| {
        let mut request = GetNextThreadRequest {
            process,
            last,
            access,
            attributes,
            flags,
            ..Default::default()
        };
        let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
        let next = if reply.header.error == status::SUCCESS {
            reply.handle
        } else {
            0
        };
        write_u32(env, handle, next)?;
        status::check(reply.header.error)
    })())
}

/// NtOpenProcess(*handle, access, *attributes, *client_id)
pub fn open_process(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[handle, access, attributes, id] = args else {
        unreachable!()
    };
    status::finish((|| {
        write_u32(env, handle, 0)?;
        let mut request = crate::protocol::OpenProcessRequest {
            pid: crate::object::read_u32(env, id)?,
            access,
            attributes: if attributes == 0 {
                0
            } else {
                crate::object::read_u32(env, attributes + 12)?
            },
            ..Default::default()
        };
        let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
        status::check(reply.header.error)?;
        write_u32(env, handle, reply.handle)?;
        Ok(())
    })())
}

/// NtOpenThread(*handle, access, *attributes, *client_id)
pub fn open_thread(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[handle, access, attributes, id] = args else {
        unreachable!()
    };
    status::finish((|| {
        write_u32(env, handle, 0)?;
        let mut request = crate::protocol::OpenThreadRequest {
            tid: crate::object::read_u32(env, id + 4)?,
            access,
            attributes: if attributes == 0 {
                0
            } else {
                crate::object::read_u32(env, attributes + 12)?
            },
            ..Default::default()
        };
        let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
        write_u32(env, handle, reply.handle)?;
        status::check(reply.header.error)
    })())
}

/// Resolve a real handle for this machine's process, preserving invalid-handle failures.
pub fn current_process(env: &mut Env<'_>, handle: u32) -> Result<(), Error> {
    if handle == CURRENT_PROCESS {
        return Ok(());
    }
    let mut request = GetProcessInfoRequest {
        handle,
        ..Default::default()
    };
    let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
    status::check(reply.header.error)?;
    if reply.pid != env.sys.pid {
        return Err(status::NOT_SUPPORTED.into());
    }
    Ok(())
}

/// NtSetInformationThread(thread, class, *info, size)
pub fn set_thread(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[thread, class, info, size] = args else {
        unreachable!()
    };
    status::finish((|| {
        let mut request = crate::protocol::SetThreadInfoRequest {
            handle: thread,
            ..Default::default()
        };
        let mut description = Vec::new();
        match class {
            0 | 1 | 6 | 7 | 10 | 11 | 12 | 15 => return Err(status::NOT_IMPLEMENTED.into()),
            2 | 3 | 4 | 5 | 9 | 14 => {
                if size != 4 {
                    return Err(status::INVALID_PARAMETER.into());
                }
                let value = crate::object::read_u32(env, info)?;
                match class {
                    2 => {
                        request.priority = value as i32;
                        request.mask = 1;
                    }
                    3 => {
                        request.base_priority = value as i32;
                        request.mask = 2;
                    }
                    4 => {
                        request.affinity = u64::from(value & ((1 << PROCESSORS) - 1));
                        request.mask = 4;
                        if request.affinity == 0 {
                            return Err(status::INVALID_PARAMETER.into());
                        }
                    }
                    5 => {
                        request.token = value;
                        request.mask = 8;
                    }
                    9 => {
                        request.entry_point = u64::from(value);
                        request.mask = 0x10;
                    }
                    14 => {
                        request.disable_boost = value as i32;
                        request.mask = 0x80;
                    }
                    _ => unreachable!(),
                }
            }
            17 => {
                if size != 0 {
                    return Err(status::INFO_LENGTH_MISMATCH.into());
                }
                request.mask = 0x40;
            }
            13 => {
                if size != 4 {
                    return Err(status::INFO_LENGTH_MISMATCH.into());
                }
                if crate::object::read_u32(env, info)? > 32 {
                    return Err(status::INVALID_PARAMETER.into());
                }
                return Ok(());
            }
            8 => {
                if thread != u32::MAX - 1 {
                    return Err(status::NOT_IMPLEMENTED.into());
                }
                if size != 4 {
                    return Err(status::INVALID_PARAMETER.into());
                }
                let index = crate::object::read_u32(env, info)?;
                if index >= 1088 {
                    return Err(status::INVALID_PARAMETER.into());
                }
                let tebs: Vec<u32> = env.sys.contexts.iter().map(|context| context.teb).collect();
                for teb in tebs {
                    let slot = if index < 64 {
                        teb + 0xe10 + index * 4
                    } else {
                        let expansion = crate::object::read_u32(env, teb + 0xf94)?;
                        if expansion == 0 {
                            continue;
                        }
                        expansion + (index - 64) * 4
                    };
                    write_u32(env, slot, 0)?;
                }
                return Ok(());
            }
            30 => {
                if size != 12 {
                    return Err(status::INVALID_PARAMETER.into());
                }
                if info == 0 {
                    return Err(status::ACCESS_VIOLATION.into());
                }
                let value = crate::object::read_u32(env, info)?;
                if value == 0
                    || value & !((1 << PROCESSORS) - 1) != 0
                    || crate::object::read_u32(env, info + 4)? != 0
                    || crate::object::read_u32(env, info + 8)? != 0
                {
                    return Err(status::INVALID_PARAMETER.into());
                }
                request.affinity = u64::from(value);
                request.mask = 4;
            }
            38 => {
                if size != 8 {
                    return Err(status::INFO_LENGTH_MISMATCH.into());
                }
                description = crate::object::read_unicode_string(env, info)?
                    .iter()
                    .flat_map(|unit| unit.to_le_bytes())
                    .collect();
                request.mask = 0x20;
            }
            class => {
                return Err(Error::Unsupported(format!(
                    "setting thread information class {class}"
                )));
            }
        }
        let (reply, _) = server::request(env, &mut request, &[&description], &mut [])?;
        status::check(reply.header.error)
    })())
}

struct ExecuteFlags(u32);

/// NtWow64QueryInformationProcess64 returns Wine's 64-bit basic record.
pub fn query_process64(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[process, class, info, size, returned] = args else {
        unreachable!()
    };
    if class != 0 {
        return Outcome::Return(status::NOT_IMPLEMENTED);
    }
    status::finish((|| {
        let mut len = 48;
        let mut result = status::INFO_LENGTH_MISMATCH;
        if size >= 48 {
            if info == 0 {
                result = status::ACCESS_VIOLATION;
                len = 0;
            } else {
                let mut request = GetProcessInfoRequest {
                    handle: process,
                    ..Default::default()
                };
                let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
                result = reply.header.error;
                if result == 0 {
                    let mut data = [0; 48];
                    data[..4].copy_from_slice(&reply.exit_code.to_le_bytes());
                    data[8..16].copy_from_slice(&u64::from(reply.peb as u32).to_le_bytes());
                    data[16..24]
                        .copy_from_slice(&(reply.affinity & ((1 << PROCESSORS) - 1)).to_le_bytes());
                    data[24..28].copy_from_slice(&u32::from(reply.base_priority).to_le_bytes());
                    data[32..40].copy_from_slice(&u64::from(reply.pid).to_le_bytes());
                    data[40..48].copy_from_slice(&u64::from(reply.ppid).to_le_bytes());
                    write_bytes(env, info, &data)?;
                }
            }
            if size > 48 {
                result = status::INFO_LENGTH_MISMATCH;
            }
        }
        if returned != 0 {
            write_u32(env, returned, len)?;
        }
        status::check(result)
    })())
}

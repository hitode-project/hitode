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
        if process != CURRENT_PROCESS {
            return Err(Error::Unsupported("querying another process".into()));
        }
        let bytes = match class {
            // Nothing debugs a Hitode process, and an i386 process is
            // native, not WoW64.
            PROCESS_DEBUG_PORT | PROCESS_WOW64_INFORMATION => vec![0; 4],
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
    let &[_process, class, info, size] = args else {
        unreachable!()
    };
    status::finish((|| match class {
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

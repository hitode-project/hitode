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
const PROCESS_WOW64_INFORMATION: u32 = 26;
const PROCESS_IMAGE_INFORMATION: u32 = 37;

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

const THREAD_BASIC_INFORMATION: u32 = 0;
const THREAD_BASIC_INFORMATION_SIZE: usize = 28;

/// NtQueryInformationThread(thread, class, *info, size, *result_size)
pub fn query_thread(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[thread, class, info, size, result_size] = args else {
        unreachable!()
    };
    status::finish((|| {
        if class != THREAD_BASIC_INFORMATION {
            return Err(Error::Unsupported(format!(
                "thread information class {class}"
            )));
        }
        let mut request = GetThreadInfoRequest {
            handle: thread,
            ..Default::default()
        };
        let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
        status::check(reply.header.error)?;
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
        write_bytes(env, info, &bytes[..count])?;
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

//! CloseHandle and DuplicateHandle, once for every kernel object in the machine's table.
use adv_abi::Registry;
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::{Env, Object, Outcome};

use super::K32;
use crate::util::{err, set_last_error};
use crate::{STD_ERROR, STD_INPUT, STD_OUTPUT, stdcall};

const CURRENT_PROCESS: u32 = u32::MAX;
const CURRENT_THREAD: u32 = u32::MAX - 1;
const DUPLICATE_CLOSE_SOURCE: u32 = 1;

/// What a duplicated current-process pseudo-handle names. Never signaled: the process is
/// still running whenever a guest could wait on it.
pub struct ProcessHandle;

fn close_handle(env: &mut Env<'_>) -> Outcome {
    let h = env.arg(0);
    // The last handle drops the object: files close, sections lose a handle.
    let object = env.sys.objects.close(h).is_some();
    super::file_mapping::prune(env);
    if object || matches!(h, STD_INPUT | STD_OUTPUT | STD_ERROR) || h >= CURRENT_THREAD {
        return Outcome::Return(1);
    }
    log::debug!("CloseHandle({h:#x}): unknown handle");
    set_last_error(env, err::INVALID_HANDLE);
    Outcome::Return(0)
}

/// A new handle to what `source` names in this process.
fn duplicate(env: &mut Env<'_>, source: u32) -> Result<u32, u32> {
    match source {
        CURRENT_PROCESS => match env.sys.objects.insert_host(ProcessHandle) {
            0 => Err(err::NOT_ENOUGH_MEMORY),
            handle => Ok(handle),
        },
        CURRENT_THREAD => {
            let ctx = env.context;
            let thread = env.sys.context(ctx).thread_handle;
            // The guest may have closed the handle CreateThread returned.
            Ok(match env.sys.objects.identity(thread) {
                Some(identity) => env
                    .sys
                    .objects
                    .alias_identity(identity)
                    .ok_or(err::NOT_ENOUGH_MEMORY)?,
                None => match env.sys.objects.insert(Object::Thread { ctx }) {
                    0 => return Err(err::NOT_ENOUGH_MEMORY),
                    handle => handle,
                },
            })
        }
        STD_INPUT | STD_OUTPUT | STD_ERROR => Ok(source),
        _ => {
            let identity = env
                .sys
                .objects
                .identity(source)
                .ok_or(err::INVALID_HANDLE)?;
            env.sys
                .objects
                .alias_identity(identity)
                .ok_or(err::NOT_ENOUGH_MEMORY)
        }
    }
}

fn duplicate_handle(env: &mut Env<'_>) -> Outcome {
    let (source_process, source, target_process, out) =
        (env.arg(0), env.arg(1), env.arg(2), env.arg(3));
    let options = env.arg(6);
    let result = (|| {
        // Only this process exists; its pseudo-handle is the only process handle.
        if source_process != CURRENT_PROCESS || target_process != CURRENT_PROCESS {
            return Err(err::INVALID_HANDLE);
        }
        if out != 0 && !env.cpu.accessible(out, 4, Prot::WRITE) {
            return Err(998); // ERROR_NOACCESS
        }
        if out == 0 {
            return Ok(());
        }
        let handle = duplicate(env, source)?;
        let _ = env.cpu.write_u32(out, handle);
        Ok(())
    })();
    // The source closes even when duplication fails.
    if options & DUPLICATE_CLOSE_SOURCE != 0 && source_process == CURRENT_PROCESS {
        env.sys.objects.close(source);
        super::file_mapping::prune(env);
    }
    match result {
        Ok(()) => Outcome::Return(1),
        Err(error) => {
            set_last_error(env, error);
            Outcome::Return(0)
        }
    }
}

pub(super) fn register(reg: &mut Registry) {
    stdcall(reg, K32, "CloseHandle", 1, close_handle);
    stdcall(reg, K32, "DuplicateHandle", 7, duplicate_handle);
}

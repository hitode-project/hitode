//! Toolhelp snapshots enumerate guest contexts; host threads are never exposed.
use pal_cpu::{GuestExt, Prot};
use pal_machine::{Env, Object, Outcome, Registry};

use super::K32;
use crate::util::{err, set_last_error};
use crate::{Win32, stdcall};

fn create_snapshot(env: &mut Env<'_>) -> Outcome {
    // This implementation supports TH32CS_SNAPTHREAD, whose process argument is ignored
    // on Windows. Other snapshot categories are not implemented yet.
    if env.arg(0) != 4 {
        set_last_error(env, err::INVALID_PARAMETER);
        return Outcome::Return(u32::MAX);
    }
    let rows = env.sys.contexts.iter().filter(|c| !c.is_done()).map(|c| (c.tid, env.sys.pid)).collect();
    let handle = env.sys.objects.insert(Object::Opaque(0));
    env.ext.get_mut::<Win32>().thread_snapshots.insert(handle, (rows, 0));
    Outcome::Return(handle)
}

fn thread_entry(env: &mut Env<'_>, first: bool) -> Outcome {
    let (handle, out) = (env.arg(0), env.arg(1));
    if !env.cpu.accessible(out, 4, Prot::READ) {
        set_last_error(env, 998); // ERROR_NOACCESS
        return Outcome::Return(0);
    }
    if env.cpu.read_u32(out).unwrap_or(0) < 28 {
        set_last_error(env, 24); // ERROR_BAD_LENGTH
        return Outcome::Return(0);
    }
    if !env.cpu.accessible(out, 28, Prot::WRITE) {
        set_last_error(env, 998); // ERROR_NOACCESS
        return Outcome::Return(0);
    }
    let Some((rows, cursor)) = env.ext.get::<Win32>().thread_snapshots.get(&handle) else {
        set_last_error(env, err::INVALID_HANDLE);
        return Outcome::Return(0);
    };
    let index = if first { 0 } else { *cursor };
    let Some(&(tid, pid)) = rows.get(index) else {
        set_last_error(env, 18); // ERROR_NO_MORE_FILES
        return Outcome::Return(0);
    };
    for (i, value) in [28, 0, tid, pid, 8, 0, 0].into_iter().enumerate() {
        if env.cpu.write_u32(out + i as u32 * 4, value).is_err() {
            set_last_error(env, 998); // ERROR_NOACCESS
            return Outcome::Return(0);
        }
    }
    // Enumeration position changes only after a successful complete output.
    env.ext.get_mut::<Win32>().thread_snapshots.get_mut(&handle).unwrap().1 = index + 1;
    Outcome::Return(1)
}

pub(super) fn register(reg: &mut Registry) {
    stdcall(reg, K32, "CreateToolhelp32Snapshot", 2, create_snapshot);
    stdcall(reg, K32, "Thread32First", 2, |e| thread_entry(e, true));
    stdcall(reg, K32, "Thread32Next", 2, |e| thread_entry(e, false));
}

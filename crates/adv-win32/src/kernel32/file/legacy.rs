//! The 16-bit-era HFILE functions. Like kernel32, they are thin layers over
//! CreateFileA, ReadFile, WriteFile, SetEndOfFile, SetFilePointer and
//! CloseHandle, invoked directly rather than through a hookable import.
use super::K32;
use crate::stdcall;
use adv_abi::Registry;
use adv_cpu::backend::{GuestExt, Reg};
use adv_machine::{
    Env, Outcome,
    layout::{THUNK_BASE, THUNK_STRIDE},
};

const HFILE_ERROR: u32 = u32::MAX;
const GENERIC_READ: u32 = 0x8000_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;

fn call(
    env: &mut Env<'_>,
    name: &str,
    arguments: Vec<u32>,
    then: impl FnOnce(&mut Env<'_>, u32) -> u32 + 'static,
) -> Outcome {
    let entry = match env.sys.thunks.resolve(env.cpu, K32, name) {
        Ok(entry) => entry,
        Err(error) => return Outcome::Fail(error.to_string()),
    };
    let binding = (entry - THUNK_BASE) / THUNK_STRIDE;
    env.invoke_host_u32(binding, arguments, move |env, result| {
        Outcome::Return(then(env, result.value()))
    })
}

/// A transfer count lands in the free stack below the caller's frame; no
/// guest callback runs while the host call uses it.
fn count_slot(env: &Env<'_>) -> u32 {
    (env.cpu.reg(Reg::Esp) - 16) & !3
}

fn open(env: &mut Env<'_>) -> Outcome {
    let (path, mode) = (env.arg(0), env.arg(1));
    let access = match mode & 3 {
        0 => GENERIC_READ,
        1 => GENERIC_WRITE,
        _ => GENERIC_READ | GENERIC_WRITE,
    };
    // OF_SHARE_EXCLUSIVE, DENY_WRITE and DENY_READ; COMPAT and DENY_NONE share both.
    let share = match mode & 0x70 {
        0x10 => 0,
        0x20 => 1,
        0x30 => 2,
        _ => 3,
    };
    call(
        env,
        "CreateFileA",
        vec![path, access, share, 0, 3, 0x80, 0],
        |_, handle| handle,
    )
}

fn create(env: &mut Env<'_>) -> Outcome {
    let (path, attributes) = (env.arg(0), env.arg(1) & 0x27);
    let attributes = if attributes == 0 { 0x80 } else { attributes };
    let access = GENERIC_READ | GENERIC_WRITE;
    call(
        env,
        "CreateFileA",
        vec![path, access, 3, 0, 2, attributes, 0],
        |_, handle| handle,
    )
}

fn transfer(env: &mut Env<'_>, name: &str) -> Outcome {
    let (handle, buffer, length) = (env.arg(0), env.arg(1), env.arg(2));
    let slot = count_slot(env);
    call(
        env,
        name,
        vec![handle, buffer, length, slot, 0],
        move |env, ok| {
            if ok == 0 {
                HFILE_ERROR
            } else {
                env.cpu.read_u32(slot).unwrap_or(HFILE_ERROR)
            }
        },
    )
}

fn write(env: &mut Env<'_>) -> Outcome {
    // A zero-length write truncates the file at the current position.
    if env.arg(2) == 0 {
        let handle = env.arg(0);
        return call(env, "SetEndOfFile", vec![handle], |_, ok| {
            if ok == 0 { HFILE_ERROR } else { 0 }
        });
    }
    transfer(env, "WriteFile")
}

fn seek(env: &mut Env<'_>) -> Outcome {
    let (handle, offset, origin) = (env.arg(0), env.arg(1), env.arg(2));
    call(
        env,
        "SetFilePointer",
        vec![handle, offset, 0, origin],
        |_, position| position,
    )
}

fn close(env: &mut Env<'_>) -> Outcome {
    let handle = env.arg(0);
    call(env, "CloseHandle", vec![handle], |_, ok| {
        if ok == 0 { HFILE_ERROR } else { 0 }
    })
}

pub(super) fn register(reg: &mut Registry) {
    stdcall(reg, K32, "_lopen", 2, open);
    stdcall(reg, K32, "_lcreat", 2, create);
    stdcall(reg, K32, "_lread", 3, |env| transfer(env, "ReadFile"));
    stdcall(reg, K32, "_hread", 3, |env| transfer(env, "ReadFile"));
    stdcall(reg, K32, "_lwrite", 3, write);
    stdcall(reg, K32, "_hwrite", 3, write);
    stdcall(reg, K32, "_llseek", 3, seek);
    stdcall(reg, K32, "_lclose", 1, close);
}

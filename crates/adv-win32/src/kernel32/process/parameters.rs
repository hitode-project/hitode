//! Process-owned, normalized x86 RTL_USER_PROCESS_PARAMETERS prefix.
//!
//! The published winternl.h layout ends after CommandLine (0x48 bytes on x86):
//! https://learn.microsoft.com/en-us/windows/win32/api/winternl/ns-winternl-rtl_user_process_parameters
//! Its internal Flags field at +8 is also read directly by desktop MSVC CRTs.
//! The normalized flag and prefix layout are described by Wine's winternl.h:
//! https://github.com/wine-mirror/wine/blob/master/include/winternl.h
//! Unmodeled reserved fields remain zero; this is not a full RTL parameter API.

use adv_cpu::backend::{Backend, GuestExt, MemFault};
use adv_machine::{
    Env, Machine,
    layout::{PEB_ADDR, peb},
};
use adv_os::Heaps;

use crate::Win32;

const SIZE: u32 = 0x48;
const NORMALIZED: u32 = 1;
const IMAGE_PATH: u32 = 0x38;
const COMMAND_LINE: u32 = 0x40;

fn unicode_string(
    cpu: &mut dyn adv_cpu::backend::Guest,
    at: u32,
    pointer: u32,
    text: &str,
) -> Result<(), MemFault> {
    let length = text
        .encode_utf16()
        .count()
        .checked_mul(2)
        .and_then(|length| u16::try_from(length).ok())
        .filter(|&length| length <= u16::MAX - 2)
        .ok_or(MemFault {
            addr: pointer,
            len: 0,
        })?;
    cpu.write_u32(at, u32::from(length) | (u32::from(length + 2) << 16))?;
    cpu.write_u32(at + 4, pointer)
}

pub(crate) fn install<B: Backend>(machine: &mut Machine<B>) -> Result<(), MemFault> {
    let mut env = Env {
        context: machine.sys.current,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: 0,
    };
    // CommandLine borrows the same stable storage as GetCommandLineW and CRT
    // _wcmdln; no host path or host command line is exposed to the guest.
    let command_line = super::command_line(&mut env, true);
    if command_line == 0 {
        return Err(MemFault { addr: 0, len: 2 });
    }
    let line = env
        .ext
        .try_get::<adv_os::processes::Parameters>()
        .map(|p| p.command_line.clone())
        .unwrap_or_else(|| format!("\"{}\"", env.ext.get::<Win32>().exe_path()));
    let (win32, heaps) = env.ext.get2_mut::<Win32, Heaps>();
    let image = win32.exe_path();
    let image_bytes: Vec<_> = image
        .encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect();
    let image_pointer =
        win32.static_bytes("peb_image_path_w", env.cpu, env.sys, heaps, &image_bytes);
    if image_pointer == 0 {
        return Err(MemFault {
            addr: 0,
            len: image_bytes.len() as u32,
        });
    }
    let parameters = win32.static_bytes(
        "peb_process_parameters",
        env.cpu,
        env.sys,
        heaps,
        &[0; SIZE as usize],
    );
    if parameters == 0 {
        return Err(MemFault { addr: 0, len: SIZE });
    }
    env.cpu.write_u32(parameters, SIZE)?;
    env.cpu.write_u32(parameters + 4, SIZE)?;
    env.cpu.write_u32(parameters + 8, NORMALIZED)?;
    unicode_string(env.cpu, parameters + IMAGE_PATH, image_pointer, &image)?;
    unicode_string(env.cpu, parameters + COMMAND_LINE, command_line, &line)?;
    env.cpu
        .write_u32(PEB_ADDR + peb::PROCESS_PARAMETERS, parameters)
}

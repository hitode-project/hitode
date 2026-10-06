//! Guest memory helpers shared by the Win32 handlers.

use adv_cpu::backend::{Guest, GuestExt};
use adv_machine::Env;
use adv_machine::layout::teb;

pub const MAX_STR: usize = 0x1_0000;

/// NUL-terminated byte string at `p` (empty for NULL).
pub fn cstr(cpu: &dyn Guest, p: u32) -> Vec<u8> {
    if p == 0 {
        return Vec::new();
    }
    cpu.read_cstr(p, MAX_STR).unwrap_or_default()
}

/// NUL-terminated UTF-16 string at `p` (empty for NULL).
pub fn wstr(cpu: &dyn Guest, p: u32) -> Vec<u16> {
    wstr_up_to(cpu, p, MAX_STR)
}

fn wstr_up_to(cpu: &dyn Guest, p: u32, max: usize) -> Vec<u16> {
    let mut out = Vec::new();
    if p == 0 {
        return out;
    }
    let mut a = p;
    while out.len() < max {
        match cpu.read_u16(a) {
            Ok(0) | Err(_) => break,
            Ok(c) => out.push(c),
        }
        a += 2;
    }
    out
}

/// `len` bytes at `p`, or up to the NUL if `len` is -1 (terminator excluded).
/// Unlike names, a -1 conversion source has no length cap: Kirikiri passes
/// whole decompressed scripts this way.
pub fn bytes_or_cstr(cpu: &dyn Guest, p: u32, len: i32) -> Vec<u8> {
    if len < 0 {
        if p == 0 {
            return Vec::new();
        }
        let rest = (u32::MAX - p) as usize + 1;
        return cpu.read_cstr(p, rest).unwrap_or_default();
    }
    let mut v = vec![0; len as usize];
    let _ = cpu.read(p, &mut v);
    v
}

/// `len` units at `p`, or up to the NUL if `len` is -1 (terminator excluded).
pub fn units_or_wstr(cpu: &dyn Guest, p: u32, len: i32) -> Vec<u16> {
    if len < 0 {
        return wstr_up_to(cpu, p, (u32::MAX - p) as usize / 2 + 1);
    }
    let mut v = vec![0u8; len as usize * 2];
    let _ = cpu.read(p, &mut v);
    v.as_chunks::<2>()
        .0
        .iter()
        .map(|&c| u16::from_le_bytes(c))
        .collect()
}

pub fn write_units(cpu: &mut dyn Guest, p: u32, units: &[u16]) {
    let bytes: Vec<u8> = units.iter().flat_map(|u| u.to_le_bytes()).collect();
    let _ = cpu.write(p, &bytes);
}

/// Copy `s` plus a NUL into a guest buffer of `cap` bytes. Returns the length without the
/// NUL, or `None` (buffer untouched except for truncation to `cap - 1`) if it didn't fit.
pub fn put_cstr(cpu: &mut dyn Guest, buf: u32, cap: u32, s: &[u8]) -> Option<u32> {
    if cap == 0 {
        return None;
    }
    let n = s.len().min(cap as usize - 1);
    let mut v = s[..n].to_vec();
    v.push(0);
    let _ = cpu.write(buf, &v);
    (n == s.len()).then_some(n as u32)
}

/// UTF-16 counterpart of [`put_cstr`]; `cap` in units.
pub fn put_wstr(cpu: &mut dyn Guest, buf: u32, cap: u32, s: &[u16]) -> Option<u32> {
    if cap == 0 {
        return None;
    }
    let n = s.len().min(cap as usize - 1);
    let mut v = s[..n].to_vec();
    v.push(0);
    write_units(cpu, buf, &v);
    (n == s.len()).then_some(n as u32)
}

/// SetLastError on the current context's TEB.
pub fn set_last_error(env: &mut Env<'_>, code: u32) {
    let t = env.ctx().teb;
    let _ = env.cpu.write_u32(t + teb::LAST_ERROR, code);
}

pub fn last_error(env: &Env<'_>) -> u32 {
    env.cpu
        .read_u32(env.ctx().teb + teb::LAST_ERROR)
        .unwrap_or(0)
}

/// Write `v` to guest `p` if `p` is not NULL.
pub fn out_u32(cpu: &mut dyn Guest, p: u32, v: u32) {
    if p != 0 {
        let _ = cpu.write_u32(p, v);
    }
}

pub fn zero(cpu: &mut dyn Guest, p: u32, len: u32) {
    let _ = cpu.write(p, &vec![0; len as usize]);
}

/// A resource/atom name argument: an integer (`MAKEINTRESOURCE`, `< 0x10000`) or a string.
pub enum IntOrStr {
    Int(u16),
    Str(String),
}

pub mod err {
    pub const SUCCESS: u32 = 0;
    pub const FILE_NOT_FOUND: u32 = 2;
    pub const PATH_NOT_FOUND: u32 = 3;
    pub const ACCESS_DENIED: u32 = 5;
    pub const INVALID_HANDLE: u32 = 6;
    pub const NOT_ENOUGH_MEMORY: u32 = 8;
    pub const NO_MORE_FILES: u32 = 18;
    pub const FILE_EXISTS: u32 = 80;
    pub const INVALID_PARAMETER: u32 = 87;
    pub const INSUFFICIENT_BUFFER: u32 = 122;
    pub const MOD_NOT_FOUND: u32 = 126;
    pub const PROC_NOT_FOUND: u32 = 127;
    pub const ALREADY_EXISTS: u32 = 183;
    pub const RESOURCE_TYPE_NOT_FOUND: u32 = 1813;
    pub const RESOURCE_NAME_NOT_FOUND: u32 = 1814;
}

pub const INVALID_HANDLE_VALUE: u32 = u32::MAX;

#[cfg(test)]
mod tests {
    use super::*;
    use adv_cpu::backend::Prot;

    #[test]
    fn terminated_conversion_sources_are_not_capped() {
        let mut cpu = adv_cpu::JitCpu::new(adv_machine::layout::PARK_ADDR, false).unwrap();
        cpu.map(0x400000, 0x30000, Prot::RW).unwrap();
        let len = MAX_STR + 0x1234;
        cpu.write(0x400000, &vec![b'a'; len]).unwrap();
        assert_eq!(bytes_or_cstr(&cpu, 0x400000, -1).len(), len);
        assert_eq!(cstr(&cpu, 0x400000).len(), MAX_STR);
        let units = len / 2 - 7;
        cpu.write(0x400000 + units as u32 * 2, &[0, 0]).unwrap();
        assert_eq!(units_or_wstr(&cpu, 0x400000, -1).len(), units);
    }
}

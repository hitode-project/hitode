//! version.dll: version resources of guest files.
//!
//! The block from GetFileVersionInfoA is the raw (UTF-16) VS_VERSIONINFO resource, the
//! "FE2X" signature and an equally large second half; VerQueryValueA converts string values
//! to ANSI into that second half, as Windows does.

use adv_abi::{GuestPtr, Registry, WideStr, guest::check_range, win32};
use adv_cpu::backend::{Guest, GuestExt, Prot};
use adv_machine::{Env, Outcome};
use adv_pe::{Pe, ResId};

use crate::util::{MAX_STR, cstr, err, out_u32, set_last_error};
use crate::{Win32, stdcall};

const VERSION: &str = "version.dll";
const RT_VERSION: u16 = 16;

/// The raw VS_VERSIONINFO resource of a guest file.
fn version_resource(env: &Env<'_>, path: &str) -> Option<Vec<u8>> {
    let w = env.ext.get::<Win32>();
    let host = w.vfs.lookup(path)?;
    let pe = Pe::parse(&std::fs::read(host).ok()?).ok()?;
    let res = pe.resources().ok()??;
    let leaf = res
        .leaves()
        .into_iter()
        .find(|l| l.ty == ResId::Id(RT_VERSION))?;
    pe.slice(leaf.data.rva, leaf.data.size)
        .ok()
        .map(<[u8]>::to_vec)
}

/// GetFileVersionInfoSize for a 32-bit (UTF-16) resource: twice the resource, the second
/// half being room for ANSI conversions, plus the 4-byte "FE2X" signature (Windows XP and
/// later; Wine alike). LL1's native trace shows 1452 for its 724-byte resource.
fn scratch_size(raw: usize) -> u32 {
    (raw * 2 + 4) as u32
}

fn get_file_version_info_size_a(env: &mut Env<'_>) -> Outcome {
    let (p, handle) = (env.arg(0), env.arg(1));
    let path = env.ext.get::<Win32>().codec.decode(&cstr(env.cpu, p));
    out_u32(env.cpu, handle, 0);
    match version_resource(env, &path) {
        Some(raw) => Outcome::Return(scratch_size(raw.len())),
        None => {
            log::info!("GetFileVersionInfoSizeA({path}): no version resource");
            set_last_error(env, err::RESOURCE_TYPE_NOT_FOUND);
            Outcome::Return(0)
        }
    }
}

fn get_file_version_info_a(env: &mut Env<'_>) -> Outcome {
    let (p, len, data) = (env.arg(0), env.arg(2), env.arg(3));
    let path = env.ext.get::<Win32>().codec.decode(&cstr(env.cpu, p));
    let Some(mut raw) = version_resource(env, &path) else {
        set_last_error(env, err::RESOURCE_TYPE_NOT_FOUND);
        return Outcome::Return(0);
    };
    let w_length = raw
        .get(..2)
        .map_or(0, |b| usize::from(u16::from_le_bytes([b[0], b[1]])));
    raw.resize(scratch_size(raw.len()).min(len) as usize, 0);
    // The signature follows the resource when the buffer has room for it.
    if let Some(sig) = raw.get_mut(w_length..w_length + 4) {
        sig.copy_from_slice(b"FE2X");
    }
    let _ = env.cpu.write(data, &raw);
    Outcome::Return(1)
}

/// A node of the VS_VERSIONINFO tree in guest memory.
struct Node {
    at: u32,
    len: u32,
    value_len: u32,
    text: bool,
    key: String,
    value_at: u32,
    children_at: u32,
}

fn node(cpu: &dyn Guest, at: u32) -> Option<Node> {
    let len = u32::from(cpu.read_u16(at).ok()?);
    let end = at.checked_add(len)?;
    if len < 6 || check_range(cpu, at, len, Prot::READ).is_err() {
        return None;
    }
    let value_len = u32::from(cpu.read_u16(at + 2).ok()?);
    let text = cpu.read_u16(at + 4).ok()? == 1;
    let mut key = Vec::new();
    let mut p = at + 6;
    loop {
        if p.checked_add(2)? > end {
            return None;
        }
        let c = cpu.read_u16(p).ok()?;
        p += 2;
        if c == 0 {
            break;
        }
        key.push(c);
    }
    let value_at = p.checked_add(3)? & !3;
    let value_bytes = if text { value_len * 2 } else { value_len };
    let value_end = value_at.checked_add(value_bytes)?;
    if value_end > end {
        return None;
    }
    let children_at = value_end.checked_add(3)? & !3;
    Some(Node {
        at,
        len,
        value_len,
        text,
        key: String::from_utf16_lossy(&key),
        value_at,
        children_at,
    })
}

fn find(cpu: &dyn Guest, root: &Node, path: &[&str]) -> Option<Node> {
    let Some((first, rest)) = path.split_first() else {
        return node(cpu, root.at);
    };
    let mut c = root.children_at;
    let end = root.at.checked_add(root.len)?;
    while c < end {
        let n = node(cpu, c)?;
        if c.checked_add(n.len)? > end {
            return None;
        }
        if n.key.eq_ignore_ascii_case(first) {
            return find(cpu, &n, rest);
        }
        c = c.checked_add(n.len)?.checked_add(3)? & !3;
    }
    None
}

fn wide_path(env: &mut Env<'_>, path: WideStr) -> Option<String> {
    match path.read(env.cpu, MAX_STR) {
        Ok(path) if !path.is_empty() => Some(String::from_utf16_lossy(&path)),
        Ok(_) => {
            set_last_error(env, err::INVALID_PARAMETER);
            None
        }
        Err(_) => {
            set_last_error(env, 998);
            None
        }
    }
}

fn wide_resource(env: &mut Env<'_>, path: WideStr) -> Option<Vec<u8>> {
    let path = wide_path(env, path)?;
    if env.ext.get::<Win32>().vfs.lookup(&path).is_none() {
        set_last_error(env, 2);
        return None;
    }
    version_resource(env, &path).or_else(|| {
        set_last_error(env, err::RESOURCE_TYPE_NOT_FOUND);
        None
    })
}

#[win32(version, name = "GetFileVersionInfoSizeW", level = implement)]
fn version_size_w(env: &mut Env<'_>, path: WideStr, handle: GuestPtr<u32>) -> u32 {
    if !handle.is_null() && check_range(env.cpu, handle.addr(), 4, Prot::WRITE).is_err() {
        set_last_error(env, 998);
        return 0;
    }
    if !handle.is_null() {
        let _ = env.cpu.write_u32(handle.addr(), 0);
    }
    wide_resource(env, path).map_or(0, |raw| scratch_size(raw.len()))
}

#[win32(version, name = "GetFileVersionInfoW", level = implement)]
fn version_info_w(
    env: &mut Env<'_>,
    path: WideStr,
    _handle: u32,
    length: u32,
    output: GuestPtr<()>,
) -> i32 {
    if length == 0 || output.is_null() {
        set_last_error(env, err::INVALID_PARAMETER);
        return 0;
    }
    let Some(mut raw) = wide_resource(env, path) else {
        return 0;
    };
    let written = scratch_size(raw.len()).min(length);
    if check_range(env.cpu, output.addr(), written, Prot::WRITE).is_err() {
        set_last_error(env, 998);
        return 0;
    }
    let root_length = raw.get(..2).map_or(0, |bytes| {
        usize::from(u16::from_le_bytes([bytes[0], bytes[1]]))
    });
    raw.resize(written as usize, 0);
    if let Some(signature) = raw.get_mut(root_length..root_length + 4) {
        signature.copy_from_slice(b"FE2X");
    }
    if env.cpu.write(output.addr(), &raw).is_err() {
        set_last_error(env, 998);
        return 0;
    }
    1
}

#[win32(version, name = "VerQueryValueW", level = implement)]
fn query_w(
    env: &mut Env<'_>,
    block: GuestPtr<()>,
    path: WideStr,
    output: GuestPtr<GuestPtr<()>>,
    length: GuestPtr<u32>,
) -> i32 {
    if output.is_null() || length.is_null() {
        set_last_error(env, err::INVALID_PARAMETER);
        return 0;
    }
    if check_range(env.cpu, output.addr(), 4, Prot::WRITE).is_err()
        || check_range(env.cpu, length.addr(), 4, Prot::WRITE).is_err()
    {
        set_last_error(env, 998);
        return 0;
    }
    let Some(path) = wide_path(env, path) else {
        return 0;
    };
    let parts: Vec<_> = path.split('\\').filter(|part| !part.is_empty()).collect();
    let found = node(env.cpu, block.addr()).and_then(|root| find(env.cpu, &root, &parts));
    let Some(found) = found.filter(|value| value.value_len != 0) else {
        return 0;
    };
    // W returns a pointer into the original UTF-16 resource; it needs no ANSI scratch conversion.
    let _ = env.cpu.write_u32(output.addr(), found.value_at);
    let _ = env.cpu.write_u32(length.addr(), found.value_len);
    1
}

fn ver_query_value_a(env: &mut Env<'_>) -> Outcome {
    let (block, sub, out_ptr, out_len) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3));
    let subpath = String::from_utf8_lossy(&cstr(env.cpu, sub)).into_owned();
    let parts: Vec<&str> = subpath.split('\\').filter(|s| !s.is_empty()).collect();
    let Some(root) = node(env.cpu, block) else {
        return Outcome::Return(0);
    };
    let Some(n) = find(env.cpu, &root, &parts) else {
        log::debug!("VerQueryValueA({subpath}): not found");
        return Outcome::Return(0);
    };
    if !n.text || n.value_len == 0 {
        out_u32(env.cpu, out_ptr, n.value_at);
        out_u32(env.cpu, out_len, n.value_len);
        return Outcome::Return(u32::from(n.value_len != 0));
    }
    // Text value: converted to ANSI at the mirrored place in the second half of the block,
    // after the resource and its 4-byte signature, at most up to the block's end (Wine
    // `VerQueryValueA`); repeated queries of one value land at the same address.
    let units: Vec<u16> = (0..n.value_len)
        .map(|i| env.cpu.read_u16(n.value_at + 2 * i).unwrap_or(0))
        .take_while(|&u| u != 0)
        .collect();
    let (mut ansi, _) = env.ext.get::<Win32>().codec.from_utf16(0, &units);
    ansi.push(0);
    let pos = n.value_at - block;
    ansi.truncate(root.len.saturating_sub(pos) as usize);
    let dst = block + root.len + 4 + pos;
    let _ = env.cpu.write(dst, &ansi);
    out_u32(env.cpu, out_ptr, dst);
    out_u32(env.cpu, out_len, ansi.len() as u32);
    Outcome::Return(1)
}

pub fn register(reg: &mut Registry) {
    reg.register(version_size_w_ABI);
    reg.register(version_info_w_ABI);
    reg.register(query_w_ABI);
    stdcall(
        reg,
        VERSION,
        "GetFileVersionInfoSizeA",
        2,
        get_file_version_info_size_a,
    );
    stdcall(
        reg,
        VERSION,
        "GetFileVersionInfoA",
        4,
        get_file_version_info_a,
    );
    stdcall(reg, VERSION, "VerQueryValueA", 4, ver_query_value_a);
}

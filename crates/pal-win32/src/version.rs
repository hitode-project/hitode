//! version.dll: version resources of guest files.
//!
//! The block from GetFileVersionInfoA is the raw (UTF-16) VS_VERSIONINFO resource, the
//! "FE2X" signature and an equally large second half; VerQueryValueA converts string values
//! to ANSI into that second half, as Windows does.

use pal_cpu::{Guest, GuestExt};
use pal_machine::{Env, Outcome, Registry};
use pal_pe::{Pe, ResId};

use crate::util::{cstr, err, out_u32, set_last_error};
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
    let value_len = u32::from(cpu.read_u16(at + 2).ok()?);
    let text = cpu.read_u16(at + 4).ok()? == 1;
    let mut key = Vec::new();
    let mut p = at + 6;
    loop {
        let c = cpu.read_u16(p).ok()?;
        p += 2;
        if c == 0 {
            break;
        }
        key.push(c);
    }
    let value_at = p.next_multiple_of(4);
    let value_bytes = if text { value_len * 2 } else { value_len };
    let children_at = (value_at + value_bytes).next_multiple_of(4);
    (len >= 6).then(|| Node {
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
    while c < root.at + root.len {
        let n = node(cpu, c)?;
        if n.key.eq_ignore_ascii_case(first) {
            return find(cpu, &n, rest);
        }
        c = (c + n.len.max(1)).next_multiple_of(4);
    }
    None
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

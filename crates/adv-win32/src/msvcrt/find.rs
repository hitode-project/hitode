//! Legacy time64/size32 searches borrow the shared VFS rows and Objects payload.
//! A typed CRT alias keeps these intptr_t values separate from Kernel32 handles.
use adv_abi::{
    GuestPtr, Registry, WideStr,
    guest::{check_range, memory_failure},
    win32,
};
use adv_cpu::backend::{MemFault, Prot};
use adv_machine::{Env, Namespace, Objects, Outcome};
use adv_os::vfs::{DirEntry, wildcard};
use std::time::{SystemTime, UNIX_EPOCH};

use super::{floating, invalid_parameter_result};
use crate::{FindObj, Win32, util::set_last_error};

const MODULES: [&str; 3] = ["msvcrt.dll", "msvcr80.dll", "msvcr90.dll"];
const DATA_BYTES: u32 = 560;
const MAX_ROWS: usize = 32768;
const MAX_SEARCHES: usize = 256;
const MAX_SNAPSHOT_BYTES: usize = 64 * 1024 * 1024;

#[derive(Default)]
struct Searches {
    // No second cursor/handle registry: all frozen rows live in FindObj payloads.
    namespace: Option<Namespace>,
}

fn snapshot_bytes(entries: &[DirEntry]) -> usize {
    entries
        .iter()
        .map(|row| std::mem::size_of::<DirEntry>() + row.name.len())
        .sum()
}

impl Searches {
    fn get<'a>(&self, objects: &'a Objects, handle: u32) -> Option<&'a FindObj> {
        let id = objects.lookup_alias::<FindObj>(self.namespace?, handle)?;
        objects.owned(id)
    }

    fn get_mut<'a>(&self, objects: &'a mut Objects, handle: u32) -> Option<&'a mut FindObj> {
        let id = objects.lookup_alias::<FindObj>(self.namespace?, handle)?;
        objects.owned_mut(id)
    }

    fn insert(&mut self, objects: &mut Objects, entries: Vec<DirEntry>) -> Option<u32> {
        if self.namespace.is_none() {
            self.namespace = objects.new_namespace_in(0x0b00_0004, 0x0bff_fffc);
        }
        let namespace = self.namespace?;
        let bytes: usize = objects
            .hosts_in::<FindObj>(namespace)
            .map(|(_, search)| snapshot_bytes(&search.entries))
            .sum();
        if objects.alias_count(namespace) >= MAX_SEARCHES
            || bytes + snapshot_bytes(&entries) > MAX_SNAPSHOT_BYTES
        {
            return None;
        }
        objects.insert_host_in(namespace, FindObj { entries, next: 0 })
    }

    fn close(&self, objects: &mut Objects, handle: u32) -> bool {
        if self.get(objects, handle).is_none() {
            return false;
        }
        objects
            .close_alias(self.namespace.unwrap(), handle)
            .is_some()
    }
}

fn api(env: &Env<'_>, name: &str) -> String {
    format!("{}!{name}", env.sys.thunks.get(env.thunk).module)
}

fn error_cell(env: &mut Env<'_>) -> Result<GuestPtr<i32>, Outcome> {
    let errno = floating::errno_address(env);
    if errno.is_null() {
        return Err(Outcome::Fail("CRT find errno allocation failed".into()));
    }
    check_range(env.cpu, errno.addr(), 4, Prot::WRITE).map_err(memory_failure)?;
    Ok(errno)
}

fn fail(env: &mut Env<'_>, errno: GuestPtr<i32>, value: i32, dos: u32) -> Outcome {
    // VC90 find/next/close leave the adjacent DOS-errno cell unchanged, even on
    // failure. Use its existing module/thread owner, but write only errno.
    if let Err(error) = errno.write(env.cpu, value) {
        return memory_failure(error);
    }
    set_last_error(env, dos);
    Outcome::Return(u32::MAX)
}

fn unsupported(
    env: &mut Env<'_>,
    errno: GuestPtr<i32>,
    name: &str,
    reason: &str,
    value: i32,
    dos: u32,
) -> Outcome {
    adv_abi::warn_unavailable(env, &api(env, name), reason);
    fail(env, errno, value, dos)
}

fn invalid(env: &mut Env<'_>, name: &str) -> Outcome {
    // The returning native invalid handler observes errno22, without a DOS
    // errno write. Publish before invoking the existing callback continuation.
    set_last_error(env, 0);
    invalid_parameter_result(env, name, 22, u32::MAX)
}

fn unix_seconds(time: Option<SystemTime>) -> Result<i64, ()> {
    let Some(time) = time else { return Ok(-1) };
    match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => i64::try_from(duration.as_secs()).map_err(|_| ()),
        Err(error) => {
            let duration = error.duration();
            let seconds = i64::try_from(duration.as_secs()).map_err(|_| ())?;
            seconds
                .checked_neg()
                .and_then(|v| v.checked_sub(i64::from(duration.subsec_nanos() != 0)))
                .ok_or(())
        }
    }
}

struct Packet {
    attrib: [u8; 4],
    body: Vec<u8>,
}

fn packet(row: &DirEntry) -> Result<Packet, (&'static str, i32, u32)> {
    let name: Vec<_> = row.name.encode_utf16().collect();
    if name.len() >= 260 {
        return Err((
            "search names exceeding 259 UTF-16 units are unavailable",
            38,
            206,
        ));
    }
    if !row.is_dir && row.size > u32::MAX as u64 {
        return Err((
            "time64/size32 searches cannot represent files larger than UINT32_MAX",
            40,
            50,
        ));
    }
    let mut body = Vec::with_capacity(28 + (name.len() + 1) * 2);
    for time in [row.created, row.accessed, row.modified] {
        let seconds = unix_seconds(time).map_err(|_| {
            (
                "filesystem timestamps exceed the signed time64 profile",
                40,
                50,
            )
        })?;
        body.extend_from_slice(&seconds.to_le_bytes());
    }
    body.extend_from_slice(&(if row.is_dir { 0 } else { row.size as u32 }).to_le_bytes());
    for unit in name.into_iter().chain([0]) {
        body.extend_from_slice(&unit.to_le_bytes());
    }
    Ok(Packet {
        // CRT _A_NORMAL is zero; other supported bits use the shared virtual
        // READONLY/HIDDEN/SYSTEM/SUBDIR/ARCHIVE attribute view, not host flags.
        attrib: (row.attributes & 0x37).to_le_bytes(),
        body,
    })
}

fn publish(env: &mut Env<'_>, data: u32, packet: &Packet) -> Result<(), MemFault> {
    // Preflight the complete declared object, then write only defined fields.
    // Never read padding or require READ access on a write-only output.
    check_range(env.cpu, data, DATA_BYTES, Prot::WRITE)?;
    env.cpu.write(data, &packet.attrib)?;
    env.cpu.write(data + 8, &packet.body)
}

#[win32(msvcrt, name = "_wfindfirst64i32", level = implement)]
fn first(env: &mut Env<'_>, pattern: WideStr, data: GuestPtr<u8>) -> Outcome {
    let errno = match error_cell(env) {
        Ok(errno) => errno,
        Err(outcome) => return outcome,
    };
    if pattern.0.is_null() || data.is_null() {
        return invalid(env, "_wfindfirst64i32");
    }
    let units = match pattern.read(env.cpu, 32768) {
        Ok(units) => units,
        Err(error) => return memory_failure(error),
    };
    let pattern = match String::from_utf16(&units) {
        Ok(pattern) => pattern,
        Err(_) => {
            return unsupported(
                env,
                errno,
                "_wfindfirst64i32",
                "unpaired UTF-16 search patterns are unavailable",
                42,
                1113,
            );
        }
    };
    if pattern.is_empty() {
        return fail(env, errno, 2, 3);
    }
    let w = env.ext.get::<Win32>();
    let full = w.vfs.normalize(&pattern);
    let (directory, mask) = full.rsplit_once('\\').unwrap_or(("", &full));
    let directory = if directory.len() == 2 {
        format!("{directory}\\")
    } else {
        directory.to_owned()
    };
    if !w.vfs.is_dir(&directory) {
        return fail(env, errno, 2, 3);
    }
    let Some(entries) = w.vfs.list(&directory) else {
        return fail(env, errno, 13, 5);
    };
    let entries: Vec<_> = entries
        .into_iter()
        .filter(|row| wildcard(mask, &row.name))
        .collect();
    if entries.len() > MAX_ROWS {
        return unsupported(
            env,
            errno,
            "_wfindfirst64i32",
            "directory snapshots are bounded to 32768 rows",
            12,
            8,
        );
    }
    let Some(row) = entries.first() else {
        return fail(env, errno, 2, 2);
    };
    let packet = match packet(row) {
        Ok(packet) => packet,
        Err((reason, value, dos)) => {
            return unsupported(env, errno, "_wfindfirst64i32", reason, value, dos);
        }
    };
    if let Err(error) = check_range(env.cpu, data.addr(), DATA_BYTES, Prot::WRITE) {
        return memory_failure(error);
    }
    if entries
        .iter()
        .any(|row| row.created.is_none() || row.accessed.is_none() || row.modified.is_none())
    {
        adv_abi::warn_unavailable(
            env,
            &api(env, "_wfindfirst64i32"),
            "filesystem timestamps missing from the shared VFS snapshot are reported as -1",
        );
    }
    // The shared VFS currently supplies real directory children, without . and
    // .. synthetic entries. Keep this existing profile observable to callers.
    if wildcard(mask, ".") || wildcard(mask, "..") {
        adv_abi::warn_unavailable(
            env,
            &api(env, "_wfindfirst64i32"),
            "shared VFS enumeration omits dot directory entries",
        );
    }
    let Some(handle) = env
        .ext
        .get_or_default::<Searches>()
        .insert(&mut env.sys.objects, entries)
    else {
        return unsupported(
            env,
            errno,
            "_wfindfirst64i32",
            "search owner capacity is exhausted (256 searches/64MiB snapshots)",
            12,
            8,
        );
    };
    if let Err(error) = publish(env, data.addr(), &packet) {
        env.ext
            .get::<Searches>()
            .close(&mut env.sys.objects, handle);
        return memory_failure(error);
    }
    env.ext
        .get::<Searches>()
        .get_mut(&mut env.sys.objects, handle)
        .unwrap()
        .next = 1;
    set_last_error(env, 0);
    crate::native::diagnose_bypass(
        env,
        &api(env, "_wfindfirst64i32"),
        &["NtOpenFile", "NtCreateFile", "NtQueryDirectoryFile"],
    );
    Outcome::Return(handle)
}

#[win32(msvcrt, name = "_wfindnext64i32", level = implement)]
fn next(env: &mut Env<'_>, handle: i32, data: GuestPtr<u8>) -> Outcome {
    let errno = match error_cell(env) {
        Ok(errno) => errno,
        Err(outcome) => return outcome,
    };
    if handle == -1 || data.is_null() {
        return invalid(env, "_wfindnext64i32");
    }
    let Some(search) = env
        .ext
        .try_get::<Searches>()
        .and_then(|s| s.get(&env.sys.objects, handle as u32))
    else {
        return fail(env, errno, 22, 6);
    };
    let Some(row) = search.entries.get(search.next) else {
        return fail(env, errno, 2, 18);
    };
    let packet = match packet(row) {
        Ok(packet) => packet,
        Err((reason, value, dos)) => {
            return unsupported(env, errno, "_wfindnext64i32", reason, value, dos);
        }
    };
    if let Err(error) = publish(env, data.addr(), &packet) {
        return memory_failure(error);
    }
    env.ext
        .get::<Searches>()
        .get_mut(&mut env.sys.objects, handle as u32)
        .unwrap()
        .next += 1;
    crate::native::diagnose_bypass(env, &api(env, "_wfindnext64i32"), &["NtQueryDirectoryFile"]);
    Outcome::Return(0)
}

#[win32(msvcrt, name = "_findclose", level = state)]
fn close(env: &mut Env<'_>, handle: i32) -> Outcome {
    let errno = match error_cell(env) {
        Ok(errno) => errno,
        Err(outcome) => return outcome,
    };
    let closed = env
        .ext
        .try_get::<Searches>()
        .is_some_and(|s| s.close(&mut env.sys.objects, handle as u32));
    if closed {
        crate::native::diagnose_bypass(env, &api(env, "_findclose"), &["NtClose"]);
        Outcome::Return(0)
    } else {
        fail(env, errno, 22, if handle == -1 { 6 } else { 998 })
    }
}

pub(super) fn register(registry: &mut Registry) {
    for module in MODULES {
        for api in [first_ABI, next_ABI, close_ABI] {
            registry.register(adv_abi::Api { module, ..api });
        }
    }
}

#[cfg(test)]
mod tests;

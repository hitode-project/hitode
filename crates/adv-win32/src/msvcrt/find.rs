//! Legacy time64/size32 searches consume the shared native directory cursor.
//! A typed CRT alias owns its original native handle, without storing another row.
use adv_abi::{
    GuestPtr, Registry, WideStr,
    guest::{check_range, memory_failure},
    win32,
};
use adv_cpu::backend::{MemFault, Prot};
use adv_machine::{Env, Namespace, Objects, Outcome};
use adv_os::vfs::wildcard;
#[cfg(test)]
use std::time::{SystemTime, UNIX_EPOCH};

use super::{floating, invalid_parameter_result, shared_file};
use crate::{
    kernel32::file::native::{find, operations, win32_error},
    util::set_last_error,
};

const MODULES: [&str; 3] = ["msvcrt.dll", "msvcr80.dll", "msvcr90.dll"];
const DATA_BYTES: u32 = 560;
const MAX_SEARCHES: usize = 256;

#[derive(Clone, Copy)]
struct Search {
    native: u32,
    owner: Option<u64>,
}

#[derive(Default)]
struct Searches {
    namespace: Option<Namespace>,
}

impl Searches {
    fn raw(&self, objects: &Objects, handle: u32) -> Option<Search> {
        let id = objects.lookup_alias::<Search>(self.namespace?, handle)?;
        objects.owned(id).copied()
    }

    fn get(&self, objects: &Objects, handle: u32) -> Option<Search> {
        let search = self.raw(objects, handle)?;
        (objects.object_id(search.native) == search.owner).then_some(search)
    }

    fn insert(&mut self, objects: &mut Objects, native: u32) -> Option<u32> {
        if self.namespace.is_none() {
            self.namespace = objects.new_namespace_in(0x0b00_0004, 0x0bff_fffc);
        }
        let namespace = self.namespace?;
        if objects.alias_count(namespace) >= MAX_SEARCHES {
            return None;
        }
        let owner = objects.object_id(native);
        objects.insert_host_in(namespace, Search { native, owner })
    }

    fn close(&self, objects: &mut Objects, handle: u32) -> bool {
        if self.raw(objects, handle).is_none() {
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

#[cfg(test)]
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

fn packet(bytes: &[u8]) -> Result<Packet, (&'static str, i32, u32)> {
    let invalid = ("native directory record is malformed", 22, 87);
    if bytes.len() < 94 {
        return Err(invalid);
    }
    let length = u32::from_le_bytes(bytes[60..64].try_into().unwrap()) as usize;
    if length & 1 != 0 || length > bytes.len() - 94 {
        return Err(invalid);
    }
    let name: Vec<_> = bytes[94..94 + length]
        .chunks_exact(2)
        .map(|unit| u16::from_le_bytes(unit.try_into().unwrap()))
        .collect();
    if String::from_utf16(&name).is_err() {
        return Err(("native directory name contains unpaired UTF-16", 42, 1113));
    }
    if name.len() >= 260 {
        return Err((
            "search names exceeding 259 UTF-16 units are unavailable",
            38,
            206,
        ));
    }
    let attributes = u32::from_le_bytes(bytes[56..60].try_into().unwrap());
    let size = u64::from_le_bytes(bytes[40..48].try_into().unwrap());
    let directory = attributes & 0x10 != 0;
    if !directory && size > u32::MAX as u64 {
        return Err((
            "time64/size32 searches cannot represent files larger than UINT32_MAX",
            40,
            50,
        ));
    }
    let mut body = Vec::with_capacity(28 + (name.len() + 1) * 2);
    for offset in [8, 16, 24] {
        let ticks = u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
        let seconds = if ticks == 0 {
            -1
        } else {
            ((i128::from(ticks) - 116_444_736_000_000_000).div_euclid(10_000_000)) as i64
        };
        body.extend_from_slice(&seconds.to_le_bytes());
    }
    body.extend_from_slice(&(if directory { 0 } else { size as u32 }).to_le_bytes());
    for unit in name.into_iter().chain([0]) {
        body.extend_from_slice(&unit.to_le_bytes());
    }
    Ok(Packet {
        // CRT _A_NORMAL is zero; other supported bits use the shared virtual
        // READONLY/HIDDEN/SYSTEM/SUBDIR/ARCHIVE attribute view, not host flags.
        attrib: (attributes & 0x37).to_le_bytes(),
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

fn native_failure(env: &mut Env<'_>, errno: GuestPtr<i32>, status: u32, first: bool) -> Outcome {
    let dos = match status {
        0x8000_0006 => {
            if first {
                2
            } else {
                18
            }
        }
        _ => win32_error(status),
    };
    let value = if dos == 18 {
        2
    } else {
        shared_file::dos_errno(dos)
    };
    fail(env, errno, value, dos)
}

fn discard(env: &mut Env<'_>, handle: u32, outcome: Outcome) -> Outcome {
    operations::close(env, handle, Box::new(move |_, _| outcome))
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
    if let Err(error) = check_range(env.cpu, data.addr(), DATA_BYTES, Prot::WRITE) {
        return memory_failure(error);
    }
    // The pristine VFS directory profile omits synthetic dot entries. Providers
    // remain authoritative and may return those entries themselves.
    if matches!(
        crate::native::overridden(env, "NtQueryDirectoryFile"),
        Ok(None)
    ) && pattern
        .rsplit('\\')
        .next()
        .is_some_and(|mask| wildcard(mask, ".") || wildcard(mask, ".."))
    {
        adv_abi::warn_unavailable(
            env,
            &api(env, "_wfindfirst64i32"),
            "shared VFS enumeration omits dot directory entries",
        );
    }
    find::open_then(
        env,
        pattern,
        Box::new(move |env, result| {
            let (native, bytes) = match result {
                Ok(result) => result,
                Err(status) => return native_failure(env, errno, status, true),
            };
            let packet = match packet(&bytes) {
                Ok(packet) => packet,
                Err((reason, value, dos)) => {
                    let outcome = unsupported(env, errno, "_wfindfirst64i32", reason, value, dos);
                    return discard(env, native, outcome);
                }
            };
            if [8, 16, 24]
                .into_iter()
                .any(|offset| bytes[offset..offset + 8] == [0; 8])
            {
                adv_abi::warn_unavailable(
                    env,
                    &api(env, "_wfindfirst64i32"),
                    "missing native directory timestamps are reported as -1",
                );
            }
            let Some(handle) = env
                .ext
                .get_or_default::<Searches>()
                .insert(&mut env.sys.objects, native)
            else {
                let outcome = unsupported(
                    env,
                    errno,
                    "_wfindfirst64i32",
                    "search owner capacity is exhausted (256 searches)",
                    12,
                    8,
                );
                return discard(env, native, outcome);
            };
            if let Err(error) = publish(env, data.addr(), &packet) {
                env.ext
                    .get::<Searches>()
                    .close(&mut env.sys.objects, handle);
                return discard(env, native, memory_failure(error));
            }
            set_last_error(env, 0);
            Outcome::Return(handle)
        }),
    )
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
    if let Err(error) = check_range(env.cpu, data.addr(), DATA_BYTES, Prot::WRITE) {
        return memory_failure(error);
    }
    find::query_then(
        env,
        search.native,
        None,
        Box::new(move |env, result| {
            let bytes = match result {
                Ok(bytes) => bytes,
                Err(status) => return native_failure(env, errno, status, false),
            };
            let packet = match packet(&bytes) {
                Ok(packet) => packet,
                Err((reason, value, dos)) => {
                    return unsupported(env, errno, "_wfindnext64i32", reason, value, dos);
                }
            };
            match publish(env, data.addr(), &packet) {
                Ok(()) => Outcome::Return(0),
                Err(error) => memory_failure(error),
            }
        }),
    )
}

#[win32(msvcrt, name = "_findclose", level = state)]
fn close(env: &mut Env<'_>, handle: i32) -> Outcome {
    let errno = match error_cell(env) {
        Ok(errno) => errno,
        Err(outcome) => return outcome,
    };
    let search = env
        .ext
        .try_get::<Searches>()
        .and_then(|s| s.get(&env.sys.objects, handle as u32));
    let Some(search) = search else {
        if let Some(searches) = env.ext.try_get::<Searches>() {
            searches.close(&mut env.sys.objects, handle as u32);
        }
        return fail(env, errno, 22, if handle == -1 { 6 } else { 998 });
    };
    operations::close(
        env,
        search.native,
        Box::new(move |env, result| match result {
            Ok(()) => {
                env.ext
                    .get::<Searches>()
                    .close(&mut env.sys.objects, handle as u32);
                Outcome::Return(0)
            }
            Err(status) => native_failure(env, errno, status, false),
        }),
    )
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

#[cfg(test)]
mod native_tests;

//! Native thread opens share the Win32 thread identity and handle owner.
use adv_abi::{GuestPtr, Registry, guest::check_range, warn_unavailable, win32};
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::{Env, Outcome};

const ACCESS_VIOLATION: u32 = 0xc000_0005;
const INVALID_PARAMETER: u32 = 0xc000_000d;
const PARAMETER_MIX: u32 = 0xc000_0030;

fn failure(env: &mut Env<'_>, output: u32, status: u32) -> Outcome {
    if check_range(env.cpu, output, 4, Prot::WRITE).is_err()
        || env.cpu.write_u32(output, 0).is_err()
    {
        return Outcome::Return(ACCESS_VIOLATION);
    }
    Outcome::Return(status)
}

#[win32(ntdll, name = "NtOpenThread", level = implement)]
fn open(
    env: &mut Env<'_>,
    thread_handle: GuestPtr<u32>,
    desired_access: u32,
    object_attributes: GuestPtr<()>,
    client_id: GuestPtr<()>,
) -> Outcome {
    let output = thread_handle.addr();
    if check_range(env.cpu, output, 4, Prot::WRITE).is_err() {
        return Outcome::Return(ACCESS_VIOLATION);
    }
    let attrs = object_attributes.addr();
    if check_range(env.cpu, attrs, 24, Prot::READ).is_err() {
        return failure(env, output, ACCESS_VIOLATION);
    }
    let mut bytes = [0; 24];
    if env.cpu.read(attrs, &mut bytes).is_err() {
        return failure(env, output, ACCESS_VIOLATION);
    }
    let fields: Vec<_> = bytes
        .chunks_exact(4)
        .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
        .collect();
    // Capture failures leave the PHANDLE untouched, matching native ordering.
    if fields[0] != 24 {
        return Outcome::Return(INVALID_PARAMETER);
    }
    if fields[4] != 0 || fields[5] != 0 {
        for pointer in [fields[4], fields[5]].into_iter().filter(|p| *p != 0) {
            if check_range(env.cpu, pointer, 12, Prot::READ).is_err() {
                return Outcome::Return(ACCESS_VIOLATION);
            }
        }
        warn_unavailable(
            env,
            "ntdll.dll!NtOpenThread",
            "explicit thread-open security descriptors or impersonation quality are unavailable",
        );
        return Outcome::Return(0xc000_00bb); // STATUS_NOT_SUPPORTED
    }
    if fields[3] & !0x1ff2 != 0 {
        return failure(env, output, INVALID_PARAMETER);
    }
    if fields[2] != 0 || client_id.addr() == 0 {
        return failure(env, output, PARAMETER_MIX);
    }
    if check_range(env.cpu, client_id.addr(), 8, Prot::READ).is_err() {
        return failure(env, output, ACCESS_VIOLATION);
    }
    let mut client = [0; 8];
    if env.cpu.read(client_id.addr(), &mut client).is_err() {
        return failure(env, output, ACCESS_VIOLATION);
    }
    let pid = u32::from_le_bytes(client[..4].try_into().unwrap());
    let tid = u32::from_le_bytes(client[4..].try_into().unwrap());
    crate::kernel32::find_thread(env, pid, tid, move |env, result| {
        let identity = match result {
            Ok(identity) => identity,
            Err(error) => {
                let status = match error.code {
                    6 | 87 => 0xc000_000b, // STATUS_INVALID_CID
                    8 => 0xc000_0017,
                    50 => {
                        warn_unavailable(env, "ntdll.dll!NtOpenThread", &error.message);
                        0xc000_00bb
                    }
                    _ => 0xc000_0001,
                };
                return failure(env, output, status);
            }
        };
        let access = match crate::kernel32::thread_access(desired_access) {
            Ok(access) => access,
            Err(status) => return failure(env, output, status),
        };
        // A remote visit may have yielded. Revalidate publication before insert.
        if check_range(env.cpu, output, 4, Prot::WRITE).is_err() {
            return Outcome::Return(ACCESS_VIOLATION);
        }
        let handle =
            match crate::kernel32::publish_thread(env, identity, access, fields[3] & 2 != 0) {
                Ok(handle) => handle,
                Err(_) => return failure(env, output, 0xc000_0017),
            };
        if env.cpu.write_u32(output, handle).is_err() {
            env.sys.objects.close(handle);
            return Outcome::Return(ACCESS_VIOLATION);
        }
        Outcome::Return(0)
    })
}

pub(super) fn register(registry: &mut Registry) {
    registry.register(open_ABI);
    registry.register(adv_abi::Api {
        name: "ZwOpenThread",
        ..open_ABI
    });
}

#[cfg(test)]
mod tests;

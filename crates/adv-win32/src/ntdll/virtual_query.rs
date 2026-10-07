//! Native x86 basic VM queries share VirtualQuery's local/remote region owner.
use adv_abi::{GuestPtr, Handle, Registry, guest::check_range, warn_unavailable, win32};
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::{Env, Object, Outcome};
use adv_os::processes;

const BASIC_SIZE: u32 = 28;

fn output_range(env: &Env<'_>, output: u32, length: u32) -> Result<(), u32> {
    if length == 0 {
        return Ok(());
    }
    if output & 3 != 0 {
        return Err(0xc0000080); // STATUS_DATATYPE_MISALIGNMENT
    }
    check_range(env.cpu, output, length, Prot::WRITE).map_err(|_| 0xc0000005u32)
}
fn outputs(env: &Env<'_>, output: u32, length: u32, returned: u32) -> Result<(), u32> {
    // The native x86 user-mode contract probes caller buffers, including their
    // alignment, before referencing a target. WOW64's output translation has
    // different NULL/alignment/error publication behavior and is not this ABI.
    output_range(env, output, length)?;
    if returned != 0 {
        output_range(env, returned, 4)?;
    }
    Ok(())
}
fn status(error: processes::Error) -> u32 {
    match error.code {
        5 => 0xc0000022,
        6 => 0xc0000008,
        8 => 0xc0000017,
        87 => 0xc000000d,
        998 => 0xc0000005,
        _ => 0xc0000002,
    }
}

#[win32(ntdll, name = "NtQueryVirtualMemory", level = implement)]
fn query(
    env: &mut Env<'_>,
    process: Handle,
    address: GuestPtr<()>,
    information_class: i32,
    information: GuestPtr<()>,
    information_length: u32,
    return_length: GuestPtr<u32>,
) -> Outcome {
    let (address, output, returned) = (address.addr(), information.addr(), return_length.addr());
    if address > crate::kernel32::MAX_APPLICATION_ADDRESS {
        return Outcome::Return(0xc000000d);
    }
    if let Err(error) = outputs(env, output, information_length, returned) {
        return Outcome::Return(error);
    }
    if information_class != 0 {
        return Outcome::Return(0xc0000003); // STATUS_INVALID_INFO_CLASS
    }
    if information_length < BASIC_SIZE {
        return Outcome::Return(0xc0000004); // STATUS_INFO_LENGTH_MISMATCH
    }
    if process.0 != u32::MAX
        && !matches!(env.sys.objects.get(process.0), Some(Object::Process { .. }))
    {
        return Outcome::Return(
            if process.0 == u32::MAX - 1 || env.sys.objects.get(process.0).is_some() {
                0xc0000024
            } else {
                0xc0000008
            },
        );
    }
    let target = match crate::kernel32::child_process::process_target(env, process.0, 0x400) {
        Ok(target) => target,
        Err(error) => return Outcome::Return(status(error)),
    };
    if target.completion.exit_code().is_some() {
        return Outcome::Return(0xc000010a);
    }
    processes::visit(
        env,
        target.pid,
        None,
        move |owner| {
            if owner.sys.process_identity.completion.exit_code().is_some() {
                return Err(processes::Error::new(87, "process has exited"));
            }
            crate::kernel32::query_memory(owner, address)
        },
        move |caller, result| {
            let information = match result {
                Ok(information) => information,
                Err(error) if error.code == 87 => return Outcome::Return(0xc000010a),
                Err(error) if error.code == 50 => {
                    warn_unavailable(caller, "ntdll.dll!NtQueryVirtualMemory", &error.message);
                    return Outcome::Return(0xc0000002);
                }
                Err(error) => return Outcome::Return(status(error)),
            };
            // The caller can change its output mapping while a remote owner is
            // serviced. Querying does not mutate that owner's VM or require rollback.
            if let Err(error) = outputs(caller, output, information_length, returned) {
                return Outcome::Return(error);
            }
            let bytes: Vec<_> = information.into_iter().flat_map(u32::to_le_bytes).collect();
            if caller.cpu.write(output, &bytes).is_err()
                || (returned != 0 && caller.cpu.write_u32(returned, BASIC_SIZE).is_err())
            {
                return Outcome::Return(0xc0000005);
            }
            Outcome::Return(0)
        },
    )
}

pub(super) fn register(registry: &mut Registry) {
    registry.register(query_ABI);
    registry.register(adv_abi::Api {
        name: "ZwQueryVirtualMemory",
        ..query_ABI
    });
}

#[cfg(test)]
mod tests;

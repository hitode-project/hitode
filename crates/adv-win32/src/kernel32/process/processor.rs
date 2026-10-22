//! Queries and validated affinity over the one OS-owned virtual CPU profile.
use crate::util::{err, set_last_error};
use adv_abi::{GuestPtr, Handle, Registry, win32};
use adv_cpu::backend::MemFault;
use adv_machine::{Env, Object};
use adv_os::processor::Processors;

mod ideal;

fn process(env: &Env<'_>, handle: Handle) -> bool {
    handle.0 == u32::MAX
        || matches!(env.sys.objects.get(handle.0), Some(Object::Process { identity, .. }) if identity.pid == env.sys.pid)
        || env
            .sys
            .objects
            .host::<super::super::handle::ProcessHandle>(handle.0)
            .is_some()
}
fn thread(env: &Env<'_>, handle: Handle) -> bool {
    handle.0 == u32::MAX - 1
        || matches!(env.sys.objects.get(handle.0), Some(Object::Thread { .. }))
        || matches!(env.sys.objects.get(handle.0), Some(Object::RemoteThread { identity, .. }) if identity.process.pid == env.sys.pid)
}
#[win32(kernel32, name = "GetProcessAffinityMask", level = implement)]
fn get_affinity(
    env: &mut Env<'_>,
    handle: Handle,
    process_out: GuestPtr<u32>,
    system_out: GuestPtr<u32>,
) -> Result<i32, MemFault> {
    if !process(env, handle) {
        set_last_error(env, err::INVALID_HANDLE);
        return Ok(0);
    }
    let p = *env.ext.get_or_default::<Processors>();
    adv_abi::guest::check_range(
        env.cpu,
        process_out.addr(),
        4,
        adv_cpu::backend::Prot::WRITE,
    )?;
    adv_abi::guest::check_range(env.cpu, system_out.addr(), 4, adv_cpu::backend::Prot::WRITE)?;
    process_out.write(env.cpu, p.process_mask())?;
    system_out.write(env.cpu, p.active_mask())?;
    Ok(1)
}
#[win32(kernel32, name = "SetProcessAffinityMask", level = implement)]
fn set_process(env: &mut Env<'_>, handle: Handle, mask: u32) -> i32 {
    if !process(env, handle) {
        set_last_error(env, err::INVALID_HANDLE);
        return 0;
    }
    if !env.ext.get_or_default::<Processors>().accepts_mask(mask) {
        set_last_error(env, err::INVALID_PARAMETER);
        return 0;
    }
    1 // The validated one-CPU mask equals the existing process affinity.
}
#[win32(kernel32, name = "SetThreadAffinityMask", level = implement)]
fn set_thread(env: &mut Env<'_>, handle: Handle, mask: u32) -> u32 {
    if !thread(env, handle) {
        set_last_error(env, err::INVALID_HANDLE);
        return 0;
    }
    let p = env.ext.get_or_default::<Processors>();
    if !p.accepts_mask(mask) {
        set_last_error(env, err::INVALID_PARAMETER);
        return 0;
    }
    p.process_mask() // The only valid requested mask and the previous mask are 1.
}
fn info(env: &mut Env<'_>, output: GuestPtr<[u32; 9]>) -> Result<(), MemFault> {
    let p = *env.ext.get_or_default::<Processors>();
    let identity = adv_cpu::PROCESSOR_IDENTITY;
    // Family/model/revision follow the shared guest CPUID identity. The
    // profile is x86 itself; there is no host WOW64/native-x64 exposure.
    output.write(
        env.cpu,
        [
            0,
            4096,
            0x10000,
            super::super::memory::MAX_APPLICATION_ADDRESS,
            p.active_mask(),
            p.count(),
            586,
            65536,
            identity.family() | ((identity.model() << 8 | identity.stepping()) << 16),
        ],
    )
}
#[win32(kernel32, name = "GetSystemInfo", level = implement)]
fn system_info(env: &mut Env<'_>, output: GuestPtr<[u32; 9]>) -> Result<(), MemFault> {
    info(env, output)
}
#[win32(kernel32, name = "GetNativeSystemInfo", level = implement)]
fn native_info(env: &mut Env<'_>, output: GuestPtr<[u32; 9]>) -> Result<(), MemFault> {
    info(env, output)
}
#[win32(kernel32, name = "GetCurrentProcessorNumber", level = implement)]
fn current_processor(env: &mut Env<'_>) -> u32 {
    env.ext.get_or_default::<Processors>().current()
}

#[win32(kernel32, name = "GetCurrentProcessorNumberEx", level = implement)]
fn current_processor_ex(env: &mut Env<'_>, output: GuestPtr<u32>) -> Result<(), MemFault> {
    // PROCESSOR_NUMBER: WORD Group, BYTE Number, BYTE Reserved.
    let number = env.ext.get_or_default::<Processors>().current();
    output.write(env.cpu, number << 16)
}

#[win32(kernel32, name = "GetNumaHighestNodeNumber", level = implement)]
fn highest_numa_node(env: &mut Env<'_>, output: GuestPtr<u32>) -> Result<i32, MemFault> {
    output.write(env.cpu, 0)?;
    Ok(1)
}

fn topology(profile: Processors, relation: u32) -> Option<Vec<u8>> {
    let relations: &[u32] = match relation {
        0 => &[0],
        1 => &[1],
        2 => &[], // The virtual processor profile exposes no cache hierarchy.
        3 => &[3],
        4 => &[4],
        0xffff => &[0, 1, 3, 4],
        _ => return None,
    };
    let mut output = Vec::new();
    for &kind in relations {
        let size = if kind == 4 { 76 } else { 44 };
        let start = output.len();
        output.resize(start + size, 0);
        let record = &mut output[start..];
        record[..4].copy_from_slice(&kind.to_le_bytes());
        record[4..8].copy_from_slice(&(size as u32).to_le_bytes());
        if kind == 4 {
            record[8..10].copy_from_slice(&1u16.to_le_bytes());
            record[10..12].copy_from_slice(&1u16.to_le_bytes());
            record[32] = profile.count() as u8;
            record[33] = profile.count() as u8;
            record[72..76].copy_from_slice(&profile.active_mask().to_le_bytes());
        } else {
            record[30..32].copy_from_slice(&1u16.to_le_bytes());
            record[32..36].copy_from_slice(&profile.active_mask().to_le_bytes());
        }
    }
    Some(output)
}

#[win32(kernel32, name = "GetLogicalProcessorInformationEx", level = implement)]
fn logical_information_ex(
    env: &mut Env<'_>,
    relation: u32,
    output: GuestPtr<u8>,
    length: GuestPtr<u32>,
) -> Result<i32, MemFault> {
    let profile = *env.ext.get_or_default::<Processors>();
    let Some(bytes) = topology(profile, relation) else {
        set_last_error(env, err::INVALID_PARAMETER);
        return Ok(0);
    };
    let capacity = length.read(env.cpu)?;
    length.write(env.cpu, bytes.len() as u32)?;
    if capacity < bytes.len() as u32 || (output.addr() == 0 && !bytes.is_empty()) {
        set_last_error(env, 122); // ERROR_INSUFFICIENT_BUFFER
        return Ok(0);
    }
    if !bytes.is_empty() {
        adv_abi::guest::check_range(
            env.cpu,
            output.addr(),
            bytes.len() as u32,
            adv_cpu::backend::Prot::WRITE,
        )?;
        env.cpu.write(output.addr(), &bytes)?;
    }
    Ok(1)
}

#[win32(kernel32, name = "GetLogicalProcessorInformation", level = implement)]
fn logical_information(
    env: &mut Env<'_>,
    output: GuestPtr<u8>,
    length: GuestPtr<u32>,
) -> Result<i32, MemFault> {
    let profile = *env.ext.get_or_default::<Processors>();
    // The same single-core, single-node, single-package profile as the Ex API;
    // it exposes no cache hierarchy. Each x86 legacy record occupies 24 bytes.
    let mut bytes = [0u8; 72];
    for (index, relation) in [0u32, 1, 3].into_iter().enumerate() {
        let record = &mut bytes[index * 24..][..24];
        record[..4].copy_from_slice(&profile.active_mask().to_le_bytes());
        record[4..8].copy_from_slice(&relation.to_le_bytes());
    }
    let capacity = length.read(env.cpu)?;
    length.write(env.cpu, bytes.len() as u32)?;
    if output.is_null() || capacity < bytes.len() as u32 {
        set_last_error(env, 122);
        return Ok(0);
    }
    adv_abi::guest::check_range(
        env.cpu,
        output.addr(),
        bytes.len() as u32,
        adv_cpu::backend::Prot::WRITE,
    )?;
    env.cpu.write(output.addr(), &bytes)?;
    Ok(1)
}

#[win32(kernel32, name = "GetThreadGroupAffinity", level = implement)]
fn get_group_affinity(
    env: &mut Env<'_>,
    handle: Handle,
    output: GuestPtr<[u32; 3]>,
) -> Result<i32, MemFault> {
    if let Err(error) = super::thread_target(env, handle.0, 0x40) {
        set_last_error(env, error.code);
        return Ok(0);
    }
    let mask = env.ext.get_or_default::<Processors>().process_mask();
    output.write(env.cpu, [mask, 0, 0])?;
    Ok(1)
}

#[win32(kernel32, name = "SetThreadGroupAffinity", level = implement)]
fn set_group_affinity(
    env: &mut Env<'_>,
    handle: Handle,
    input: GuestPtr<[u32; 3]>,
    previous: GuestPtr<[u32; 3]>,
) -> Result<i32, MemFault> {
    if let Err(error) = super::thread_target(env, handle.0, 0x20) {
        set_last_error(env, error.code);
        return Ok(0);
    }
    // x86 GROUP_AFFINITY is a DWORD mask, a WORD group and three reserved WORDs.
    let [mask, group_reserved, reserved] = input.read(env.cpu)?;
    let profile = *env.ext.get_or_default::<Processors>();
    // Native accepts zero to restore the group's default affinity.
    if (mask != 0 && !profile.accepts_mask(mask)) || group_reserved != 0 || reserved != 0 {
        set_last_error(env, err::INVALID_PARAMETER);
        return Ok(0);
    }
    if previous.addr() != 0 {
        previous.write(env.cpu, [profile.process_mask(), 0, 0])?;
    }
    // Only group zero and its single processor exist in the shared profile.
    Ok(1)
}
pub(super) fn register(reg: &mut Registry) {
    ideal::register(reg);
    for api in [
        get_affinity_ABI,
        set_process_ABI,
        set_thread_ABI,
        system_info_ABI,
        native_info_ABI,
        current_processor_ABI,
        current_processor_ex_ABI,
        highest_numa_node_ABI,
        logical_information_ex_ABI,
        logical_information_ABI,
        get_group_affinity_ABI,
        set_group_affinity_ABI,
    ] {
        reg.register(api);
    }
}

//! System information, after Wine's `unix/system.c`.

use adv_cpu::PROCESSOR_IDENTITY;
use adv_machine::layout::{GRANULE, PAGE};
use adv_machine::{Env, Outcome};

use crate::memory::USER_END;
use crate::object::{write_bytes, write_u32};
use crate::process::PROCESSORS;
use crate::status::{self, Error};
use crate::timezone;

const SYSTEM_BASIC_INFORMATION: u32 = 0;
const SYSTEM_CPU_INFORMATION: u32 = 1;
const SYSTEM_TIME_OF_DAY_INFORMATION: u32 = 3;
const SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION: u32 = 8;
const SYSTEM_CURRENT_TIME_ZONE_INFORMATION: u32 = 44;
const SYSTEM_DYNAMIC_TIME_ZONE_INFORMATION: u32 = 102;
const SYSTEM_INTERRUPT_INFORMATION: u32 = 23;
/// The size of SYSTEM_INTERRUPT_INFORMATION.
const INTERRUPT_SIZE: usize = 24;
const SYSTEM_WINE_VERSION_INFORMATION: u32 = 1000;
/// The Wine release whose PE modules the platform serves.
const WINE_VERSION: &str = "11.0";
const WINE_BUILD: &str = "wine-11.0";

/// NtQuerySystemInformation(class, *info, size, *result_size)
pub fn query(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[class, info, size, result_size] = args else {
        unreachable!()
    };
    status::finish((|| {
        // The information, and whether the buffer must be its exact size.
        let (bytes, exact) = match class {
            SYSTEM_BASIC_INFORMATION | 62 => (basic_information().to_vec(), true),
            2 => (performance_information(), false),
            SYSTEM_CPU_INFORMATION | 63 => (processor_information(env).to_vec(), false),
            SYSTEM_TIME_OF_DAY_INFORMATION => {
                let mut bytes = [0u8; 48];
                let system = u64::from(crate::object::read_u32(env, crate::kuser::ADDRESS + 0x14)?)
                    | (u64::from(crate::object::read_u32(env, crate::kuser::ADDRESS + 0x18)?)
                        << 32);
                let interrupt = u64::from(crate::object::read_u32(env, crate::kuser::ADDRESS + 8)?)
                    | (u64::from(crate::object::read_u32(env, crate::kuser::ADDRESS + 12)?) << 32);
                let bias = u64::from(crate::object::read_u32(env, crate::kuser::ADDRESS + 0x20)?)
                    | (u64::from(crate::object::read_u32(env, crate::kuser::ADDRESS + 0x24)?)
                        << 32);
                let boot = match env.ext.try_get::<BootTime>() {
                    Some(boot) => boot.0,
                    None => {
                        let boot = system - interrupt;
                        env.ext.insert(BootTime(boot));
                        boot
                    }
                };
                bytes[..8].copy_from_slice(&boot.to_le_bytes());
                bytes[8..16].copy_from_slice(&system.to_le_bytes());
                bytes[16..24].copy_from_slice(&bias.to_le_bytes());
                if result_size != 0 {
                    write_u32(env, result_size, if size <= 48 { size } else { 0 })?;
                }
                if size > 48 {
                    return Err(status::INFO_LENGTH_MISMATCH.into());
                }
                if info == 0 {
                    return Err(status::ACCESS_VIOLATION.into());
                }
                write_bytes(env, info, &bytes[..size as usize])?;
                return Ok(());
            }
            SYSTEM_PROCESSOR_PERFORMANCE_INFORMATION => {
                let count = (size / 48).min(PROCESSORS);
                if count == 0 {
                    if result_size != 0 {
                        write_u32(env, result_size, 0)?;
                    }
                    return Err(status::INFO_LENGTH_MISMATCH.into());
                }
                let bytes = processor_performance(count);
                (bytes, false)
            }
            88 => return process_image_name(env, info, size, result_size),
            73 => (logical_processors(false, u32::MAX), false),
            175 => return Err(status::INVALID_PARAMETER.into()),
            103 => {
                if size < 8 {
                    if result_size != 0 {
                        write_u32(env, result_size, 8)?;
                    }
                    return Err(status::INFO_LENGTH_MISMATCH.into());
                }
                write_u32(env, info + 4, 1)?;
                if result_size != 0 {
                    write_u32(env, result_size, 8)?;
                }
                return Ok(());
            }
            16 | 64 => return handle_information(env, class, info, size, result_size),
            21 => (vec![0; 36], false),
            28 => {
                let mut bytes = vec![0; 12];
                bytes[..4].copy_from_slice(&156250u32.to_le_bytes());
                bytes[4..8].copy_from_slice(&156250u32.to_le_bytes());
                bytes[8] = 1;
                (bytes, true)
            }
            35 => (vec![0, 1], false),
            37 => (
                [0x2000000u32, 0x200000, 0x200000]
                    .into_iter()
                    .flat_map(u32::to_le_bytes)
                    .collect(),
                false,
            ),
            58 => (64u32.to_le_bytes().to_vec(), false),
            105 => {
                if info & 3 != 0 {
                    return Err(0x8000_0002.into());
                }
                let mut bytes = vec![0; 49];
                bytes[..PROCESSOR_IDENTITY.name.len()]
                    .copy_from_slice(PROCESSOR_IDENTITY.name.as_bytes());
                (bytes, false)
            }
            149 => (vec![0; 3], false),
            154 => {
                let mut bytes = vec![0; 32];
                bytes[..4].copy_from_slice(&processor_information(env)[8..12]);
                (bytes, false)
            }
            206 => (vec![1, 0, 0, 0, 0, 0, 0, 0], false),
            250 => (vec![0; 16], true),
            11 | 77 => {
                let names = [
                    r"\SystemRoot\system32\ntoskrnl.exe",
                    r"\SystemRoot\system32\hal.dll",
                    r"\SystemRoot\system32\drivers\mountmgr.sys",
                ];
                let mut bytes = vec![
                    0;
                    if class == 11 {
                        4 + names.len() * 284
                    } else {
                        names.len() * 300 + 2
                    }
                ];
                if class == 11 {
                    bytes[..4].copy_from_slice(&(names.len() as u32).to_le_bytes());
                }
                for (index, name) in names.iter().enumerate() {
                    let start = if class == 11 {
                        4 + index * 284
                    } else {
                        bytes[index * 300..index * 300 + 2].copy_from_slice(&300u16.to_le_bytes());
                        index * 300 + 4
                    };
                    bytes[start + 8..start + 12]
                        .copy_from_slice(&(0x10000000u32 + 0x200000 * index as u32).to_le_bytes());
                    bytes[start + 12..start + 16].copy_from_slice(&0x200000u32.to_le_bytes());
                    bytes[start + 20..start + 22].copy_from_slice(&(index as u16).to_le_bytes());
                    bytes[start + 24..start + 26].copy_from_slice(&1u16.to_le_bytes());
                    bytes[start + 26..start + 28]
                        .copy_from_slice(&((name.rfind('\\').unwrap() + 1) as u16).to_le_bytes());
                    bytes[start + 28..start + 28 + name.len()].copy_from_slice(name.as_bytes());
                }
                (bytes, false)
            }
            5 | 57 => return process_information(env, class, info, size, result_size),
            76 => return firmware_table(env, info, size, result_size),
            83 => (vec![0; PROCESSORS as usize * 8], false),
            114 => return Err(status::INVALID_INFO_CLASS.into()),
            SYSTEM_CURRENT_TIME_ZONE_INFORMATION => {
                let zone = timezone::current(env)?.encode();
                (zone[..timezone::STATIC_SIZE].to_vec(), false)
            }
            SYSTEM_DYNAMIC_TIME_ZONE_INFORMATION => {
                (timezone::current(env)?.encode().to_vec(), false)
            }
            // Wine fills it with random bytes, which RtlGenRandom uses.
            SYSTEM_INTERRUPT_INFORMATION => (random(PROCESSORS as usize * INTERRUPT_SIZE)?, false),
            SYSTEM_WINE_VERSION_INFORMATION => (wine_version(), false),
            class
                if !matches!(
                    class,
                    11 | 16
                        | 21
                        | 28
                        | 35
                        | 37
                        | 58
                        | 64
                        | 73
                        | 77
                        | 88
                        | 103
                        | 105
                        | 149
                        | 154
                        | 175
                        | 206
                        | 250
                ) =>
            {
                if result_size != 0 {
                    write_u32(env, result_size, 0)?;
                }
                return Err(status::INVALID_INFO_CLASS.into());
            }
            class => {
                return Err(Error::Unsupported(format!(
                    "system information class {class}"
                )));
            }
        };
        if result_size != 0 {
            write_u32(env, result_size, bytes.len() as u32)?;
        }
        let size = size as usize;
        if size < bytes.len() || (exact && size != bytes.len()) {
            return Err(status::INFO_LENGTH_MISMATCH.into());
        }
        write_bytes(env, info, &bytes)?;
        Ok(())
    })())
}

/// The boot instant stays fixed while the shared clocks advance.
struct BootTime(u64);

/// `length` bytes from the host's entropy source.
fn random(length: usize) -> Result<Vec<u8>, Error> {
    let mut bytes = vec![0u8; length];
    for chunk in bytes.chunks_mut(256) {
        // SAFETY: the chunk is writable and at most 256 bytes long.
        if unsafe { libc::getentropy(chunk.as_mut_ptr().cast(), chunk.len()) } != 0 {
            return Err(Error::Unsupported(format!(
                "host entropy: {}",
                std::io::Error::last_os_error()
            )));
        }
    }
    Ok(bytes)
}

/// SYSTEM_BASIC_INFORMATION, as Wine's `virtual_get_system_info`: the
/// host's physical memory in guest pages, and the guest's address space.
fn basic_information() -> [u8; 0x2c] {
    let lowest = 1;
    let highest = physical_pages() + lowest;
    let fields = [
        0,
        // KeMaximumIncrement, which Wine leaves unset.
        0,
        PAGE,
        highest - lowest,
        lowest,
        highest,
        GRANULE,
        0x1_0000,
        USER_END - 1,
        (1 << PROCESSORS) - 1,
        PROCESSORS,
    ];
    let mut bytes = [0u8; 0x2c];
    for (index, field) in fields.into_iter().enumerate() {
        bytes[4 * index..4 * index + 4].copy_from_slice(&field.to_le_bytes());
    }
    bytes
}

/// NumberOfPhysicalPages: the host's memory in guest pages, above the
/// first page, as Wine counts it.
pub fn physical_pages() -> u32 {
    // SAFETY: sysconf has no preconditions.
    let (pages, page_size) = unsafe {
        (
            libc::sysconf(libc::_SC_PHYS_PAGES),
            libc::sysconf(libc::_SC_PAGESIZE),
        )
    };
    let memory = u64::try_from(pages).unwrap_or(0) * u64::try_from(page_size).unwrap_or(0);
    u32::try_from(memory / u64::from(PAGE))
        .unwrap_or(u32::MAX)
        .max(2)
        - 1
}

/// The version, build and host system, each NUL-terminated, as Wine reports
/// them from `uname`.
fn wine_version() -> Vec<u8> {
    let (sysname, release) = host_system();
    let mut bytes = Vec::new();
    for part in [
        WINE_VERSION.as_bytes(),
        WINE_BUILD.as_bytes(),
        &sysname,
        &release,
    ] {
        bytes.extend_from_slice(part);
        bytes.push(0);
    }
    bytes
}

fn host_system() -> (Vec<u8>, Vec<u8>) {
    // SAFETY: uname fills the zeroed struct it is given.
    let mut name: libc::utsname = unsafe { std::mem::zeroed() };
    if unsafe { libc::uname(&mut name) } != 0 {
        return (Vec::new(), Vec::new());
    }
    let text = |field: &[libc::c_char]| {
        field
            .iter()
            .take_while(|&&c| c != 0)
            .map(|&c| c as u8)
            .collect()
    };
    (text(&name.sysname), text(&name.release))
}

/// NtQueryPerformanceCounter(*counter, *frequency): 100 ns ticks of the
/// monotonic clock, as the server's.
pub fn query_performance_counter(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    env.sys.poll_observation();
    let &[counter, frequency] = args else {
        unreachable!()
    };
    if counter & 3 != 0 {
        return Outcome::Return(0x8000_0002);
    }
    if !env
        .cpu
        .accessible(counter, 8, adv_cpu::backend::Prot::WRITE)
    {
        return Outcome::Return(status::ACCESS_VIOLATION);
    }
    if frequency & 3 != 0 {
        return Outcome::Return(0x8000_0002);
    }
    if frequency != 0
        && !env
            .cpu
            .accessible(frequency, 8, adv_cpu::backend::Prot::WRITE)
    {
        return Outcome::Return(status::ACCESS_VIOLATION);
    }
    let ticks = performance_counter(env);
    status::finish((|| {
        write_bytes(env, counter, &ticks.to_le_bytes())?;
        if frequency != 0 {
            write_bytes(env, frequency, &TICKS_PER_SECOND.to_le_bytes())?;
        }
        Ok::<_, Error>(())
    })())
}

pub(crate) const TICKS_PER_SECOND: u64 = 10_000_000;

pub(crate) fn performance_counter(env: &Env<'_>) -> u64 {
    env.sys.clock.now_us() * 10
}

/// NtQuerySystemTime(*time)
pub fn query_time(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    env.sys.poll_observation();
    let time = env.sys.clock.system_time();
    status::finish(write_bytes(env, args[0], &time.to_le_bytes()).map_err(Error::from))
}

/// SYSTEM_CPU_INFORMATION, using the same guest identity and feature bits as KUSER.
fn processor_information(env: &Env<'_>) -> [u8; 12] {
    let cpuid = env.cpu.cpuid_features();
    let mut features: u32 = 0x0080_0275;
    for (bit, mask) in [(4, 2), (23, 0x100), (26, 0x10000)] {
        if cpuid & (1 << bit) != 0 {
            features |= mask;
        }
    }
    features |= 0x80; // PF_COMPARE_EXCHANGE_DOUBLE is always present.
    if cpuid & (1 << 24) != 0 && cpuid & (1 << 25) != 0 {
        features |= 0x42800;
    }
    let mut bytes = [0; 12];
    bytes[2..4].copy_from_slice(&(PROCESSOR_IDENTITY.family() as u16).to_le_bytes());
    let revision = ((PROCESSOR_IDENTITY.model() << 8) | PROCESSOR_IDENTITY.stepping()) as u16;
    bytes[4..6].copy_from_slice(&revision.to_le_bytes());
    bytes[6..8].copy_from_slice(&(PROCESSORS as u16).to_le_bytes());
    bytes[8..12].copy_from_slice(&features.to_le_bytes());
    bytes
}

/// NtSetSystemTime(*new, *old), with Wine's half-second tolerance.
pub fn set_time(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    status::finish((|| {
        let now = env.sys.clock.system_time();
        if args[1] != 0 {
            write_bytes(env, args[1], &now.to_le_bytes())?;
        }
        let mut bytes = [0; 8];
        env.cpu
            .read(args[0], &mut bytes)
            .map_err(|_| status::ACCESS_VIOLATION)?;
        let difference = i64::from_le_bytes(bytes).wrapping_sub(now as i64);
        if difference > -5_000_000 && difference < 5_000_000 {
            Ok(())
        } else {
            Err(0xc000_0061.into())
        }
    })())
}

/// NtQueryTimerResolution(*minimum, *maximum, *current)
pub fn query_timer_resolution(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    status::finish((|| {
        for (address, value) in [(args[0], 156250), (args[1], 10000), (args[2], 10000)] {
            write_u32(env, address, value)?;
        }
        Ok(())
    })())
}

struct TimerResolution(bool);

/// NtSetTimerResolution(resolution, set, *current)
pub fn set_timer_resolution(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    status::finish((|| {
        write_u32(env, args[2], 10000)?;
        let requested = env
            .ext
            .try_get::<TimerResolution>()
            .is_some_and(|value| value.0);
        if !requested && args[1] as u8 == 0 {
            return Err(0xc000_0245.into());
        }
        env.ext.insert(TimerResolution(args[1] as u8 != 0));
        Ok(())
    })())
}

/// NtWow64GetNativeSystemInformation: this i386 machine is native.
pub fn query_native(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let mut args = args.to_vec();
    if args[0] == 114 {
        args[0] = 0;
    }
    query(env, &args)
}

/// Wine leaves the unmeasured counters zero and fills host memory and idle time.
#[allow(deprecated)]
fn performance_information() -> Vec<u8> {
    let mut bytes = vec![0; 312];
    let total = u64::from(physical_pages()) + 1;
    let mut available = 0u64;
    let mut idle = 0u64;
    #[cfg(target_vendor = "apple")]
    {
        // SAFETY: Mach receives initialized records with their exact word counts.
        unsafe {
            let host = libc::mach_host_self();
            let mut load: libc::host_cpu_load_info = std::mem::zeroed();
            let mut count = libc::HOST_CPU_LOAD_INFO_COUNT;
            if libc::host_statistics(
                host,
                libc::HOST_CPU_LOAD_INFO,
                (&mut load as *mut libc::host_cpu_load_info).cast(),
                &mut count,
            ) == 0
            {
                idle = u64::from(load.cpu_ticks[libc::CPU_STATE_IDLE as usize]) * 100_000;
            }
            let mut vm: libc::vm_statistics64 = std::mem::zeroed();
            count = libc::HOST_VM_INFO64_COUNT;
            if libc::host_statistics64(
                host,
                libc::HOST_VM_INFO64,
                (&mut vm as *mut libc::vm_statistics64).cast(),
                &mut count,
            ) == 0
            {
                let page = libc::sysconf(libc::_SC_PAGESIZE) as u64;
                available = (u64::from(vm.free_count) + u64::from(vm.inactive_count)) * page
                    / u64::from(PAGE);
            }
            unsafe extern "C" {
                fn mach_port_deallocate(task: u32, name: u32) -> i32;
            }
            mach_port_deallocate(libc::mach_task_self(), host);
        }
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        if let Ok(uptime) = std::fs::read_to_string("/proc/uptime") {
            idle = uptime
                .split_whitespace()
                .nth(1)
                .and_then(|value| value.parse::<f64>().ok())
                .map_or(0, |value| (value * 10_000_000.0) as u64);
        }
        if let Ok(meminfo) = std::fs::read_to_string("/proc/meminfo") {
            available = meminfo
                .lines()
                .find_map(|line| line.strip_prefix("MemAvailable:"))
                .and_then(|line| line.split_whitespace().next())
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(0)
                * 1024
                / u64::from(PAGE);
        }
    }
    bytes[..8].copy_from_slice(&idle.to_le_bytes());
    for (offset, value) in [
        (44, available),
        (48, total.saturating_sub(available)),
        (52, total + 1),
    ] {
        bytes[offset..offset + 4].copy_from_slice(&(value as u32).to_le_bytes());
    }
    bytes
}

const SYSTEM_SUPPORTED_PROCESSOR_ARCHITECTURES: u32 = 181;
/// Flags of `SYSTEM_SUPPORTED_PROCESSOR_ARCHITECTURES_INFORMATION`, after
/// its 16-bit machine.
const KERNEL_MODE: u32 = 1 << 16;
const USER_MODE: u32 = 1 << 17;
const NATIVE: u32 = 1 << 18;
const PROCESS: u32 = 1 << 19;

/// SYSTEM_SUPPORTED_PROCESSOR_ARCHITECTURES_INFORMATION, as Wine's
/// `NtQuerySystemInformationEx` gives it.
fn supported_architectures(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[class, query, query_length, info, size, result_size] = args else {
        unreachable!()
    };
    status::finish((|| {
        let (bytes, status) = match class {
            SYSTEM_SUPPORTED_PROCESSOR_ARCHITECTURES => {
                if query == 0 || query_length < 4 {
                    return Err(status::INVALID_PARAMETER.into());
                }
                let process = crate::object::read_u32(env, query)?;
                let mut machine = 0;
                if process != 0 {
                    let mut request = crate::protocol::GetProcessInfoRequest {
                        handle: process,
                        ..Default::default()
                    };
                    let (reply, _) = crate::server::request(env, &mut request, &[], &mut [])?;
                    status::check(reply.header.error)?;
                    machine = reply.machine;
                }
                // The server's machines, native first: i386 alone.
                let machines = [crate::server::MACHINE_I386];
                let mut bytes = vec![0u8; 4 * (machines.len() + 1)];
                for (index, &supported) in machines.iter().enumerate() {
                    let mut entry = u32::from(supported) | USER_MODE;
                    if index == 0 {
                        entry |= KERNEL_MODE | NATIVE;
                    }
                    if supported == machine {
                        entry |= PROCESS;
                    }
                    bytes[4 * index..4 * index + 4].copy_from_slice(&entry.to_le_bytes());
                }
                if (size as usize) < bytes.len() {
                    (bytes, status::BUFFER_TOO_SMALL)
                } else {
                    (bytes, status::SUCCESS)
                }
            }
            _ => {
                return Err(Error::Unsupported(format!(
                    "extended system information class {class}"
                )));
            }
        };
        if status == status::SUCCESS {
            write_bytes(env, info, &bytes)?;
        }
        if result_size != 0 {
            write_u32(env, result_size, bytes.len() as u32)?;
        }
        status::check(status)
    })())
}

/// NtQuerySystemInformationEx(class, query, query_length, info, size, result_size)
pub fn query_ex(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    if args[0] == SYSTEM_SUPPORTED_PROCESSOR_ARCHITECTURES {
        return supported_architectures(env, args);
    }
    if args[0] == 107 {
        return status::finish((|| {
            if args[1] == 0 || args[2] < 4 {
                return Err(status::INVALID_PARAMETER.into());
            }
            let relation = crate::object::read_u32(env, args[1])?;
            let bytes = logical_processors(true, relation);
            if args[5] != 0 {
                write_u32(env, args[5], bytes.len() as u32)?;
            }
            if args[4] < bytes.len() as u32 {
                return Err(status::INFO_LENGTH_MISMATCH.into());
            }
            write_bytes(env, args[3], &bytes)?;
            Ok(())
        })());
    }
    if args[0] == 175 {
        return status::finish((|| {
            if args[1] == 0 || args[2] < 4 {
                return Err(status::INVALID_PARAMETER.into());
            }
            let process = crate::object::read_u32(env, args[1])?;
            if process != 0 {
                crate::information::current_process(env, process)?;
            }
            let length = PROCESSORS * 32;
            if args[4] >= length && args[3] == 0 {
                return Err(status::ACCESS_VIOLATION.into());
            }
            if args[5] != 0 {
                write_u32(env, args[5], length)?;
            }
            if args[4] < length {
                return Err(0xc000_0023.into());
            }
            let mut bytes = vec![0; length as usize];
            for index in 0..PROCESSORS {
                let entry = &mut bytes[index as usize * 32..];
                entry[..4].copy_from_slice(&32u32.to_le_bytes());
                entry[8..12].copy_from_slice(&(0x100 + index).to_le_bytes());
                entry[14] = index as u8;
                entry[15] = index as u8;
            }
            write_bytes(env, args[3], &bytes)?;
            Ok(())
        })());
    }
    if args[0] == 83 {
        return status::finish((|| {
            if args[1] == 0 || args[2] < 2 {
                return Err(status::INVALID_PARAMETER.into());
            }
            if crate::object::read_u32(env, args[1])? & 0xffff != 0 {
                return Err(status::INVALID_PARAMETER.into());
            }
            let length = PROCESSORS * 8;
            if args[5] != 0 {
                write_u32(env, args[5], length)?;
            }
            if args[4] < length {
                return Err(0xc000_0023.into());
            }
            write_bytes(env, args[3], &vec![0; length as usize])?;
            Ok(())
        })());
    }
    Outcome::Fail(format!("extended system information class {}", args[0]))
}

/// `RSMB`, the raw SMBIOS provider.
const RSMB: u32 = u32::from_be_bytes(*b"RSMB");
const FIRMWARE_ENUMERATE: u32 = 0;
const FIRMWARE_GET: u32 = 1;
/// `FIELD_OFFSET(SYSTEM_FIRMWARE_TABLE_INFORMATION, TableBuffer)`.
const FIRMWARE_HEADER: u32 = 16;

/// SystemFirmwareTableInformation, as Wine's `enum_firmware_info` and
/// `get_firmware_info`.
fn firmware_table(env: &mut Env<'_>, info: u32, size: u32, result_size: u32) -> Result<(), Error> {
    if size < FIRMWARE_HEADER {
        if result_size != 0 {
            write_u32(env, result_size, 0)?;
        }
        return Err(status::INFO_LENGTH_MISMATCH.into());
    }
    let provider = crate::object::read_u32(env, info)?;
    let action = crate::object::read_u32(env, info + 4)?;
    if provider != RSMB || !matches!(action, FIRMWARE_ENUMERATE | FIRMWARE_GET) {
        return Err(Error::Unsupported(format!(
            "firmware table provider {provider:08x} action {action}"
        )));
    }
    // The one table, of ID 0.
    let table = match action {
        FIRMWARE_ENUMERATE => 0u32.to_le_bytes().to_vec(),
        _ => smbios(env),
    };
    write_u32(env, info + 12, table.len() as u32)?;
    let required = FIRMWARE_HEADER + table.len() as u32;
    if result_size != 0 {
        write_u32(env, result_size, required)?;
    }
    if size < required {
        return Err(status::BUFFER_TOO_SMALL.into());
    }
    write_bytes(env, info + FIRMWARE_HEADER, &table)?;
    Ok(())
}

/// The SMBIOS tables of the virtual machine, as Wine's `create_smbios_data`
/// builds them, from fixed strings rather than the host's: no serial
/// numbers, no UUID, and the emulated processor.
fn smbios(env: &Env<'_>) -> Vec<u8> {
    let mut tables = Smbios::default();
    // BIOS: vendor, version, start segment, date, ROM size, characteristics
    // "not supported", no extensions, releases "not supported".
    let mut bios = vec![1, 2, 0x00, 0xe0, 3, 0];
    bios.extend_from_slice(&8u64.to_le_bytes());
    bios.extend_from_slice(&[0, 0, 0xff, 0xff, 0xff, 0xff]);
    tables.append(0, &bios, &[VENDOR, "1.0", "01/01/2025"]);
    // System: vendor, product, no version or serial, a nil UUID, woken by
    // the power switch, no SKU or family.
    let mut system = vec![1, 2, 0, 0];
    system.extend_from_slice(&[0; 16]);
    system.extend_from_slice(&[6, 0, 0]);
    tables.append(1, &system, &[VENDOR, PRODUCT]);
    // Chassis: vendor, a desktop, no version, serial or tag, states
    // unknown.
    let mut chassis = vec![1, 3, 0, 0, 0, 2, 2, 2, 2];
    chassis.extend_from_slice(&[0; 4]);
    chassis.extend_from_slice(&[0, 0, 0, 3]);
    let chassis = tables.append(3, &chassis, &[VENDOR]);
    // Board: vendor, product, a removable hosting motherboard in the chassis.
    let mut board = vec![1, 2, 0, 0, 0, 5, 0];
    board.extend_from_slice(&chassis.to_le_bytes());
    board.extend_from_slice(&[0xa, 0]);
    tables.append(2, &board, &[VENDOR, PRODUCT]);
    tables.append(
        4,
        &processor(env),
        &[
            "Socket #0",
            PROCESSOR_IDENTITY.vendor,
            PROCESSOR_IDENTITY.name,
        ],
    );
    tables.append(32, &[0; 16], &[]);
    tables.append(127, &[], &[]);
    let mut bytes = vec![0, SMBIOS_MAJOR, SMBIOS_MINOR, 0];
    bytes.extend_from_slice(&(tables.data.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&tables.data);
    bytes
}

const SMBIOS_MAJOR: u8 = 3;
const SMBIOS_MINOR: u8 = 0;
const VENDOR: &str = "Hitode";
const PRODUCT: &str = "Hitode Virtual Machine";

/// `append_smbios_processor` for one package of every processor, one thread
/// a core.
fn processor(env: &Env<'_>) -> Vec<u8> {
    let cores = PROCESSORS as u8;
    // Socket, a central processor of unknown family, vendor.
    let mut processor = vec![1, 3, 2, 2];
    // The CPUID signature and feature bits.
    processor.extend_from_slice(&PROCESSOR_IDENTITY.signature.to_le_bytes());
    processor.extend_from_slice(&env.cpu.cpuid_features().to_le_bytes());
    // Version, no voltage or clocks, enabled, unknown socket and caches.
    processor.extend_from_slice(&[3, 0, 0, 0, 0, 0, 0, 0, 0x41, 2]);
    processor.extend_from_slice(&[0xff; 6]);
    // No serial, tag or part number.
    processor.extend_from_slice(&[0, 0, 0, cores, cores, cores]);
    let characteristics: u16 = if cores > 1 { 1 << 3 } else { 0 };
    for word in [characteristics, 2, cores.into(), cores.into(), cores.into()] {
        processor.extend_from_slice(&word.to_le_bytes());
    }
    processor
}

#[derive(Default)]
struct Smbios {
    data: Vec<u8>,
    handles: u16,
}

impl Smbios {
    /// `append_smbios`: a structure of `kind` with `fields` after its
    /// header, then its strings; returns its handle.
    fn append(&mut self, kind: u8, fields: &[u8], strings: &[&str]) -> u16 {
        let handle = self.handles;
        self.handles += 1;
        self.data.push(kind);
        self.data.push(4 + fields.len() as u8);
        self.data.extend_from_slice(&handle.to_le_bytes());
        self.data.extend_from_slice(fields);
        for string in strings {
            self.data.extend_from_slice(string.as_bytes());
            self.data.push(0);
        }
        if strings.is_empty() {
            self.data.push(0);
        }
        self.data.push(0);
        handle
    }
}

/// Wine's list_processes reply converted to i386 SYSTEM_PROCESS_INFORMATION.
fn process_information(
    env: &mut Env<'_>,
    class: u32,
    info: u32,
    size: u32,
    returned: u32,
) -> Result<(), Error> {
    let mut request = crate::protocol::ListProcessesRequest::default();
    let mut buffer = vec![0; size as usize];
    let (reply, _) = crate::server::request(env, &mut request, &[], &mut buffer)?;
    let thread_size = if class == 57 { 96 } else { 64 };
    if reply.header.error != 0 {
        if returned != 0 {
            let length = if reply.header.error == status::INFO_LENGTH_MISMATCH {
                248 * reply.process_count as u32
                    + (reply.total_name_len + reply.process_count as u32) * 2
                    + reply.total_thread_count as u32 * thread_size
            } else {
                0
            };
            write_u32(env, returned, length)?;
        }
        return Err(reply.header.error.into());
    }
    let word = |at: usize| u32::from_le_bytes(buffer[at..at + 4].try_into().unwrap());
    let mut pos = 0usize;
    let mut length = 0u32;
    for i in 0..reply.process_count {
        pos = pos.next_multiple_of(8);
        let start = pos;
        let name_length = word(start + 8) as usize;
        let threads = word(start + 12);
        let pid = word(start + 20);
        let name = &buffer[start + 40..start + 40 + name_length];
        let name = name.as_chunks::<2>().0;
        let last = name
            .iter()
            .rposition(|pair| *pair == [b'\\', 0])
            .map_or(0, |index| index + 1);
        let name: Vec<u8> = name[last..].iter().flatten().copied().collect();
        let proc_length = (248 + threads * thread_size + name.len() as u32 + 2).next_multiple_of(8);
        let mut bytes = vec![0; proc_length as usize];
        let put = |bytes: &mut [u8], offset: usize, value: u32| {
            bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes())
        };
        if i + 1 < reply.process_count {
            put(&mut bytes, 0, proc_length);
        }
        put(&mut bytes, 4, threads);
        bytes[32..40].copy_from_slice(&buffer[start..start + 8]);
        for (offset, value) in [
            (64, word(start + 16)),
            (68, pid),
            (72, word(start + 24)),
            (76, word(start + 32)),
            (80, word(start + 28)),
        ] {
            put(&mut bytes, offset, value);
        }
        pos = (start + 40 + name_length).next_multiple_of(8);
        for j in 0..threads {
            let output = 184 + j as usize * thread_size as usize;
            bytes[output + 16..output + 24].copy_from_slice(&buffer[pos..pos + 8]);
            for (offset, value) in [
                (32, pid),
                (36, word(pos + 8)),
                (40, word(pos + 16)),
                (44, word(pos + 12)),
            ] {
                put(&mut bytes, output + offset, value);
            }
            if class == 57 {
                put(&mut bytes, output + 72, word(pos + 32));
                put(&mut bytes, output + 76, word(pos + 24));
            }
            pos += 40;
        }
        let name_at = 184 + threads * thread_size;
        put(
            &mut bytes,
            56,
            name.len() as u32 | ((name.len() as u32 + 2) << 16),
        );
        put(&mut bytes, 60, info + length + name_at);
        bytes[name_at as usize..name_at as usize + name.len()].copy_from_slice(&name);
        if length + proc_length <= size {
            write_bytes(env, info + length, &bytes)?;
        }
        length += proc_length;
    }
    if returned != 0 {
        write_u32(env, returned, length)?;
    }
    if length > size {
        return Err(status::INFO_LENGTH_MISMATCH.into());
    }
    Ok(())
}

/// The server's handle list, translated to either NT handle-information layout.
fn handle_information(
    env: &mut Env<'_>,
    class: u32,
    info: u32,
    size: u32,
    returned: u32,
) -> Result<(), Error> {
    let (header, entry_size) = if class == 16 { (4, 16) } else { (8, 28) };
    if size < header + entry_size {
        if returned != 0 {
            write_u32(env, returned, 0)?;
        }
        return Err(status::INFO_LENGTH_MISMATCH.into());
    }
    if info == 0 {
        return Err(status::ACCESS_VIOLATION.into());
    }
    let mut request = crate::protocol::GetSystemHandlesRequest::default();
    let mut buffer = vec![0; ((size - header) / entry_size * 20) as usize];
    let (reply, count) = crate::server::request(env, &mut request, &[], &mut buffer)?;
    let length = header + entry_size * reply.count;
    if returned != 0 {
        write_u32(env, returned, length)?;
    }
    if reply.header.error == 0xc000_0023 {
        return Err(status::INFO_LENGTH_MISMATCH.into());
    }
    status::check(reply.header.error)?;
    let count = count / 20;
    let mut bytes = vec![0; header as usize + entry_size as usize * count];
    bytes[..4].copy_from_slice(&(count as u32).to_le_bytes());
    for (index, item) in buffer[..count * 20].as_chunks::<20>().0.iter().enumerate() {
        let word = |offset| u32::from_le_bytes(item[offset..offset + 4].try_into().unwrap());
        let entry = &mut bytes[header as usize + index * entry_size as usize..];
        if class == 16 {
            entry[..2].copy_from_slice(&(word(0) as u16).to_le_bytes());
            entry[4] = word(16) as u8;
            entry[5] = word(12) as u8;
            entry[6..8].copy_from_slice(&(word(4) as u16).to_le_bytes());
            entry[12..16].copy_from_slice(&word(8).to_le_bytes());
        } else {
            entry[4..8].copy_from_slice(&word(0).to_le_bytes());
            entry[8..12].copy_from_slice(&word(4).to_le_bytes());
            entry[12..16].copy_from_slice(&word(8).to_le_bytes());
            entry[18..20].copy_from_slice(&(word(16) as u16).to_le_bytes());
            entry[20..24].copy_from_slice(&word(12).to_le_bytes());
        }
    }
    write_bytes(env, info, &bytes)?;
    Ok(())
}

/// The guest has one processor group; each processor is a separate core.
fn logical_processors(extended: bool, relation: u32) -> Vec<u8> {
    let relation = if relation == 0xffff {
        u32::MAX
    } else {
        relation
    };
    let mut bytes = Vec::new();
    for kind in [0u32, 1, 3, 4] {
        if relation != u32::MAX && relation != kind {
            continue;
        }
        if !extended {
            if kind == 4 {
                continue;
            }
            let mut entry = vec![0; 24];
            entry[..4].copy_from_slice(&((1u32 << PROCESSORS) - 1).to_le_bytes());
            entry[4..8].copy_from_slice(&kind.to_le_bytes());
            bytes.extend(entry);
            continue;
        }
        let length = if kind == 4 { 76 } else { 44 };
        let mut entry = vec![0; length];
        entry[..4].copy_from_slice(&kind.to_le_bytes());
        entry[4..8].copy_from_slice(&(length as u32).to_le_bytes());
        if kind == 4 {
            entry[8..10].copy_from_slice(&1u16.to_le_bytes());
            entry[10..12].copy_from_slice(&1u16.to_le_bytes());
            entry[32] = 32;
            entry[33] = PROCESSORS as u8;
            entry[72..76].copy_from_slice(&((1u32 << PROCESSORS) - 1).to_le_bytes());
        } else {
            entry[30..32].copy_from_slice(&1u16.to_le_bytes());
            entry[32..36].copy_from_slice(&((1u32 << PROCESSORS) - 1).to_le_bytes());
        }
        bytes.extend(entry);
    }
    if relation == u32::MAX || relation == 2 {
        #[cfg(target_vendor = "apple")]
        for (name, level, kind) in [
            (c"hw.l1icachesize", 1u8, 1u32),
            (c"hw.l1dcachesize", 1, 2),
            (c"hw.l2cachesize", 2, 0),
            (c"hw.l3cachesize", 3, 0),
        ] {
            let mut size = 0u64;
            let mut length = 8;
            // SAFETY: sysctl fills a writable integer with a bounded size.
            if unsafe {
                libc::sysctlbyname(
                    name.as_ptr(),
                    (&mut size as *mut u64).cast(),
                    &mut length,
                    std::ptr::null_mut(),
                    0,
                )
            } != 0
                || size == 0
            {
                continue;
            }
            let mut entry = vec![0; if extended { 52 } else { 24 }];
            if extended {
                entry[..4].copy_from_slice(&2u32.to_le_bytes());
                entry[4..8].copy_from_slice(&52u32.to_le_bytes());
                entry[40..44].copy_from_slice(&((1u32 << PROCESSORS) - 1).to_le_bytes());
            } else {
                entry[..4].copy_from_slice(&((1u32 << PROCESSORS) - 1).to_le_bytes());
                entry[4..8].copy_from_slice(&2u32.to_le_bytes());
            }
            entry[8] = level;
            entry[9] = if level == 3 { 12 } else { 8 };
            entry[10..12].copy_from_slice(&64u16.to_le_bytes());
            entry[12..16].copy_from_slice(&(size as u32).to_le_bytes());
            entry[16..20].copy_from_slice(&kind.to_le_bytes());
            bytes.extend(entry);
        }
    }
    bytes
}

fn process_image_name(env: &mut Env<'_>, info: u32, size: u32, returned: u32) -> Result<(), Error> {
    if returned != 0 {
        write_u32(env, returned, 12)?;
    }
    if size < 12 {
        return Err(status::INFO_LENGTH_MISMATCH.into());
    }
    let pid = crate::object::read_u32(env, info)?;
    let string = crate::object::read_u32(env, info + 4)?;
    if string & 0xffff != 0 {
        return Err(status::INVALID_PARAMETER.into());
    }
    if pid == 0 {
        return Err(0xc000_000b.into());
    }
    let maximum = string >> 16;
    let mut bytes = vec![0; maximum as usize];
    let mut request = crate::protocol::GetProcessImageNameRequest {
        pid,
        ..Default::default()
    };
    let (reply, size) = crate::server::request(env, &mut request, &[], &mut bytes)?;
    let mut result = reply.header.error;
    if result == 0xc000_0023 || result == 0 && reply.len + 2 > maximum {
        result = status::INFO_LENGTH_MISMATCH;
    }
    if result == 0 || result == status::INFO_LENGTH_MISMATCH {
        write_u32(env, info + 4, (reply.len + 2) << 16)?;
    }
    status::check(result)?;
    let output = crate::object::read_u32(env, info + 8)?;
    write_bytes(env, output, &bytes[..size])?;
    write_bytes(env, output + size as u32, &[0; 2])?;
    write_u32(env, info + 4, reply.len | ((reply.len + 2) << 16))?;
    Ok(())
}

/// CPU counters from the host, after Wine's SystemProcessorPerformanceInformation.
#[allow(deprecated)]
fn processor_performance(count: u32) -> Vec<u8> {
    let mut bytes = vec![0; count as usize * 48];
    #[cfg(target_vendor = "apple")]
    unsafe {
        let host = libc::mach_host_self();
        let mut cpus = 0;
        let mut info = std::ptr::null_mut();
        let mut words = 0;
        // SAFETY: Mach fills the count and the pointer to its allocated word array.
        if libc::host_processor_info(
            host,
            libc::PROCESSOR_CPU_LOAD_INFO,
            &mut cpus,
            &mut info,
            &mut words,
        ) == 0
        {
            let data = std::slice::from_raw_parts(info, words as usize);
            for (index, entry) in bytes
                .as_chunks_mut::<48>()
                .0
                .iter_mut()
                .enumerate()
                .take(cpus as usize)
            {
                let ticks = &data[index * 4..index * 4 + 4];
                let idle = ticks[libc::CPU_STATE_IDLE as usize] as u64 * 100_000;
                let kernel = ticks[libc::CPU_STATE_SYSTEM as usize] as u64 * 100_000 + idle;
                let user = ticks[libc::CPU_STATE_USER as usize] as u64 * 100_000;
                for (offset, value) in [(0, idle), (8, kernel), (16, user)] {
                    entry[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
                }
            }
            unsafe extern "C" {
                fn vm_deallocate(task: u32, address: usize, size: usize) -> i32;
                fn mach_port_deallocate(task: u32, name: u32) -> i32;
            }
            vm_deallocate(libc::mach_task_self(), info as usize, words as usize * 4);
            mach_port_deallocate(libc::mach_task_self(), host);
        }
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        if let Ok(stat) = std::fs::read_to_string("/proc/stat") {
            // SAFETY: sysconf has no preconditions.
            let frequency = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as u64;
            for (entry, line) in bytes
                .as_chunks_mut::<48>()
                .0
                .iter_mut()
                .zip(stat.lines().skip(1))
            {
                let values: Vec<u64> = line
                    .split_whitespace()
                    .skip(1)
                    .filter_map(|value| value.parse().ok())
                    .collect();
                if values.len() < 4 {
                    break;
                }
                let idle = values[3] * 10_000_000 / frequency;
                let kernel = values[2..].iter().sum::<u64>() * 10_000_000 / frequency;
                let user = (values[0] + values[1]) * 10_000_000 / frequency;
                for (offset, value) in [(0, idle), (8, kernel), (16, user)] {
                    entry[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
                }
            }
        }
    }
    bytes
}

/// Wine ignores system time adjustment requests, leaving the shared clock unchanged.
pub fn set(_env: &mut Env<'_>, args: &[u32]) -> Outcome {
    match args[0] {
        28 => Outcome::Return(status::SUCCESS),
        class => Outcome::Fail(format!("setting system information class {class}")),
    }
}

const PROCESSOR_INFORMATION: u32 = 11;
const SYSTEM_EXECUTION_STATE: u32 = 16;
const SYSTEM_BATTERY_STATE: u32 = 5;
/// The size of SYSTEM_BATTERY_STATE.
const BATTERY_SIZE: u32 = 32;
/// `ES_USER_PRESENT`.
const USER_PRESENT: u32 = 4;
/// The size of PROCESSOR_POWER_INFORMATION.
const POWER_SIZE: u32 = 24;

/// NtPowerInformation(level, *input, input_size, *output, output_size)
pub fn power_information(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[level, input, _, output, size] = args else {
        unreachable!()
    };
    status::finish((|| match level {
        PROCESSOR_INFORMATION => {
            if output == 0 || size == 0 {
                return Err(status::INVALID_PARAMETER.into());
            }
            if size / POWER_SIZE < PROCESSORS {
                return Err(status::BUFFER_TOO_SMALL.into());
            }
            // Every processor runs at the guest's time stamp frequency.
            let mhz = (adv_cpu::TSC_FREQ / 1_000_000) as u32;
            for number in 0..PROCESSORS {
                let mut bytes = [0u8; POWER_SIZE as usize];
                for (index, field) in [number, mhz, mhz, mhz].into_iter().enumerate() {
                    bytes[4 * index..4 * index + 4].copy_from_slice(&field.to_le_bytes());
                }
                write_bytes(env, output + number * POWER_SIZE, &bytes)?;
            }
            Ok(())
        }
        // The virtual machine is a desktop on AC power, as Wine's fallback
        // where the host has no battery provider.
        SYSTEM_BATTERY_STATE => {
            if size < BATTERY_SIZE {
                return Err(status::BUFFER_TOO_SMALL.into());
            }
            let mut bytes = [0; BATTERY_SIZE as usize];
            bytes[0] = 1;
            write_bytes(env, output, &bytes)?;
            Ok(())
        }
        SYSTEM_EXECUTION_STATE => {
            if input != 0 {
                return Err(status::INVALID_PARAMETER.into());
            }
            write_u32(env, output, USER_PRESENT)?;
            Ok(())
        }
        level => Err(Error::Unsupported(format!(
            "power information level {level}"
        ))),
    })())
}

/// NtDisplayString writes the Unicode string to Wine's error channel.
pub fn display_string(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    if args[0] == 0 {
        log::error!(target: "wine", "(null)");
        return Outcome::Return(status::SUCCESS);
    }
    status::finish((|| {
        let string = crate::object::read_unicode_string(env, args[0])?;
        log::error!(target: "wine", "{}", String::from_utf16_lossy(&string));
        Ok(())
    })())
}

/// Wine 11.0's NtInitiatePowerAction stub.
pub fn initiate_power_action(_env: &mut Env<'_>, _args: &[u32]) -> Outcome {
    Outcome::Return(status::NOT_IMPLEMENTED)
}

/// Wine 11.0's NtLoadDriver stub.
pub fn load_driver(_env: &mut Env<'_>, _args: &[u32]) -> Outcome {
    Outcome::Return(status::NOT_IMPLEMENTED)
}

/// Wine 11.0's NtUnloadDriver stub.
pub fn unload_driver(_env: &mut Env<'_>, _args: &[u32]) -> Outcome {
    Outcome::Return(status::NOT_IMPLEMENTED)
}

/// Wine 11.0's NtQuerySystemEnvironmentValue stub.
pub fn query_environment(_env: &mut Env<'_>, _args: &[u32]) -> Outcome {
    Outcome::Return(status::NOT_IMPLEMENTED)
}

/// Wine 11.0's NtQuerySystemEnvironmentValueEx stub.
pub fn query_environment_ex(_env: &mut Env<'_>, _args: &[u32]) -> Outcome {
    Outcome::Return(status::NOT_IMPLEMENTED)
}

/// Wine 11.0's NtRaiseHardError stub.
pub fn raise_hard_error(_env: &mut Env<'_>, _args: &[u32]) -> Outcome {
    Outcome::Return(status::NOT_IMPLEMENTED)
}

/// Wine 11.0's NtShutdownSystem stub.
pub fn shutdown(_env: &mut Env<'_>, _args: &[u32]) -> Outcome {
    Outcome::Return(status::SUCCESS)
}

/// Wine 11.0's NtSystemDebugControl stub.
pub fn debug_control(_env: &mut Env<'_>, _args: &[u32]) -> Outcome {
    Outcome::Return(0xc000_0354)
}

struct ExecutionState(u32);

/// NtSetThreadExecutionState retains Wine's process-wide, harmless state.
pub fn set_execution_state(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let current = env
        .ext
        .try_get::<ExecutionState>()
        .map_or(7, |state| state.0);
    status::finish((|| {
        write_u32(env, args[1], current)?;
        if current & 0x8000_0000 == 0 || args[0] & 0x8000_0000 != 0 {
            env.ext.insert(ExecutionState(args[0]));
        }
        Ok(())
    })())
}

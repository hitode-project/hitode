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
            76 => return firmware(env, info, size, result_size),
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
    let &[counter, frequency] = args else {
        unreachable!()
    };
    let ticks = env.sys.clock.now_us() * 10;
    status::finish((|| {
        write_bytes(env, counter, &ticks.to_le_bytes())?;
        if frequency != 0 {
            write_bytes(env, frequency, &TICKS_PER_SECOND.to_le_bytes())?;
        }
        Ok::<_, Error>(())
    })())
}

const TICKS_PER_SECOND: u64 = 10_000_000;

/// NtQuerySystemTime(*time)
pub fn query_time(env: &mut Env<'_>, args: &[u32]) -> Outcome {
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

/// NtQuerySystemInformationEx(class, query, query_length, info, size, result_size)
pub fn query_ex(env: &mut Env<'_>, args: &[u32]) -> Outcome {
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

/// Wine's fallback SMBIOS describes the virtual machine when no host table exists.
fn smbios() -> Vec<u8> {
    let mut table = Vec::new();
    let mut handle = 0u16;
    let mut append = |kind: u8, mut bytes: Vec<u8>, strings: &[(usize, &str)]| {
        bytes[0] = kind;
        bytes[1] = bytes.len() as u8;
        bytes[2..4].copy_from_slice(&handle.to_le_bytes());
        handle += 1;
        let mut pool = Vec::new();
        for (index, &(offset, value)) in strings.iter().enumerate() {
            bytes[offset] = index as u8 + 1;
            pool.extend_from_slice(value.as_bytes());
            pool.push(0);
        }
        if pool.is_empty() {
            pool.push(0);
        }
        pool.push(0);
        table.extend(bytes);
        table.extend(pool);
    };
    let vendor = "The Wine project";
    let mut bios = vec![0; 24];
    bios[6..8].copy_from_slice(&0xe000u16.to_le_bytes());
    bios[10] = 8;
    bios[20..24].fill(0xff);
    append(
        0,
        bios,
        &[(4, vendor), (5, WINE_VERSION), (8, "01/01/2021")],
    );
    let mut system = vec![0; 27];
    system[24] = 6;
    append(
        1,
        system,
        &[(4, vendor), (5, "Wine"), (6, WINE_VERSION), (7, "0")],
    );
    let mut chassis = vec![0; 21];
    chassis[5] = 2;
    chassis[9..13].fill(2);
    chassis[20] = 3;
    append(3, chassis, &[(4, vendor), (6, WINE_VERSION), (7, "0")]);
    let mut board = vec![0; 15];
    board[9] = 5;
    board[11..13].copy_from_slice(&2u16.to_le_bytes());
    board[13] = 0xa;
    append(
        2,
        board,
        &[(4, vendor), (5, "Wine"), (6, WINE_VERSION), (7, "0")],
    );
    let mut processor = vec![0; 48];
    processor[5] = 3;
    processor[6] = 2;
    processor[8..12].copy_from_slice(&PROCESSOR_IDENTITY.signature.to_le_bytes());
    processor[24] = 0x41;
    processor[25] = 2;
    processor[26..32].fill(0xff);
    processor[35..38].fill(PROCESSORS as u8);
    processor[38] = if PROCESSORS > 1 { 8 } else { 0 };
    processor[40..42].copy_from_slice(&2u16.to_le_bytes());
    for offset in [42, 44, 46] {
        processor[offset..offset + 2].copy_from_slice(&(PROCESSORS as u16).to_le_bytes());
    }
    append(
        4,
        processor,
        &[
            (4, "Socket #0"),
            (7, PROCESSOR_IDENTITY.vendor),
            (16, PROCESSOR_IDENTITY.name),
        ],
    );
    append(32, vec![0; 20], &[]);
    append(127, vec![0; 4], &[]);
    let mut bytes = vec![0, 3, 0, 0];
    bytes.extend_from_slice(&(table.len() as u32).to_le_bytes());
    bytes.extend(table);
    bytes
}

fn firmware(env: &mut Env<'_>, info: u32, size: u32, result_size: u32) -> Result<(), Error> {
    let mut length = 16;
    let result = (|| {
        if size < 16 {
            return Err(status::INFO_LENGTH_MISMATCH.into());
        }
        length = 0;
        let provider = crate::object::read_u32(env, info)?;
        let action = crate::object::read_u32(env, info + 4)?;
        if provider != 0x5253_4d42 || action > 1 {
            return Err(status::NOT_IMPLEMENTED.into());
        }
        let bytes = if action == 0 { vec![0; 4] } else { smbios() };
        write_u32(env, info + 12, bytes.len() as u32)?;
        length = 16 + bytes.len() as u32;
        if size < length {
            return Err(0xc000_0023.into());
        }
        write_bytes(env, info + 16, &bytes)?;
        Ok(())
    })();
    if result_size != 0 {
        write_u32(env, result_size, length)?;
    }
    result
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

/// NtPowerInformation(level, input, input_size, output, output_size)
pub fn power(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    status::finish((|| {
        match args[0] {
            5 => {
                if args[4] < 32 {
                    return Err(0xc000_0023.into());
                }
                write_bytes(env, args[3], &battery_state()?)?;
            }
            11 => {
                if args[3] == 0 || args[4] == 0 {
                    return Err(status::INVALID_PARAMETER.into());
                }
                if args[4] / 24 < PROCESSORS {
                    return Err(0xc000_0023.into());
                }
                let current = cpu_frequency(c"hw.cpufrequency").unwrap_or(1000);
                let maximum = cpu_frequency(c"hw.cpufrequency_max").unwrap_or(current);
                for index in 0..PROCESSORS {
                    for (field, value) in [index, maximum, current, maximum, 0, 0]
                        .into_iter()
                        .enumerate()
                    {
                        write_u32(env, args[3] + index * 24 + field as u32 * 4, value)?;
                    }
                }
            }
            level => {
                return Err(Error::Unsupported(format!(
                    "power information level {level}"
                )));
            }
        }
        Ok(())
    })())
}

fn cpu_frequency(name: &std::ffi::CStr) -> Option<u32> {
    #[cfg(target_vendor = "apple")]
    {
        let mut value = 0u64;
        let mut size = 8;
        // SAFETY: sysctl receives a writable integer buffer and its size.
        if unsafe {
            libc::sysctlbyname(
                name.as_ptr(),
                (&mut value as *mut u64).cast(),
                &mut size,
                std::ptr::null_mut(),
                0,
            )
        } == 0
        {
            return Some((value / 1_000_000) as u32);
        }
    }
    #[cfg(not(target_vendor = "apple"))]
    let _ = name;
    None
}

#[cfg(target_vendor = "apple")]
fn battery_state() -> Result<[u8; 32], Error> {
    use std::ffi::c_void;
    type Cf = *const c_void;
    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOPSCopyPowerSourcesInfo() -> Cf;
        fn IOPSCopyPowerSourcesList(blob: Cf) -> Cf;
        fn IOPSGetPowerSourceDescription(blob: Cf, source: Cf) -> Cf;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFRelease(value: Cf);
        fn CFArrayGetCount(array: Cf) -> isize;
        fn CFArrayGetValueAtIndex(array: Cf, index: isize) -> Cf;
        fn CFDictionaryGetValue(dict: Cf, key: Cf) -> Cf;
        fn CFStringCreateWithCString(alloc: Cf, text: *const libc::c_char, encoding: u32) -> Cf;
        fn CFStringCompare(a: Cf, b: Cf, options: u64) -> isize;
        fn CFBooleanGetValue(value: Cf) -> u8;
        fn CFNumberGetValue(value: Cf, kind: i32, output: *mut i32) -> u8;
    }
    // SAFETY: IOKit returns retained CF objects; every property is read with
    // its documented CF type and every temporary or retained object is released.
    unsafe {
        let blob = IOPSCopyPowerSourcesInfo();
        let sources = IOPSCopyPowerSourcesList(blob);
        if sources.is_null() {
            if !blob.is_null() {
                CFRelease(blob);
            }
            return Err(0xc000_0022.into());
        }
        let property = |dict, key: &std::ffi::CStr| {
            let key = CFStringCreateWithCString(std::ptr::null(), key.as_ptr(), 0x08000100);
            let value = CFDictionaryGetValue(dict, key);
            CFRelease(key);
            value
        };
        let equals = |value: Cf, text: &std::ffi::CStr| {
            let string = CFStringCreateWithCString(std::ptr::null(), text.as_ptr(), 0x08000100);
            let equal = !value.is_null() && CFStringCompare(value, string, 0) == 0;
            CFRelease(string);
            equal
        };
        let number = |dict, key, default| {
            let prop = property(dict, key);
            let mut value = default;
            if !prop.is_null() {
                CFNumberGetValue(prop, 9, &mut value);
            }
            value
        };
        let mut bytes = [0; 32];
        bytes[0] = 1;
        for index in 0..CFArrayGetCount(sources) {
            let source =
                IOPSGetPowerSourceDescription(blob, CFArrayGetValueAtIndex(sources, index));
            if source.is_null() || !equals(property(source, c"Transport Type"), c"Internal") {
                continue;
            }
            let present = property(source, c"Is Present");
            if present.is_null() || CFBooleanGetValue(present) == 0 {
                continue;
            }
            bytes[1] = 1;
            let charging = property(source, c"Is Charging");
            let online = equals(property(source, c"Power Source State"), c"AC Power");
            bytes[0] = u8::from(online);
            bytes[2] = u8::from(online && !charging.is_null() && CFBooleanGetValue(charging) != 0);
            bytes[3] = u8::from(!online);
            let voltage = number(source, c"Voltage", 11400);
            let maximum = number(source, c"Max Capacity", 0) * voltage;
            let remaining = number(source, c"Current Capacity", 0) * voltage;
            let rate = number(source, c"Current", 0) * voltage / 1000;
            let estimate = number(source, c"Time to Empty", 0).max(0) * 60;
            for (offset, value) in [
                (8, maximum),
                (12, remaining),
                (16, rate),
                (20, estimate),
                (24, maximum / 20),
                (28, maximum / 3),
            ] {
                bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
            }
            break;
        }
        CFRelease(sources);
        CFRelease(blob);
        Ok(bytes)
    }
}

#[cfg(not(target_vendor = "apple"))]
fn battery_state() -> Result<[u8; 32], Error> {
    // Wine's fallback on a host without a battery provider is an AC-powered desktop.
    let mut bytes = [0; 32];
    bytes[0] = 1;
    Ok(bytes)
}

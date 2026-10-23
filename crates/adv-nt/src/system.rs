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
const SYSTEM_FIRMWARE_TABLE_INFORMATION: u32 = 76;
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
            SYSTEM_BASIC_INFORMATION => (basic_information().to_vec(), true),
            SYSTEM_CPU_INFORMATION => (processor_information(env).to_vec(), false),
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
                let ticks = u64::from(crate::object::read_u32(env, crate::kuser::ADDRESS + 8)?)
                    | (u64::from(crate::object::read_u32(env, crate::kuser::ADDRESS + 12)?) << 32);
                // Wine's fallback CPU counters, advanced by the shared clock.
                let mut bytes = vec![0; count as usize * 48];
                for cpu in bytes.as_chunks_mut::<48>().0 {
                    cpu[..8].copy_from_slice(&(3 * ticks).to_le_bytes());
                    cpu[8..16].copy_from_slice(&ticks.to_le_bytes());
                    cpu[16..24].copy_from_slice(&(2 * ticks).to_le_bytes());
                }
                (bytes, false)
            }
            u32::MAX => {
                if result_size != 0 {
                    write_u32(env, result_size, 0)?;
                }
                return Err(status::INVALID_INFO_CLASS.into());
            }
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
            SYSTEM_FIRMWARE_TABLE_INFORMATION => {
                return firmware_table(env, info, size, result_size);
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

const SYSTEM_SUPPORTED_PROCESSOR_ARCHITECTURES: u32 = 181;
/// Flags of `SYSTEM_SUPPORTED_PROCESSOR_ARCHITECTURES_INFORMATION`, after
/// its 16-bit machine.
const KERNEL_MODE: u32 = 1 << 16;
const USER_MODE: u32 = 1 << 17;
const NATIVE: u32 = 1 << 18;
const PROCESS: u32 = 1 << 19;
const BUFFER_TOO_SMALL: status::Status = 0xc000_0023;

/// NtQuerySystemInformationEx(class, *query, query_length, *info, size,
/// *result_size)
pub fn query_ex(env: &mut Env<'_>, args: &[u32]) -> Outcome {
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
                    (bytes, BUFFER_TOO_SMALL)
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
        return Err(BUFFER_TOO_SMALL.into());
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

const PROCESSOR_INFORMATION: u32 = 11;
const SYSTEM_EXECUTION_STATE: u32 = 16;
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
                return Err(BUFFER_TOO_SMALL.into());
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

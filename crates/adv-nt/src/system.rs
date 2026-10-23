//! System information, after Wine's `unix/system.c`.

use adv_machine::layout::{GRANULE, PAGE};
use adv_machine::{Env, Outcome};

use crate::memory::USER_END;
use crate::object::{write_bytes, write_u32};
use crate::process::PROCESSORS;
use crate::status::{self, Error};
use crate::timezone;

const SYSTEM_BASIC_INFORMATION: u32 = 0;
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
            SYSTEM_BASIC_INFORMATION => (basic_information().to_vec(), true),
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

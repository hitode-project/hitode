//! KUSER_SHARED_DATA, the read-only page where ntdll.dll reads the system
//! version, the processor features and the clocks.
//!
//! In Wine the server shares the page, wineboot and ntdll.so fill its fixed
//! fields, and the server refreshes its clocks every tick. Here the platform
//! owns it: the processor features are the guest CPU's, and the clocks are
//! refreshed from the machine clock as each system call enters.

use adv_cpu::backend::{Guest, MemFault, Prot};
use adv_machine::Env;
use adv_machine::layout::PAGE;

use crate::timezone;

pub const ADDRESS: u32 = 0x7ffe_0000;

const TICK_COUNT_LOW: u32 = 0x000;
const TICK_COUNT_MULTIPLIER: u32 = 0x004;
const INTERRUPT_TIME: u32 = 0x008;
const SYSTEM_TIME: u32 = 0x014;
const TIME_ZONE_BIAS: u32 = 0x020;
const IMAGE_NUMBER_LOW: u32 = 0x02c;
const NT_SYSTEM_ROOT: u32 = 0x030;
const LARGE_PAGE_MINIMUM: u32 = 0x244;
const NT_BUILD_NUMBER: u32 = 0x260;
const NT_PRODUCT_TYPE: u32 = 0x264;
const PRODUCT_TYPE_IS_VALID: u32 = 0x268;
const NATIVE_PROCESSOR_ARCHITECTURE: u32 = 0x26a;
const NT_MAJOR_VERSION: u32 = 0x26c;
const PROCESSOR_FEATURES: u32 = 0x274;
const SUITE_MASK: u32 = 0x2d0;
const NX_SUPPORT_POLICY: u32 = 0x2d5;
const NUMBER_OF_PHYSICAL_PAGES: u32 = 0x2e8;
const SYSTEM_CALL: u32 = 0x308;
const TICK_COUNT: u32 = 0x320;
const ACTIVE_PROCESSOR_COUNT: u32 = 0x3c0;
const ACTIVE_GROUP_COUNT: u32 = 0x3c4;

/// Windows 10 22H2, as the PEB reports it.
const BUILD_NUMBER: u32 = 19045;
const MAJOR_VERSION: u32 = 10;
const VER_NT_WORKSTATION: u32 = 1;
const VER_SUITE_SINGLEUSERTS: u32 = 0x100;
const PROCESSOR_ARCHITECTURE_INTEL: u16 = 0;
const NX_SUPPORT_POLICY_OPTIN: u8 = 2;
const MACHINE_I386: u16 = 0x014c;

/// PF_* indices and the CPUID leaf 1 EDX bits that grant them, as Wine's
/// `init_shared_data_cpuinfo` reads them.
const PF_COMPARE_EXCHANGE_DOUBLE: u32 = 2;
const PF_MMX_INSTRUCTIONS_AVAILABLE: u32 = 3;
const PF_XMMI_INSTRUCTIONS_AVAILABLE: u32 = 6;
const PF_RDTSC_INSTRUCTION_AVAILABLE: u32 = 8;
const PF_PAE_ENABLED: u32 = 9;
const PF_XMMI64_INSTRUCTIONS_AVAILABLE: u32 = 10;
const PF_FASTFAIL_AVAILABLE: u32 = 23;

/// Maps the page with its fixed fields; `features` is the guest CPU's
/// CPUID leaf 1 EDX.
pub fn map(
    cpu: &mut dyn Guest,
    features: u32,
    physical_pages: u32,
    processors: u32,
) -> Result<(), MemFault> {
    cpu.map(ADDRESS, PAGE, Prot::READ)?;
    let mut page = vec![0u8; PAGE as usize];
    let mut put = |offset: u32, bytes: &[u8]| {
        let offset = offset as usize;
        page[offset..offset + bytes.len()].copy_from_slice(bytes);
    };
    put(TICK_COUNT_MULTIPLIER, &(1u32 << 24).to_le_bytes());
    put(
        IMAGE_NUMBER_LOW,
        &[MACHINE_I386.to_le_bytes(), MACHINE_I386.to_le_bytes()].concat(),
    );
    let root: Vec<u8> = r"C:\windows"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    put(NT_SYSTEM_ROOT, &root);
    put(LARGE_PAGE_MINIMUM, &(2u32 << 20).to_le_bytes());
    put(NT_BUILD_NUMBER, &BUILD_NUMBER.to_le_bytes());
    put(NT_PRODUCT_TYPE, &VER_NT_WORKSTATION.to_le_bytes());
    put(PRODUCT_TYPE_IS_VALID, &[1]);
    put(
        NATIVE_PROCESSOR_ARCHITECTURE,
        &PROCESSOR_ARCHITECTURE_INTEL.to_le_bytes(),
    );
    put(NT_MAJOR_VERSION, &MAJOR_VERSION.to_le_bytes());
    put(SUITE_MASK, &VER_SUITE_SINGLEUSERTS.to_le_bytes());
    put(NX_SUPPORT_POLICY, &[NX_SUPPORT_POLICY_OPTIN]);
    put(NUMBER_OF_PHYSICAL_PAGES, &physical_pages.to_le_bytes());
    put(SYSTEM_CALL, &1u32.to_le_bytes());
    put(ACTIVE_PROCESSOR_COUNT, &processors.to_le_bytes());
    put(ACTIVE_GROUP_COUNT, &1u32.to_le_bytes());

    let has = |bit: u32| u8::from(features & (1 << bit) != 0);
    for (feature, value) in [
        (PF_FASTFAIL_AVAILABLE, 1),
        (PF_COMPARE_EXCHANGE_DOUBLE, 1),
        (PF_RDTSC_INSTRUCTION_AVAILABLE, has(4)),
        (PF_PAE_ENABLED, has(6)),
        (PF_MMX_INSTRUCTIONS_AVAILABLE, has(23)),
        (PF_XMMI_INSTRUCTIONS_AVAILABLE, has(24) & has(25)),
        (PF_XMMI64_INSTRUCTIONS_AVAILABLE, has(26)),
    ] {
        put(PROCESSOR_FEATURES + feature, &[value]);
    }
    cpu.write(ADDRESS, &page)
}

/// When the time zone bias was last read, and its value.
#[derive(Default)]
struct Bias {
    read_at_us: Option<u64>,
    seconds: libc::time_t,
}

/// Refreshes the clocks, as the server's `set_user_shared_data_time`: the
/// time zone bias at most once a second, the others every time.
pub fn refresh(env: &mut Env<'_>) {
    let system = env.sys.clock.system_time();
    let now_us = env.sys.clock.now_us();
    let interrupt = now_us * 10;
    let tick_count = now_us / 1000;
    let bias = env.ext.get_or_default::<Bias>();
    if bias.read_at_us.is_none_or(|at| now_us - at > 1_000_000) {
        *bias = Bias {
            read_at_us: Some(now_us),
            seconds: timezone::bias_now(),
        };
    }
    let bias = (bias.seconds * 10_000_000) as u64;
    for (offset, time) in [
        (SYSTEM_TIME, system),
        (INTERRUPT_TIME, interrupt),
        (TIME_ZONE_BIAS, bias),
        (TICK_COUNT, tick_count),
    ] {
        write_time(env.cpu, ADDRESS + offset, time);
    }
    let _ = env
        .cpu
        .write(ADDRESS + TICK_COUNT_LOW, &(tick_count as u32).to_le_bytes());
}

/// KSYSTEM_TIME: the low part between two copies of the high part, written
/// high, low, high so a reader can tell a torn read.
fn write_time(cpu: &mut dyn Guest, address: u32, time: u64) {
    let (low, high) = (time as u32, (time >> 32) as u32);
    let _ = cpu.write(address + 8, &high.to_le_bytes());
    let _ = cpu.write(address, &low.to_le_bytes());
    let _ = cpu.write(address + 4, &high.to_le_bytes());
}

const PROCESSOR_FEATURE_MAX: u32 = 64;

/// NtWow64IsProcessorFeaturePresent(feature)
pub fn is_processor_feature_present(env: &mut Env<'_>, args: &[u32]) -> adv_machine::Outcome {
    let feature = args[0];
    let mut present = [0u8];
    if feature < PROCESSOR_FEATURE_MAX {
        let _ = env
            .cpu
            .read(ADDRESS + PROCESSOR_FEATURES + feature, &mut present);
    }
    adv_machine::Outcome::Return(u32::from(present[0] != 0))
}

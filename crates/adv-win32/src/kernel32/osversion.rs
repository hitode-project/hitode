//! Version APIs over the PEB-carried personality (crate::personality), not
//! the host OS version. Native reads suite and product type from
//! KUSER_SHARED_DATA; without that page they come from the same profile.
use adv_abi::Registry;
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::{
    Env, Outcome,
    layout::{PEB_ADDR, peb},
};

use crate::{Win32, stdcall, util::set_last_error};

/// Values indexed by the bit position of VER_*, plus szCSDVersion.
struct Identity {
    version: [u32; 8],
    csd: Vec<u16>,
}

fn identity(env: &Env<'_>) -> Result<Identity, u32> {
    const NOACCESS: u32 = 998;
    let cpu = &env.cpu;
    let word = |offset| cpu.read_u32(PEB_ADDR + offset).map_err(|_| NOACCESS);
    let short = |offset| cpu.read_u16(PEB_ADDR + offset).map_err(|_| NOACCESS);
    let service_pack = short(peb::OS_CSD_VERSION)?;
    let length = u32::from(short(peb::CSD_VERSION)?).min(254) / 2;
    let buffer = word(peb::CSD_VERSION + 4)?;
    let csd = (0..length)
        .map(|index| cpu.read_u16(buffer + index * 2).map_err(|_| NOACCESS))
        .collect::<Result<_, _>>()?;
    let profile = &env.ext.get::<Win32>().cfg.personality;
    Ok(Identity {
        version: [
            word(peb::OS_MINOR_VERSION)?,
            word(peb::OS_MAJOR_VERSION)?,
            u32::from(short(peb::OS_BUILD_NUMBER)?),
            word(peb::OS_PLATFORM_ID)?,
            u32::from(service_pack & 0xff),
            u32::from(service_pack >> 8),
            u32::from(profile.suite_mask),
            u32::from(profile.product_type),
        ],
        csd,
    })
}

fn fail(env: &mut Env<'_>, code: u32) -> Outcome {
    set_last_error(env, code);
    Outcome::Return(0)
}
fn get_version(env: &mut Env<'_>) -> Outcome {
    match identity(env) {
        Ok(Identity { version: v, .. }) => Outcome::Return((v[2] << 16) | (v[0] << 8) | v[1]),
        Err(code) => fail(env, code),
    }
}
fn get_version_ex(env: &mut Env<'_>, wide: bool) -> Outcome {
    match write_version(env, env.arg(0), wide) {
        Ok(()) => Outcome::Return(1),
        Err(code) => fail(env, code),
    }
}
/// RtlGetVersion reports the same identity as an NTSTATUS, without LastError.
fn rtl_get_version(env: &mut Env<'_>) -> Outcome {
    const STATUS_INVALID_PARAMETER: u32 = 0xc000_000d;
    match write_version(env, env.arg(0), true) {
        Ok(()) => Outcome::Return(0),
        Err(_) => Outcome::Return(STATUS_INVALID_PARAMETER),
    }
}
/// Windows 7 reports the installed edition for any queried version.
fn get_product_info(env: &mut Env<'_>) -> Outcome {
    let edition = env.ext.get::<Win32>().cfg.personality.edition;
    let pointer = env.arg(4);
    match env.cpu.write(pointer, &edition.to_le_bytes()) {
        Ok(()) => Outcome::Return(1),
        Err(_) => fail(env, 998),
    }
}
/// Desktop processes have no package identity; the buffer is left untouched.
fn get_current_package_id(_: &mut Env<'_>) -> Outcome {
    const APPMODEL_ERROR_NO_PACKAGE: u32 = 15700;
    Outcome::Return(APPMODEL_ERROR_NO_PACKAGE)
}
/// Fill an OSVERSIONINFO[EX]{A,W} whose size field selects the layout.
fn write_version(env: &mut Env<'_>, pointer: u32, wide: bool) -> Result<(), u32> {
    let base = if wide { 276 } else { 148 };
    let size = env.cpu.read_u32(pointer).unwrap_or(0);
    if (size != base && size != base + 8) || !env.cpu.accessible(pointer, size, Prot::WRITE) {
        return Err(87);
    }
    let Identity { version, csd } = identity(env)?;
    let mut bytes = [0u8; 284];
    bytes[..4].copy_from_slice(&size.to_le_bytes());
    for (index, value) in [version[1], version[0], version[2], version[3]]
        .into_iter()
        .enumerate()
    {
        bytes[4 + index * 4..8 + index * 4].copy_from_slice(&value.to_le_bytes());
    }
    let csd = if wide {
        csd.iter()
            .flat_map(|unit| unit.to_le_bytes())
            .collect::<Vec<_>>()
    } else {
        env.ext.get::<Win32>().codec.from_utf16(0, &csd).0
    };
    let room = if wide { 254 } else { 127 };
    bytes[20..20 + csd.len().min(room)].copy_from_slice(&csd[..csd.len().min(room)]);
    if size == base + 8 {
        let at = base as usize;
        bytes[at..at + 2].copy_from_slice(&(version[5] as u16).to_le_bytes());
        bytes[at + 2..at + 4].copy_from_slice(&(version[4] as u16).to_le_bytes());
        bytes[at + 4..at + 6].copy_from_slice(&(version[6] as u16).to_le_bytes());
        bytes[at + 6] = version[7] as u8;
    }
    env.cpu
        .write(pointer, &bytes[..size as usize])
        .map_err(|_| 998)
}

fn condition_mask(env: &mut Env<'_>) -> Outcome {
    let incoming = u64::from(env.arg(0)) | (u64::from(env.arg(1)) << 32);
    let types = env.arg(2) & 0xff;
    if types == 0 {
        return Outcome::Return64(0);
    }
    let bit = 31 - types.leading_zeros();
    let shift = bit * 3;
    let condition = u64::from(env.arg(3) & 7);
    // Bit 63 is the native condition-mask marker; comparisons use only the
    // selected three-bit fields. Replacing an operator clears its old bits.
    Outcome::Return64((incoming & !(7u64 << shift)) | (condition << shift) | (1 << 63))
}
fn compare(actual: u32, requested: u32, condition: u64) -> bool {
    match condition {
        1 => actual == requested,
        2 => actual > requested,
        3 => actual >= requested,
        4 => actual < requested,
        5 => actual <= requested,
        _ => false,
    }
}
fn matches(actual: [u32; 8], requested: [u32; 8], types: u32, condition: u64) -> Result<bool, u32> {
    if types == 0 || types & !0xff != 0 {
        return Err(160);
    }
    let op = |bit: usize| (condition >> (3 * bit)) & 7;
    // Independent attributes must match even if an earlier version tuple
    // component already satisfies a greater/less comparison.
    for bit in [2, 3, 7] {
        if types & (1 << bit) != 0 && !compare(actual[bit], requested[bit], op(bit)) {
            return Ok(false);
        }
    }
    if types & 0x40 != 0 {
        let good = match op(6) {
            6 => actual[6] & requested[6] == requested[6],
            7 => requested[6] == 0 || actual[6] & requested[6] != 0,
            _ => return Err(160),
        };
        if !good {
            return Ok(false);
        }
    }
    let mut operation = 0;
    for bit in [1, 0, 5, 4] {
        if types & (1 << bit) == 0 {
            continue;
        }
        if operation == 0 || operation == 1 {
            operation = op(bit);
        }
        if operation == 0 {
            return Ok(false);
        }
        if actual[bit] != requested[bit] || bit == 4 {
            return Ok(compare(actual[bit], requested[bit], operation));
        }
    }
    Ok(true)
}
fn verify(env: &mut Env<'_>, wide: bool) -> Outcome {
    let pointer = env.arg(0);
    let base = if wide { 276 } else { 148 };
    if !env.cpu.accessible(pointer, base + 8, Prot::READ) {
        return fail(env, 998);
    }
    let requested = [
        env.cpu.read_u32(pointer + 8).unwrap(),
        env.cpu.read_u32(pointer + 4).unwrap(),
        env.cpu.read_u32(pointer + 12).unwrap(),
        env.cpu.read_u32(pointer + 16).unwrap(),
        u32::from(env.cpu.read_u16(pointer + base + 2).unwrap()),
        u32::from(env.cpu.read_u16(pointer + base).unwrap()),
        u32::from(env.cpu.read_u16(pointer + base + 4).unwrap()),
        u32::from(env.cpu.read_u8(pointer + base + 6).unwrap()),
    ];
    let condition = u64::from(env.arg(2)) | (u64::from(env.arg(3)) << 32);
    let actual = match identity(env) {
        Ok(identity) => identity.version,
        Err(code) => return fail(env, code),
    };
    match matches(actual, requested, env.arg(1), condition) {
        Ok(true) => Outcome::Return(1),
        Ok(false) => fail(env, 1150),
        Err(code) => fail(env, code),
    }
}
pub(super) fn register(registry: &mut Registry) {
    stdcall(registry, super::K32, "GetVersion", 0, get_version);
    stdcall(registry, super::K32, "GetVersionExA", 1, |e| {
        get_version_ex(e, false)
    });
    stdcall(registry, super::K32, "GetVersionExW", 1, |e| {
        get_version_ex(e, true)
    });
    stdcall(registry, "ntdll.dll", "RtlGetVersion", 1, rtl_get_version);
    stdcall(registry, super::K32, "GetProductInfo", 5, get_product_info);
    stdcall(
        registry,
        super::K32,
        "GetCurrentPackageId",
        2,
        get_current_package_id,
    );
    stdcall(
        registry,
        super::K32,
        "VerSetConditionMask",
        4,
        condition_mask,
    );
    stdcall(registry, super::K32, "VerifyVersionInfoA", 4, |e| {
        verify(e, false)
    });
    stdcall(registry, super::K32, "VerifyVersionInfoW", 4, |e| {
        verify(e, true)
    });
}

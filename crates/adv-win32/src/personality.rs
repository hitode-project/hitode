//! The Windows identity a guest observes (docs/personality-contracts.md).
//! One profile generates every version fact. As on native Windows, the PEB
//! carries the version at runtime and the version APIs read it back, so a
//! patched PEB and the APIs cannot disagree.
use adv_cpu::backend::{Guest, GuestExt, MemFault};
use adv_machine::layout::{PEB_ADDR, peb};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Profile {
    pub major: u32,
    pub minor: u32,
    pub build: u16,
    pub platform_id: u32,
    /// (major, minor), as OSVERSIONINFOEX wServicePack{Major,Minor}.
    pub service_pack: (u16, u16),
    /// OSVERSIONINFO szCSDVersion; at most 127 UTF-16 units.
    pub csd_version: &'static str,
    /// Native reads these two from KUSER_SHARED_DATA, which is not mapped.
    pub suite_mask: u16,
    pub product_type: u8,
    /// GetProductInfo edition for any queried version.
    pub edition: u32,
}

/// The target of 32-bit ADV compatibility paths, and the existing identity.
pub const WIN7_SP1: Profile = Profile {
    major: 6,
    minor: 1,
    build: 7601,
    platform_id: 2, // VER_PLATFORM_WIN32_NT
    service_pack: (1, 0),
    csd_version: "Service Pack 1",
    suite_mask: 0,
    product_type: 1, // VER_NT_WORKSTATION
    edition: 1,      // PRODUCT_ULTIMATE
};

/// Write the profile's PEB fields, including the CSDVersion string that
/// RtlGetVersion copies from.
pub(crate) fn install_peb<C: Guest>(cpu: &mut C, profile: &Profile) -> Result<(), MemFault> {
    let csd: Vec<u16> = profile.csd_version.encode_utf16().collect();
    assert!(csd.len() < 128, "CSD version exceeds OSVERSIONINFO");
    cpu.write_u32(PEB_ADDR + peb::OS_MAJOR_VERSION, profile.major)?;
    cpu.write_u32(PEB_ADDR + peb::OS_MINOR_VERSION, profile.minor)?;
    cpu.write(
        PEB_ADDR + peb::OS_BUILD_NUMBER,
        &profile.build.to_le_bytes(),
    )?;
    let (major, minor) = profile.service_pack;
    cpu.write(
        PEB_ADDR + peb::OS_CSD_VERSION,
        &((major << 8) | minor).to_le_bytes(),
    )?;
    cpu.write_u32(PEB_ADDR + peb::OS_PLATFORM_ID, profile.platform_id)?;
    let bytes: Vec<u8> = csd.iter().flat_map(|unit| unit.to_le_bytes()).collect();
    let length = bytes.len() as u32;
    cpu.write(PEB_ADDR + peb::CSD_BUFFER, &bytes)?;
    cpu.write(PEB_ADDR + peb::CSD_BUFFER + length, &[0, 0])?;
    // UNICODE_STRING { Length, MaximumLength, Buffer }.
    cpu.write_u32(PEB_ADDR + peb::CSD_VERSION, length | ((length + 2) << 16))?;
    cpu.write_u32(PEB_ADDR + peb::CSD_VERSION + 4, PEB_ADDR + peb::CSD_BUFFER)
}

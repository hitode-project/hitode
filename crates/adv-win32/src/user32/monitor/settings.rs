//! DEVMODE A/W views of the one virtual primary display, with checked writes.
use super::{PRIMARY_DEVICE_NAME, fail};
use adv_abi::{AnsiStr, Env, GuestPtr, Registry, WideStr, guest::check_range, win32};
use adv_cpu::backend::{GuestExt, MemFault, Prot};

fn settings(
    env: &mut Env<'_>,
    device: String,
    mode: u32,
    output: u32,
    flags: u32,
    wide: bool,
) -> i32 {
    if !device.is_empty() && !device.eq_ignore_ascii_case(PRIMARY_DEVICE_NAME) {
        return 0;
    }
    // Both EDS flags describe the same sole orientation and virtual mode.
    if flags & !6 != 0 {
        adv_abi::warn_unavailable(
            env,
            "EnumDisplaySettingsEx",
            "unknown display enumeration flags",
        );
        return fail(env, 50);
    }
    if !matches!(mode, 0 | 0xffff_ffff | 0xffff_fffe) {
        return 0;
    }
    let (size, size_at, fields, bits) = if wide {
        (220, 68, 72, 168)
    } else {
        (156, 36, 40, 104)
    };
    if output == 0 || check_range(env.cpu, output, size, Prot::WRITE).is_err() {
        return fail(env, 87);
    }
    // Full SDK structures are the supported caller contract; driver-private
    // bytes are neither advertised nor copied from the host display driver.
    if env.cpu.read_u16(output + size_at).ok() != Some(size as u16) {
        return fail(env, 87);
    }
    let (width, height) = super::super::user(env).screen;
    let mut bytes = vec![0u8; size as usize];
    let label = "adv-host virtual display";
    if wide {
        for (i, unit) in label.encode_utf16().enumerate() {
            bytes[i * 2..i * 2 + 2].copy_from_slice(&unit.to_le_bytes());
        }
    } else {
        bytes[..label.len()].copy_from_slice(label.as_bytes());
    }
    let size_at = size_at as usize;
    bytes[size_at - 4..size_at - 2].copy_from_slice(&0x0401u16.to_le_bytes());
    bytes[size_at..size_at + 2].copy_from_slice(&(size as u16).to_le_bytes());
    bytes[fields as usize..fields as usize + 4].copy_from_slice(&0x207c_00a0u32.to_le_bytes());
    for (offset, value) in [
        (bits, 32),
        (bits + 4, width as u32),
        (bits + 8, height as u32),
        (bits + 16, 60),
    ] {
        bytes[offset as usize..offset as usize + 4].copy_from_slice(&value.to_le_bytes());
    }
    if env.cpu.write(output, &bytes).is_err() {
        return fail(env, 87);
    }
    1
}
fn ansi(env: &Env<'_>, device: AnsiStr) -> Result<String, MemFault> {
    if device.0.is_null() {
        Ok(String::new())
    } else {
        Ok(env
            .ext
            .get::<crate::Win32>()
            .codec
            .decode(&device.read(env.cpu, 260)?))
    }
}
fn wide(env: &Env<'_>, device: WideStr) -> Result<String, MemFault> {
    if device.0.is_null() {
        Ok(String::new())
    } else {
        Ok(String::from_utf16_lossy(&device.read(env.cpu, 260)?))
    }
}
#[win32(user32,name="EnumDisplaySettingsA",level=state)]
fn enum_a(
    env: &mut Env<'_>,
    device: AnsiStr,
    mode: u32,
    output: GuestPtr<()>,
) -> Result<i32, MemFault> {
    let device = ansi(env, device)?;
    Ok(settings(env, device, mode, output.addr(), 0, false))
}
#[win32(user32,name="EnumDisplaySettingsW",level=state)]
fn enum_w(
    env: &mut Env<'_>,
    device: WideStr,
    mode: u32,
    output: GuestPtr<()>,
) -> Result<i32, MemFault> {
    let device = wide(env, device)?;
    Ok(settings(env, device, mode, output.addr(), 0, true))
}
#[win32(user32,name="EnumDisplaySettingsExA",level=state)]
fn enum_ex_a(
    env: &mut Env<'_>,
    device: AnsiStr,
    mode: u32,
    output: GuestPtr<()>,
    flags: u32,
) -> Result<i32, MemFault> {
    let device = ansi(env, device)?;
    Ok(settings(env, device, mode, output.addr(), flags, false))
}
#[win32(user32,name="EnumDisplaySettingsExW",level=state)]
fn enum_ex_w(
    env: &mut Env<'_>,
    device: WideStr,
    mode: u32,
    output: GuestPtr<()>,
    flags: u32,
) -> Result<i32, MemFault> {
    let device = wide(env, device)?;
    Ok(settings(env, device, mode, output.addr(), flags, true))
}
pub(super) fn register(reg: &mut Registry) {
    for api in [enum_a_ABI, enum_w_ABI, enum_ex_a_ABI, enum_ex_w_ABI] {
        reg.register(api);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::misc::test_support::{DATA, Fixture};
    use adv_cpu::backend::Guest;
    #[test]
    fn a_w_display_modes_preserve_tails_and_reject_bad_buffers_without_partial_output() {
        let mut f = Fixture::new("display-modes");
        for (api, size, size_at, bits) in [
            ("EnumDisplaySettingsA", 156, 36, 104),
            ("EnumDisplaySettingsExA", 156, 36, 104),
            ("EnumDisplaySettingsW", 220, 68, 168),
            ("EnumDisplaySettingsExW", 220, 68, 168),
        ] {
            f.m.cpu.write(DATA, &[0xcc; 256]).unwrap();
            f.m.cpu
                .write(DATA + size_at, &(size as u16).to_le_bytes())
                .unwrap();
            let mut args = vec![0, u32::MAX, DATA];
            if api.contains("Ex") {
                args.push(6);
            }
            assert_eq!(f.call("user32.dll", api, &args), 1);
            assert_eq!(f.m.cpu.read_u32(DATA + bits).unwrap(), 32);
            assert_eq!(f.m.cpu.read_u32(DATA + bits + 4).unwrap(), 1920);
            assert_eq!(f.m.cpu.read_u32(DATA + bits + 8).unwrap(), 1080);
            let mut tail = vec![0; 256 - size as usize];
            f.m.cpu.read(DATA + size, &mut tail).unwrap();
            assert!(tail.iter().all(|&b| b == 0xcc));
            args[1] = 1;
            assert_eq!(f.call("user32.dll", api, &args), 0);
            args[1] = u32::MAX;
            f.m.cpu.write(DATA, &[0xcc; 256]).unwrap();
            assert_eq!(f.call("user32.dll", api, &args), 0);
            let mut actual = [0; 256];
            f.m.cpu.read(DATA, &mut actual).unwrap();
            assert_eq!(actual, [0xcc; 256]);
        }
        f.cleanup();
    }
}

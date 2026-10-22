//! Local SID, privilege-name and security-descriptor queries over guest identity.
use crate::{Win32, util::set_last_error};
use adv_abi::{AnsiStr, GuestPtr, Registry, guest::check_range};
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::{Env, Outcome};
fn fail(env: &mut Env<'_>, error: u32) -> Outcome {
    set_last_error(env, error);
    Outcome::Return(0)
}
pub(super) fn sid(env: &Env<'_>, address: u32) -> Option<Vec<u8>> {
    if check_range(env.cpu, address, 8, Prot::READ).is_err() {
        return None;
    }
    let mut header = [0; 8];
    env.cpu.read(address, &mut header).ok()?;
    if header[0] != 1 || header[1] > 15 {
        return None;
    }
    let mut bytes = vec![0; 8 + 4 * header[1] as usize];
    check_range(env.cpu, address, bytes.len() as u32, Prot::READ).ok()?;
    env.cpu.read(address, &mut bytes).ok()?;
    Some(bytes)
}
fn text(env: &Env<'_>, address: u32) -> Result<String, u32> {
    if address == 0 {
        return Ok(String::new());
    }
    let bytes = AnsiStr(GuestPtr::new(address))
        .read(env.cpu, 32768)
        .map_err(|_| 998u32)?;
    Ok(env.ext.get::<Win32>().codec.decode(&bytes))
}
fn local(env: &Env<'_>, address: u32) -> Result<(), u32> {
    let name = text(env, address)?;
    let name = name.trim_start_matches('\\');
    if name.is_empty() || name == "." || name.eq_ignore_ascii_case(adv_os::identity::COMPUTER_NAME)
    {
        Ok(())
    } else {
        Err(1722)
    }
}
pub(super) fn privilege(name: &str) -> Option<u32> {
    const NAMES: [&str; 34] = [
        "SeCreateTokenPrivilege",
        "SeAssignPrimaryTokenPrivilege",
        "SeLockMemoryPrivilege",
        "SeIncreaseQuotaPrivilege",
        "SeMachineAccountPrivilege",
        "SeTcbPrivilege",
        "SeSecurityPrivilege",
        "SeTakeOwnershipPrivilege",
        "SeLoadDriverPrivilege",
        "SeSystemProfilePrivilege",
        "SeSystemtimePrivilege",
        "SeProfileSingleProcessPrivilege",
        "SeIncreaseBasePriorityPrivilege",
        "SeCreatePagefilePrivilege",
        "SeCreatePermanentPrivilege",
        "SeBackupPrivilege",
        "SeRestorePrivilege",
        "SeShutdownPrivilege",
        "SeDebugPrivilege",
        "SeAuditPrivilege",
        "SeSystemEnvironmentPrivilege",
        "SeChangeNotifyPrivilege",
        "SeRemoteShutdownPrivilege",
        "SeUndockPrivilege",
        "SeSyncAgentPrivilege",
        "SeEnableDelegationPrivilege",
        "SeManageVolumePrivilege",
        "SeImpersonatePrivilege",
        "SeCreateGlobalPrivilege",
        "SeTrustedCredManAccessPrivilege",
        "SeRelabelPrivilege",
        "SeIncreaseWorkingSetPrivilege",
        "SeTimeZonePrivilege",
        "SeCreateSymbolicLinkPrivilege",
    ];
    NAMES
        .iter()
        .position(|n| name.eq_ignore_ascii_case(n))
        .map(|i| i as u32 + 2)
}
fn privilege_value(env: &mut Env<'_>) -> Outcome {
    if let Err(error) = local(env, env.arg(0)) {
        return fail(env, error);
    }
    let name = match text(env, env.arg(1)) {
        Ok(text) => text,
        Err(error) => return fail(env, error),
    };
    let Some(value) = privilege(&name) else {
        return fail(env, 1313);
    };
    if check_range(env.cpu, env.arg(2), 8, Prot::WRITE).is_err() {
        return fail(env, 998);
    }
    let _ = env.cpu.write(env.arg(2), &u64::from(value).to_le_bytes());
    Outcome::Return(1)
}
fn owner(env: &mut Env<'_>) -> Outcome {
    let descriptor = env.arg(0);
    let output = env.arg(1);
    let defaulted = env.arg(2);
    if check_range(env.cpu, descriptor, 20, Prot::READ).is_err() {
        return fail(env, 1338);
    }
    if env.cpu.read_u8(descriptor).ok() != Some(1) {
        return fail(env, 1305);
    }
    if check_range(env.cpu, output, 4, Prot::WRITE).is_err()
        || check_range(env.cpu, defaulted, 4, Prot::WRITE).is_err()
    {
        return fail(env, 998);
    }
    let flags = env.cpu.read_u16(descriptor + 2).unwrap();
    let raw = env.cpu.read_u32(descriptor + 4).unwrap();
    let owner = if raw != 0 && flags & 0x8000 != 0 {
        let Some(address) = descriptor.checked_add(raw) else {
            return fail(env, 1338);
        };
        address
    } else {
        raw
    };
    let _ = env.cpu.write_u32(output, owner);
    let _ = env.cpu.write_u32(defaulted, u32::from(flags & 1 != 0));
    Outcome::Return(1)
}
fn account(env: &mut Env<'_>) -> Outcome {
    if let Err(error) = local(env, env.arg(0)) {
        return fail(env, error);
    }
    let Some(sid) = sid(env, env.arg(1)) else {
        return fail(env, 1337);
    };
    let (name, domain, usage) = if sid == adv_os::identity::current_user_sid() {
        (
            env.ext.get::<Win32>().cfg.os.user_name.clone(),
            adv_os::identity::COMPUTER_NAME,
            1,
        )
    } else {
        match adv_os::identity::sid_string(&sid).as_deref() {
            Some("S-1-1-0") => ("Everyone".into(), "", 5),
            Some("S-1-5-18") => ("SYSTEM".into(), "NT AUTHORITY", 5),
            Some("S-1-5-19") => ("LOCAL SERVICE".into(), "NT AUTHORITY", 5),
            Some("S-1-5-20") => ("NETWORK SERVICE".into(), "NT AUTHORITY", 5),
            Some("S-1-5-32-544") => ("Administrators".into(), "BUILTIN", 4),
            Some("S-1-5-32-545") => ("Users".into(), "BUILTIN", 4),
            _ => return fail(env, 1332),
        }
    };
    let sizes = [env.arg(3), env.arg(5)];
    if sizes
        .iter()
        .any(|&p| check_range(env.cpu, p, 4, Prot::RW).is_err())
    {
        return fail(env, 998);
    }
    let codec = env.ext.get::<Win32>().codec;
    let names = [codec.encode(&name), codec.encode(domain)];
    let capacity = [
        env.cpu.read_u32(sizes[0]).unwrap(),
        env.cpu.read_u32(sizes[1]).unwrap(),
    ];
    if capacity
        .iter()
        .zip(&names)
        .any(|(&cap, name)| cap <= name.len() as u32)
    {
        for (size, name) in sizes.into_iter().zip(&names) {
            let _ = env.cpu.write_u32(size, name.len() as u32 + 1);
        }
        return fail(env, 122);
    }
    let output = [env.arg(2), env.arg(4)];
    if output
        .iter()
        .zip(&names)
        .any(|(&p, name)| check_range(env.cpu, p, name.len() as u32 + 1, Prot::WRITE).is_err())
        || check_range(env.cpu, env.arg(6), 4, Prot::WRITE).is_err()
    {
        return fail(env, 998);
    }
    for ((output, size), name) in output.into_iter().zip(sizes).zip(names) {
        let mut bytes = name;
        let length = bytes.len() as u32;
        bytes.push(0);
        let _ = env.cpu.write(output, &bytes);
        let _ = env.cpu.write_u32(size, length);
    }
    let _ = env.cpu.write_u32(env.arg(6), usage);
    Outcome::Return(1)
}
pub(super) fn register(registry: &mut Registry) {
    for (name, count, handler) in [
        (
            "IsValidSid",
            1,
            (|env: &mut Env<'_>| Outcome::Return(u32::from(sid(env, env.arg(0)).is_some())))
                as adv_machine::Handler,
        ),
        ("LookupPrivilegeValueA", 3, privilege_value),
        ("GetSecurityDescriptorOwner", 3, owner),
        ("LookupAccountSidA", 7, account),
    ] {
        crate::stdcall(registry, "advapi32.dll", name, count, handler);
    }
}

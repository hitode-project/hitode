//! NT file entry points adapt to the same VFS handles used by kernel32.
use adv_abi::{Registry, guest::check_range};
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::{Env, Outcome};

fn open_file(env: &mut Env<'_>) -> Outcome {
    let (out, access, attrs, iosb, share, options) = (
        env.arg(0),
        env.arg(1),
        env.arg(2),
        env.arg(3),
        env.arg(4),
        env.arg(5),
    );
    let result = (|| -> Result<u32, u32> {
        for (pointer, length, permission) in [
            (out, 4, Prot::WRITE),
            (attrs, 24, Prot::READ),
            (iosb, 8, Prot::WRITE),
        ] {
            check_range(env.cpu, pointer, length, permission).map_err(|_| 0xc0000005u32)?;
        }
        if env.cpu.read_u32(attrs).map_err(|_| 0xc0000005u32)? != 24 || share & !7 != 0 {
            return Err(0xc000000d);
        }
        if access & !0x8012_0089 != 0 {
            adv_abi::warn_unavailable(
                env,
                "ntdll.dll!NtOpenFile",
                "only read access is modeled by this NT adapter",
            );
            return Err(0xc0000002);
        }
        if options & 0x20 != 0 && access & 0x100000 == 0 {
            return Err(0xc0000022);
        }
        let object_flags = env.cpu.read_u32(attrs + 12).map_err(|_| 0xc0000005u32)?;
        if object_flags & !0x40 != 0
            || env.cpu.read_u32(attrs + 16).map_err(|_| 0xc0000005u32)? != 0
            || env.cpu.read_u32(attrs + 20).map_err(|_| 0xc0000005u32)? != 0
        {
            adv_abi::warn_unavailable(
                env,
                "ntdll.dll!NtOpenFile",
                "object inheritance, security descriptors and QoS are not modeled",
            );
            return Err(0xc0000002);
        }
        if env.cpu.read_u32(attrs + 4).map_err(|_| 0xc0000005u32)? != 0 || options & !0x866 != 0 {
            adv_abi::warn_unavailable(
                env,
                "ntdll.dll!NtOpenFile",
                "relative directory handles or file open options are not modeled",
            );
            return Err(0xc0000002);
        }
        let name = env.cpu.read_u32(attrs + 8).map_err(|_| 0xc0000005u32)?;
        check_range(env.cpu, name, 8, Prot::READ).map_err(|_| 0xc0000005u32)?;
        let length = env.cpu.read_u16(name).map_err(|_| 0xc0000005u32)? as u32;
        let maximum = env.cpu.read_u16(name + 2).map_err(|_| 0xc0000005u32)? as u32;
        if length == 0 || length & 1 != 0 || length > maximum {
            return Err(0xc0000033);
        }
        let buffer = env.cpu.read_u32(name + 4).map_err(|_| 0xc0000005u32)?;
        check_range(env.cpu, buffer, length, Prot::READ).map_err(|_| 0xc0000005u32)?;
        let mut bytes = vec![0; length as usize];
        env.cpu
            .read(buffer, &mut bytes)
            .map_err(|_| 0xc0000005u32)?;
        let units: Vec<_> = bytes
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect();
        let path = String::from_utf16_lossy(&units);
        let path = path
            .strip_prefix(r"\??\")
            .or_else(|| path.strip_prefix(r"\DosDevices\"))
            .unwrap_or(&path);
        // An NT name must be absolute; do not silently interpret an object-manager
        // path as a relative path under the game directory.
        let raw = path.as_bytes();
        if raw.len() < 3
            || !raw[0].is_ascii_alphabetic()
            || raw[1] != b':'
            || !matches!(raw[2], b'\\' | b'/')
        {
            return Err(0xc0000033);
        }
        let last = crate::util::last_error(env);
        let mapped_access = access | if access & 1 != 0 { 0x80000000 } else { 0 };
        let handle = crate::kernel32::open_with_share(env, path, mapped_access, 3, share);
        let error = crate::util::last_error(env);
        crate::util::set_last_error(env, last);
        if handle == u32::MAX {
            return Err(match error {
                2 => 0xc0000034,
                3 => 0xc000003a,
                8 => 0xc0000017,
                32 => 0xc0000043,
                _ => 0xc0000022,
            });
        }
        if env.cpu.write_u32(out, handle).is_err() {
            env.sys.objects.close(handle);
            return Err(0xc0000005);
        }
        Ok(handle)
    })();
    let status = result.as_ref().err().copied().unwrap_or(0);
    if check_range(env.cpu, iosb, 8, Prot::WRITE).is_ok() {
        let _ = env.cpu.write_u32(iosb, status);
        let _ = env
            .cpu
            .write_u32(iosb + 4, if result.is_ok() { 1 } else { 0 });
    }
    Outcome::Return(status)
}

pub(super) fn register(reg: &mut Registry) {
    crate::stdcall(reg, "ntdll.dll", "NtOpenFile", 6, open_file);
    crate::stdcall(reg, "ntdll.dll", "ZwOpenFile", 6, open_file);
}

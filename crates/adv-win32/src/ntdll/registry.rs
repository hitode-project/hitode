//! NT key opens share the private Win32 registry and its Objects namespace.
use adv_abi::{GuestPtr, Registry, guest::check_range, warn_unavailable, win32};
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::Env;
use adv_os::registry::{self as store, Key, VirtualRegistry};

const ACCESS_VIOLATION: u32 = 0xc000_0005;
const INVALID_HANDLE: u32 = 0xc000_0008;
const INVALID_PARAMETER: u32 = 0xc000_000d;
const NAME_INVALID: u32 = 0xc000_0033;
const NAME_NOT_FOUND: u32 = 0xc000_0034;
const PATH_NOT_FOUND: u32 = 0xc000_003a;
const PATH_SYNTAX_BAD: u32 = 0xc000_003b;
const NOT_SUPPORTED: u32 = 0xc000_00bb;

fn status(error: u32) -> u32 {
    match error {
        store::NOT_FOUND => NAME_NOT_FOUND,
        store::ACCESS_DENIED => 0xc000_0022,
        store::INVALID_HANDLE => INVALID_HANDLE,
        store::NO_MEMORY => 0xc000_0017,
        store::INVALID_DATA => 0xc000_014c, // STATUS_REGISTRY_CORRUPT
        store::NOT_SUPPORTED => NOT_SUPPORTED,
        store::INVALID_PARAMETER => INVALID_PARAMETER,
        store::NOACCESS => ACCESS_VIOLATION,
        32 => 0xc000_0043,   // STATUS_SHARING_VIOLATION
        1018 => 0xc000_017c, // STATUS_KEY_DELETED
        _ => 0xc000_014d,    // STATUS_REGISTRY_IO_FAILED
    }
}

fn counted_name(env: &Env<'_>, descriptor: u32) -> Result<Vec<u16>, u32> {
    check_range(env.cpu, descriptor, 8, Prot::READ).map_err(|_| ACCESS_VIOLATION)?;
    let length = env.cpu.read_u16(descriptor).map_err(|_| ACCESS_VIOLATION)?;
    // MaximumLength is storage capacity, not the extent of this input name.
    // Native accepts input descriptors whose MaximumLength is below Length.
    if length & 1 != 0 {
        return Err(NAME_INVALID);
    }
    let buffer = env
        .cpu
        .read_u32(descriptor + 4)
        .map_err(|_| ACCESS_VIOLATION)?;
    check_range(env.cpu, buffer, u32::from(length), Prot::READ).map_err(|_| ACCESS_VIOLATION)?;
    let mut bytes = vec![0; usize::from(length)];
    if length != 0 {
        env.cpu
            .read(buffer, &mut bytes)
            .map_err(|_| ACCESS_VIOLATION)?;
    }
    let units: Vec<_> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|bytes| u16::from_le_bytes(*bytes))
        .collect();
    // Keep counted NULs and raw UTF-16 units; neither NUL termination nor lossy
    // decoding may turn the requested name into another existing key.
    Ok(units)
}

fn absolute_key(name: &[u16]) -> Result<Key, u32> {
    if name.first() != Some(&92) {
        return Err(PATH_SYNTAX_BAD);
    }
    let parts: Vec<_> = name[1..]
        .split(|&unit| unit == 92)
        .filter(|part| !part.is_empty())
        .collect();
    let matches = |part: &[u16], expected: &str| {
        store::fold(part) == expected.encode_utf16().collect::<Vec<_>>()
    };
    if parts.len() < 2 || !matches(parts[0], "REGISTRY") {
        return Err(PATH_NOT_FOUND);
    }
    let root = if matches(parts[1], "MACHINE") {
        0x8000_0002
    } else if matches(parts[1], "USER") {
        0x8000_0003
    } else {
        return Err(PATH_NOT_FOUND);
    };
    let mut suffix = Vec::new();
    for part in &parts[2..] {
        if !suffix.is_empty() {
            suffix.push(92);
        }
        suffix.extend_from_slice(part);
    }
    store::child_key(
        &Key {
            root,
            path: Vec::new(),
        },
        &suffix,
    )
    .map(|(key, _)| current_user_key(key))
    .map_err(|_| NAME_INVALID)
}

fn current_user_key(mut key: Key) -> Key {
    if key.root == 0x8000_0003 {
        let end = key
            .path
            .iter()
            .position(|&unit| unit == 92)
            .unwrap_or(key.path.len());
        let sid: Vec<_> = adv_os::identity::current_user_sid_string()
            .encode_utf16()
            .collect();
        if key.path[..end] == sid {
            key.root = 0x8000_0001;
            key.path = key.path.get(end + 1..).unwrap_or_default().to_vec();
        }
    }
    key
}

#[win32(ntdll, name = "NtOpenKey", level = implement)]
fn open(env: &mut Env<'_>, output: GuestPtr<u32>, access: u32, attributes: GuestPtr<()>) -> i32 {
    let result = (|| -> Result<(), u32> {
        check_range(env.cpu, output.addr(), 4, Prot::WRITE).map_err(|_| ACCESS_VIOLATION)?;
        let attributes = attributes.addr();
        check_range(env.cpu, attributes, 24, Prot::READ).map_err(|_| ACCESS_VIOLATION)?;
        if env.cpu.read_u32(attributes).map_err(|_| ACCESS_VIOLATION)? != 24 {
            return Err(INVALID_PARAMETER);
        }
        // Native preserves the output for a bad OBJECT_ATTRIBUTES length,
        // then clears it before resolving the key or checking its access.
        env.cpu
            .write_u32(output.addr(), 0)
            .map_err(|_| ACCESS_VIOLATION)?;
        let root = env
            .cpu
            .read_u32(attributes + 4)
            .map_err(|_| ACCESS_VIOLATION)?;
        let descriptor = env
            .cpu
            .read_u32(attributes + 8)
            .map_err(|_| ACCESS_VIOLATION)?;
        let flags = env
            .cpu
            .read_u32(attributes + 12)
            .map_err(|_| ACCESS_VIOLATION)?;
        if flags & !0x40 != 0 {
            warn_unavailable(
                env,
                "ntdll.dll!NtOpenKey",
                "object attributes outside case-insensitive lookup are unavailable",
            );
            return Err(NOT_SUPPORTED);
        }
        // Opening a key ignores security information in OBJECT_ATTRIBUTES.
        let access = crate::advapi32::desired_access(access).map_err(|error| {
            if error == store::NOT_SUPPORTED {
                warn_unavailable(
                    env,
                    "ntdll.dll!NtOpenKey",
                    "the explicit 64-bit registry view is unavailable",
                );
            }
            status(error)
        })?;
        let name = counted_name(env, descriptor)?;
        let key = if root == 0 {
            absolute_key(&name)?
        } else {
            if root & 0x8000_0000 != 0 {
                return Err(INVALID_HANDLE);
            }
            let parent = crate::advapi32::opened(env, root).map_err(status)?;
            if name.first() == Some(&92) {
                return Err(PATH_SYNTAX_BAD);
            }
            store::child_key(&parent.key, &name)
                .map(|(key, _)| current_user_key(key))
                .map_err(|_| NAME_INVALID)?
        };
        crate::advapi32::state(env);
        let registry = env.ext.get_mut::<VirtualRegistry>();
        if let Some(error) = registry.load_error {
            return Err(status(error));
        }
        if !registry.keys.contains_key(&key) {
            return Err(NAME_NOT_FOUND);
        }
        // The provider's default ACL is shared with RegOpenKeyEx; the parent
        // handle's access mask does not define the child's ACL.
        let handle = registry
            .allocate(&mut env.sys.objects, key, access)
            .map_err(status)?;
        if env.cpu.write_u32(output.addr(), handle).is_err() {
            let _ = registry.close(&mut env.sys.objects, handle);
            return Err(ACCESS_VIOLATION);
        }
        Ok(())
    })();
    result.err().unwrap_or(0) as i32
}

pub(super) fn register(registry: &mut Registry) {
    registry.register(open_ABI);
    registry.register(adv_abi::Api {
        name: "ZwOpenKey",
        ..open_ABI
    });
}

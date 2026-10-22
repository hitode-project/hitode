//! Privilege changes and token copies retain the existing typed handle owner.
use super::*;
use adv_machine::Outcome;
fn result(value: i32) -> Outcome {
    Outcome::Return(value as u32)
}
fn adjust(env: &mut Env<'_>) -> Outcome {
    let handle = env.arg(0);
    let all = env.arg(1) != 0;
    let input = env.arg(2);
    let capacity = env.arg(3);
    let previous = env.arg(4);
    let required = env.arg(5);
    let Some(token) = env.sys.objects.host::<Token>(handle) else {
        return result(fail(env, 6));
    };
    if token.access & 0x20 == 0 || previous != 0 && token.access & QUERY == 0 {
        return result(fail(env, 5));
    }
    let data = token.data.clone();
    let original = data.borrow().clone();
    let mut next = original.privileges.clone();
    let mut changed = BTreeMap::new();
    let mut assigned = true;
    if all {
        for (&luid, attrs) in next.iter_mut() {
            if *attrs & 2 != 0 {
                changed.insert(luid, *attrs);
                *attrs &= !2;
            }
        }
    } else {
        let count = match env.cpu.read_u32(input) {
            Ok(count) => count,
            Err(_) => return result(fail(env, 998)),
        };
        let Some(length) = count.checked_mul(12).and_then(|n| n.checked_add(4)) else {
            return result(fail(env, 87));
        };
        if check_range(env.cpu, input, length, Prot::READ).is_err() {
            return result(fail(env, 998));
        }
        for i in 0..count {
            let at = input + 4 + i * 12;
            let mut bytes = [0; 12];
            if env.cpu.read(at, &mut bytes).is_err() {
                return result(fail(env, 998));
            }
            let luid = u64::from_le_bytes(bytes[..8].try_into().unwrap());
            let attributes = u32::from_le_bytes(bytes[8..].try_into().unwrap());
            let Some(old) = next.get(&luid).copied() else {
                assigned = false;
                continue;
            };
            if attributes & 4 != 0 {
                next.remove(&luid);
                changed.remove(&luid);
            } else {
                let value = (old & !2) | (attributes & 2);
                if value != old {
                    changed.entry(luid).or_insert(original.privileges[&luid]);
                    next.insert(luid, value);
                }
            }
        }
    }
    let length = 4 + 12 * changed.len() as u32;
    if previous != 0 {
        if check_range(env.cpu, required, 4, Prot::WRITE).is_err() {
            return result(fail(env, 998));
        }
        let _ = env.cpu.write_u32(required, length);
        if capacity < length {
            return result(fail(env, 122));
        }
        if check_range(env.cpu, previous, length, Prot::WRITE).is_err() {
            return result(fail(env, 998));
        }
        let mut bytes = Vec::with_capacity(length as usize);
        bytes.extend_from_slice(&(changed.len() as u32).to_le_bytes());
        for (luid, attrs) in changed {
            bytes.extend_from_slice(&luid.to_le_bytes());
            bytes.extend_from_slice(&attrs.to_le_bytes());
        }
        let _ = env.cpu.write(previous, &bytes);
    }
    data.borrow_mut().privileges = next;
    set_last_error(env, if assigned { 0 } else { 1300 });
    Outcome::Return(1)
}
fn duplicate_ex(env: &mut Env<'_>) -> Outcome {
    let Some(token) = env.sys.objects.host::<Token>(env.arg(0)) else {
        return result(fail(env, 6));
    };
    if token.access & 2 == 0 {
        return result(fail(env, 5));
    }
    let process = token._process.clone();
    let original = token.data.borrow().clone();
    let source_access = token.access;
    let level = env.arg(3);
    let kind = env.arg(4);
    if level > 3 || !matches!(kind, 1 | 2) {
        return result(fail(env, 87));
    }
    let desired = if env.arg(1) == 0 {
        source_access
    } else {
        let Some(access) = access(env.arg(1)) else {
            return result(fail(env, 5));
        };
        access
    };
    let attributes = env.arg(2);
    let mut inherit = false;
    if attributes != 0 {
        if check_range(env.cpu, attributes, 12, Prot::READ).is_err() {
            return result(fail(env, 998));
        }
        if env.cpu.read_u32(attributes).ok() != Some(12) {
            return result(fail(env, 87));
        }
        if env.cpu.read_u32(attributes + 4).ok() != Some(0) {
            warn_unavailable(
                env,
                "advapi32.dll!DuplicateTokenEx",
                "custom token security descriptors are unavailable",
            );
            return result(fail(env, 50));
        }
        inherit = env.cpu.read_u32(attributes + 8).ok() != Some(0);
    }
    let output = env.arg(5);
    if check_range(env.cpu, output, 4, Prot::WRITE).is_err() {
        return result(fail(env, 998));
    }
    let data = TokenData {
        kind,
        level,
        ..original
    };
    let handle = env.sys.objects.insert_host(Token {
        _process: process,
        access: desired,
        data: Rc::new(RefCell::new(data)),
    });
    if handle == 0 {
        return result(fail(env, 8));
    }
    env.sys
        .objects
        .set_handle_flags(handle, 1, u32::from(inherit));
    let _ = env.cpu.write_u32(output, handle);
    Outcome::Return(1)
}
fn impersonate(env: &mut Env<'_>) -> Outcome {
    let Some(token) = env.sys.objects.host::<Token>(env.arg(0)) else {
        return result(fail(env, 6));
    };
    let mut data = token.data.borrow().clone();
    let required = if data.kind == 1 { QUERY | 2 } else { QUERY | 4 };
    if token.access & required != required {
        return result(fail(env, 5));
    }
    if data.kind == 2 && data.level < 2 {
        return result(fail(env, 1346));
    }
    data.kind = 2;
    data.level = 2;
    let completion = Arc::downgrade(&env.ctx().completion);
    let context = env.context;
    let owner = env.ext.get_or_default::<TokenOwner>();
    owner.threads.retain(|_, (thread, _)| {
        thread
            .upgrade()
            .is_some_and(|thread| thread.exit_code().is_none())
    });
    owner
        .threads
        .insert(context, (completion, Rc::new(RefCell::new(data))));
    Outcome::Return(1)
}
fn create_process(env: &mut Env<'_>) -> Outcome {
    let Some(token) = env.sys.objects.host::<Token>(env.arg(0)) else {
        return result(fail(env, 6));
    };
    if token.access & 0xb != 0xb {
        return result(fail(env, 5));
    }
    // No token can grant SeImpersonatePrivilege to the ordinary calling user.
    // AdjustTokenPrivileges only changes privileges already present in a token.
    result(fail(env, 1314))
}
pub(super) fn register(registry: &mut Registry) {
    for (name, count, handler) in [
        ("AdjustTokenPrivileges", 6, adjust as adv_machine::Handler),
        ("DuplicateTokenEx", 6, duplicate_ex),
        ("ImpersonateLoggedOnUser", 1, impersonate),
        ("RevertToSelf", 0, |env| {
            env.ext
                .get_or_default::<TokenOwner>()
                .threads
                .remove(&env.context);
            Outcome::Return(1)
        }),
        ("CreateProcessWithTokenW", 9, create_process),
    ] {
        crate::stdcall(registry, "advapi32.dll", name, count, handler);
    }
}

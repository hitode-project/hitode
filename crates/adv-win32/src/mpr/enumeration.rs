//! Connected and remembered-resource enumerations are local configuration. The
//! offline profile owns real empty enumerators without requiring a transport.
use super::*;
use adv_machine::objects::Namespace;
struct Enumerations(Namespace);
struct Enumerator {
    _scope: u32,
    _kind: u32,
}
fn namespace(env: &mut Env<'_>) -> Option<Namespace> {
    if let Some(state) = env.ext.try_get::<Enumerations>() {
        return Some(state.0);
    }
    let namespace = env.sys.objects.new_namespace_in(0x0f00_0004, 0x0fff_fffc)?;
    env.ext.insert(Enumerations(namespace));
    Some(namespace)
}
pub(super) fn open(env: &mut Env<'_>, scope: u32, kind: u32, output: u32) -> Outcome {
    let Some(namespace) = namespace(env) else {
        return Outcome::Return(8);
    };
    let Some(handle) = env.sys.objects.insert_host_in(
        namespace,
        Enumerator {
            _scope: scope,
            _kind: kind,
        },
    ) else {
        return Outcome::Return(8);
    };
    if env.cpu.write_u32(output, handle).is_err() {
        env.sys.objects.close_alias(namespace, handle);
        return Outcome::Return(INVALID_ADDRESS);
    }
    Outcome::Return(0)
}
fn valid(env: &Env<'_>, handle: u32) -> Option<Namespace> {
    let namespace = env.ext.try_get::<Enumerations>()?.0;
    env.sys
        .objects
        .lookup_alias::<Enumerator>(namespace, handle)?;
    Some(namespace)
}
pub(super) fn close(env: &mut Env<'_>) -> Outcome {
    let handle = env.arg(0);
    let Some(namespace) = valid(env, handle) else {
        return Outcome::Return(6);
    };
    env.sys.objects.close_alias(namespace, handle);
    Outcome::Return(0)
}
pub(super) fn next(env: &mut Env<'_>) -> Outcome {
    if valid(env, env.arg(0)).is_none() {
        return Outcome::Return(6);
    }
    let count = env.arg(1);
    let length = env.arg(3);
    if check_range(env.cpu, count, 4, Prot::READ | Prot::WRITE).is_err()
        || check_range(env.cpu, length, 4, Prot::READ | Prot::WRITE).is_err()
    {
        return Outcome::Return(INVALID_ADDRESS);
    }
    let _ = env.cpu.write_u32(count, 0);
    Outcome::Return(259)
}

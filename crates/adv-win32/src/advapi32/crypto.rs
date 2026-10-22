//! CryptoAPI contexts and streaming hashes. CSP handles live in Objects' own
//! alias namespace, distinct from CloseHandle's kernel handles.
mod cipher;
mod digest;
mod hash;
mod hmac;
mod key;
mod provider;

use crate::util::set_last_error;
use adv_abi::{Registry, guest::check_range};
use adv_cpu::backend::{GuestExt, Prot};
use adv_machine::{
    Env,
    objects::{Identity, Namespace},
};
use std::sync::Arc;

const BAD_HASH: u32 = 0x80090002;
const BAD_ALGID: u32 = 0x80090008;
const BAD_FLAGS: u32 = 0x80090009;
const BAD_TYPE: u32 = 0x8009000a;
const BAD_HASH_STATE: u32 = 0x8009000c;
const BAD_UID: u32 = 0x80090001;
const NO_MEMORY: u32 = 0x8009000e;

struct Store(Namespace);
struct Provider {
    kind: u32,
    name: &'static str,
    machine: bool,
}
impl Provider {
    fn base(&self) -> bool {
        self.name == "Microsoft Base Cryptographic Provider v1.0"
    }
    fn rc_default(&self) -> u32 {
        if self.base() { 40 } else { 128 }
    }
    fn rc_maximum(&self) -> u32 {
        if self.base() { 56 } else { 128 }
    }
}
struct Context {
    provider: Arc<Provider>,
    references: u32,
}
#[derive(Clone)]
struct Hash {
    _provider: Arc<Provider>,
    algorithm: u32,
    digest: digest::Digest,
    finished: Option<Vec<u8>>,
    hmac: Option<hmac::Hmac>,
    started: bool,
}
impl Hash {
    fn value(&self) -> Vec<u8> {
        self.finished.clone().unwrap_or_else(|| {
            self.hmac
                .as_ref()
                .map_or_else(|| self.digest.finish(), |hmac| hmac.finish(&self.digest))
        })
    }
}
fn fail(env: &mut Env<'_>, error: u32) -> i32 {
    set_last_error(env, error);
    0
}
fn namespace(env: &mut Env<'_>) -> Option<Namespace> {
    if let Some(store) = env.ext.try_get::<Store>() {
        return Some(store.0);
    }
    let ns = env.sys.objects.new_namespace_in(0x0e00_0004, 0x0eff_fffc)?;
    env.ext.insert(Store(ns));
    Some(ns)
}
fn identity<T: 'static>(env: &Env<'_>, handle: u32) -> Option<Identity<T>> {
    env.sys
        .objects
        .lookup_alias(env.ext.try_get::<Store>()?.0, handle)
}
fn context(env: &Env<'_>, handle: u32) -> Option<Arc<Provider>> {
    Some(
        env.sys
            .objects
            .owned(identity::<Context>(env, handle)?)?
            .provider
            .clone(),
    )
}
fn readable(env: &Env<'_>, pointer: u32, length: u32) -> Result<Vec<u8>, u32> {
    check_range(env.cpu, pointer, length, Prot::READ).map_err(|_| 87u32)?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length as usize)
        .map_err(|_| NO_MEMORY)?;
    bytes.resize(length as usize, 0);
    env.cpu.read(pointer, &mut bytes).map_err(|_| 87u32)?;
    Ok(bytes)
}
fn output(env: &Env<'_>, pointer: u32) -> bool {
    pointer != 0 && check_range(env.cpu, pointer, 4, Prot::WRITE).is_ok()
}
fn publish<T: 'static>(env: &mut Env<'_>, value: T, pointer: u32) -> i32 {
    let Some(ns) = namespace(env) else {
        return fail(env, NO_MEMORY);
    };
    let Some(handle) = env.sys.objects.insert_host_in(ns, value) else {
        return fail(env, NO_MEMORY);
    };
    if env.cpu.write_u32(pointer, handle).is_err() {
        env.sys.objects.close_alias(ns, handle);
        return fail(env, 87);
    }
    1
}
/// CryptoAPI output sizes are bytes for both A and W entry points.
fn sized(env: &mut Env<'_>, bytes: &[u8], pointer: u32, length: u32) -> i32 {
    if !output(env, length) {
        return fail(env, 87);
    }
    let required = bytes.len() as u32;
    if pointer == 0 {
        let _ = env.cpu.write_u32(length, required);
        return 1;
    }
    let Ok(capacity) = env.cpu.read_u32(length) else {
        return fail(env, 87);
    };
    if capacity < required {
        let _ = env.cpu.write_u32(length, required);
        return fail(env, 234);
    }
    if check_range(env.cpu, pointer, required, Prot::WRITE).is_err() {
        return fail(env, 87);
    }
    if env.cpu.write(pointer, bytes).is_err() {
        return fail(env, 87);
    }
    let _ = env.cpu.write_u32(length, required);
    1
}
pub(super) fn register(registry: &mut Registry) {
    provider::register(registry);
    hash::register(registry);
    key::register(registry);
}

#[cfg(test)]
mod tests;

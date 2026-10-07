//! The legacy CRT PRNG is per CRT module/thread, as observed on VC80.
//! Authored native evidence: target/engine-loop/20261007-resume/crt-random-native.
use super::{CrtState, Env, Registry, crt_module};
use adv_abi::{Api, win32};

const MODULES: [&str; 3] = ["msvcrt.dll", "msvcr80.dll", "msvcr90.dll"];

fn owner(env: &Env<'_>) -> (String, u32) {
    (
        crt_module(&env.sys.thunks.get(env.thunk).module).to_owned(),
        env.context,
    )
}

#[win32(msvcrt, name = "rand", level = state)]
fn random(env: &mut Env<'_>) -> i32 {
    let key = owner(env);
    let seed = env
        .ext
        .get_or_default::<CrtState>()
        .random_seeds
        .entry(key)
        .or_insert(1);
    *seed = seed.wrapping_mul(214_013).wrapping_add(2_531_011);
    ((*seed >> 16) & 0x7fff) as i32
}

#[win32(msvcrt, name = "srand", level = state)]
fn seed(env: &mut Env<'_>, value: u32) {
    let key = owner(env);
    env.ext
        .get_or_default::<CrtState>()
        .random_seeds
        .insert(key, value);
}

pub(super) fn register(registry: &mut Registry) {
    // The six SDK declarations have the same typed signatures and conventions.
    for module in MODULES {
        for api in [random_ABI, seed_ABI] {
            registry.register(Api { module, ..api });
        }
    }
}

pub(super) fn release(env: &mut Env<'_>, context: Option<u32>) {
    let Some(state) = env.ext.try_get_mut::<CrtState>() else {
        return;
    };
    if let Some(context) = context {
        state.random_seeds.retain(|(_, owner), _| *owner != context);
    } else {
        state.random_seeds.clear();
    }
}

#[cfg(test)]
mod tests;

//! Guest thread WH_GETMESSAGE hooks, owned by the shared USER object table.
use std::collections::{BTreeMap, VecDeque};

use adv_abi::{GuestPtr, Handle, Registry, warn_unavailable, win32};
use adv_cpu::backend::Prot;
use adv_machine::{CtxId, Env, Identity, Namespace, Outcome};

use crate::util::set_last_error;

struct Hook {
    callback: u32,
    installer: CtxId,
    thread: CtxId,
    live: bool,
}

struct Invocation {
    current: Identity<Hook>,
    remaining: VecDeque<Identity<Hook>>,
}

#[derive(Default)]
struct Hooks {
    namespace: Option<Namespace>,
    chains: BTreeMap<CtxId, Vec<u32>>,
    active: BTreeMap<CtxId, Vec<Invocation>>,
}

pub(super) fn register(registry: &mut Registry) {
    for api in [install_a_ABI, install_w_ABI, uninstall_ABI, next_ABI] {
        registry.register(api);
    }
}

fn error(env: &mut Env<'_>, code: u32) -> u32 {
    set_last_error(env, code);
    0
}

fn install(
    env: &mut Env<'_>,
    kind: i32,
    callback: GuestPtr<()>,
    module: Handle,
    tid: u32,
    api: &str,
) -> Handle {
    if !(-1..=14).contains(&kind) {
        return Handle(error(env, 1426)); // ERROR_INVALID_HOOK_FILTER
    }
    if callback.is_null() || !env.cpu.accessible(callback.addr(), 1, Prot::EXEC) {
        return Handle(error(env, 1427)); // ERROR_INVALID_FILTER_PROC
    }
    if tid == 0 {
        warn_unavailable(env, api, "global hook injection is unavailable");
        return Handle(error(env, if module.0 == 0 { 1428 } else { 50 }));
    }
    let Some(thread) = env
        .sys
        .contexts
        .iter()
        .find(|context| context.tid == tid && !context.is_done())
        .map(|context| context.id)
    else {
        return Handle(error(env, 87)); // Native nonexistent thread: ERROR_INVALID_PARAMETER.
    };
    if kind != 3 {
        warn_unavailable(
            env,
            api,
            "only guest thread WH_GETMESSAGE hooks are available",
        );
        return Handle(error(env, 50));
    }
    if module.0 != 0 {
        // DLL-hook module lifetime requires a loader reference through callback
        // completion. Refuse it until that contract can be provided.
        warn_unavailable(env, api, "DLL-backed hook module ownership is unavailable");
        return Handle(error(env, 50));
    }
    let hooks = env.ext.get_or_default::<Hooks>();
    if hooks.namespace.is_none() {
        hooks.namespace = env.sys.objects.new_namespace_in(0x0900_0004, 0x09ff_fffc);
    }
    let Some(namespace) = hooks.namespace else {
        return Handle(error(env, 8));
    };
    if env.sys.objects.alias_count(namespace) >= 16384 {
        return Handle(error(env, 8));
    }
    let Some(handle) = env.sys.objects.insert_host_in(
        namespace,
        Hook {
            callback: callback.addr(),
            installer: env.context,
            thread,
            live: true,
        },
    ) else {
        return Handle(error(env, 8));
    };
    hooks.chains.entry(thread).or_default().insert(0, handle);
    Handle(handle)
}

#[win32(user32, name = "SetWindowsHookExA", level = state)]
fn install_a(
    env: &mut Env<'_>,
    kind: i32,
    callback: GuestPtr<()>,
    module: Handle,
    tid: u32,
) -> Handle {
    install(
        env,
        kind,
        callback,
        module,
        tid,
        "user32.dll!SetWindowsHookExA",
    )
}

#[win32(user32, name = "SetWindowsHookExW", level = state)]
fn install_w(
    env: &mut Env<'_>,
    kind: i32,
    callback: GuestPtr<()>,
    module: Handle,
    tid: u32,
) -> Handle {
    install(
        env,
        kind,
        callback,
        module,
        tid,
        "user32.dll!SetWindowsHookExW",
    )
}

#[win32(user32, name = "UnhookWindowsHookEx", level = state)]
fn uninstall(env: &mut Env<'_>, handle: Handle) -> i32 {
    let Some(namespace) = env.ext.get_or_default::<Hooks>().namespace else {
        return error(env, 1404) as i32;
    };
    let Some(identity) = env.sys.objects.lookup_alias::<Hook>(namespace, handle.0) else {
        return error(env, 1404) as i32;
    };
    let hook = env.sys.objects.owned_mut(identity).unwrap();
    hook.live = false;
    let thread = hook.thread;
    if let Some(chain) = env.ext.get_mut::<Hooks>().chains.get_mut(&thread) {
        chain.retain(|&alias| alias != handle.0);
    }
    env.sys.objects.close_alias(namespace, handle.0);
    1
}

type Done = Box<dyn FnOnce(&mut Env<'_>, u32) -> Outcome>;

fn invoke(
    env: &mut Env<'_>,
    mut chain: VecDeque<Identity<Hook>>,
    code: i32,
    wparam: u32,
    lparam: u32,
    then: Done,
) -> Outcome {
    while let Some(identity) = chain.pop_front() {
        let Some(hook) = env.sys.objects.owned(identity) else {
            continue;
        };
        if !hook.live || hook.thread != env.context {
            continue;
        }
        let target = hook.callback;
        if !env.sys.objects.retain(identity) {
            return Outcome::Fail("message hook invocation exhausted its object references".into());
        }
        let context = env.context;
        env.ext
            .get_mut::<Hooks>()
            .active
            .entry(context)
            .or_default()
            .push(Invocation {
                current: identity,
                remaining: chain,
            });
        return Outcome::call(target, [code as u32, wparam, lparam], move |env, result| {
            let invocation = env
                .ext
                .get_mut::<Hooks>()
                .active
                .get_mut(&context)
                .unwrap()
                .pop()
                .unwrap();
            debug_assert_eq!(invocation.current, identity);
            env.sys.objects.release(identity);
            then(env, result.value())
        });
    }
    then(env, 0)
}

#[win32(user32, name = "CallNextHookEx", level = state)]
fn next(env: &mut Env<'_>, _ignored: Handle, code: i32, wparam: u32, lparam: i32) -> Outcome {
    let context = env.context;
    let chain = env
        .ext
        .get_or_default::<Hooks>()
        .active
        .get(&context)
        .and_then(|stack| stack.last())
        .map(|invocation| invocation.remaining.clone())
        .unwrap_or_default();
    invoke(
        env,
        chain,
        code,
        wparam,
        lparam as u32,
        Box::new(|_, result| Outcome::Return(result)),
    )
}

/// Snapshot an invocation's chain. New hooks belong to subsequent retrievals;
/// uninstalled hooks not already running are skipped, including during reentry.
pub(super) fn retrieved(
    env: &mut Env<'_>,
    output: u32,
    removed: bool,
    then: impl FnOnce(&mut Env<'_>) -> Outcome + 'static,
) -> Outcome {
    let context = env.context;
    let hooks = env.ext.get_or_default::<Hooks>();
    let chain = hooks.namespace.map_or_else(VecDeque::new, |namespace| {
        hooks
            .chains
            .get(&context)
            .into_iter()
            .flatten()
            .filter_map(|&handle| env.sys.objects.lookup_alias::<Hook>(namespace, handle))
            .collect()
    });
    invoke(
        env,
        chain,
        0,
        u32::from(removed),
        output,
        Box::new(move |env, _| then(env)),
    )
}

fn cleanup(env: &mut Env<'_>, exiting: Option<CtxId>) {
    let Some(hooks) = env.ext.try_get::<Hooks>() else {
        return;
    };
    let Some(namespace) = hooks.namespace else {
        return;
    };
    let handles: Vec<_> = hooks
        .chains
        .values()
        .flatten()
        .copied()
        .filter(|&handle| {
            env.sys
                .objects
                .lookup_alias::<Hook>(namespace, handle)
                .and_then(|identity| env.sys.objects.owned(identity))
                .is_some_and(|hook| {
                    exiting
                        .is_none_or(|context| hook.thread == context || hook.installer == context)
                })
        })
        .collect();
    for handle in handles {
        uninstall(env, Handle(handle));
    }
    let hooks = env.ext.get_mut::<Hooks>();
    let active = if let Some(context) = exiting {
        hooks.active.remove(&context).unwrap_or_default()
    } else {
        std::mem::take(&mut hooks.active)
            .into_values()
            .flatten()
            .collect()
    };
    for invocation in active {
        env.sys.objects.release(invocation.current);
    }
}

pub(crate) fn exit_thread(env: &mut Env<'_>) {
    cleanup(env, Some(env.context));
}

pub(crate) fn exit_process(env: &mut Env<'_>) {
    cleanup(env, None);
}

//! CRT termination handlers are thread-local; the purecall handler is module
//! global. The split VC14 runtime calls UCRT's terminate owner while retaining
//! its own unexpected/translator/purecall state. No host exception is thrown.
use super::*;
use std::rc::{Rc, Weak};

#[derive(Default)]
pub(super) struct Thread {
    terminate: u32,
    unexpected: u32,
    translator: u32,
    terminating: Weak<()>,
}

#[derive(Clone, Copy)]
enum Slot {
    Terminate,
    Unexpected,
    Translator,
}

const EXPORTS: &[&str] = &[
    "set_terminate",
    "_get_terminate",
    "terminate",
    "__std_terminate",
    "set_unexpected",
    "_get_unexpected",
    "unexpected",
    "_set_se_translator",
    "_set_purecall_handler",
    "_get_purecall_handler",
    "_purecall",
    "?set_terminate@@YAP6AXXZP6AXXZ@Z",
    "?set_terminate@@YAP6AXXZH@Z",
    "?set_unexpected@@YAP6AXXZP6AXXZ@Z",
    "?set_unexpected@@YAP6AXXZH@Z",
    "?_set_se_translator@@YAP6AXIPAU_EXCEPTION_POINTERS@@@ZP6AXI0@Z@Z",
    "?_set_se_translator@@YAP6AXIPAU_EXCEPTION_POINTERS@@@ZH@Z",
    "?terminate@@YAXXZ",
    "?unexpected@@YAXXZ",
    "_o__get_terminate",
    "_o__purecall",
    "_o_set_terminate",
    "_o_terminate",
];

pub(super) fn register(registry: &mut Registry) {
    let exports: Vec<_> = EXPORTS
        .iter()
        .map(|&name| (name, call as adv_machine::Handler))
        .collect();
    versions::register(registry, &exports);
}

fn module(env: &Env<'_>, slot: Slot) -> String {
    let module = crt_module(&env.sys.thunks.get(env.thunk).module);
    if matches!(slot, Slot::Terminate) && module == "vcruntime140.dll" {
        "ucrtbase.dll".to_owned()
    } else {
        module.to_owned()
    }
}

fn thread<'a>(env: &'a mut Env<'_>, slot: Slot) -> &'a mut Thread {
    let key = (module(env, slot), env.context);
    env.ext
        .get_or_default::<CrtState>()
        .cpp_threads
        .entry(key)
        .or_default()
}

fn value(thread: &mut Thread, slot: Slot) -> &mut u32 {
    match slot {
        Slot::Terminate => &mut thread.terminate,
        Slot::Unexpected => &mut thread.unexpected,
        Slot::Translator => &mut thread.translator,
    }
}

fn handler(env: &mut Env<'_>, slot: Slot, setting: bool) -> Outcome {
    let new = env.arg(0);
    let value = value(thread(env, slot), slot);
    let old = *value;
    if setting {
        *value = new;
    }
    // Microsoft documents that the default-restoring value may be NULL.
    // A NULL slot selects the actual default path; it is never invoked.
    Outcome::Return(old)
}

pub(super) fn terminate(env: &mut Env<'_>) -> Outcome {
    let thread = thread(env, Slot::Terminate);
    if thread.terminating.upgrade().is_some() {
        return abort(env);
    }
    let handler = thread.terminate;
    if handler == 0 {
        return abort(env);
    }
    let guard = Rc::new(());
    thread.terminating = Rc::downgrade(&guard);
    Outcome::call(handler, [], move |env, _| {
        drop(guard);
        abort(env)
    })
}

fn abort(env: &mut Env<'_>) -> Outcome {
    crate::kernel32::exit_process(env, 3)
}

fn unexpected(env: &mut Env<'_>) -> Outcome {
    let handler = thread(env, Slot::Unexpected).unexpected;
    if handler == 0 {
        return terminate(env);
    }
    // Unlike terminate handlers, an unexpected handler is allowed to throw.
    Outcome::call(handler, [], |env, _| terminate(env)).unwindable()
}

pub(super) fn purecall(env: &mut Env<'_>) -> Outcome {
    let module = crt_module(&env.sys.thunks.get(env.thunk).module).to_owned();
    let handler = env
        .ext
        .get_or_default::<CrtState>()
        .purecall_handlers
        .get(&module)
        .copied()
        .unwrap_or(0);
    // The system/legacy CRT reports R6025 through its _amsg_exit path. The
    // split VC14/UCRT profile follows the documented abort path.
    let code = if matches!(module.as_str(), "ucrtbase.dll" | "vcruntime140.dll") {
        3
    } else {
        255
    };
    if handler == 0 {
        return crate::kernel32::exit_process(env, code);
    }
    Outcome::call(handler, [], move |env, _| {
        crate::kernel32::exit_process(env, code)
    })
}

fn call(env: &mut Env<'_>) -> Outcome {
    let original = env.sys.thunks.get(env.thunk).name.clone();
    let name = if original.starts_with('?') {
        original.trim_start_matches('?').split('@').next().unwrap()
    } else {
        original.strip_prefix("_o_").unwrap_or(&original)
    };
    match name {
        "terminate" | "__std_terminate" => terminate(env),
        "unexpected" => unexpected(env),
        "set_terminate" => handler(env, Slot::Terminate, true),
        "_get_terminate" => handler(env, Slot::Terminate, false),
        "set_unexpected" => handler(env, Slot::Unexpected, true),
        "_get_unexpected" => handler(env, Slot::Unexpected, false),
        "_set_se_translator" => handler(env, Slot::Translator, true),
        "_purecall" => purecall(env),
        "_set_purecall_handler" | "_get_purecall_handler" => {
            let module = crt_module(&env.sys.thunks.get(env.thunk).module).to_owned();
            let new = env.arg(0);
            let value = env
                .ext
                .get_or_default::<CrtState>()
                .purecall_handlers
                .entry(module)
                .or_default();
            let old = *value;
            if name == "_set_purecall_handler" {
                *value = new;
            }
            Outcome::Return(old)
        }
        _ => unreachable!("registered CRT runtime name"),
    }
}

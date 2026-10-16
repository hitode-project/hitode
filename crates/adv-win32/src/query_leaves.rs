//! Audited synchronous queries. Invoke the original adapter after validating
//! complete output ranges, so leaves observe exactly the ordinary data source.
use adv_abi::{Env, Outcome, Registry};
use adv_cpu::backend::Prot;

pub(crate) const USER: &[&str] = &[
    "GetFocus",
    "GetActiveWindow",
    "GetForegroundWindow",
    "GetCapture",
    "GetDesktopWindow",
    "GetSystemMetrics",
    "IsWindow",
    "IsWindowVisible",
    "IsWindowEnabled",
    "IsIconic",
    "IsZoomed",
    "GetParent",
    "IsChild",
    "GetWindow",
    "GetTopWindow",
    "GetCursorPos",
    "GetClipCursor",
    "GetWindowRect",
    "GetClientRect",
    "ClientToScreen",
    "ScreenToClient",
];
pub(crate) const KERNEL: &[&str] = &[
    "GetLocalTime",
    "GetSystemTime",
    "GetSystemTimeAsFileTime",
    "lstrlenA",
    "lstrlenW",
    "lstrcpyA",
    "lstrcpyW",
    "lstrcmpA",
    "lstrcmpW",
];

pub(crate) fn install(reg: &mut Registry) {
    for (module, names) in [("user32.dll", USER), ("kernel32.dll", KERNEL)] {
        for name in names {
            reg.set_env_leaf(module, name, query);
        }
    }
}

fn query(env: &mut Env<'_>) -> Option<u64> {
    let thunk = env.sys.thunks.get(env.thunk);
    let (argument, size, access) = match thunk.name.as_str() {
        "GetCursorPos" => (0, 8, Prot::WRITE),
        "GetClipCursor" => (0, 16, Prot::WRITE),
        "GetWindowRect" | "GetClientRect" => (1, 16, Prot::WRITE),
        "ClientToScreen" | "ScreenToClient" => (1, 8, Prot::RW),
        "GetLocalTime" | "GetSystemTime" => (0, 16, Prot::WRITE),
        "GetSystemTimeAsFileTime" => (0, 8, Prot::WRITE),
        // Copy helpers preserve their caught-fault and partial-copy semantics
        // in the ordinary implementation; no decline occurs after entry.
        _ => (0, 0, Prot::READ),
    };
    if size != 0 && adv_abi::guest::check_range(env.cpu, env.arg(argument), size, access).is_err() {
        return None;
    }
    let handler = thunk.f.handler;
    match handler(env) {
        Outcome::Return(value) => {
            Some((u64::from(adv_abi::leaf_edx(env)) << 32) | u64::from(value))
        }
        _ => unreachable!("audited synchronous query"),
    }
}

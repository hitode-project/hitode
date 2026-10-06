//! D3D9 HRESULT capability diagnostics at typed API boundaries.
//! Successful synchronous calls only reuse a small per-context scope stack.
use crate::{Env, Outcome};
use adv_cpu::backend::GuestExt;
use std::collections::BTreeMap;

const NOT_AVAILABLE: u32 = 0x8876086a;
const MAX_ARGUMENTS: usize = 16;

struct Warning {
    api: String,
    reason: String,
}
struct Scope {
    api: &'static str,
    argc: u8,
    selectors: &'static [(&'static str, u8)],
    caller: u32,
    arguments: Option<[u32; MAX_ARGUMENTS]>,
    warnings: Vec<Warning>,
}
#[derive(Default)]
struct Scopes(BTreeMap<u32, Vec<Scope>>);

/// Called only for SDK-declared D3D9 HRESULT exports by the typed macro.
#[doc(hidden)]
pub fn begin(
    env: &mut Env<'_>,
    api: &'static str,
    argc: u8,
    selectors: &'static [(&'static str, u8)],
) {
    let scope = Scope {
        api,
        argc,
        selectors,
        caller: env.caller(),
        arguments: None,
        warnings: Vec::new(),
    };
    push(env, scope);
}
fn push(env: &mut Env<'_>, scope: Scope) {
    env.ext
        .get_or_default::<Scopes>()
        .0
        .entry(env.context)
        .or_default()
        .push(scope);
}
fn same_api(left: &str, right: &str) -> bool {
    let (module, name) = left.split_once('!').unwrap();
    match right.split_once('!') {
        Some((other_module, other_name)) => {
            module.eq_ignore_ascii_case(other_module) && name == other_name
        }
        None => name == right,
    }
}

/// Delay an explicit same-API warning until its final result is known. This
/// retains the frontend's specific reason and avoids a second fallback event.
pub(crate) fn defer_warning(env: &mut Env<'_>, api: &str, reason: &str) -> bool {
    let Some(scope) = env
        .ext
        .try_get_mut::<Scopes>()
        .and_then(|scopes| scopes.0.get_mut(&env.context))
        .and_then(|stack| stack.last_mut())
    else {
        return false;
    };
    if !same_api(scope.api, api) {
        return false;
    }
    scope.warnings.push(Warning {
        api: api.to_owned(),
        reason: reason.to_owned(),
    });
    true
}
fn arguments(env: &Env<'_>, scope: &Scope) -> [u32; MAX_ARGUMENTS] {
    scope.arguments.unwrap_or_else(|| {
        let mut values = [0; MAX_ARGUMENTS];
        for (index, value) in values.iter_mut().enumerate().take(usize::from(scope.argc)) {
            *value = env.cpu.arg(index as u32).unwrap_or(0);
        }
        values
    })
}

/// Preserve every Outcome edge, including completion after a guest callback,
/// wait, or presentation handoff. Only final Return(NOT_AVAILABLE) is inferred.
#[doc(hidden)]
pub fn finish(env: &mut Env<'_>, outcome: Outcome) -> Outcome {
    let scope = env
        .ext
        .get_mut::<Scopes>()
        .0
        .get_mut(&env.context)
        .and_then(Vec::pop)
        .expect("typed D3D9 diagnostic scope");
    observe(env, scope, outcome)
}
fn observe(env: &mut Env<'_>, mut scope: Scope, outcome: Outcome) -> Outcome {
    match outcome {
        Outcome::Block {
            wait,
            then,
            allow_time_skip,
        } => {
            scope.arguments = Some(arguments(env, &scope));
            Outcome::Block {
                wait,
                allow_time_skip,
                then: Box::new(move |env, resume| {
                    push(env, scope);
                    let outcome = then(env, resume);
                    finish(env, outcome)
                }),
            }
        }
        Outcome::Call {
            target,
            args,
            then,
            stack,
        } => {
            scope.arguments = Some(arguments(env, &scope));
            Outcome::Call {
                target,
                args,
                stack,
                then: Box::new(move |env, resume| {
                    push(env, scope);
                    let outcome = then(env, resume);
                    finish(env, outcome)
                }),
            }
        }
        Outcome::Defer { then } => {
            scope.arguments = Some(arguments(env, &scope));
            Outcome::Defer {
                then: Box::new(move |env| {
                    push(env, scope);
                    let outcome = then(env);
                    finish(env, outcome)
                }),
            }
        }
        completed => {
            let unsupported = matches!(completed, Outcome::Return(NOT_AVAILABLE));
            if unsupported && scope.warnings.is_empty() {
                let values = arguments(env, &scope);
                let mut reason = "D3DERR_NOTAVAILABLE (HRESULT=0x8876086a): operation or requested option is unsupported".to_owned();
                // Stable selectors describe the rejected option. Addresses,
                // dimensions and variable batch sizes never become warning keys.
                for (name, word) in scope.selectors {
                    if let Some(value) = values.get(usize::from(*word)) {
                        reason.push_str(&format!("; {name}={value:#x}"));
                    }
                }
                let sample = values[..usize::from(scope.argc).min(MAX_ARGUMENTS)]
                    .iter()
                    .map(|value| format!("{value:x}"))
                    .collect::<Vec<_>>()
                    .join(",");
                crate::stub::emit_warning(env, scope.api, &reason, scope.caller, Some(&sample));
            }
            for warning in scope.warnings {
                let reason = if unsupported {
                    format!(
                        "D3DERR_NOTAVAILABLE (HRESULT=0x8876086a): {}",
                        warning.reason
                    )
                } else {
                    warning.reason
                };
                crate::stub::emit_warning(env, &warning.api, &reason, scope.caller, None);
            }
            completed
        }
    }
}

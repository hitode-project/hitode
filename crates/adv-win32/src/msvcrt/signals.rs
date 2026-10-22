//! CRT signal slots belong to the existing module/thread runtime owner.
//! Fault signals retain their native per-exception entries; process signals
//! share module slots. Explicit delivery never invents a Windows exception.
use super::*;
use adv_abi::guest::memory_failure;

type After = Box<dyn FnOnce(&mut Env<'_>) -> Outcome>;
// Native x86 CRT action-table order. UCRT adds two SSE fault entries.
const FAULTS: [(u32, u32, u32); 12] = [
    (0xc0000005, 11, 0),
    (0xc000001d, 4, 0),
    (0xc0000096, 4, 0),
    (0xc000008d, 8, 0x82),
    (0xc000008e, 8, 0x83),
    (0xc000008f, 8, 0x86),
    (0xc0000090, 8, 0x81),
    (0xc0000091, 8, 0x84),
    (0xc0000092, 8, 0x8a),
    (0xc0000093, 8, 0x85),
    (0xc00002b4, 8, 0x8e),
    (0xc00002b5, 8, 0x8d),
];
#[derive(Default)]
pub(super) struct State {
    process: HashMap<String, [u32; 4]>,
    threads: HashMap<(String, u32), [u32; 12]>,
}
impl State {
    pub(super) fn release(&mut self, context: Option<u32>) {
        if let Some(context) = context {
            self.threads.retain(|(_, id), _| *id != context);
        } else {
            *self = Self::default();
        }
    }
}
pub(super) fn register(registry: &mut Registry) {
    versions::register(
        registry,
        &[
            ("signal", install),
            ("raise", raise),
            ("__fpecode", cell),
            ("__pxcptinfoptrs", cell),
            ("_XcptFilter", filter),
            ("__CppXcptFilter", filter),
            ("_seh_filter_exe", filter),
            ("_seh_filter_dll", filter),
        ],
    );
}
fn module(env: &Env<'_>) -> String {
    let module = crt_module(&env.sys.thunks.get(env.thunk).module);
    if module == "vcruntime140.dll" {
        "ucrtbase.dll".into()
    } else {
        module.into()
    }
}
fn normalize(signal: u32) -> u32 {
    if signal == 6 { 22 } else { signal }
}
fn process_index(signal: u32) -> Option<usize> {
    [2, 21, 22, 15].iter().position(|&n| n == signal)
}
fn local_index(signal: u32) -> Option<usize> {
    FAULTS.iter().position(|&(_, n, _)| n == signal)
}
fn value(env: &mut Env<'_>, signal: u32, replacement: Option<u32>) -> u32 {
    let module = module(env);
    let state = &mut env.ext.get_or_default::<CrtState>().signals;
    if let Some(i) = process_index(signal) {
        let slots = state.process.entry(module).or_default();
        let old = slots[i];
        if let Some(value) = replacement {
            slots[i] = value;
        }
        old
    } else {
        let slots = state.threads.entry((module, env.context)).or_default();
        let old = slots[local_index(signal).expect("validated local signal")];
        if let Some(value) = replacement {
            for (i, &(_, n, _)) in FAULTS.iter().enumerate() {
                if n == signal {
                    slots[i] = value;
                }
            }
        }
        old
    }
}
fn cells(env: &mut Env<'_>) -> Option<(u32, u32)> {
    let address = floating::errno_address(env).addr();
    (address != 0).then_some((address + 8, address + 12))
}
fn cell(env: &mut Env<'_>) -> Outcome {
    match cells(env) {
        Some((fpe, pointers)) => {
            Outcome::Return(if env.sys.thunks.get(env.thunk).name == "__fpecode" {
                fpe
            } else {
                pointers
            })
        }
        // Native __fpecode/__pxcptinfoptrs use the fatal PTD accessor on OOM.
        None => crate::kernel32::exit_process(env, 255),
    }
}
fn install(env: &mut Env<'_>) -> Outcome {
    let (raw, action) = (env.arg(0), env.arg(1));
    let signal = normalize(raw);
    if matches!(action, 3 | 4) || (process_index(signal).is_none() && local_index(signal).is_none())
    {
        // Native VC80/VC90/UCRT preserve errors for reserved POSIX numbers.
        if matches!(raw, 1 | 3 | 13 | 16 | 17) {
            return Outcome::Return(u32::MAX);
        }
        let profile = versions::profile(&env.sys.thunks.get(env.thunk).module);
        if !profile.signal_validation {
            return match floating::errno_address(env).write(env.cpu, 22) {
                Ok(()) => Outcome::Return(u32::MAX),
                Err(fault) => memory_failure(fault),
            };
        }
        return errors::invalid(env, u32::MAX);
    }
    if local_index(signal).is_some() && cells(env).is_none() {
        return crate::kernel32::exit_process(env, 255);
    }
    Outcome::Return(value(
        env,
        signal,
        if action == 2 { None } else { Some(action) },
    ))
}
fn clear_entry(env: &mut Env<'_>, all_fp: bool, index: usize) {
    let module = module(env);
    let slots = env
        .ext
        .get_mut::<CrtState>()
        .signals
        .threads
        .entry((module, env.context))
        .or_default();
    if all_fp {
        for (i, &(_, n, _)) in FAULTS.iter().enumerate() {
            if n == 8 {
                slots[i] = 0;
            }
        }
    } else {
        slots[index] = 0;
    }
}
fn call_local(
    env: &mut Env<'_>,
    handler: u32,
    signal: u32,
    pointers: u32,
    fpe: u32,
    after: After,
) -> Outcome {
    let Some((fpe_cell, pointer_cell)) = cells(env) else {
        return Outcome::Return(u32::MAX);
    };
    let saved = env
        .cpu
        .read_u32(pointer_cell)
        .and_then(|pointer| env.cpu.read_u32(fpe_cell).map(|value| (pointer, value)));
    let (old_pointer, old_fpe) = match saved {
        Ok(saved) => saved,
        Err(fault) => return memory_failure(fault),
    };
    if let Err(fault) = env.cpu.write_u32(pointer_cell, pointers).and_then(|_| {
        if signal == 8 {
            env.cpu.write_u32(fpe_cell, fpe)
        } else {
            Ok(())
        }
    }) {
        return memory_failure(fault);
    }
    let args = if signal == 8 {
        vec![signal, fpe]
    } else {
        vec![signal]
    };
    Outcome::call(handler, args, move |env, _| {
        if let Err(fault) = env.cpu.write_u32(pointer_cell, old_pointer).and_then(|_| {
            if signal == 8 {
                env.cpu.write_u32(fpe_cell, old_fpe)
            } else {
                Ok(())
            }
        }) {
            return memory_failure(fault);
        }
        after(env)
    })
    .unwindable()
}
fn deliver(env: &mut Env<'_>, signal: u32, argument: u32, after: After) -> Outcome {
    let handler = value(env, signal, None);
    if handler == 1 {
        return after(env);
    }
    if handler == 0 {
        return crate::kernel32::exit_process(env, 3);
    }
    if let Some(index) = local_index(signal) {
        clear_entry(env, signal == 8, index);
        call_local(env, handler, signal, 0, 0x8c, after)
    } else {
        value(env, signal, Some(0));
        Outcome::call(handler, [argument], move |env, _| after(env)).unwindable()
    }
}
fn raise(env: &mut Env<'_>) -> Outcome {
    let argument = env.arg(0);
    let signal = normalize(argument);
    if process_index(signal).is_none() && local_index(signal).is_none() {
        return errors::invalid(env, u32::MAX);
    }
    if local_index(signal).is_some() && cells(env).is_none() {
        return Outcome::Return(u32::MAX);
    }
    deliver(env, signal, argument, Box::new(|_| Outcome::Return(0)))
}
pub(super) fn abort(env: &mut Env<'_>) -> Outcome {
    // A returning or ignored SIGABRT still ends the process; a guest handler
    // may leave nonlocally instead, as in the native runtime.
    if value(env, 22, None) == 0 {
        return crate::kernel32::exit_process(env, 3);
    }
    deliver(
        env,
        22,
        22,
        Box::new(|env| crate::kernel32::exit_process(env, 3)),
    )
}
fn filter(env: &mut Env<'_>) -> Outcome {
    let code = env.arg(0);
    if matches!(
        env.sys.thunks.get(env.thunk).name.as_str(),
        "__CppXcptFilter" | "_seh_filter_dll"
    ) && code != 0xe06d7363
    {
        return Outcome::Return(0);
    }
    let module = module(env);
    let count = if module == "ucrtbase.dll" { 12 } else { 10 };
    let Some(index) = FAULTS[..count]
        .iter()
        .position(|&(status, _, _)| status == code)
    else {
        return Outcome::Return(0);
    };
    let (_, signal, fpe) = FAULTS[index];
    let handler = env
        .ext
        .get_or_default::<CrtState>()
        .signals
        .threads
        .entry((module, env.context))
        .or_default()[index];
    match handler {
        0 => Outcome::Return(0),
        1 => Outcome::Return(u32::MAX),
        5 => {
            clear_entry(env, false, index);
            Outcome::Return(1)
        }
        _ => {
            clear_entry(env, signal == 8, index);
            call_local(
                env,
                handler,
                signal,
                env.arg(1),
                fpe,
                Box::new(|_| Outcome::Return(u32::MAX)),
            )
        }
    }
}

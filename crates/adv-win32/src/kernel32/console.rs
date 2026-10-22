//! The GUI process has no console. Redirected standard streams remain ordinary
//! file/stream handles; they never become console screen or input buffers.
use super::K32;
use crate::util::{out_u32, set_last_error};
use adv_abi::{Registry, guest::check_range, signature};
use adv_cpu::backend::Prot;
use adv_machine::{Env, Outcome};

#[derive(Default)]
struct Control {
    handlers: Vec<u32>,
    ignore_ctrl_c: bool,
}
fn absent(env: &mut Env<'_>) -> Outcome {
    set_last_error(env, 6);
    Outcome::Return(0)
}
fn absent_handle(env: &mut Env<'_>) -> Outcome {
    set_last_error(env, 6);
    Outcome::Return(u32::MAX)
}
fn read_write(env: &mut Env<'_>) -> Outcome {
    // ReadConsole and WriteConsole initialize their count before the failed I/O.
    let count = env.arg(3);
    if count != 0 && check_range(env.cpu, count, 4, Prot::WRITE).is_ok() {
        out_u32(env.cpu, count, 0);
    }
    absent(env)
}
fn allocate(env: &mut Env<'_>) -> Outcome {
    adv_abi::warn_unavailable(
        env,
        "kernel32.dll!AllocConsole",
        "console creation has no host console transport",
    );
    set_last_error(env, 50);
    Outcome::Return(0)
}
fn detach(env: &mut Env<'_>) -> Outcome {
    *env.ext.get_or_default::<Control>() = Control::default();
    Outcome::Return(1) // Documented success even if already detached.
}
fn attach(env: &mut Env<'_>) -> Outcome {
    let pid = env.arg(0);
    let exists = pid == u32::MAX
        || pid == env.sys.pid
        || env
            .ext
            .try_get::<adv_os::processes::Services>()
            .is_some_and(|service| {
                service
                    .live_processes()
                    .iter()
                    .any(|&(candidate, _)| candidate == pid)
            });
    set_last_error(env, if exists { 6 } else { 87 });
    Outcome::Return(0)
}
fn handler(env: &mut Env<'_>) -> Outcome {
    let (routine, add) = (env.arg(0), env.arg(1) != 0);
    let control = env.ext.get_or_default::<Control>();
    if routine == 0 {
        control.ignore_ctrl_c = add;
        return Outcome::Return(1);
    }
    if add {
        if control.handlers.try_reserve(1).is_err() {
            set_last_error(env, 8);
            return Outcome::Return(0);
        }
        control.handlers.push(routine);
    } else if let Some(index) = control.handlers.iter().rposition(|&entry| entry == routine) {
        control.handlers.remove(index);
    } else {
        set_last_error(env, 87);
        return Outcome::Return(0);
    }
    Outcome::Return(1)
}
fn generate_event(env: &mut Env<'_>) -> Outcome {
    if env.arg(0) > 1 {
        set_last_error(env, 87);
        return Outcome::Return(0);
    }
    absent(env)
}
/// These functions address the absent console (explicitly or implicitly).
/// Their documented zero/false failure does not create a capability diagnostic.
const ABSENT: &[&str] = &[
    "AddConsoleAliasA",
    "AddConsoleAliasW",
    "ExpungeConsoleCommandHistoryA",
    "ExpungeConsoleCommandHistoryW",
    "FillConsoleOutputAttribute",
    "FillConsoleOutputCharacterA",
    "FillConsoleOutputCharacterW",
    "FlushConsoleInputBuffer",
    "GetConsoleAliasA",
    "GetConsoleAliasW",
    "GetConsoleAliasExesA",
    "GetConsoleAliasExesW",
    "GetConsoleAliasExesLengthA",
    "GetConsoleAliasExesLengthW",
    "GetConsoleAliasesA",
    "GetConsoleAliasesW",
    "GetConsoleAliasesLengthA",
    "GetConsoleAliasesLengthW",
    "GetConsoleCP",
    "GetConsoleOutputCP",
    "GetConsoleCommandHistoryA",
    "GetConsoleCommandHistoryW",
    "GetConsoleCommandHistoryLengthA",
    "GetConsoleCommandHistoryLengthW",
    "GetConsoleCursorInfo",
    "GetConsoleDisplayMode",
    "GetConsoleHistoryInfo",
    "GetConsoleMode",
    "GetConsoleOriginalTitleA",
    "GetConsoleOriginalTitleW",
    "GetConsoleProcessList",
    "GetConsoleScreenBufferInfo",
    "GetConsoleScreenBufferInfoEx",
    "GetConsoleSelectionInfo",
    "GetConsoleTitleA",
    "GetConsoleTitleW",
    "GetCurrentConsoleFont",
    "GetCurrentConsoleFontEx",
    "GetNumberOfConsoleInputEvents",
    "GetNumberOfConsoleMouseButtons",
    "PeekConsoleInputA",
    "PeekConsoleInputW",
    "ReadConsoleInputA",
    "ReadConsoleInputW",
    "ReadConsoleOutputA",
    "ReadConsoleOutputW",
    "ReadConsoleOutputAttribute",
    "ReadConsoleOutputCharacterA",
    "ReadConsoleOutputCharacterW",
    "ScrollConsoleScreenBufferA",
    "ScrollConsoleScreenBufferW",
    "SetConsoleActiveScreenBuffer",
    "SetConsoleCP",
    "SetConsoleOutputCP",
    "SetConsoleCursorInfo",
    "SetConsoleCursorPosition",
    "SetConsoleDisplayMode",
    "SetConsoleHistoryInfo",
    "SetConsoleMode",
    "SetConsoleScreenBufferInfoEx",
    "SetConsoleScreenBufferSize",
    "SetConsoleTextAttribute",
    "SetConsoleTitleA",
    "SetConsoleTitleW",
    "SetConsoleWindowInfo",
    "SetCurrentConsoleFontEx",
    "WriteConsoleInputA",
    "WriteConsoleInputW",
    "WriteConsoleOutputA",
    "WriteConsoleOutputW",
    "WriteConsoleOutputAttribute",
    "WriteConsoleOutputCharacterA",
    "WriteConsoleOutputCharacterW",
];
fn bind(registry: &mut Registry, name: &str, handler: adv_machine::Handler) {
    let signature = signature::lookup(K32, name).expect("catalogued console API");
    registry.add(
        K32,
        name,
        signature.conv.expect("callable console declaration"),
        handler,
    );
}
pub(super) fn register(registry: &mut Registry) {
    for &name in ABSENT {
        bind(registry, name, absent);
    }
    for name in [
        "ReadConsoleA",
        "ReadConsoleW",
        "WriteConsoleA",
        "WriteConsoleW",
    ] {
        bind(registry, name, read_write);
    }
    bind(registry, "CreateConsoleScreenBuffer", absent_handle);
    bind(registry, "AllocConsole", allocate);
    bind(registry, "FreeConsole", detach);
    bind(registry, "AttachConsole", attach);
    bind(registry, "SetConsoleCtrlHandler", handler);
    bind(registry, "GenerateConsoleCtrlEvent", generate_event);
}
#[cfg(test)]
mod tests;

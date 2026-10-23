//! The win32k system calls and user-mode callbacks, after Wine's i386
//! syscall dispatcher and `KeUserModeCallback`.
//!
//! `win32u.so` registers its functions as system call table 1 when its
//! Unix library starts. A call numbered from 0x1000 runs the registered
//! guest function with the arguments the stub left on the stack. That code
//! calls back into user32 through `KeUserModeCallback`, which enters
//! ntdll.dll's `KiUserCallbackDispatcher`; the callback ends with
//! `NtCallbackReturn`, which ends the guest call the callback made.
//!
//! Wine runs the Unix side on a separate kernel stack and the callback on
//! the user stack. Here both share the thread's stack, so a callback starts
//! a gap below the code that made it, which keeps the results it returns
//! on its stack intact while that code reads them.

use std::collections::HashMap;

use adv_cpu::backend::{GuestExt, Reg};
use adv_machine::{CtxId, Env, Outcome};

use crate::object::write_u32;
use crate::status::{self, Status};

/// Wine numbers a call by table in bits 12 and 13.
const TABLE_SHIFT: u32 = 12;
const TABLES: usize = 4;
const INVALID_SYSTEM_SERVICE: Status = 0xc000_001c;
const NO_CALLBACK_ACTIVE: Status = 0xc000_0258;
/// Room left on the stack between a callback and the code that made it.
const CALLBACK_GAP: u32 = 0x1000;

/// A registered table: the function addresses and each function's argument
/// bytes, as `KeAddSystemServiceTable` takes them.
#[derive(Clone, Copy)]
struct Table {
    functions: u32,
    arguments: u32,
    limit: u32,
}

#[derive(Default)]
struct Tables([Option<Table>; TABLES]);

/// Each context's callbacks in progress, innermost last: where to store
/// the results `NtCallbackReturn` gives.
#[derive(Default)]
struct Callbacks(HashMap<CtxId, Vec<(u32, u32)>>);

/// Where ntdll.dll takes user-mode callbacks.
#[derive(Clone, Copy)]
pub struct Dispatcher(pub u32);

/// KeAddSystemServiceTable(functions, counters, limit, arguments, index)
pub fn add_table(
    env: &mut Env<'_>,
    functions: u32,
    limit: u32,
    arguments: u32,
    index: u32,
) -> bool {
    let Some(slot) = env.ext.get_or_default::<Tables>().0.get_mut(index as usize) else {
        return false;
    };
    if slot.is_some() {
        return false;
    }
    *slot = Some(Table {
        functions,
        arguments,
        limit,
    });
    true
}

/// System call `number` of a table other than the NT table, entered like
/// [`crate::dispatch`]'s system calls: argument `i` is `env.arg(i + 1)`.
pub fn call(env: &mut Env<'_>, number: u32) -> Outcome {
    let (table, index) = ((number >> TABLE_SHIFT) as usize % TABLES, number & 0xfff);
    let Some(table) = env.ext.get_or_default::<Tables>().0[table] else {
        return Outcome::Return(INVALID_SYSTEM_SERVICE);
    };
    if index >= table.limit {
        return Outcome::Return(INVALID_SYSTEM_SERVICE);
    }
    let entry = (|| {
        let function = env.cpu.read_u32(table.functions + index * 4)?;
        let mut bytes = [0];
        env.cpu.read(table.arguments + index, &mut bytes)?;
        Ok::<_, adv_cpu::backend::MemFault>((function, bytes[0]))
    })();
    let Ok((function, bytes)) = entry else {
        return Outcome::Return(status::ACCESS_VIOLATION);
    };
    let args: Vec<u32> = (1..=u32::from(bytes) / 4).map(|i| env.arg(i)).collect();
    Outcome::call(function, args, |_, returned| {
        Outcome::Return(returned.value())
    })
}

/// KeUserModeCallback(id, args, length, *ret_ptr, *ret_length): copies the
/// arguments onto the stack and calls `KiUserCallbackDispatcher(id, args,
/// length, 0)` there.
pub fn user_mode_callback(
    env: &mut Env<'_>,
    id: u32,
    args: u32,
    length: u32,
    ret_ptr: u32,
    ret_length: u32,
) -> Outcome {
    let Some(&Dispatcher(dispatcher)) = env.ext.try_get::<Dispatcher>() else {
        return Outcome::Fail("user-mode callbacks before ntdll.dll is mapped".into());
    };
    let esp = env.cpu.reg(Reg::Esp);
    let Some(data) = esp
        .checked_sub(CALLBACK_GAP + length)
        .map(|data| data & !15)
    else {
        return Outcome::Return(status::STACK_OVERFLOW);
    };
    let mut bytes = vec![0; length as usize];
    if env.cpu.read(args, &mut bytes).is_err() || env.cpu.write(data, &bytes).is_err() {
        return Outcome::Return(status::ACCESS_VIOLATION);
    }
    let context = env.context;
    let callbacks = env
        .ext
        .get_or_default::<Callbacks>()
        .0
        .entry(context)
        .or_default();
    let depth = callbacks.len();
    callbacks.push((ret_ptr, ret_length));
    Outcome::call_on_stack(
        dispatcher,
        [id, data, length, 0],
        data,
        move |env, returned| {
            // NtCallbackReturn has taken this callback's entry, unless the
            // dispatcher returned without calling back.
            if let Some(callbacks) = env.ext.get_or_default::<Callbacks>().0.get_mut(&context) {
                callbacks.truncate(depth);
            }
            Outcome::Return(returned.value())
        },
    )
}

/// NtCallbackReturn(ret_ptr, ret_length, status)
pub fn callback_return(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[ret_ptr, ret_length, result] = args else {
        unreachable!()
    };
    let context = env.context;
    let Some((ret_ptr_out, ret_length_out)) = env
        .ext
        .get_or_default::<Callbacks>()
        .0
        .get_mut(&context)
        .and_then(Vec::pop)
    else {
        return Outcome::Return(NO_CALLBACK_ACTIVE);
    };
    for (address, value) in [(ret_ptr_out, ret_ptr), (ret_length_out, ret_length)] {
        if address != 0 && write_u32(env, address, value).is_err() {
            return Outcome::Return(status::ACCESS_VIOLATION);
        }
    }
    Outcome::ReturnFromCall(result)
}

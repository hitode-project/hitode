//! x86 EH2/EH3/EH4 scope tables share local cleanup and live global unwinding.
//! Filter/finally funclets run with the establisher's EBP; catch entry retires
//! only callback frames explicitly admitted by the machine unwind protocol.
use super::*;
use adv_abi::{GuestPtr, guest::memory_failure};
use adv_cpu::backend::MemFault;

type Done = Box<dyn FnOnce(&mut Env<'_>) -> Outcome>;

pub(super) fn register(registry: &mut Registry) {
    versions::register(
        registry,
        &[
            ("_except_handler2", except),
            ("_except_handler3", except),
            ("_except_handler4_common", except4),
            ("_local_unwind2", local),
            ("_local_unwind4", local),
            ("_global_unwind2", global),
        ],
    );
}

#[derive(Clone, Copy)]
struct Table {
    frame: u32,
    entries: u32,
    root: i32,
}

#[derive(Clone, Copy)]
struct Scope {
    previous: i32,
    filter: u32,
    handler: u32,
}

fn table(env: &Env<'_>, frame: u32, cookie: Option<u32>) -> Result<Table, MemFault> {
    let mut entries = GuestPtr::<u32>::new(frame.wrapping_add(8)).read(env.cpu)?;
    if let Some(cookie) = cookie {
        entries ^= GuestPtr::<u32>::new(cookie).read(env.cpu)?;
    }
    Ok(Table {
        frame,
        entries: entries.wrapping_add(if cookie.is_some() { 16 } else { 0 }),
        root: if cookie.is_some() { -2 } else { -1 },
    })
}

fn scope(env: &Env<'_>, table: Table, state: i32) -> Result<Scope, MemFault> {
    let address = u32::try_from(state)
        .ok()
        .and_then(|s| s.checked_mul(12))
        .and_then(|offset| table.entries.checked_add(offset))
        .ok_or(MemFault {
            addr: table.entries,
            len: u32::MAX,
        })?;
    let [previous, filter, handler] = GuestPtr::<[u32; 3]>::new(address).read(env.cpu)?;
    Ok(Scope {
        previous: previous as i32,
        filter,
        handler,
    })
}

fn state(env: &Env<'_>, table: Table) -> Result<i32, MemFault> {
    GuestPtr::<i32>::new(table.frame.wrapping_add(12)).read(env.cpu)
}

fn set_state(env: &mut Env<'_>, table: Table, state: i32) -> Result<(), MemFault> {
    GuestPtr::new(table.frame.wrapping_add(12)).write(env.cpu, state)
}

fn local(env: &mut Env<'_>) -> Outcome {
    let four = env.sys.thunks.get(env.thunk).name == "_local_unwind4";
    let frame = env.arg(u32::from(four));
    let stop = env.arg(u32::from(four) + 1) as i32;
    match table(env, frame, four.then(|| env.arg(0))) {
        Ok(table) => unwind_local(env, table, stop, 0, Box::new(|_| Outcome::Return(0))),
        Err(fault) => memory_failure(fault),
    }
}

pub(super) fn unwind_to(env: &mut Env<'_>, frame: u32, stop: i32, then: Done) -> Outcome {
    match table(env, frame, None) {
        Ok(table) => unwind_local(env, table, stop, 0, then),
        Err(fault) => memory_failure(fault),
    }
}

fn unwind_local(
    env: &mut Env<'_>,
    table: Table,
    stop: i32,
    mut visited: u32,
    then: Done,
) -> Outcome {
    loop {
        let current = match state(env, table) {
            Ok(state) => state,
            Err(fault) => return memory_failure(fault),
        };
        if current == table.root || current == stop {
            return then(env);
        }
        if visited >= 65536 {
            return Outcome::Fail("cyclic CRT local-unwind scope table".into());
        }
        let scope = match scope(env, table, current) {
            Ok(scope) => scope,
            Err(fault) => return memory_failure(fault),
        };
        // Publish the outer state before calling a finally funclet. A nested
        // exception/unwind cannot run the same cleanup a second time.
        if let Err(fault) = set_state(env, table, scope.previous) {
            return memory_failure(fault);
        }
        if scope.filter == 0 && scope.handler != 0 {
            let saved_ebp = env.cpu.reg(Reg::Ebp);
            env.cpu.set_reg(Reg::Ebp, table.frame.wrapping_add(16));
            return Outcome::call(scope.handler, [], move |env, _| {
                env.cpu.set_reg(Reg::Ebp, saved_ebp);
                unwind_local(env, table, stop, visited + 1, then)
            })
            .unwindable();
        }
        visited += 1;
    }
}

fn global(env: &mut Env<'_>) -> Outcome {
    unwind_global(env, env.arg(0), 0, Box::new(|_| Outcome::Return(0)))
}

pub(super) fn unwind_global(env: &mut Env<'_>, target: u32, record: u32, then: Done) -> Outcome {
    match crate::native::public_entry(env, "kernel32.dll", "RtlUnwind") {
        Ok(entry) => {
            Outcome::call(entry, [target, 0, record, 0], move |env, _| then(env)).unwindable()
        }
        Err(reason) => Outcome::Fail(format!("CRT live RtlUnwind resolution: {reason}")),
    }
}

fn except(env: &mut Env<'_>) -> Outcome {
    begin(env, env.arg(0), env.arg(1), env.arg(2), None)
}

fn except4(env: &mut Env<'_>) -> Outcome {
    let (cookie, check, record, frame, context) =
        (env.arg(0), env.arg(1), env.arg(2), env.arg(3), env.arg(4));
    let decoded = match GuestPtr::<u32>::new(frame.wrapping_add(8))
        .read(env.cpu)
        .and_then(|encoded| {
            GuestPtr::<u32>::new(cookie)
                .read(env.cpu)
                .map(|key| encoded ^ key)
        }) {
        Ok(address) => address,
        Err(fault) => return memory_failure(fault),
    };
    let offsets = match GuestPtr::<[i32; 4]>::new(decoded).read(env.cpu) {
        Ok(offsets) => offsets,
        Err(fault) => return memory_failure(fault),
    };
    verify_cookies(
        env,
        frame,
        check,
        offsets.to_vec(),
        Box::new(move |env| begin(env, record, frame, context, Some(cookie))),
    )
}

fn verify_cookies(
    env: &mut Env<'_>,
    frame: u32,
    check: u32,
    mut offsets: Vec<i32>,
    then: Done,
) -> Outcome {
    if offsets.is_empty() {
        return then(env);
    }
    let offset = offsets.remove(0);
    let xor = offsets.remove(0);
    if offset == -2 {
        return verify_cookies(env, frame, check, offsets, then);
    }
    let ebp = frame.wrapping_add(16);
    let value = match GuestPtr::<u32>::new(ebp.wrapping_add(offset as u32)).read(env.cpu) {
        Ok(value) => value ^ ebp.wrapping_add(xor as u32),
        Err(fault) => return memory_failure(fault),
    };
    // The compiler-provided __security_check_cookie callback uses ECX on x86,
    // not a guessed stack parameter. Call the actual guest callback, including
    // its terminating failure path and any guest replacement of that routine.
    let ecx = env.cpu.reg(Reg::Ecx);
    env.cpu.set_reg(Reg::Ecx, value);
    Outcome::call(check, [], move |env, _| {
        env.cpu.set_reg(Reg::Ecx, ecx);
        verify_cookies(env, frame, check, offsets, then)
    })
    .unwindable()
}

fn begin(env: &mut Env<'_>, record: u32, frame: u32, context: u32, cookie: Option<u32>) -> Outcome {
    let table = match table(env, frame, cookie) {
        Ok(table) => table,
        Err(fault) => return memory_failure(fault),
    };
    let flags = match GuestPtr::<u32>::new(record.wrapping_add(4)).read(env.cpu) {
        Ok(flags) => flags,
        Err(fault) => return memory_failure(fault),
    };
    if flags & 6 != 0 {
        return unwind_local(env, table, table.root, 0, Box::new(|_| Outcome::Return(1)));
    }
    let scratch = env.cpu.reg(Reg::Esp).wrapping_sub(16) & !3;
    if let Err(fault) = GuestPtr::new(scratch).write(env.cpu, [record, context]) {
        return memory_failure(fault);
    }
    if let Err(fault) = GuestPtr::new(frame.wrapping_sub(4)).write(env.cpu, scratch) {
        return memory_failure(fault);
    }
    match state(env, table) {
        Ok(state) => search(env, table, state, record, scratch, 0),
        Err(fault) => memory_failure(fault),
    }
}

fn search(
    env: &mut Env<'_>,
    table: Table,
    mut current: i32,
    record: u32,
    scratch: u32,
    mut visited: u32,
) -> Outcome {
    let scope = loop {
        if current == table.root {
            return Outcome::Return(1);
        }
        if visited >= 65536 {
            return Outcome::Fail("cyclic CRT filter scope table".into());
        }
        let scope = match scope(env, table, current) {
            Ok(scope) => scope,
            Err(fault) => return memory_failure(fault),
        };
        if scope.filter != 0 {
            break scope;
        }
        current = scope.previous;
        visited += 1;
    };
    let saved_ebp = env.cpu.reg(Reg::Ebp);
    env.cpu.set_reg(Reg::Ebp, table.frame.wrapping_add(16));
    Outcome::call_on_stack(
        scope.filter,
        [],
        scratch.min(env.cpu.reg(Reg::Esp)),
        move |env, result| {
            env.cpu.set_reg(Reg::Ebp, saved_ebp);
            match result.value() as i32 {
                value if value < 0 => Outcome::Return(0),
                0 => search(env, table, scope.previous, record, scratch, visited + 1),
                _ => unwind_global(
                    env,
                    table.frame,
                    record,
                    Box::new(move |env| {
                        unwind_local(
                            env,
                            table,
                            current,
                            0,
                            Box::new(move |env| {
                                if let Err(fault) = set_state(env, table, scope.previous) {
                                    return memory_failure(fault);
                                }
                                let stack = match GuestPtr::<u32>::new(table.frame.wrapping_sub(8))
                                    .read(env.cpu)
                                {
                                    Ok(stack) => stack,
                                    Err(fault) => return memory_failure(fault),
                                };
                                match env.checkpoint_for_stack_unwind(stack) {
                                    Ok(mut checkpoint) => {
                                        checkpoint.state.regs[5] = table.frame.wrapping_add(16);
                                        checkpoint.state.eip = scope.handler;
                                        Outcome::ResumeExecution(checkpoint)
                                    }
                                    Err(reason) => {
                                        Outcome::Fail(format!("CRT except continuation: {reason}"))
                                    }
                                }
                            }),
                        )
                    }),
                ),
            }
        },
    )
    .unwindable()
}

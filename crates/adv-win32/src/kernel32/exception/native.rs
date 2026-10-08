//! Native exception boundaries retain the original machine checkpoint. Pending
//! deliveries are borrowed weakly; nonlocal continuation or thread exit drops
//! their owning callback, so a stale delivery cannot resume another exception.
use super::*;
use adv_machine::{Conv, GuestCode};
use std::cell::RefCell;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Source {
    Hardware,
    Raise,
    NativeRaise,
    NativeService,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Ki,
    Continue,
    NativeRaise,
}
enum Payload {
    Search(Search),
    Checkpoint(Checkpoint),
}
struct Delivery {
    kind: Kind,
    record: u32,
    context: u32,
    value: Weak<RefCell<Option<Payload>>>,
}
#[derive(Default)]
struct Deliveries(BTreeMap<adv_machine::CtxId, Vec<Delivery>>);

fn retain(
    env: &mut Env<'_>,
    kind: Kind,
    record: u32,
    context: u32,
    value: Payload,
) -> Rc<RefCell<Option<Payload>>> {
    let owned = Rc::new(RefCell::new(Some(value)));
    let pending = env
        .ext
        .get_or_default::<Deliveries>()
        .0
        .entry(env.context)
        .or_default();
    pending.retain(|item| item.value.strong_count() != 0);
    pending.push(Delivery {
        kind,
        record,
        context,
        value: Rc::downgrade(&owned),
    });
    owned
}
fn take(env: &mut Env<'_>, kind: Kind, record: u32, context: u32) -> Option<Payload> {
    let pending = env
        .ext
        .get_or_default::<Deliveries>()
        .0
        .get_mut(&env.context)?;
    let item = pending.iter().rev().find(|item| {
        item.kind == kind
            && item.record == record
            && item.context == context
            && item.value.strong_count() != 0
    })?;
    let owned = item.value.upgrade()?;
    let value = owned.borrow_mut().take();
    value
}
fn unsupported(env: &mut Env<'_>, name: &str, reason: &str) -> Outcome {
    adv_abi::warn_structural(env, name, reason);
    Outcome::Fail(format!("{name}: {reason}"))
}

// x86 KiUserExceptionDispatcher has no return address: [esp] is the record,
// [esp+4] the CONTEXT. An authored native breakpoint fixture verifies this.
static JUMP: GuestCode = GuestCode {
    code: &[0x8d, 0x64, 0x24, 0x04, 0xff, 0x64, 0x24, 0x08],
    host: &[],
    links: &[],
};
pub(super) fn register(reg: &mut Registry) {
    reg.add(
        "<win32>",
        "<exception_jump>",
        Conv::Cdecl { argc: 3 },
        |env| {
            let target = env.arg(2);
            let mut checkpoint = match env.checkpoint() {
                Ok(value) => value,
                Err(error) => return Outcome::Fail(error.to_string()),
            };
            checkpoint.state.regs[4] += 4;
            checkpoint.state.eip = target;
            Outcome::ResumeExecution(checkpoint)
        },
    );
    reg.set_guest("<win32>", "<exception_jump>", &JUMP);
    reg.add(
        "ntdll.dll",
        "KiUserExceptionDispatcher",
        Conv::Cdecl { argc: 0 },
        ki,
    );
    crate::stdcall(reg, "ntdll.dll", "RtlRaiseException", 1, rtl_raise);
    crate::stdcall(reg, "ntdll.dll", "NtContinue", 2, nt_continue);
    crate::stdcall(reg, "ntdll.dll", "NtRaiseException", 3, nt_raise);
}

pub(super) fn deliver(env: &mut Env<'_>, search: Search, source: Source) -> Outcome {
    if source == Source::Raise {
        match crate::native::overridden(env, "RtlRaiseException") {
            Ok(Some(target)) => {
                let scratch = search.scratch;
                // The actual RTL call captures its own context and returns
                // through the hook. Only then finish the Win32 front call.
                return Outcome::call_on_stack(target, [scratch], scratch, move |_, _| {
                    drop(search._phase);
                    Outcome::ResumeExecution(search.checkpoint.unwind_callbacks())
                })
                .unwindable();
            }
            Err(_) => return crate::native::unavailable(env, "RtlRaiseException"),
            Ok(None) => {}
        }
    }
    if matches!(source, Source::Raise | Source::NativeRaise) {
        match crate::native::overridden(env, "NtRaiseException") {
            Ok(Some(target)) => {
                let scratch = search.scratch;
                let context = scratch + CONTEXT;
                let owned = retain(
                    env,
                    Kind::NativeRaise,
                    scratch,
                    context,
                    Payload::Search(search),
                );
                return Outcome::call_on_stack(
                    target,
                    [scratch, context, 1],
                    scratch,
                    move |env, _| {
                        drop(owned);
                        unsupported(
                            env,
                            "NtRaiseException",
                            "native exception delivery returned without continuing its context",
                        )
                    },
                )
                .unwindable();
            }
            Err(_) => return crate::native::unavailable(env, "NtRaiseException"),
            Ok(None) => {}
        }
    }
    // Both hardware stops and raised software exceptions enter the same raw
    // dispatcher. The authored native fixture observes both deliveries.
    match crate::native::overridden(env, "KiUserExceptionDispatcher") {
        Ok(Some(target)) => {
            let scratch = search.scratch;
            let context = scratch + CONTEXT;
            let owned = retain(env, Kind::Ki, scratch, context, Payload::Search(search));
            let jump = match env
                .sys
                .thunks
                .resolve(env.cpu, "<win32>", "<exception_jump>")
            {
                Ok(jump) => jump,
                Err(error) => return Outcome::Fail(error.to_string()),
            };
            Outcome::call_on_stack(jump, [scratch, context, target], scratch, move |env, _| {
                drop(owned);
                unsupported(
                    env,
                    "KiUserExceptionDispatcher",
                    "exception dispatcher returned instead of continuing its captured context",
                )
            })
            .unwindable()
        }
        Err(_) => crate::native::unavailable(env, "KiUserExceptionDispatcher"),
        Ok(None) => super::search(env, search),
    }
}

fn ki(env: &mut Env<'_>) -> Outcome {
    let esp = env.cpu.reg(Reg::Esp);
    let (Ok(record), Ok(context)) = (env.cpu.read_u32(esp), env.cpu.read_u32(esp + 4)) else {
        return Outcome::Fail("unreadable native exception entry frame".into());
    };
    match take(env, Kind::Ki, record, context) {
        Some(Payload::Search(mut search)) => {
            search.record = match read_record(env, record) {
                Ok(record) => record,
                Err(error) => return Outcome::Fail(error),
            };
            super::search(env, search)
        }
        _ => unsupported(
            env,
            "ntdll.dll!KiUserExceptionDispatcher",
            "no matching hardware exception delivery",
        ),
    }
}

fn read_record(env: &mut Env<'_>, address: u32) -> Result<Record, String> {
    let mut bytes = [0; 80];
    if !env.cpu.accessible(address, 80, Prot::READ) {
        return Err("unreadable exception record".into());
    }
    env.cpu
        .read(address, &mut bytes)
        .map_err(|e| e.to_string())?;
    let word = |offset: usize| u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
    let count = word(16) as usize;
    if count > 15 {
        return Err("exception parameter count exceeds native record".into());
    }
    Ok(Record {
        code: word(0),
        flags: word(4),
        address: word(12),
        count,
        information: std::array::from_fn(|index| word(20 + index * 4)),
    })
}
fn rtl_raise(env: &mut Env<'_>) -> Outcome {
    let address = env.arg(0);
    let mut record = match read_record(env, address) {
        Ok(record) => record,
        Err(error) => return Outcome::Fail(error),
    };
    // RtlRaiseException overwrites the supplied ExceptionAddress with its
    // caller's return address, including calls through a relocated hook body.
    record.address = match env.cpu.read_u32(env.cpu.reg(Reg::Esp)) {
        Ok(caller) => caller,
        Err(error) => return Outcome::Fail(error.to_string()),
    };
    let Some(field) = address.checked_add(12) else {
        return Outcome::Fail("exception address field overflows guest memory".into());
    };
    if !env.cpu.accessible(field, 4, Prot::WRITE) {
        return Outcome::Fail("unwritable native exception address field".into());
    }
    if let Err(error) = env.cpu.write_u32(field, record.address) {
        return Outcome::Fail(error.to_string());
    }
    let checkpoint = match env.checkpoint() {
        Ok(checkpoint) => checkpoint,
        Err(error) => return Outcome::Fail(error.to_string()),
    };
    super::dispatch_origin(env, checkpoint, record, Source::NativeRaise)
}

fn nt_raise(env: &mut Env<'_>) -> Outcome {
    let (record, context, first) = (env.arg(0), env.arg(1), env.arg(2));
    let record_value = match read_record(env, record) {
        Ok(record) => record,
        Err(_) => return Outcome::Return(0xc0000005),
    };
    let mut bytes = [0; context::SIZE];
    if !env
        .cpu
        .accessible(context, context::SIZE as u32, Prot::READ)
        || env.cpu.read(context, &mut bytes).is_err()
    {
        return Outcome::Return(0xc0000005);
    }
    let mut checkpoint = match take(env, Kind::NativeRaise, record, context) {
        Some(Payload::Search(search)) => search.checkpoint,
        _ => match env.checkpoint() {
            Ok(checkpoint) => checkpoint,
            Err(error) => return Outcome::Fail(error.to_string()),
        },
    };
    if let Err(reason) = context::decode(&bytes, &mut checkpoint.state) {
        adv_abi::warn_unavailable(env, "ntdll.dll!NtRaiseException", &reason);
        return Outcome::Return(0xc00000bb);
    }
    if first as u8 == 0 {
        // With no guest debugger, a second-chance service request is terminal.
        // It must not restart VEH/SEH or call the user-mode top-level filter.
        // Use the existing Windows failure owner so child process exit status
        // and diagnostics retain the supplied exception code.
        return terminal(env, &record_value);
    }
    super::dispatch_origin(env, checkpoint, record_value, Source::NativeService)
}

fn terminal(env: &mut Env<'_>, record: &Record) -> Outcome {
    env.sys.guest_failure_code = Some(record.code);
    Outcome::Fail(format!(
        "unhandled Windows exception {:#010x} at {:#010x} (second chance)",
        record.code, record.address
    ))
}

/// Exhausted first-chance search reaches the same live NT service, with FALSE.
/// The observer can still end the process; calling the original is terminal.
pub(super) fn unhandled(env: &mut Env<'_>, search: Search) -> Outcome {
    match crate::native::overridden(env, "NtRaiseException") {
        Ok(Some(target)) => {
            let scratch = search.scratch;
            let owned = retain(
                env,
                Kind::NativeRaise,
                scratch,
                scratch + CONTEXT,
                Payload::Search(search),
            );
            Outcome::call_on_stack(
                target,
                [scratch, scratch + CONTEXT, 0],
                scratch,
                move |env, _| {
                    drop(owned);
                    unsupported(
                        env,
                        "ntdll.dll!NtRaiseException",
                        "second-chance delivery returned without completing the exception",
                    )
                },
            )
            .unwindable()
        }
        Ok(None) => match read_record(env, search.scratch) {
            Ok(record) => terminal(env, &record),
            Err(reason) => Outcome::Fail(reason),
        },
        Err(_) => crate::native::unavailable(env, "NtRaiseException"),
    }
}

pub(super) fn continue_execution(
    env: &mut Env<'_>,
    checkpoint: Checkpoint,
    context: u32,
    scratch: u32,
) -> Outcome {
    match crate::native::overridden(env, "NtContinue") {
        Ok(Some(target)) => {
            let owned = retain(
                env,
                Kind::Continue,
                0,
                context,
                Payload::Checkpoint(checkpoint),
            );
            Outcome::call_on_stack(
                target,
                [context, 0],
                scratch.min(env.cpu.reg(Reg::Esp)),
                move |env, _| {
                    drop(owned);
                    unsupported(
                        env,
                        "ntdll.dll!NtContinue",
                        "continuation returned without restoring its captured context",
                    )
                },
            )
            .unwindable()
        }
        Err(_) => crate::native::unavailable(env, "NtContinue"),
        Ok(None) => restore_context(env, checkpoint, context),
    }
}
fn nt_continue(env: &mut Env<'_>) -> Outcome {
    let context = env.arg(0);
    if env.arg(1) & 0xff != 0 {
        adv_abi::warn_unavailable(
            env,
            "ntdll.dll!NtContinue",
            "alert testing on context restoration is unavailable",
        );
        return Outcome::Return(0xc00000bb);
    }
    if !env
        .cpu
        .accessible(context, context::SIZE as u32, Prot::READ)
    {
        return Outcome::Return(0xc0000005);
    }
    let checkpoint = match take(env, Kind::Continue, 0, context) {
        Some(Payload::Checkpoint(checkpoint)) => checkpoint,
        _ => match env.checkpoint() {
            Ok(checkpoint) => checkpoint,
            Err(error) => return Outcome::Fail(error.to_string()),
        },
    };
    restore_context(env, checkpoint, context)
}

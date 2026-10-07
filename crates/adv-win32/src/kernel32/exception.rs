//! x86 Windows exception records and handler search. CPU stops and callback
//! scheduling stay in adv-machine; all ABI layouts and dispositions live here.
pub(super) mod context;
mod mapping;
pub(super) mod phase;
mod system_service;
pub(crate) use phase::uncaught_query_blocker;

use adv_abi::{GuestPtr, Registry, win32};
use adv_cpu::backend::{Backend, GuestExt, Prot, Reg};
use adv_machine::exception::{Checkpoint, Exception};
use adv_machine::{Env, Identity, Machine, Namespace, Outcome};
use std::collections::VecDeque;
use std::{
    collections::BTreeMap,
    rc::{Rc, Weak},
};

#[derive(Clone, Copy)]
struct VectoredHandler(u32);
#[derive(Default)]
struct Vectored {
    namespace: Option<Namespace>,
    order: VecDeque<Identity<VectoredHandler>>,
}
/// Frame handlers running per context, innermost last. Borrowed observations
/// only: the callback continuation owns each marker, so returning or dropping
/// (unwinding) it ends that handler's call.
#[derive(Default)]
struct FrameDispatch(BTreeMap<adv_machine::CtxId, Vec<Active>>);
struct Active {
    marker: Weak<()>,
    /// Where native code would hold the dispatcher's own registration: the
    /// handler runs on the stack below this dispatch's scratch records.
    scratch: u32,
    establisher: u32,
}
const LIMIT: usize = 1024;
const EXCEPTION_NESTED_CALL: u32 = 0x10;
const CONTEXT: u32 = 0x50;
const POINTERS: u32 = 0x320;
const DISPATCHER: u32 = 0x330;
const HANDLER_FRAME: u32 = 0x340;
const SCRATCH: u32 = 0x360;

pub(crate) fn install<B: Backend>(machine: &mut Machine<B>) {
    machine.sys.exception_handler = Some(cpu_exception);
    machine.ext.insert(Vectored::default());
}

pub(super) fn register(registry: &mut Registry) {
    for api in [
        add_vectored_ABI,
        remove_vectored_ABI,
        raise_ABI,
        debug_break_ABI,
    ] {
        registry.register(api);
    }
}

#[win32(kernel32, name = "DebugBreak", level = implement)]
fn debug_break(env: &mut Env<'_>) -> Outcome {
    // A real INT3 preserves the native breakpoint address and lets the handler
    // edit CONTEXT before execution resumes. Keep this behavior when optional
    // direct guest API routes have been disabled for diagnosis.
    let Some(body) = env.sys.thunks.get(env.thunk).guest_body() else {
        return Outcome::Fail("DebugBreak breakpoint body is missing".into());
    };
    match env.checkpoint() {
        Ok(mut checkpoint) => {
            checkpoint.state.eip = body;
            Outcome::ResumeExecution(checkpoint)
        }
        Err(error) => Outcome::Fail(error.to_string()),
    }
}

#[win32(kernel32, name = "AddVectoredExceptionHandler", level = implement)]
fn add_vectored(env: &mut Env<'_>, first: u32, handler: GuestPtr<()>) -> Outcome {
    let state = env.ext.get_or_default::<Vectored>();
    if state.order.len() >= LIMIT {
        return Outcome::Return(0);
    }
    if state.namespace.is_none() {
        state.namespace = env.sys.objects.new_namespace();
    }
    let Some(namespace) = state.namespace else {
        return Outcome::Return(0);
    };
    let Some(alias) = env
        .sys
        .objects
        .insert_host_in(namespace, VectoredHandler(handler.addr()))
    else {
        return Outcome::Return(0);
    };
    let identity = env.sys.objects.lookup_alias(namespace, alias).unwrap();
    if first != 0 {
        state.order.push_front(identity);
    } else {
        state.order.push_back(identity);
    }
    Outcome::Return(alias)
}

#[win32(kernel32, name = "RemoveVectoredExceptionHandler", level = implement)]
fn remove_vectored(env: &mut Env<'_>, handle: GuestPtr<()>) -> Outcome {
    let state = env.ext.get_or_default::<Vectored>();
    let Some(namespace) = state.namespace else {
        return Outcome::Return(0);
    };
    let Some(identity) = env
        .sys
        .objects
        .lookup_alias::<VectoredHandler>(namespace, handle.addr())
    else {
        return Outcome::Return(0);
    };
    state.order.retain(|entry| *entry != identity);
    env.sys.objects.close_alias(namespace, handle.addr());
    Outcome::Return(1)
}

#[win32(kernel32, name = "RaiseException", level = implement)]
pub(super) fn raise(
    env: &mut Env<'_>,
    code: u32,
    flags: u32,
    count: u32,
    arguments: GuestPtr<u32>,
) -> Outcome {
    let count = count.min(15) as usize;
    let mut information = [0u32; 15];
    if count != 0 && arguments.addr() != 0 {
        let mut bytes = [0; 60];
        if adv_abi::guest::check_range(env.cpu, arguments.addr(), count as u32 * 4, Prot::READ)
            .is_err()
            || env
                .cpu
                .read(arguments.addr(), &mut bytes[..count * 4])
                .is_err()
        {
            return Outcome::Fail("RaiseException: unreadable argument array".into());
        }
        for (slot, bytes) in information[..count]
            .iter_mut()
            .zip(bytes[..count * 4].as_chunks::<4>().0)
        {
            *slot = u32::from_le_bytes(*bytes);
        }
    }
    let checkpoint = match env.checkpoint() {
        Ok(value) => value,
        Err(error) => return Outcome::Fail(error.to_string()),
    };
    // Native software exceptions identify an instruction inside RaiseException,
    // not its application's caller. Resume the actually executed thunk epilogue;
    // it performs the ordinary stdcall cleanup, including relocated hook thunks.
    let address = checkpoint.state.eip;
    dispatch(
        env,
        checkpoint,
        Record {
            code: code & !0x1000_0000,
            flags: flags & 1,
            address,
            count: if arguments.addr() == 0 { 0 } else { count },
            information,
        },
    )
}

fn cpu_exception(env: &mut Env<'_>, event: Exception, mut checkpoint: Checkpoint) -> Outcome {
    if let Exception::Trap { vector: 0x2e, pc } = &event {
        if *pc != checkpoint.state.eip {
            return Outcome::Fail(
                "native system-service PC disagrees with captured x86 EIP".into(),
            );
        }
        if let Err(reason) = mapping::raw_int2e_address(env.cpu, *pc) {
            adv_abi::warn_unavailable(env, "Win32 exception mapping", &reason);
            return Outcome::Fail(reason);
        }
        // This native x86 OS profile publishes an empty numeric directory. Keep
        // the retired PC and every other register; never dereference EDX or
        // borrow a host thunk token to guess a service implementation.
        adv_abi::warn_unavailable(
            env,
            "native x86 INT 2E",
            "direct-service numeric directory has no registered services in the native x86 guest profile",
        );
        checkpoint.state.regs[0] = system_service::failure(checkpoint.state.regs[0]);
        return Outcome::ResumeExecution(checkpoint);
    }
    let record = match mapping::map(env.cpu, &event, &checkpoint.state) {
        Ok(record) => record,
        Err(reason) => {
            adv_abi::warn_unavailable(env, "Win32 exception mapping", &reason);
            return Outcome::Fail(reason);
        }
    };
    mapping::normalize_context(&record, &mut checkpoint.state);
    dispatch(env, checkpoint, record)
}

struct Record {
    code: u32,
    flags: u32,
    address: u32,
    count: usize,
    information: [u32; 15],
}
struct Search {
    checkpoint: Checkpoint,
    scratch: u32,
    record: Record,
    vectored: VecDeque<Identity<VectoredHandler>>,
    frame: Option<u32>,
    searched: usize,
    // Survives vectored/frame callbacks, and expires on resume or nonlocal exit.
    _phase: Rc<phase::Phase>,
    /// (scratch, establisher) of interrupted frame handlers, innermost first.
    outer: VecDeque<(u32, u32)>,
    /// Native NestedFrame: the highest establisher whose handler was running.
    nested: Option<u32>,
}

fn dispatch(env: &mut Env<'_>, checkpoint: Checkpoint, record: Record) -> Outcome {
    let mut outer: Vec<_> = env
        .ext
        .try_get::<FrameDispatch>()
        .and_then(|state| state.0.get(&env.context))
        .into_iter()
        .flatten()
        .filter(|active| active.marker.strong_count() != 0)
        .map(|active| (active.scratch, active.establisher))
        .collect();
    outer.sort_unstable();
    // Guest stack owns the transient records. Callback frames live below them;
    // cancellation/exit needs no side heap allocation or cleanup continuation.
    let Some(scratch) = env
        .cpu
        .reg(Reg::Esp)
        .checked_sub(SCRATCH + 15)
        .map(|sp| sp & !15)
    else {
        return Outcome::Fail("exception scratch stack underflow".into());
    };
    if scratch < env.ctx().stack_lo || !env.cpu.accessible(scratch, SCRATCH, Prot::RW) {
        return Outcome::Fail("exception scratch needs writable guest stack".into());
    }
    let mut bytes = [0; SCRATCH as usize];
    let word = |bytes: &mut [u8], offset: usize, value: u32| {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes())
    };
    word(&mut bytes, 0, record.code);
    word(&mut bytes, 4, record.flags);
    word(&mut bytes, 12, record.address);
    word(&mut bytes, 16, record.count as u32);
    for (index, value) in record.information[..record.count].iter().enumerate() {
        word(&mut bytes, 20 + index * 4, *value);
    }
    bytes[CONTEXT as usize..CONTEXT as usize + context::SIZE]
        .copy_from_slice(&context::encode(&checkpoint.state));
    word(&mut bytes, POINTERS as usize, scratch);
    word(&mut bytes, POINTERS as usize + 4, scratch + CONTEXT);
    if let Err(error) = env.cpu.write(scratch, &bytes) {
        return Outcome::Fail(error.to_string());
    }
    let vectored = env.ext.get_or_default::<Vectored>().order.clone();
    let active = phase::enter(env, phase::Phase::Dispatch, record.code);
    search(
        env,
        Search {
            checkpoint,
            scratch,
            record,
            vectored,
            frame: None,
            searched: 0,
            _phase: active,
            outer: outer.into(),
            nested: None,
        },
    )
}

/// Bytes of the frame `handler_frame` writes.
pub(super) const HANDLER_FRAME_SIZE: u32 = 28;

/// Lay out ntdll's ExecuteHandler2 frame at `at` and point EBP at it before a
/// frame-based handler runs: saved EBP, return slot, then the handler's four
/// arguments and the handler. Borland's handler entry reads its arguments as
/// `[ebp+8..+0x14]` without setting up a frame of its own.
pub(super) fn handler_frame(
    env: &mut Env<'_>,
    at: u32,
    args: [u32; 4],
    handler: u32,
) -> Result<(), String> {
    let mut bytes = [0; HANDLER_FRAME_SIZE as usize];
    let words = [
        env.cpu.reg(Reg::Ebp),
        0,
        args[0],
        args[1],
        args[2],
        args[3],
        handler,
    ];
    for (slot, word) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(words) {
        *slot = word.to_le_bytes();
    }
    env.cpu.write(at, &bytes).map_err(|e| e.to_string())?;
    env.cpu.set_reg(Reg::Ebp, at);
    Ok(())
}

/// Native ExceptionFlags live in the guest record, which handlers may edit.
fn nested_flag(env: &mut Env<'_>, search: &mut Search, set: bool) -> Result<(), String> {
    let address = search.scratch + 4;
    let flags = env.cpu.read_u32(address).map_err(|e| e.to_string())?;
    let flags = if set {
        flags | EXCEPTION_NESTED_CALL
    } else {
        flags & !EXCEPTION_NESTED_CALL
    };
    search.record.flags = flags;
    env.cpu.write_u32(address, flags).map_err(|e| e.to_string())
}

fn search(env: &mut Env<'_>, mut search: Search) -> Outcome {
    while let Some(identity) = search.vectored.pop_front() {
        let Some(handler) = env.sys.objects.owned(identity).copied() else {
            continue;
        };
        return Outcome::call_on_stack(
            handler.0,
            [search.scratch + POINTERS],
            search.scratch,
            move |env, result| match result.value() {
                u32::MAX => resume(env, search),
                0 => self::search(env, search),
                value => {
                    Outcome::Fail(format!("invalid vectored exception disposition {value:#x}"))
                }
            },
        )
        .unwindable();
    }
    // A vectored handler can repair the live registration head before declining.
    let frame = match search.frame {
        Some(frame) => frame,
        None => match env.cpu.read_u32(env.ctx().teb) {
            Ok(frame) => frame,
            Err(error) => return Outcome::Fail(format!("SEH head read failed: {error}")),
        },
    };
    // Passing an interrupted handler's dispatcher registration: native
    // RtlpExceptionHandler answers ExceptionNestedException with that
    // handler's establisher frame.
    while let Some(&(scratch, establisher)) = search.outer.front()
        && frame > scratch
    {
        search.outer.pop_front();
        if let Err(error) = nested_flag(env, &mut search, true) {
            return Outcome::Fail(format!("nested exception record write failed: {error}"));
        }
        search.nested = Some(search.nested.map_or(establisher, |n| n.max(establisher)));
    }
    if frame == u32::MAX {
        env.sys.guest_failure_code = Some(search.record.code);
        return Outcome::Fail(format!(
            "unhandled Windows exception {:#010x} at {:#010x}",
            search.record.code, search.record.address
        ));
    }
    if search.searched >= LIMIT
        || frame & 3 != 0
        || frame < env.ctx().stack_lo
        || frame
            .checked_add(8)
            .is_none_or(|end| end > env.ctx().stack_hi)
        || !env.cpu.accessible(frame, 8, Prot::READ)
    {
        return Outcome::Fail(format!("invalid or cyclic SEH chain at {frame:#x}"));
    }
    let next = match env.cpu.read_u32(frame) {
        Ok(value) => value,
        Err(error) => return Outcome::Fail(format!("SEH chain read failed: {error}")),
    };
    if next != u32::MAX && next <= frame {
        return Outcome::Fail("SEH chain is not ascending".into());
    }
    let handler = match env.cpu.read_u32(frame + 4) {
        Ok(value) => value,
        Err(error) => return Outcome::Fail(format!("SEH handler read failed: {error}")),
    };
    search.frame = Some(next);
    search.searched += 1;
    let active = Rc::new(());
    let running = env
        .ext
        .get_or_default::<FrameDispatch>()
        .0
        .entry(env.context)
        .or_default();
    running.retain(|active| active.marker.strong_count() != 0);
    running.push(Active {
        marker: Rc::downgrade(&active),
        scratch: search.scratch,
        establisher: frame,
    });
    let args = [
        search.scratch,
        frame,
        search.scratch + CONTEXT,
        search.scratch + DISPATCHER,
    ];
    if let Err(error) = handler_frame(env, search.scratch + HANDLER_FRAME, args, handler) {
        return Outcome::Fail(format!("SEH handler frame write failed: {error}"));
    }
    Outcome::call_on_stack(handler, args, search.scratch, move |env, result| {
        drop(active);
        if search.nested == Some(frame) {
            search.nested = None;
            if let Err(error) = nested_flag(env, &mut search, false) {
                return Outcome::Fail(format!("nested exception record write failed: {error}"));
            }
        }
        match result.value() {
            0 => resume(env, search),
            1 => self::search(env, search),
            value => Outcome::Fail(format!("unsupported SEH disposition {value:#x}")),
        }
    })
    // An `__except` or catch target resumes on the establisher's stack and
    // never returns here; dropping this continuation also ends the phase.
    .unwindable()
}

fn resume(env: &mut Env<'_>, mut search: Search) -> Outcome {
    if search.record.flags & 1 != 0 {
        return Outcome::Fail("cannot continue a noncontinuable Windows exception".into());
    }
    let address = search.scratch + CONTEXT;
    if !env
        .cpu
        .accessible(address, context::SIZE as u32, Prot::READ)
    {
        return Outcome::Fail("exception CONTEXT became unreadable".into());
    }
    let mut bytes = [0; context::SIZE];
    if let Err(error) = env.cpu.read(address, &mut bytes) {
        return Outcome::Fail(error.to_string());
    }
    if let Err(error) = context::decode(&bytes, &mut search.checkpoint.state) {
        return Outcome::Fail(error);
    }
    Outcome::ResumeExecution(search.checkpoint)
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_machine::{Clock, ObjectLimits, Objects};
    #[test]
    fn vectored_registration_has_one_bounded_owner_and_no_kernel_handle_lifetime() {
        let mut machine = Machine::new(
            adv_cpu::JitCpu::new(adv_machine::layout::PARK_ADDR, false).unwrap(),
            Registry::default(),
            Clock::virtual_time(),
        )
        .unwrap();
        machine.sys.objects = Objects::with_limits(ObjectLimits {
            objects: 2,
            ..ObjectLimits::default()
        });
        let kernel = machine.sys.objects.insert_host(123_u32);
        let mut env = Env {
            context: machine.sys.current,
            cpu: &mut machine.cpu,
            sys: &mut machine.sys,
            ext: &mut machine.ext,
            thunk: 0,
        };
        let Outcome::Return(first) = add_vectored(&mut env, 0, GuestPtr::new(0x400000)) else {
            panic!()
        };
        assert_ne!(first, 0);
        assert_eq!(env.sys.objects.object_count(), 2);
        assert!(matches!(
            add_vectored(&mut env, 1, GuestPtr::new(0x400001)),
            Outcome::Return(0)
        ));
        env.sys.objects.close(kernel);
        assert_eq!(env.sys.objects.object_count(), 1);
        let identity = env.ext.get::<Vectored>().order[0];
        assert_eq!(env.sys.objects.owned(identity).unwrap().0, 0x400000);
        assert!(matches!(
            remove_vectored(&mut env, GuestPtr::new(first)),
            Outcome::Return(1)
        ));
        assert!(env.sys.objects.owned(identity).is_none());
        assert_eq!(env.sys.objects.object_count(), 0);
        assert!(matches!(
            remove_vectored(&mut env, GuestPtr::new(first)),
            Outcome::Return(0)
        ));
        let Outcome::Return(second) = add_vectored(&mut env, 0, GuestPtr::new(0x400100)) else {
            panic!()
        };
        assert_ne!(second, first);
        assert_ne!(second, 0);
    }
}

//! x86 Windows exception records and handler search. CPU stops and callback
//! scheduling stay in adv-machine; all ABI layouts and dispositions live here.
mod context;
pub(super) mod phase;
pub(crate) use phase::uncaught_query_blocker;

use adv_abi::{GuestPtr, Registry, win32};
use adv_cpu::backend::{Backend, FaultKind, GuestExt, Prot, Reg};
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
/// Borrowed phase observations only. The existing callback continuation owns
/// each marker; returning or dropping that continuation releases the phase.
#[derive(Default)]
struct FrameDispatch(BTreeMap<adv_machine::CtxId, Weak<()>>);
const LIMIT: usize = 1024;
const CONTEXT: u32 = 0x50;
const POINTERS: u32 = 0x320;
const DISPATCHER: u32 = 0x330;
const SCRATCH: u32 = 0x340;

pub(crate) fn install<B: Backend>(machine: &mut Machine<B>) {
    machine.sys.exception_handler = Some(cpu_exception);
    machine.ext.insert(Vectored::default());
}

pub(super) fn register(registry: &mut Registry) {
    for api in [add_vectored_ABI, remove_vectored_ABI, raise_ABI] {
        registry.register(api);
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
fn raise(
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

fn cpu_exception(env: &mut Env<'_>, event: Exception, checkpoint: Checkpoint) -> Outcome {
    let mut information = [0; 15];
    let (code, count) = match event {
        Exception::Fault(fault) => match fault.kind {
            FaultKind::Read(address) | FaultKind::Write(address) | FaultKind::Fetch(address) => {
                information[0] = match fault.kind {
                    FaultKind::Read(_) => 0,
                    FaultKind::Write(_) => 1,
                    _ => 8,
                };
                information[1] = address;
                (0xc000_0005, 2)
            }
            FaultKind::InvalidInstruction => (0xc000_001d, 0),
            FaultKind::Other(message) => {
                return Outcome::Fail(format!("unclassified CPU fault: {message}"));
            }
        },
        Exception::Trap { vector: 6, .. } => (0xc000_001d, 0),
        other => {
            return Outcome::Fail(format!(
                "Windows exception mapping not implemented for {other:?}"
            ));
        }
    };
    let address = checkpoint.state.eip;
    dispatch(
        env,
        checkpoint,
        Record {
            code,
            flags: 0,
            address,
            count,
            information,
        },
    )
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
}

fn dispatch(env: &mut Env<'_>, checkpoint: Checkpoint, record: Record) -> Outcome {
    if env
        .ext
        .try_get::<FrameDispatch>()
        .and_then(|state| state.0.get(&env.context))
        .and_then(Weak::upgrade)
        .is_some()
    {
        return Outcome::Fail("nested frame-based exception dispatch is not implemented".into());
    }
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
        },
    )
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
    if frame == u32::MAX {
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
    env.ext
        .get_or_default::<FrameDispatch>()
        .0
        .insert(env.context, Rc::downgrade(&active));
    Outcome::call_on_stack(
        handler,
        [
            search.scratch,
            frame,
            search.scratch + CONTEXT,
            search.scratch + DISPATCHER,
        ],
        search.scratch,
        move |env, result| {
            drop(active);
            match result.value() {
                0 => resume(env, search),
                1 => self::search(env, search),
                value => Outcome::Fail(format!("unsupported SEH disposition {value:#x}")),
            }
        },
    )
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

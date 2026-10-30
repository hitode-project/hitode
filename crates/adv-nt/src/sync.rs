//! Synchronization, after Wine's `unix/sync.c` and `server_select`.
//!
//! Waitable objects use the emulated ntsync device when the server supplies
//! it, with Wine's handle cache and access checks. Pending acquisitions park
//! on machine events; server wake-ups and APCs retain the server path.
//!
//! A user APC queued to an alertable wait ends it; the thread then runs the
//! APC in ntdll.dll's `KiUserApcDispatcher`, which resumes the interrupted
//! context through `NtContinue`.

use crate::ntsync;
use adv_cpu::backend::{ArchitecturalState, GuestExt, Reg};
use adv_machine::{Env, Outcome, Wait};
use std::os::fd::AsRawFd;

use crate::exception::{self, Dispatchers};
use crate::object::{Attributes, read_u32, read_unicode_string, write_bytes, write_u32};
use crate::protocol::{
    CancelTimerRequest, CreateEventRequest, CreateMutexRequest, CreateSemaphoreRequest,
    CreateTimerRequest, DupHandleRequest, EventOpRequest, GetTimerInfoRequest,
    OpenSemaphoreRequest, OpenTimerRequest, QuerySemaphoreRequest, QueueApcRequest,
    ReleaseMutexRequest, ReleaseSemaphoreRequest, SelectRequest, SetTimerRequest,
};
use crate::server::{self, Time};
use crate::status::{self, Status};

const NOTIFICATION_EVENT: u32 = 0;
const SYNCHRONIZATION_EVENT: u32 = 1;
const SET_EVENT: i32 = 1;
const RESET_EVENT: i32 = 2;

const SELECT_ALERTABLE: i32 = 1;
const SELECT_INTERRUPTIBLE: i32 = 2;
const SELECT_WAIT: u32 = 1;
const SELECT_WAIT_ALL: u32 = 2;
const MAXIMUM_WAIT_OBJECTS: u32 = 64;
/// `union apc_result`, which every `select` carries ahead of its operation.
const APC_RESULT_SIZE: usize = 40;
/// `union apc_call`, in which the server hands over an APC.
const APC_CALL_SIZE: usize = 64;
const APC_USER: u32 = 1;
/// `struct apc_stack_layout`: the APC, then the CONTEXT it interrupted.
const APC_LAYOUT_SIZE: u32 = 0x308;
const APC_CONTEXT: usize = 0x14;

const TIMEOUT_INFINITE: i64 = i64::MAX;
const USER_APC: Status = 0xc0;
const KERNEL_APC: Status = 0x100;
const INVALID_PARAMETER_1: Status = 0xc000_00ef;

/// How long a wait whose deadline passed without a wake-up blocks before
/// it lets the server run again.
const RETRY_US: u64 = 1000;

/// NtCreateEvent(*handle, access, *attributes, type, initial_state)
pub fn create_event(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[handle, access, attributes, kind, state] = args else {
        unreachable!()
    };
    status::finish((|| {
        write_u32(env, handle, 0)?;
        if kind != NOTIFICATION_EVENT && kind != SYNCHRONIZATION_EVENT {
            return Err(status::INVALID_PARAMETER.into());
        }
        let objattr = match attributes {
            0 => Vec::new(),
            attributes => Attributes::read(env, attributes)?.wire(),
        };
        let mut request = CreateEventRequest {
            access,
            manual_reset: i32::from(kind == NOTIFICATION_EVENT),
            initial_state: i32::from(state as u8 != 0),
            ..Default::default()
        };
        let (reply, _) = server::request(env, &mut request, &[&objattr], &mut [])?;
        if reply.header.error != status::SUCCESS && reply.header.error != status::OBJECT_NAME_EXISTS
        {
            return Err(reply.header.error.into());
        }
        write_u32(env, handle, reply.handle)?;
        status::check(reply.header.error)
    })())
}

/// NtCreateMutant(*handle, access, *attributes, owned)
pub fn create_mutant(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[handle, access, attributes, owned] = args else {
        unreachable!()
    };
    status::finish((|| {
        write_u32(env, handle, 0)?;
        let objattr = match attributes {
            0 => Vec::new(),
            attributes => Attributes::read(env, attributes)?.wire(),
        };
        let mut request = CreateMutexRequest {
            access,
            owned: i32::from(owned as u8 != 0),
            ..Default::default()
        };
        let (reply, _) = server::request(env, &mut request, &[&objattr], &mut [])?;
        if reply.header.error != status::SUCCESS && reply.header.error != status::OBJECT_NAME_EXISTS
        {
            return Err(reply.header.error.into());
        }
        write_u32(env, handle, reply.handle)?;
        status::check(reply.header.error)
    })())
}

/// NtReleaseMutant(handle, *previous_count)
pub fn release_mutant(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[handle, previous] = args else {
        unreachable!()
    };
    status::finish((|| {
        let tid = server::current_tid(env)?;
        if let Some(value) = inproc_operation(env, handle, 3, 0, ntsync::MUTEX_UNLOCK, [tid, 0])? {
            if previous != 0 {
                write_u32(env, previous, 1u32.wrapping_sub(value[1]))?;
            }
            return Ok(());
        }
        let mut request = ReleaseMutexRequest {
            handle,
            ..Default::default()
        };
        let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
        status::check(reply.header.error)?;
        if previous != 0 {
            write_u32(env, previous, 1u32.wrapping_sub(reply.prev_count))?;
        }
        Ok(())
    })())
}

/// NtYieldExecution(): another thread runs if one is ready.
pub fn yield_execution(env: &mut Env<'_>, _: &[u32]) -> Outcome {
    env.sys.poll_observation();
    Outcome::block_then_return(Wait::Yield, status::SUCCESS)
}

/// NtSetEvent(handle, *previous_state)
pub fn set_event(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    event_op(env, args, SET_EVENT)
}

/// NtResetEvent(handle, *previous_state)
pub fn reset_event(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    event_op(env, args, RESET_EVENT)
}

fn event_op(env: &mut Env<'_>, args: &[u32], op: i32) -> Outcome {
    let &[handle, previous] = args else {
        unreachable!()
    };
    status::finish((|| {
        let state = signal(env, handle, op)?;
        if previous != 0 {
            write_u32(env, previous, state)?;
        }
        Ok(())
    })())
}

/// Set, reset or pulse an event, in process when it has an in-process sync,
/// and return its previous state. The server rejects event operations on those.
pub(crate) fn signal(env: &mut Env<'_>, handle: u32, op: i32) -> Result<u32, status::Error> {
    signal_as(env, handle, op, adv_machine::WakePriority::Normal)
}
fn signal_as(
    env: &mut Env<'_>,
    handle: u32,
    op: i32,
    priority: adv_machine::WakePriority,
) -> Result<u32, status::Error> {
    let command = match op {
        SET_EVENT => ntsync::EVENT_SET,
        RESET_EVENT => ntsync::EVENT_RESET,
        _ => ntsync::EVENT_PULSE,
    };
    if let Some(value) = inproc_operation_as(env, handle, 2, 2, command, [0; 2], priority)? {
        return Ok(value[0]);
    }
    let mut request = EventOpRequest {
        handle,
        op,
        ..Default::default()
    };
    let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
    status::check(reply.header.error)?;
    Ok(reply.state as u32)
}

/// NtWaitForSingleObject(handle, alertable, *timeout)
pub fn wait_single(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[handle, alertable, timeout] = args else {
        unreachable!()
    };
    wait(env, &[handle], true, alertable, timeout)
}

/// NtWaitForMultipleObjects(count, *handles, wait_any, alertable, *timeout)
pub fn wait_multiple(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[count, handles, wait_any, alertable, timeout] = args else {
        unreachable!()
    };
    if count == 0 || count > MAXIMUM_WAIT_OBJECTS {
        return Outcome::Return(INVALID_PARAMETER_1);
    }
    let handles: Result<Vec<u32>, Status> = (0..count)
        .map(|index| read_u32(env, handles + 4 * index))
        .collect();
    match handles {
        Ok(handles) if handles.iter().any(|&handle| handle >= u32::MAX - 5) => {
            Outcome::Return(status::INVALID_HANDLE)
        }
        Ok(handles) => wait(env, &handles, wait_any as u8 != 0, alertable, timeout),
        Err(status) => Outcome::Return(status),
    }
}

fn wait(env: &mut Env<'_>, handles: &[u32], any: bool, alertable: u32, timeout: u32) -> Outcome {
    if let Some(outcome) = inproc_wait(env, handles, any, alertable, timeout, None) {
        return outcome;
    }
    let mut op = Vec::with_capacity(4 + 4 * handles.len());
    let code = if any { SELECT_WAIT } else { SELECT_WAIT_ALL };
    for word in std::iter::once(code).chain(handles.iter().copied()) {
        op.extend_from_slice(&word.to_le_bytes());
    }
    let mut flags = SELECT_INTERRUPTIBLE;
    if alertable as u8 != 0 {
        flags |= SELECT_ALERTABLE;
    }
    begin_select(env, op, flags, timeout)
}

/// Waits, not alertable and without a timeout, for `handle`, then passes
/// the wait's status to `then`.
pub fn wait_then(
    env: &mut Env<'_>,
    handle: u32,
    then: impl FnOnce(&mut Env<'_>, Status) -> Outcome + 'static,
) -> Outcome {
    wait_then_alertable(env, handle, false, Box::new(then))
}

type WaitThen = Box<dyn FnOnce(&mut Env<'_>, Status) -> Outcome>;

/// An internal object wait with Wine's requested alertability.
pub fn wait_then_alertable(
    env: &mut Env<'_>,
    handle: u32,
    alertable: bool,
    then: WaitThen,
) -> Outcome {
    match prepare_inproc_wait(env, &[handle], true, u32::from(alertable), 0, None) {
        Ok(wait) => {
            return run_inproc_wait(
                env,
                wait,
                Box::new(move |env, ended| match ended {
                    Ended::Status(code) => then(env, code),
                    Ended::Apc { func, args } => match then(env, USER_APC) {
                        Outcome::Return(_) => run_apc(env, None, func, args, USER_APC),
                        outcome => outcome,
                    },
                }),
            );
        }
        Err(status::Error::Status(status::NOT_IMPLEMENTED)) => {}
        Err(error) => return status::finish(Err(error)),
    }
    let mut op = Vec::with_capacity(8);
    for word in [SELECT_WAIT, handle] {
        op.extend_from_slice(&word.to_le_bytes());
    }
    let select = new_select(
        env,
        op,
        SELECT_INTERRUPTIBLE | if alertable { SELECT_ALERTABLE } else { 0 },
        None,
    );
    run_select(
        env,
        select,
        Box::new(move |env, ended| match ended {
            Ended::Status(status) => then(env, status),
            Ended::Apc { func, args } => match then(env, USER_APC) {
                Outcome::Return(_) => run_apc(env, None, func, args, USER_APC),
                outcome => outcome,
            },
        }),
    )
}

/// NtDelayExecution(alertable, *timeout)
pub fn delay(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[alertable, timeout] = args else {
        unreachable!()
    };
    if alertable as u8 != 0 {
        let timeout = match read_timeout(env, timeout) {
            Ok(timeout) => timeout,
            Err(code) => return Outcome::Return(code),
        };
        let select = new_select(
            env,
            Vec::new(),
            SELECT_INTERRUPTIBLE | SELECT_ALERTABLE,
            timeout,
        );
        return run_select(
            env,
            select,
            Box::new(|env, ended| match ended {
                Ended::Status(0x102) => Outcome::Return(status::SUCCESS),
                Ended::Status(code) => Outcome::Return(code),
                Ended::Apc { func, args } => run_apc(env, None, func, args, USER_APC),
            }),
        );
    }
    let timeout = match read_timeout(env, timeout) {
        Ok(Some(timeout)) => timeout,
        Ok(None) => return Outcome::Fail("a sleep that never ends".into()),
        Err(status) => return Outcome::Return(status),
    };
    // Wine yields even when the time has already come.
    let deadline = deadline_us(env, timeout);
    let wait = if deadline > env.sys.clock.now_us() {
        Wait::Until(deadline)
    } else {
        env.sys.poll_observation();
        Wait::Yield
    };
    Outcome::block_then_return(wait, status::SUCCESS)
}

/// A `select` the thread repeats until the server ends it, as Wine's
/// `server_select`.
struct Select {
    op: Vec<u8>,
    flags: i32,
    /// The server's absolute timeout: a system time, or the performance
    /// counter negated.
    timeout: i64,
    /// When the machine lets the server see the timeout pass; none for an
    /// infinite wait.
    deadline: Option<u64>,
    /// Wine names a wait by an address on the waiting thread's stack.
    cookie: u64,
}

/// How a `select` ended.
enum Ended {
    Status(Status),
    /// A user APC to run, as `func(args)`.
    Apc {
        func: u32,
        args: [u32; 3],
    },
}

type Then = Box<dyn FnOnce(&mut Env<'_>, Ended) -> Outcome>;

/// A wait from a system call, which returns its status or runs the APC
/// that ended it.
fn begin_select(env: &mut Env<'_>, op: Vec<u8>, flags: i32, timeout: u32) -> Outcome {
    let timeout = match read_timeout(env, timeout) {
        Ok(timeout) => timeout,
        Err(status) => return Outcome::Return(status),
    };
    let select = new_select(env, op, flags, timeout);
    run_select(
        env,
        select,
        Box::new(|env, ended| match ended {
            Ended::Status(status) => Outcome::Return(status),
            Ended::Apc { func, args } => run_apc(env, None, func, args, USER_APC),
        }),
    )
}

fn new_select(env: &mut Env<'_>, op: Vec<u8>, flags: i32, timeout: Option<i64>) -> Select {
    // As Wine's `server_wait`: a relative timeout becomes an absolute time
    // on the performance counter, which the server reads negated.
    let time = Time::now(&mut env.sys.clock);
    let absolute = match timeout {
        None => TIMEOUT_INFINITE,
        Some(timeout) if timeout < 0 => timeout - time.monotonic as i64,
        Some(timeout) => timeout,
    };
    Select {
        op,
        flags,
        timeout: absolute,
        deadline: timeout.map(|timeout| deadline_us(env, timeout)),
        cookie: u64::from(env.cpu.reg(Reg::Esp)),
    }
}

/// Sends the `select`; if the server cannot end it at once, blocks until
/// it does.
fn run_select(env: &mut Env<'_>, select: Select, then: Then) -> Outcome {
    run_select_result(env, select, then, 0, [0; APC_RESULT_SIZE])
}

fn run_select_result(
    env: &mut Env<'_>,
    select: Select,
    then: Then,
    previous: u32,
    result: [u8; APC_RESULT_SIZE],
) -> Outcome {
    let mut request = SelectRequest {
        prev_apc: previous,
        flags: select.flags,
        cookie: select.cookie,
        timeout: select.timeout,
        size: select.op.len() as u32,
        ..Default::default()
    };
    let mut call = [0u8; APC_CALL_SIZE];
    let reply = server::request(env, &mut request, &[&result, &select.op], &mut call);
    let (reply, size) = match reply {
        Ok(reply) => reply,
        Err(error) => return status::finish(Err(error)),
    };
    if reply.header.error == KERNEL_APC {
        let result = match crate::apc::invoke(env, &call, false) {
            Ok(result) => result,
            Err(error) => return status::finish(Err(error)),
        };
        return run_select_result(env, select, then, reply.apc_handle, result);
    }
    if reply.signaled == 0 {
        return block(env, select, then);
    }
    env.ext
        .get_or_default::<Timers>()
        .waits
        .remove(&env.context);
    if reply.header.error != USER_APC {
        return then(env, Ended::Status(reply.header.error));
    }
    let word = |offset: usize| u32::from_le_bytes(call[offset..offset + 4].try_into().unwrap());
    if size < APC_CALL_SIZE || word(0) != APC_USER {
        return Outcome::Fail("a user APC without its call".into());
    }
    // struct user_apc: type, flags, then the 64-bit function and arguments.
    let apc = Ended::Apc {
        func: word(8),
        args: [word(16), word(24), word(32)],
    };
    then(env, apc)
}

fn block(env: &mut Env<'_>, select: Select, then: Then) -> Outcome {
    let wake = match server::begin_wait(env) {
        Ok(wake) => wake,
        Err(error) => return status::finish(Err(error)),
    };
    env.ext
        .get_or_default::<Timers>()
        .waits
        .insert(env.context, wake);
    let deadline = match timer_deadline(env) {
        Ok(timer) => match (select.deadline, timer) {
            (Some(wait), Some(timer)) => Some(wait.min(timer)),
            (wait, timer) => wait.or(timer),
        },
        Err(error) => return status::finish(Err(error)),
    };
    let timed = select.op.is_empty() && select.deadline == deadline;
    let wait = Wait::Object {
        handle: wake,
        deadline,
    };
    // spec: PROC-CLOCK-1
    // Only a delay is eligible. An object or I/O producer needs real time.
    if timed {
        Outcome::block_timed(wait, move |env, _| wake_up(env, select, then))
    } else {
        Outcome::block(wait, move |env, _| wake_up(env, select, then))
    }
}

/// Ends a blocked wait with the server's wake-up, or blocks again.
fn wake_up(env: &mut Env<'_>, mut select: Select, then: Then) -> Outcome {
    match server::wake_up(env) {
        Err(error) => status::finish(Err(error)),
        // The thread was killed while it waited.
        Ok(Some(wake_up)) if wake_up.cookie == 0 => {
            server::end_thread(env, env.context);
            Outcome::ExitThread(wake_up.signaled)
        }
        Ok(Some(wake_up)) if wake_up.cookie != select.cookie => Outcome::Fail(format!(
            "a wake-up for wait {:#x} ended wait {:#x}",
            wake_up.cookie, select.cookie
        )),
        // An APC ended the wait: asked again, the server hands it over.
        Ok(Some(wake_up)) if matches!(wake_up.signaled, USER_APC | KERNEL_APC) => {
            run_select(env, select, then)
        }
        Ok(Some(wake_up)) => {
            env.ext
                .get_or_default::<Timers>()
                .waits
                .remove(&env.context);
            then(env, Ended::Status(wake_up.signaled))
        }
        Ok(None) => {
            let now = env.sys.clock.now_us();
            select.deadline = select.deadline.map(|deadline| deadline.max(now + RETRY_US));
            block(env, select, then)
        }
    }
}

/// NtContinue(*context, alertable) when alertable: as Wine, the thread
/// runs an APC that is due before it resumes `context`.
pub fn continue_alertable(env: &mut Env<'_>, context: Box<ArchitecturalState>) -> Outcome {
    let select = new_select(
        env,
        Vec::new(),
        SELECT_INTERRUPTIBLE | SELECT_ALERTABLE,
        Some(0),
    );
    run_select(
        env,
        select,
        Box::new(move |env, ended| match ended {
            Ended::Status(_) => exception::resume_with(env, context),
            Ended::Apc { func, args } => run_apc(env, Some(context), func, args, USER_APC),
        }),
    )
}

/// Runs a user APC as Wine's `call_user_apc_dispatcher`: the APC and the
/// context it interrupted go on the stack, and the thread continues in
/// `KiUserApcDispatcher`. Without a context, the APC interrupts the system
/// call, which then returns STATUS_USER_APC.
fn run_apc(
    env: &mut Env<'_>,
    context: Option<Box<ArchitecturalState>>,
    func: u32,
    args: [u32; 3],
    return_status: Status,
) -> Outcome {
    let Some(&Dispatchers {
        apc: dispatcher, ..
    }) = env.ext.try_get::<Dispatchers>()
    else {
        return Outcome::Fail("no APC dispatcher".into());
    };
    let mut checkpoint = match env.checkpoint() {
        Ok(checkpoint) => checkpoint,
        Err(error) => return Outcome::Fail(format!("APC: {error}")),
    };
    let interrupted = match context {
        Some(context) => context,
        None => {
            // The system call's return: to its stub, which pops its frame.
            let mut state = checkpoint.state.clone();
            let esp = state.regs[4];
            state.eip = match env.cpu.read_u32(esp) {
                Ok(address) => address,
                Err(error) => return Outcome::Fail(format!("APC: {error}")),
            };
            state.regs[4] = esp + 4;
            state.regs[0] = return_status;
            state
        }
    };
    let stack = interrupted.regs[4] - APC_LAYOUT_SIZE;
    let mut frame = vec![0u8; APC_LAYOUT_SIZE as usize];
    for (index, value) in [func, args[0], args[1], args[2], 1].into_iter().enumerate() {
        frame[4 * index..4 * index + 4].copy_from_slice(&value.to_le_bytes());
    }
    exception::put_context(env, &mut frame, APC_CONTEXT, &interrupted);
    if let Err(error) = env.cpu.write(stack, &frame) {
        return Outcome::Fail(format!("APC frame: {error}"));
    }
    checkpoint.state = interrupted;
    checkpoint.state.regs[4] = stack;
    checkpoint.state.eip = dispatcher;
    Outcome::ResumeExecution(checkpoint)
}

/// NtQueueApcThread(thread, func, arg1, arg2, arg3)
pub fn queue_apc(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[thread, func, arg1, arg2, arg3] = args else {
        unreachable!()
    };
    queue_apc_ex2(env, &[thread, 0, 0, func, arg1, arg2, arg3])
}

/// NtQueueApcThreadEx2(thread, reserve, flags, func, arg1, arg2, arg3)
pub fn queue_apc_ex2(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[thread, reserve, flags, func, arg1, arg2, arg3] = args else {
        unreachable!()
    };
    status::finish((|| {
        let mut request = QueueApcRequest {
            handle: thread,
            reserve_handle: reserve,
            ..Default::default()
        };
        let mut call = [0u8; APC_CALL_SIZE];
        if func != 0 {
            for (offset, value) in [
                (0, APC_USER),
                (4, flags & 3),
                (8, func),
                (16, arg1),
                (24, arg2),
                (32, arg3),
            ] {
                call[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
            }
        }
        let data: &[u8] = if func != 0 { &call } else { &[] };
        let (reply, _) = server::request(env, &mut request, &[data], &mut [])?;
        status::check(reply.header.error)
    })())
}

/// The LARGE_INTEGER timeout at `address`; none when it is null or
/// infinite.
fn read_timeout(env: &Env<'_>, address: u32) -> Result<Option<i64>, Status> {
    if address == 0 {
        return Ok(None);
    }
    let timeout = i64::from(read_u32(env, address)?) | i64::from(read_u32(env, address + 4)?) << 32;
    Ok((timeout != TIMEOUT_INFINITE).then_some(timeout))
}

/// The machine time a timeout ends: relative in 100 ns ticks when
/// negative, else an absolute system time.
fn deadline_us(env: &mut Env<'_>, timeout: i64) -> u64 {
    let now = env.sys.clock.now_us();
    let ticks = if timeout < 0 {
        timeout.unsigned_abs()
    } else {
        (timeout as u64).saturating_sub(env.sys.clock.system_time())
    };
    now + ticks.div_ceil(10)
}

/// Timer handles whose next expiration may wake a server wait. The server
/// remains the owner of timer state and APC queues.
#[derive(Default)]
struct Timers {
    handles: std::collections::BTreeSet<u32>,
    waits: std::collections::HashMap<adv_machine::CtxId, adv_machine::sched::Handle>,
}

fn timer_deadline(env: &mut Env<'_>) -> Result<Option<u64>, status::Error> {
    let handles: Vec<_> = env
        .ext
        .get_or_default::<Timers>()
        .handles
        .iter()
        .copied()
        .collect();
    let mut deadline = None;
    for handle in handles {
        let mut request = GetTimerInfoRequest {
            handle,
            ..Default::default()
        };
        let (mut reply, _) = server::request(env, &mut request, &[], &mut [])?;
        if reply.header.error == 0xc000_0022 {
            // Setting a timer needs MODIFY_STATE, not QUERY_STATE. Obtain a
            // temporary query handle without keeping the timer alive.
            let mut duplicate = DupHandleRequest {
                src_process: u32::MAX,
                src_handle: handle,
                dst_process: u32::MAX,
                access: 1,
                ..Default::default()
            };
            let (duplicate, _) = server::request(env, &mut duplicate, &[], &mut [])?;
            status::check(duplicate.header.error)?;
            request.handle = duplicate.handle;
            reply = server::request(env, &mut request, &[], &mut [])?.0;
            server::close(env, duplicate.handle)?;
        }
        let now = env.sys.clock.now_us();
        let when = if reply.when > 0 {
            now + (reply.when as u64)
                .saturating_sub(env.sys.clock.system_time())
                .div_ceil(10)
        } else {
            reply.when.unsigned_abs().div_ceil(10)
        };
        if reply.header.error != status::SUCCESS || reply.when == TIMEOUT_INFINITE || when <= now {
            env.ext.get_mut::<Timers>().handles.remove(&handle);
        } else {
            deadline = Some(deadline.map_or(when, |old: u64| old.min(when)));
        }
    }
    Ok(deadline)
}

/// NtCreateSemaphore(*handle, access, *attributes, initial, maximum)
pub fn create_semaphore(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[handle, access, attributes, initial, max] = args else {
        unreachable!()
    };
    status::finish((|| {
        write_u32(env, handle, 0)?;
        if (max as i32) <= 0 || (initial as i32) < 0 || initial > max {
            return Err(status::INVALID_PARAMETER.into());
        }
        let attributes = if attributes == 0 {
            Vec::new()
        } else {
            Attributes::read(env, attributes)?.wire()
        };
        let mut request = CreateSemaphoreRequest {
            access,
            initial,
            max,
            ..Default::default()
        };
        let (reply, _) = server::request(env, &mut request, &[&attributes], &mut [])?;
        write_u32(env, handle, reply.handle)?;
        status::check(reply.header.error)
    })())
}

pub(crate) fn open_attributes(env: &Env<'_>, address: u32) -> Result<Attributes, status::Error> {
    if address == 0 || read_u32(env, address)? != 24 {
        return Err(status::INVALID_PARAMETER.into());
    }
    let root = read_u32(env, address + 4)?;
    let name = read_u32(env, address + 8)?;
    let name = if name == 0 {
        if root != 0 {
            return Err(status::OBJECT_NAME_INVALID.into());
        }
        None
    } else {
        if read_u32(env, name + 4)? & 1 != 0 {
            return Err(0x8000_0002.into());
        }
        Some(read_unicode_string(env, name)?)
    };
    Ok(Attributes {
        security: Vec::new(),
        root,
        attributes: read_u32(env, address + 12)?,
        name,
    })
}

/// NtOpenSemaphore(*handle, access, *attributes)
pub fn open_semaphore(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[handle, access, attributes] = args else {
        unreachable!()
    };
    status::finish((|| {
        write_u32(env, handle, 0)?;
        let attr = open_attributes(env, attributes)?;
        let mut request = OpenSemaphoreRequest {
            access,
            attributes: attr.attributes,
            rootdir: attr.root,
            ..Default::default()
        };
        let (reply, _) = server::request(env, &mut request, &[&attr.name_bytes()], &mut [])?;
        write_u32(env, handle, reply.handle)?;
        status::check(reply.header.error)
    })())
}

/// NtReleaseSemaphore(handle, count, *previous)
pub fn release_semaphore(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[handle, count, previous] = args else {
        unreachable!()
    };
    // Windows 11 rejects zero and negative releases with ERROR_INVALID_PARAMETER.
    if count as i32 <= 0 {
        return Outcome::Return(status::INVALID_PARAMETER);
    }
    status::finish((|| {
        if let Some(value) = inproc_operation(env, handle, 4, 2, ntsync::SEM_RELEASE, [count, 0])? {
            if previous != 0 {
                write_u32(env, previous, value[0])?;
            }
            return Ok(());
        }
        let mut request = ReleaseSemaphoreRequest {
            handle,
            count,
            ..Default::default()
        };
        let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
        status::check(reply.header.error)?;
        if previous != 0 {
            write_u32(env, previous, reply.prev_count)?;
        }
        Ok(())
    })())
}

/// NtQuerySemaphore(handle, class, *info, length, *returned)
pub fn query_semaphore(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[handle, class, info, length, returned] = args else {
        unreachable!()
    };
    status::finish((|| {
        if class != 0 {
            return Err(status::INVALID_INFO_CLASS.into());
        }
        if length != 8 {
            return Err(status::INFO_LENGTH_MISMATCH.into());
        }
        if let Some(value) = inproc_operation(env, handle, 4, 1, ntsync::SEM_READ, [0; 2])? {
            write_u32(env, info, value[0])?;
            write_u32(env, info + 4, value[1])?;
            if returned != 0 {
                write_u32(env, returned, 8)?;
            }
            return Ok(());
        }
        let mut request = QuerySemaphoreRequest {
            handle,
            ..Default::default()
        };
        let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
        status::check(reply.header.error)?;
        write_u32(env, info, reply.current)?;
        write_u32(env, info + 4, reply.max)?;
        if returned != 0 {
            write_u32(env, returned, 8)?;
        }
        Ok(())
    })())
}

/// NtCreateTimer(*handle, access, *attributes, type)
pub fn create_timer(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[handle, access, attributes, kind] = args else {
        unreachable!()
    };
    status::finish((|| {
        write_u32(env, handle, 0)?;
        if kind > 1 {
            return Err(status::INVALID_PARAMETER.into());
        }
        let attributes = if attributes == 0 {
            Vec::new()
        } else {
            Attributes::read(env, attributes)?.wire()
        };
        let mut request = CreateTimerRequest {
            access,
            manual: i32::from(kind == 0),
            ..Default::default()
        };
        let (reply, _) = server::request(env, &mut request, &[&attributes], &mut [])?;
        write_u32(env, handle, reply.handle)?;
        status::check(reply.header.error)
    })())
}

/// NtOpenTimer(*handle, access, *attributes)
pub fn open_timer(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[handle, access, attributes] = args else {
        unreachable!()
    };
    status::finish((|| {
        write_u32(env, handle, 0)?;
        let attr = open_attributes(env, attributes)?;
        let mut request = OpenTimerRequest {
            access,
            attributes: attr.attributes,
            rootdir: attr.root,
            ..Default::default()
        };
        let (reply, _) = server::request(env, &mut request, &[&attr.name_bytes()], &mut [])?;
        write_u32(env, handle, reply.handle)?;
        status::check(reply.header.error)
    })())
}

/// NtSetTimer(handle, *when, callback, arg, resume, period, *state)
pub fn set_timer(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[handle, when, callback, arg, resume, period, state] = args else {
        unreachable!()
    };
    status::finish((|| {
        let expire = i64::from(read_u32(env, when)?) | i64::from(read_u32(env, when + 4)?) << 32;
        let mut request = SetTimerRequest {
            handle,
            expire,
            callback: u64::from(callback),
            arg: u64::from(arg),
            period: period as i32,
            ..Default::default()
        };
        let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
        if state != 0 {
            write_bytes(env, state, &[reply.signaled as u8])?;
        }
        status::check(reply.header.error)?;
        let timers = env.ext.get_or_default::<Timers>();
        timers.handles.insert(handle);
        for &wake in timers.waits.values() {
            env.sys.set_event(wake);
        }
        if resume as u8 != 0 {
            return Err(0x4000_0025.into());
        }
        Ok(())
    })())
}

/// NtCancelTimer(handle, *state)
pub fn cancel_timer(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[handle, state] = args else {
        unreachable!()
    };
    status::finish((|| {
        let mut request = CancelTimerRequest {
            handle,
            ..Default::default()
        };
        let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
        if state != 0 {
            write_bytes(env, state, &[reply.signaled as u8])?;
        }
        status::check(reply.header.error)
    })())
}

/// NtQueryTimer(handle, class, *info, length, *returned)
pub fn query_timer(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[handle, class, info, length, returned] = args else {
        unreachable!()
    };
    status::finish((|| {
        if class != 0 {
            return Err(status::INVALID_INFO_CLASS.into());
        }
        if length < 16 {
            return Err(status::INFO_LENGTH_MISMATCH.into());
        }
        let mut request = GetTimerInfoRequest {
            handle,
            ..Default::default()
        };
        let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
        let now = if reply.when > 0 {
            env.sys.clock.system_time()
        } else {
            Time::now(&mut env.sys.clock).monotonic
        };
        let remaining = reply.when.unsigned_abs().saturating_sub(now);
        write_bytes(env, info, &remaining.to_le_bytes())?;
        write_bytes(env, info + 8, &[reply.signaled as u8])?;
        if returned != 0 {
            write_u32(env, returned, 16)?;
        }
        status::check(reply.header.error)
    })())
}

/// NtTestAlert(): deliver a pending user APC and return success.
pub fn test_alert(env: &mut Env<'_>, _: &[u32]) -> Outcome {
    let select = new_select(
        env,
        Vec::new(),
        SELECT_INTERRUPTIBLE | SELECT_ALERTABLE,
        Some(0),
    );
    run_select(
        env,
        select,
        Box::new(|env, ended| match ended {
            Ended::Status(_) => Outcome::Return(status::SUCCESS),
            Ended::Apc { func, args } => run_apc(env, None, func, args, status::SUCCESS),
        }),
    )
}

#[derive(Default)]
struct Alerts(std::collections::HashMap<u32, adv_machine::sched::Handle>);

fn alert_event(env: &mut Env<'_>, tid: u32) -> Result<adv_machine::sched::Handle, Status> {
    // Wine indexes its alert table by (tid >> 2) - 1, even for a thread
    // that does not exist yet. Alerts persist until the next wait.
    let index = (tid >> 2).wrapping_sub(1);
    if index / 16384 > 4096 {
        return Err(0xc000_000b);
    }
    if let Some(&event) = env.ext.get_or_default::<Alerts>().0.get(&index) {
        return Ok(event);
    }
    let event = env.sys.objects.insert(adv_machine::sched::Object::Event {
        manual_reset: true,
        signaled: false,
    });
    env.ext.get_or_default::<Alerts>().0.insert(index, event);
    Ok(event)
}

/// NtAlertThreadByThreadId(tid)
pub fn alert_thread(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    match alert_event(env, args[0]) {
        Ok(event) => {
            env.sys.set_event(event);
            Outcome::Return(status::SUCCESS)
        }
        Err(status) => Outcome::Return(status),
    }
}

/// NtWaitForAlertByThreadId(address, *timeout)
pub fn wait_alert(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let result = (|| {
        let tid = server::current_tid(env)?;
        let event = alert_event(env, tid)?;
        let timeout = read_timeout(env, args[1])?;
        let deadline = timeout.map(|timeout| deadline_us(env, timeout));
        Ok((event, deadline))
    })();
    match result {
        Ok((handle, deadline)) => {
            Outcome::block(Wait::Object { handle, deadline }, move |env, resume| {
                if matches!(
                    resume,
                    adv_machine::Resume::Woke(adv_machine::WakeReason::Timeout)
                ) {
                    Outcome::Return(0x102)
                } else {
                    env.sys.reset_event(handle);
                    Outcome::Return(0x101)
                }
            })
        }
        Err(error) => status::finish(Err(error)),
    }
}

/// NtOpenEvent(*handle, access, *attributes)
pub fn open_event(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    status::finish((|| {
        write_u32(env, args[0], 0)?;
        let attr = open_attributes(env, args[2])?;
        let mut request = crate::protocol::OpenEventRequest {
            access: args[1],
            attributes: attr.attributes,
            rootdir: attr.root,
            ..Default::default()
        };
        let (reply, _) = server::request(env, &mut request, &[&attr.name_bytes()], &mut [])?;
        write_u32(env, args[0], reply.handle)?;
        status::check(reply.header.error)
    })())
}

/// NtQueryEvent(handle, class, *info, length, *result_length)
pub fn query_event(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    status::finish((|| {
        if args[1] != 0 {
            return Err(status::INVALID_INFO_CLASS.into());
        }
        if args[3] != 8 {
            return Err(status::INFO_LENGTH_MISMATCH.into());
        }
        if let Some(value) = inproc_operation(env, args[0], 2, 1, ntsync::EVENT_READ, [0; 2])? {
            write_u32(env, args[2], u32::from(value[0] == 0))?;
            write_u32(env, args[2] + 4, value[1])?;
            if args[4] != 0 {
                write_u32(env, args[4], 8)?;
            }
            return Ok(());
        }
        let mut request = crate::protocol::QueryEventRequest {
            handle: args[0],
            ..Default::default()
        };
        let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
        status::check(reply.header.error)?;
        write_u32(env, args[2], u32::from(reply.manual_reset == 0))?;
        write_u32(env, args[2] + 4, reply.state as u32)?;
        if args[4] != 0 {
            write_u32(env, args[4], 8)?;
        }
        Ok(())
    })())
}

/// NtPulseEvent(handle, *previous_state)
pub fn pulse_event(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    event_op(env, args, 0)
}

/// NtSignalAndWaitForSingleObject(signal, wait, alertable, *timeout)
pub fn signal_and_wait(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    if args[0] == 0 {
        return Outcome::Return(status::INVALID_HANDLE);
    }
    if let Some(outcome) = inproc_wait(env, &[args[1]], true, args[2], args[3], Some(args[0])) {
        return outcome;
    }
    let op = [3u32, args[1], args[0]]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect();
    begin_select(
        env,
        op,
        SELECT_INTERRUPTIBLE
            | if args[2] as u8 != 0 {
                SELECT_ALERTABLE
            } else {
                0
            },
        args[3],
    )
}

/// NtCreateDebugObject(*handle, access, *attributes, flags)
pub fn create_debug(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    status::finish((|| {
        write_u32(env, args[0], 0)?;
        if args[3] & !1 != 0 {
            return Err(status::INVALID_PARAMETER.into());
        }
        let attributes = if args[2] == 0 {
            Vec::new()
        } else {
            Attributes::read(env, args[2])?.wire()
        };
        let mut request = crate::protocol::CreateDebugObjRequest {
            access: args[1],
            flags: args[3],
            ..Default::default()
        };
        let (reply, _) = server::request(env, &mut request, &[&attributes], &mut [])?;
        write_u32(env, args[0], reply.handle)?;
        status::check(reply.header.error)
    })())
}

/// Wine exposes no auxiliary hardware counter.
pub fn convert_counter(_: &mut Env<'_>, args: &[u32]) -> Outcome {
    Outcome::Return(if args[1] == 0 {
        status::ACCESS_VIOLATION
    } else {
        status::NOT_SUPPORTED
    })
}

/// NtOpenMutant(*handle, access, *attributes)
pub fn open_mutant(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    status::finish((|| {
        write_u32(env, args[0], 0)?;
        let attr = open_attributes(env, args[2])?;
        let mut request = crate::protocol::OpenMutexRequest {
            access: args[1],
            attributes: attr.attributes,
            rootdir: attr.root,
            ..Default::default()
        };
        let (reply, _) = server::request(env, &mut request, &[&attr.name_bytes()], &mut [])?;
        write_u32(env, args[0], reply.handle)?;
        status::check(reply.header.error)
    })())
}

/// NtSetEventBoostPriority(handle)
pub fn boost_event(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    set_event(env, &[args[0], 0])
}

/// NtQueryMutant(handle, class, *info, length, *result_length)
pub fn query_mutant(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    status::finish((|| {
        if args[1] != 0 {
            return Err(status::INVALID_INFO_CLASS.into());
        }
        if args[3] != 8 {
            return Err(status::INFO_LENGTH_MISMATCH.into());
        }
        if let Some(value) = inproc_operation(env, args[0], 3, 1, ntsync::MUTEX_READ, [0; 2])? {
            write_u32(env, args[2], 1u32.wrapping_sub(value[1]))?;
            let tid = server::current_tid(env)?;
            write_bytes(
                env,
                args[2] + 4,
                &[u8::from(value[0] == tid), u8::from(value[0] == u32::MAX)],
            )?;
            if args[4] != 0 {
                write_u32(env, args[4], 8)?;
            }
            return Ok(());
        }
        let mut request = crate::protocol::QueryMutexRequest {
            handle: args[0],
            ..Default::default()
        };
        let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
        status::check(reply.header.error)?;
        write_u32(env, args[2], 1u32.wrapping_sub(reply.count))?;
        write_bytes(
            env,
            args[2] + 4,
            &[reply.owned as u8, reply.abandoned as u8],
        )?;
        if args[4] != 0 {
            write_u32(env, args[4], 8)?;
        }
        Ok(())
    })())
}

/// NtCreateKeyedEvent(*handle, access, *attributes, flags)
pub fn create_keyed_event(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    status::finish((|| {
        write_u32(env, args[0], 0)?;
        let attr = if args[2] == 0 {
            Vec::new()
        } else {
            Attributes::read(env, args[2])?.wire()
        };
        let mut request = crate::protocol::CreateKeyedEventRequest {
            access: args[1],
            ..Default::default()
        };
        let (reply, _) = server::request(env, &mut request, &[&attr], &mut [])?;
        write_u32(env, args[0], reply.handle)?;
        status::check(reply.header.error)
    })())
}

/// NtOpenKeyedEvent(*handle, access, *attributes)
pub fn open_keyed_event(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    status::finish((|| {
        write_u32(env, args[0], 0)?;
        let attr = open_attributes(env, args[2])?;
        let mut request = crate::protocol::OpenKeyedEventRequest {
            access: args[1],
            attributes: attr.attributes,
            rootdir: attr.root,
            ..Default::default()
        };
        let (reply, _) = server::request(env, &mut request, &[&attr.name_bytes()], &mut [])?;
        write_u32(env, args[0], reply.handle)?;
        status::check(reply.header.error)
    })())
}

/// NtCreateIoCompletion(*handle, access, *attributes, threads)
pub fn create_completion(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    status::finish((|| {
        write_u32(env, args[0], 0)?;
        let attr = if args[2] == 0 {
            Vec::new()
        } else {
            Attributes::read(env, args[2])?.wire()
        };
        let mut request = crate::protocol::CreateCompletionRequest {
            access: args[1],
            concurrent: args[3],
            ..Default::default()
        };
        let (reply, _) = server::request(env, &mut request, &[&attr], &mut [])?;
        write_u32(env, args[0], reply.handle)?;
        status::check(reply.header.error)
    })())
}

struct KeyedEvent(u32);

/// NtWaitForKeyedEvent(handle, key, alertable, *timeout)
pub fn wait_keyed_event(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    keyed_event(env, args, 4)
}

/// NtReleaseKeyedEvent(handle, key, alertable, *timeout)
pub fn release_keyed_event(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    keyed_event(env, args, 5)
}

fn keyed_event(env: &mut Env<'_>, args: &[u32], op: u32) -> Outcome {
    if args[1] & 1 != 0 {
        return Outcome::Return(INVALID_PARAMETER_1);
    }
    let handle = if args[0] != 0 {
        args[0]
    } else if let Some(event) = env.ext.try_get::<KeyedEvent>() {
        event.0
    } else {
        let mut request = crate::protocol::CreateKeyedEventRequest {
            access: 0x1f0003,
            ..Default::default()
        };
        let result = server::request(env, &mut request, &[], &mut []);
        match result {
            Ok((reply, _)) if reply.header.error == 0 => {
                env.ext.insert(KeyedEvent(reply.handle));
                reply.handle
            }
            Ok((reply, _)) => return Outcome::Return(reply.header.error),
            Err(error) => return status::finish(Err(error)),
        }
    };
    let mut data = Vec::new();
    for value in [op, handle, args[1], 0] {
        data.extend_from_slice(&value.to_le_bytes());
    }
    begin_select(
        env,
        data,
        SELECT_INTERRUPTIBLE
            | if args[2] as u8 != 0 {
                SELECT_ALERTABLE
            } else {
                0
            },
        args[3],
    )
}

/// NtOpenIoCompletion(*handle, access, *attributes)
pub fn open_completion(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    status::finish((|| {
        write_u32(env, args[0], 0)?;
        let attr = open_attributes(env, args[2])?;
        let mut request = crate::protocol::OpenCompletionRequest {
            access: args[1],
            attributes: attr.attributes,
            rootdir: attr.root,
            ..Default::default()
        };
        let (reply, _) = server::request(env, &mut request, &[&attr.name_bytes()], &mut [])?;
        write_u32(env, args[0], reply.handle)?;
        status::check(reply.header.error)
    })())
}

/// NtAlertMultipleThreadByThreadId(*ids, count, unknown, unknown)
pub fn alert_multiple(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    status::finish((|| {
        let mut events = Vec::new();
        for index in 0..args[1] {
            events.push(alert_event(env, read_u32(env, args[0] + index * 4)?)?);
        }
        for event in events {
            env.sys.set_event(event);
        }
        Ok(())
    })())
}

/// NtCreateJobObject(*handle, access, *attributes)
pub fn create_job(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    status::finish((|| {
        write_u32(env, args[0], 0)?;
        let attr = if args[2] == 0 {
            Vec::new()
        } else {
            Attributes::read(env, args[2])?.wire()
        };
        let mut request = crate::protocol::CreateJobRequest {
            access: args[1],
            ..Default::default()
        };
        let (reply, _) = server::request(env, &mut request, &[&attr], &mut [])?;
        write_u32(env, args[0], reply.handle)?;
        status::check(reply.header.error)
    })())
}

/// NtOpenJobObject(*handle, access, *attributes)
pub fn open_job(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    status::finish((|| {
        write_u32(env, args[0], 0)?;
        let attr = open_attributes(env, args[2])?;
        let mut request = crate::protocol::OpenJobRequest {
            access: args[1],
            attributes: attr.attributes,
            rootdir: attr.root,
            ..Default::default()
        };
        let (reply, _) = server::request(env, &mut request, &[&attr.name_bytes()], &mut [])?;
        write_u32(env, args[0], reply.handle)?;
        status::check(reply.header.error)
    })())
}

/// NtSetIoCompletion(handle, key, value, status, information)
pub fn set_completion(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    status::finish((|| {
        let mut request = crate::protocol::AddCompletionRequest {
            handle: args[0],
            ckey: u64::from(args[1]),
            cvalue: u64::from(args[2]),
            status: args[3],
            information: u64::from(args[4]),
            ..Default::default()
        };
        let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
        status::check(reply.header.error)
    })())
}

/// NtRemoveIoCompletion(handle, *key, *value, *io, *timeout)
pub fn remove_completion(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let args: [u32; 5] = args.try_into().unwrap();
    let mut request = crate::protocol::RemoveCompletionRequest {
        handle: args[0],
        ..Default::default()
    };
    let reply = match server::request(env, &mut request, &[], &mut []) {
        Ok((reply, _)) => reply,
        Err(error) => return status::finish(Err(error)),
    };
    if reply.header.error == 0 {
        return completion_result(
            env,
            args,
            reply.ckey,
            reply.cvalue,
            reply.status,
            reply.information,
        );
    }
    if reply.header.error != 0x103 {
        return Outcome::Return(reply.header.error);
    }
    let timeout = match read_timeout(env, args[4]) {
        Ok(timeout) => timeout,
        Err(error) => return Outcome::Return(error),
    };
    if timeout == Some(0) {
        return Outcome::Return(0x102);
    }
    let op = [SELECT_WAIT, reply.wait_handle]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect();
    let select = new_select(env, op, SELECT_INTERRUPTIBLE, timeout);
    run_select(
        env,
        select,
        Box::new(move |env, ended| match ended {
            Ended::Status(0) => {
                let mut request = crate::protocol::GetThreadCompletionRequest::default();
                match server::request(env, &mut request, &[], &mut []) {
                    Ok((reply, _)) if reply.header.error == 0 => completion_result(
                        env,
                        args,
                        reply.ckey,
                        reply.cvalue,
                        reply.status,
                        reply.information,
                    ),
                    Ok((reply, _)) => Outcome::Return(reply.header.error),
                    Err(error) => status::finish(Err(error)),
                }
            }
            Ended::Status(status) => Outcome::Return(status),
            Ended::Apc { .. } => unreachable!(),
        }),
    )
}

fn completion_result(
    env: &mut Env<'_>,
    args: [u32; 5],
    key: u64,
    value: u64,
    code: u32,
    information: u64,
) -> Outcome {
    status::finish((|| {
        for (address, value) in [
            (args[1], key as u32),
            (args[2], value as u32),
            (args[3], code),
            (args[3] + 4, information as u32),
        ] {
            write_u32(env, address, value)?;
        }
        Ok(())
    })())
}

/// NtRemoveIoCompletionEx(handle, *entries, count, *written, *timeout, alertable)
pub fn remove_completion_ex(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let args: [u32; 6] = args.try_into().unwrap();
    if args[2] == 0 {
        return Outcome::Return(status::INVALID_PARAMETER);
    }
    let mut count = 0;
    let (code, handle) = loop {
        let mut request = crate::protocol::RemoveCompletionRequest {
            handle: args[0],
            alertable: i32::from(args[5] as u8 != 0),
            ..Default::default()
        };
        let reply = match server::request(env, &mut request, &[], &mut []) {
            Ok((reply, _)) => reply,
            Err(error) => return status::finish(Err(error)),
        };
        if reply.header.error != 0 {
            break (reply.header.error, reply.wait_handle);
        }
        if let Err(error) = completion_entry(
            env,
            args[1] + count * 16,
            reply.ckey,
            reply.cvalue,
            reply.status,
            reply.information,
        ) {
            return status::finish(Err(error));
        }
        count += 1;
        if count == args[2] {
            break (0, 0);
        }
    };
    if count != 0 || !matches!(code, 0x103 | USER_APC) {
        return completion_ex_end(env, args[3], count, if count != 0 { 0 } else { code });
    }
    let timeout = match read_timeout(env, args[4]) {
        Ok(timeout) => timeout,
        Err(error) => return Outcome::Return(error),
    };
    if code != USER_APC && timeout == Some(0) {
        return completion_ex_end(env, args[3], 0, 0x102);
    }
    let op = if code == USER_APC {
        Vec::new()
    } else {
        [SELECT_WAIT, handle]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect()
    };
    let flags = SELECT_INTERRUPTIBLE
        | if args[5] as u8 != 0 {
            SELECT_ALERTABLE
        } else {
            0
        };
    let select = new_select(env, op, flags, timeout);
    run_select(
        env,
        select,
        Box::new(move |env, ended| match ended {
            Ended::Status(0) => {
                let mut request = crate::protocol::GetThreadCompletionRequest::default();
                match server::request(env, &mut request, &[], &mut []) {
                    Ok((reply, _)) if reply.header.error == 0 => {
                        match completion_entry(
                            env,
                            args[1],
                            reply.ckey,
                            reply.cvalue,
                            reply.status,
                            reply.information,
                        ) {
                            Ok(()) => completion_ex_end(env, args[3], 1, 0),
                            Err(error) => status::finish(Err(error)),
                        }
                    }
                    Ok((reply, _)) => completion_ex_end(env, args[3], 0, reply.header.error),
                    Err(error) => status::finish(Err(error)),
                }
            }
            Ended::Status(code) => completion_ex_end(env, args[3], 0, code),
            Ended::Apc {
                func,
                args: apc_args,
            } => {
                if let Err(error) = write_u32(env, args[3], 1) {
                    return Outcome::Return(error);
                }
                run_apc(env, None, func, apc_args, USER_APC)
            }
        }),
    )
}

fn completion_entry(
    env: &mut Env<'_>,
    output: u32,
    key: u64,
    value: u64,
    code: u32,
    information: u64,
) -> Result<(), crate::status::Error> {
    for (index, value) in [key as u32, value as u32, code, information as u32]
        .into_iter()
        .enumerate()
    {
        write_u32(env, output + index as u32 * 4, value)?;
    }
    Ok(())
}

fn completion_ex_end(env: &mut Env<'_>, written: u32, count: u32, code: u32) -> Outcome {
    match write_u32(env, written, count.max(1)) {
        Ok(()) => Outcome::Return(code),
        Err(error) => Outcome::Return(error),
    }
}

/// NtClearEvent(handle)
pub fn clear_event(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    reset_event(env, &[args[0], 0])
}

/// NtQueueApcThreadEx(thread, reserve_and_flags, func, arg1, arg2, arg3)
pub fn queue_apc_ex(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    queue_apc_ex2(
        env,
        &[
            args[0],
            args[1] & !3,
            args[1] & 3,
            args[2],
            args[3],
            args[4],
            args[5],
        ],
    )
}

/// NtQueryIoCompletion(handle, class, *buffer, length, *returned)
pub fn query_completion(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    status::finish((|| {
        let &[handle, class, buffer, length, returned] = args else {
            unreachable!()
        };
        if buffer == 0 || class != 0 {
            return Err(status::INVALID_PARAMETER.into());
        }
        if returned != 0 {
            write_u32(env, returned, 4)?;
        }
        if length != 4 {
            return Err(status::INFO_LENGTH_MISMATCH.into());
        }
        let mut request = crate::protocol::QueryCompletionRequest {
            handle,
            ..Default::default()
        };
        let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
        status::check(reply.header.error)?;
        write_u32(env, buffer, reply.depth)?;
        Ok(())
    })())
}

/// NtSetIoCompletionEx(handle, reserve, key, value, status, information)
pub fn set_completion_ex(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    if args[1] == 0 {
        return Outcome::Return(status::INVALID_HANDLE);
    }
    status::finish((|| {
        let mut request = crate::protocol::AddCompletionRequest {
            handle: args[0],
            reserve_handle: args[1],
            ckey: args[2].into(),
            cvalue: args[3].into(),
            status: args[4],
            information: args[5].into(),
            ..Default::default()
        };
        let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
        status::check(reply.header.error)
    })())
}

/// NtAssignProcessToJobObject(job, process)
pub fn assign_job(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    status::finish((|| {
        crate::information::current_process(env, args[1])?;
        let mut request = crate::protocol::AssignJobRequest {
            job: args[0],
            process: args[1],
            ..Default::default()
        };
        let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
        status::check(reply.header.error)
    })())
}

/// NtIsProcessInJob(process, job)
pub fn process_in_job(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    status::finish((|| {
        crate::information::current_process(env, args[0])?;
        let mut request = crate::protocol::ProcessInJobRequest {
            process: args[0],
            job: args[1],
            ..Default::default()
        };
        let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
        status::check(reply.header.error)
    })())
}

/// NtTerminateJobObject(job, status)
pub fn terminate_job(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let mut request = crate::protocol::TerminateJobRequest {
        handle: args[0],
        status: args[1] as i32,
        ..Default::default()
    };
    match server::terminate_job(env, &mut request) {
        Ok(true) => Outcome::ExitProcess(args[1]),
        Ok(false) => Outcome::Return(status::SUCCESS),
        Err(error) => status::finish(Err(error)),
    }
}

/// NtQueryInformationJobObject(job, class, *info, length, *returned)
pub fn query_job(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    status::finish((|| {
        let &[handle, class, info, length, returned] = args else {
            unreachable!()
        };
        if class >= 53 {
            return Err(status::INVALID_PARAMETER.into());
        }
        let size = match class {
            1 => 48,
            2 => 48,
            3 => 12,
            9 => 112,
            _ => return Err(status::NOT_IMPLEMENTED.into()),
        };
        if length < size {
            return Err(status::INFO_LENGTH_MISMATCH.into());
        }
        if class == 2 || class == 9 {
            write_bytes(env, info, &vec![0; size as usize])?;
            if returned != 0 {
                write_u32(env, returned, size)?;
            }
            return Ok(());
        }
        let mut request = crate::protocol::GetJobInfoRequest {
            handle,
            ..Default::default()
        };
        let mut pids = vec![
            0;
            if class == 3 {
                ((length - 8) / 4 * 4) as usize
            } else {
                0
            }
        ];
        let (reply, count) = server::request(env, &mut request, &[], &mut pids)?;
        if class == 1 {
            if reply.header.error == 0 {
                write_bytes(env, info, &[0; 48])?;
                write_u32(env, info + 36, reply.total_processes as u32)?;
                write_u32(env, info + 40, reply.active_processes as u32)?;
            }
            if returned != 0 {
                write_u32(env, returned, 48)?;
            }
            return status::check(reply.header.error);
        }
        status::check(reply.header.error)?;
        write_bytes(env, info + 8, &pids[..count])?;
        let listed = ((length - 8) / 4).min(reply.active_processes as u32);
        write_u32(env, info, reply.active_processes as u32)?;
        write_u32(env, info + 4, listed)?;
        if returned != 0 {
            write_u32(env, returned, 8 + listed * 4)?;
        }
        if (length - 8) / 4 < reply.active_processes as u32 {
            return Err(0x105.into());
        }
        Ok(())
    })())
}

/// NtSetInformationJobObject(job, class, *info, length)
pub fn set_job(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    status::finish((|| {
        let &[handle, class, info, length] = args else {
            unreachable!()
        };
        if class >= 53 {
            return Err(status::INVALID_PARAMETER.into());
        }
        match class {
            2 | 9 => {
                let (size, valid) = if class == 2 {
                    (48, 0xff)
                } else {
                    (112, 0x7fff)
                };
                if length != size {
                    return Err(status::INVALID_PARAMETER.into());
                }
                let flags = read_u32(env, info + 16)?;
                if flags & !valid != 0 {
                    return Err(status::INVALID_PARAMETER.into());
                }
                let mut request = crate::protocol::SetJobLimitsRequest {
                    handle,
                    limit_flags: flags,
                    ..Default::default()
                };
                let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
                status::check(reply.header.error)
            }
            7 => {
                if length != 8 {
                    return Err(status::INVALID_PARAMETER.into());
                }
                let mut request = crate::protocol::SetJobCompletionPortRequest {
                    job: handle,
                    key: read_u32(env, info)?.into(),
                    port: read_u32(env, info + 4)?,
                    ..Default::default()
                };
                let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
                status::check(reply.header.error)
            }
            4 => Ok(()),
            _ => Err(status::NOT_IMPLEMENTED.into()),
        }
    })())
}

/// Wine 11.0's NtSetIntervalProfile stub.
pub fn set_interval_profile(_env: &mut Env<'_>, _args: &[u32]) -> Outcome {
    Outcome::Return(status::SUCCESS)
}

/// Wine 11.0's LPC port stub.
pub fn port_stub(_env: &mut Env<'_>, _args: &[u32]) -> Outcome {
    Outcome::Return(status::NOT_IMPLEMENTED)
}

/// Wine 11.0's NtCommitTransaction stub.
pub fn commit_transaction(_env: &mut Env<'_>, _args: &[u32]) -> Outcome {
    Outcome::Return(status::SUCCESS)
}

/// Wine 11.0's NtRollbackTransaction stub.
pub fn rollback_transaction(_env: &mut Env<'_>, _args: &[u32]) -> Outcome {
    Outcome::Return(status::ACCESS_VIOLATION)
}

/// NtCreateTransaction: Wine supplies its fixed dummy handle.
pub fn create_transaction(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    status::finish(write_u32(env, args[0], 1).map_err(Into::into))
}

/// NtSetInformationDebugObject(debug, class, *info, length, *returned)
pub fn set_debug_object(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    status::finish((|| {
        if args[1] != 1 {
            return Err(status::INVALID_PARAMETER.into());
        }
        if args[3] != 4 {
            if args[4] != 0 {
                write_u32(env, args[4], 4)?;
            }
            return Err(status::INFO_LENGTH_MISMATCH.into());
        }
        let flags = read_u32(env, args[2])?;
        if flags & !1 != 0 {
            return Err(status::INVALID_PARAMETER.into());
        }
        let mut request = crate::protocol::SetDebugObjInfoRequest {
            debug: args[0],
            flags,
            ..Default::default()
        };
        let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
        status::check(reply.header.error)?;
        if args[4] != 0 {
            write_u32(env, args[4], 0)?;
        }
        Ok(())
    })())
}

/// NtWaitForDebugEvent(debug, alertable, *timeout, *state)
pub fn wait_debug_event(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    debug_event(env, args.try_into().unwrap(), true)
}

fn debug_event(env: &mut Env<'_>, args: [u32; 4], wait: bool) -> Outcome {
    let mut request = crate::protocol::WaitDebugEventRequest {
        debug: args[0],
        ..Default::default()
    };
    // union debug_event_data from server_protocol.h.
    let mut data = [0; 160];
    let (reply, _) = match server::request(env, &mut request, &[], &mut data) {
        Ok(reply) => reply,
        Err(error) => return status::finish(Err(error)),
    };
    let result = (|| {
        status::check(reply.header.error)?;
        let word = |offset| u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
        let wide = |offset| u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
        let code = word(0);
        let state = args[3];
        write_u32(env, state, code)?;
        write_u32(env, state + 4, reply.pid)?;
        write_u32(env, state + 8, reply.tid)?;
        let info = state + 12;
        let mut partial = false;
        match code {
            0 | 1 => return Err(status::PENDING.into()),
            2 => {
                write_u32(env, info, word(4))?;
                write_u32(env, info + 8, word(8))?;
            }
            3 => {
                for (dest, src) in [
                    (0, 8),
                    (4, 12),
                    (12, 4),
                    (16, 16),
                    (20, 24),
                    (24, 28),
                    (32, 32),
                ] {
                    write_u32(env, info + dest, word(src))?;
                }
            }
            4 | 5 => write_u32(env, info, word(4))?,
            6..=8 => {
                for (dest, src) in [(0, 8), (4, 12), (8, 16), (12, 24), (16, 32), (80, 4)] {
                    write_u32(env, info + dest, word(src))?;
                }
                for i in 0..word(32) {
                    write_u32(env, info + 20 + i * 4, word(40 + i as usize * 8))?;
                }
            }
            9 => {
                for (dest, src) in [(0, 4), (4, 8), (8, 16), (12, 20), (16, 24)] {
                    write_u32(env, info + dest, word(src))?;
                }
                partial = wide(8) > u64::from(u32::MAX);
                if !partial {
                    // Wine drops load notifications for non-i386 images.
                    if let Ok(fd) = server::handle_fd(env, word(4)) {
                        let mut dos = [0u8; 64];
                        // SAFETY: pread fills bounded buffers; the borrowed descriptor stays open.
                        unsafe {
                            if libc::pread(fd.fd, dos.as_mut_ptr().cast(), dos.len(), 0)
                                == dos.len() as isize
                            {
                                let offset = u32::from_le_bytes(dos[60..64].try_into().unwrap());
                                let mut header = [0u8; 6];
                                if libc::pread(
                                    fd.fd,
                                    header.as_mut_ptr().cast(),
                                    header.len(),
                                    i64::from(offset),
                                ) == header.len() as isize
                                {
                                    partial = u16::from_le_bytes([header[4], header[5]]) != 0x14c;
                                }
                                if let Some(file) = &fd.file {
                                    file.position.set(0);
                                } else {
                                    libc::lseek(fd.fd, 0, libc::SEEK_SET);
                                }
                            }
                        }
                    }
                }
            }
            10 => {
                write_u32(env, info, word(8))?;
                partial = wide(8) > u64::from(u32::MAX);
            }
            _ => return Err(0xc000_00e5u32.into()),
        }
        if partial {
            if code == 9 {
                server::close(env, word(4))?;
            }
            let mut request = crate::protocol::ContinueDebugEventRequest {
                debug: args[0],
                pid: reply.pid,
                tid: reply.tid,
                status: 0x0001_0002,
                ..Default::default()
            };
            let _ = server::request(env, &mut request, &[], &mut [])?;
            return Err(0x8000_000du32.into());
        }
        Ok(())
    })();
    match result {
        Err(status::Error::Status(0x8000_000d)) => debug_event(env, args, true),
        Err(status::Error::Status(status::PENDING)) if !wait => Outcome::Return(0x102),
        Err(status::Error::Status(status::PENDING)) => {
            let then: Then = Box::new(move |env, ended| match ended {
                Ended::Status(0) => debug_event(env, args, false),
                Ended::Status(code) => Outcome::Return(code),
                Ended::Apc { func, args } => run_apc(env, None, func, args, USER_APC),
            });
            // As NtWaitForSingleObject: a debug object's sync is in-process
            // when the device exists, and the server cannot queue on it.
            let then = match prepare_inproc_wait(env, &[args[0]], true, args[1], args[2], None) {
                Ok(wait) => return run_inproc_wait(env, wait, then),
                Err(status::Error::Status(status::NOT_IMPLEMENTED)) => then,
                Err(error) => return status::finish(Err(error)),
            };
            let timeout = match read_timeout(env, args[2]) {
                Ok(timeout) => timeout,
                Err(error) => return Outcome::Return(error),
            };
            let op = [SELECT_WAIT, args[0]]
                .into_iter()
                .flat_map(u32::to_le_bytes)
                .collect();
            let flags = SELECT_INTERRUPTIBLE
                | if args[1] as u8 != 0 {
                    SELECT_ALERTABLE
                } else {
                    0
                };
            let select = new_select(env, op, flags, timeout);
            run_select(env, select, then)
        }
        result => status::finish(result),
    }
}

fn inproc_get(
    env: &mut Env<'_>,
    handle: u32,
    kind: u32,
    access: u32,
) -> Result<server::InprocFd, status::Error> {
    if server::inproc_device(env).is_none() {
        return Err(status::NOT_IMPLEMENTED.into());
    }
    let sync = server::inproc_fd(env, Some(handle))?;
    if kind != 0 && kind != sync.kind {
        return Err(status::OBJECT_TYPE_MISMATCH.into());
    }
    if sync.access & access != access {
        return Err(0xc000_0022.into());
    }
    Ok(sync)
}
fn inproc_status(errno: i32, command: u32) -> Status {
    match errno {
        libc::EOVERFLOW if command == ntsync::SEM_RELEASE => 0xc000_0047,
        libc::EOVERFLOW => 0xc000_0191,
        libc::EPERM => 0xc000_0046,
        libc::EINVAL => status::INVALID_PARAMETER,
        libc::ETIMEDOUT => 0x102,
        libc::EBADF => status::INVALID_HANDLE,
        libc::ENOMEM => status::NO_MEMORY,
        _ => status::INVALID_PARAMETER,
    }
}
fn inproc_operation(
    env: &mut Env<'_>,
    handle: u32,
    kind: u32,
    access: u32,
    command: u32,
    value: [u32; 2],
) -> Result<Option<[u32; 2]>, status::Error> {
    let priority = adv_machine::WakePriority::Normal;
    inproc_operation_as(env, handle, kind, access, command, value, priority)
}
fn inproc_operation_as(
    env: &mut Env<'_>,
    handle: u32,
    kind: u32,
    access: u32,
    command: u32,
    mut value: [u32; 2],
    priority: adv_machine::WakePriority,
) -> Result<Option<[u32; 2]>, status::Error> {
    let sync = match inproc_get(env, handle, kind, access) {
        Err(status::Error::Status(status::NOT_IMPLEMENTED)) => return Ok(None),
        result => result?,
    };
    // SAFETY: value holds the two fields used by every object ioctl.
    match unsafe { ntsync::ioctl(sync.fd.as_raw_fd(), command, value.as_mut_ptr()) } {
        Ok(_) => {}
        Err(libc::EOWNERDEAD) if command == ntsync::MUTEX_READ => value[0] = u32::MAX,
        Err(error) => return Err(inproc_status(error, command).into()),
    }
    deliver_inproc_as(env, priority);
    Ok(Some(value))
}

type InprocCompletion = std::rc::Weak<ntsync::Completion>;
#[derive(Default)]
struct InprocWaits(
    std::collections::HashMap<adv_machine::CtxId, (adv_machine::sched::Handle, InprocCompletion)>,
);
pub(crate) fn deliver_inproc(env: &mut Env<'_>) -> bool {
    deliver_inproc_as(env, adv_machine::WakePriority::Normal)
}
fn deliver_inproc_as(env: &mut Env<'_>, priority: adv_machine::WakePriority) -> bool {
    let Some(waits) = env.ext.try_get_mut::<InprocWaits>() else {
        return false;
    };
    let mut delivered = false;
    waits.0.retain(|_, (wake, result)| {
        let Some(result) = result.upgrade() else {
            return false;
        };
        if result.get().is_some() {
            delivered |= env.sys.set_event_with_priority(*wake, priority);
        }
        true
    });
    delivered
}

/// Set an audio notification event; the waiters it satisfies run first, as
/// the built-in stack's audio wakes do.
pub fn set_audio_event(env: &mut Env<'_>, handle: u32) -> Result<(), status::Error> {
    signal_as(env, handle, SET_EVENT, adv_machine::WakePriority::Audio).map(drop)
}
struct InprocWait {
    pending: ntsync::Pending,
    _objects: Vec<server::InprocFd>,
    _alert: Option<server::InprocFd>,
    count: u32,
    any: bool,
    deadline: Option<u64>,
    wake: adv_machine::sched::Handle,
}
fn inproc_wait(
    env: &mut Env<'_>,
    handles: &[u32],
    any: bool,
    alertable: u32,
    timeout: u32,
    signal: Option<u32>,
) -> Option<Outcome> {
    server::inproc_device(env)?;
    match prepare_inproc_wait(env, handles, any, alertable, timeout, signal) {
        Err(status::Error::Status(status::NOT_IMPLEMENTED)) => None,
        Err(error) => Some(status::finish(Err(error))),
        Ok(wait) => Some(run_inproc_wait(
            env,
            wait,
            Box::new(|env, ended| match ended {
                Ended::Status(code) => Outcome::Return(code),
                Ended::Apc { func, args } => run_apc(env, None, func, args, USER_APC),
            }),
        )),
    }
}
fn prepare_inproc_wait(
    env: &mut Env<'_>,
    handles: &[u32],
    any: bool,
    alertable: u32,
    timeout: u32,
    signal: Option<u32>,
) -> Result<InprocWait, status::Error> {
    let device = server::inproc_device(env).ok_or(status::NOT_IMPLEMENTED)?;
    (|| {
        let signal = signal
            .map(|handle| inproc_get(env, handle, 0, 0))
            .transpose()?;
        if let Some(signal) = &signal {
            let access = match signal.kind {
                2 | 4 => 2,
                3 => 0x10_0000,
                _ => return Err(status::OBJECT_TYPE_MISMATCH.into()),
            };
            if signal.access & access == 0 {
                return Err(0xc000_0022.into());
            }
        }
        let objects = handles
            .iter()
            .map(|&handle| inproc_get(env, handle, 0, 0x10_0000))
            .collect::<Result<Vec<_>, _>>()?;
        if let Some(signal) = signal {
            let tid = server::current_tid(env)?;
            let (command, mut value) = match signal.kind {
                2 => (ntsync::EVENT_SET, [0, 0]),
                3 => (ntsync::MUTEX_UNLOCK, [tid, 0]),
                4 => (ntsync::SEM_RELEASE, [1, 0]),
                _ => unreachable!(),
            };
            // SAFETY: value supplies the command's UAPI fields.
            unsafe { ntsync::ioctl(signal.fd.as_raw_fd(), command, value.as_mut_ptr()) }
                .map_err(|e| status::Error::from(inproc_status(e, command)))?;
            deliver_inproc(env);
        }
        let alert = if alertable as u8 != 0 {
            Some(server::inproc_fd(env, None)?)
        } else {
            None
        };
        let timeout = read_timeout(env, timeout)?;
        let deadline = timeout
            .filter(|&t| t != TIMEOUT_INFINITE)
            .map(|t| deadline_us(env, t));
        let fds: Vec<_> = objects.iter().map(|o| o.fd.as_raw_fd()).collect();
        let tid = server::current_tid(env)?;
        let args = ntsync::WaitArgs {
            timeout: deadline.map_or(u64::MAX, |d| d.saturating_mul(1000)),
            objs: fds.as_ptr() as u64,
            count: fds.len() as u32,
            owner: tid,
            alert: alert.as_ref().map_or(0, |o| o.fd.as_raw_fd() as u32),
            ..Default::default()
        };
        let command = if !any && handles.len() != 1 {
            ntsync::WAIT_ALL
        } else {
            ntsync::WAIT_ANY
        };
        // SAFETY: fds stays live until wait_ioctl has registered the objects.
        let pending = unsafe { ntsync::wait_ioctl(device, command, &args) }
            .map_err(|e| status::Error::from(inproc_status(e, command)))?;
        let wake = env.sys.objects.insert(adv_machine::sched::Object::Event {
            manual_reset: false,
            signaled: false,
        });
        env.ext
            .get_or_default::<InprocWaits>()
            .0
            .insert(env.context, (wake, pending.completion()));
        Ok::<_, status::Error>(InprocWait {
            pending,
            _objects: objects,
            _alert: alert,
            count: handles.len() as u32,
            any,
            deadline,
            wake,
        })
    })()
}
fn run_inproc_wait(env: &mut Env<'_>, wait: InprocWait, then: Then) -> Outcome {
    let result = wait.pending.result().map(|result| {
        result
            .map(|(index, dead)| {
                if index == wait.count {
                    USER_APC
                } else {
                    (if dead { 0x80 } else { 0 }) + if wait.any { index } else { 0 }
                }
            })
            .unwrap_or_else(|e| inproc_status(e, 0))
    });
    let result = result.or_else(|| {
        wait.deadline
            .filter(|&d| d <= env.sys.clock.now_us())
            .map(|_| 0x102)
    });
    if let Some(result) = result {
        env.ext
            .get_or_default::<InprocWaits>()
            .0
            .remove(&env.context);
        env.ext
            .get_or_default::<Timers>()
            .waits
            .remove(&env.context);
        env.sys.objects.close(wait.wake);
        drop(wait);
        if result == USER_APC {
            let select = new_select(
                env,
                Vec::new(),
                SELECT_INTERRUPTIBLE | SELECT_ALERTABLE,
                Some(0),
            );
            return run_select(env, select, then);
        }
        return then(env, Ended::Status(result));
    }
    let timer = match timer_deadline(env) {
        Ok(timer) => timer,
        Err(error) => return status::finish(Err(error)),
    };
    let deadline = match (wait.deadline, timer) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    env.ext
        .get_or_default::<Timers>()
        .waits
        .insert(env.context, wait.wake);
    Outcome::block(
        Wait::Object {
            handle: wait.wake,
            deadline,
        },
        move |env, _| {
            server::run(env);
            run_inproc_wait(env, wait, then)
        },
    )
}

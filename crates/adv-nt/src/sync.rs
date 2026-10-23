//! Synchronization, after Wine's `unix/sync.c` and `server_select`.
//!
//! Events live in the server. A wait is a `select` request; when the server
//! cannot end it at once, it ends it later with a wake-up on the thread's
//! wait pipe. Meanwhile the guest context blocks in the machine on its wake
//! object, which the client sets when it finds the wake-up after any server
//! run, or until the wait's deadline, which the server then sees pass. A
//! plain sleep needs no server, as in Wine.
//!
//! A user APC queued to an alertable wait ends it; the thread then runs the
//! APC in ntdll.dll's `KiUserApcDispatcher`, which resumes the interrupted
//! context through `NtContinue`.

use adv_cpu::backend::{ArchitecturalState, GuestExt, Reg};
use adv_machine::{Env, Outcome, Wait};

use crate::exception::{self, Dispatchers};
use crate::object::{Attributes, read_u32, write_u32};
use crate::protocol::{CreateEventRequest, EventOpRequest, QueueApcRequest, SelectRequest};
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
        let mut request = EventOpRequest {
            handle,
            op,
            ..Default::default()
        };
        let (reply, _) = server::request(env, &mut request, &[], &mut [])?;
        status::check(reply.header.error)?;
        if previous != 0 {
            write_u32(env, previous, reply.state as u32)?;
        }
        Ok(())
    })())
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
        Ok(handles) => wait(env, &handles, wait_any as u8 != 0, alertable, timeout),
        Err(status) => Outcome::Return(status),
    }
}

fn wait(env: &mut Env<'_>, handles: &[u32], any: bool, alertable: u32, timeout: u32) -> Outcome {
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

/// NtDelayExecution(alertable, *timeout)
pub fn delay(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[alertable, timeout] = args else {
        unreachable!()
    };
    if alertable as u8 != 0 {
        return begin_select(
            env,
            Vec::new(),
            SELECT_INTERRUPTIBLE | SELECT_ALERTABLE,
            timeout,
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
            Ended::Apc { func, args } => run_apc(env, None, func, args),
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
    let mut request = SelectRequest {
        flags: select.flags,
        cookie: select.cookie,
        timeout: select.timeout,
        size: select.op.len() as u32,
        ..Default::default()
    };
    let mut call = [0u8; APC_CALL_SIZE];
    let reply = server::request(
        env,
        &mut request,
        &[&[0; APC_RESULT_SIZE], &select.op],
        &mut call,
    );
    let (reply, size) = match reply {
        Ok(reply) => reply,
        Err(error) => return status::finish(Err(error)),
    };
    if reply.header.error == KERNEL_APC {
        return Outcome::Fail("system APCs".into());
    }
    if reply.signaled == 0 {
        return block(env, select, then);
    }
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
    let wait = Wait::Object {
        handle: wake,
        deadline: select.deadline,
    };
    Outcome::block(wait, move |env, _| wake_up(env, select, then))
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
        Ok(Some(wake_up)) => then(env, Ended::Status(wake_up.signaled)),
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
            Ended::Apc { func, args } => run_apc(env, Some(context), func, args),
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
            state.regs[0] = USER_APC;
            state
        }
    };
    let stack = interrupted.regs[4] - APC_LAYOUT_SIZE;
    let mut frame = vec![0u8; APC_LAYOUT_SIZE as usize];
    for (index, value) in [func, args[0], args[1], args[2], 1].into_iter().enumerate() {
        frame[4 * index..4 * index + 4].copy_from_slice(&value.to_le_bytes());
    }
    exception::put_context(&mut frame, APC_CONTEXT, &interrupted);
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
    status::finish((|| {
        let mut request = QueueApcRequest {
            handle: thread,
            ..Default::default()
        };
        let mut call = [0u8; APC_CALL_SIZE];
        if func != 0 {
            for (offset, value) in [(0, APC_USER), (8, func), (16, arg1), (24, arg2), (32, arg3)] {
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

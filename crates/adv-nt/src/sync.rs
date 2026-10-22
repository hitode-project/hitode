//! Synchronization, after Wine's `unix/sync.c` and `server_select`.
//!
//! Events live in the server. A wait is a `select` request; when the server
//! cannot end it at once, it ends it later with a wake-up on the thread's
//! wait pipe. Meanwhile the guest context blocks in the machine until the
//! wait's deadline, which the server then sees pass. A plain sleep needs no
//! server, as in Wine.

use adv_cpu::backend::Reg;
use adv_machine::{Env, Outcome, Wait};

use crate::object::{Attributes, read_u32, write_u32};
use crate::protocol::{CreateEventRequest, EventOpRequest, SelectRequest};
use crate::server::{self, Time};
use crate::status::{self, Error, Status};

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
    select(env, &op, flags, timeout)
}

/// NtDelayExecution(alertable, *timeout)
pub fn delay(env: &mut Env<'_>, args: &[u32]) -> Outcome {
    let &[alertable, timeout] = args else {
        unreachable!()
    };
    if alertable as u8 != 0 {
        return select(env, &[], SELECT_INTERRUPTIBLE | SELECT_ALERTABLE, timeout);
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

/// Sends a `select` with `op`, then, if the server cannot end the wait at
/// once, blocks until it does.
fn select(env: &mut Env<'_>, op: &[u8], flags: i32, timeout: u32) -> Outcome {
    let timeout = match read_timeout(env, timeout) {
        Ok(timeout) => timeout,
        Err(status) => return Outcome::Return(status),
    };
    // As Wine's `server_wait`: a relative timeout becomes an absolute time
    // on the performance counter, which the server reads negated.
    let time = Time::now(&mut env.sys.clock);
    let absolute = match timeout {
        None => TIMEOUT_INFINITE,
        Some(timeout) if timeout < 0 => timeout - time.monotonic as i64,
        Some(timeout) => timeout,
    };
    // Wine names a wait by an address on the waiting thread's stack.
    let cookie = u64::from(env.cpu.reg(Reg::Esp));
    let mut request = SelectRequest {
        flags,
        cookie,
        timeout: absolute,
        size: op.len() as u32,
        ..Default::default()
    };
    let reply = server::request(env, &mut request, &[&[0; APC_RESULT_SIZE], op], &mut []);
    let reply = match reply {
        Ok((reply, _)) => reply,
        Err(error) => return status::finish(Err(error)),
    };
    if reply.signaled != 0 {
        return completed(reply.header.error);
    }
    let Some(timeout) = timeout else {
        return Outcome::Fail("a wait that only another thread could end".into());
    };
    let deadline = deadline_us(env, timeout);
    Outcome::block(Wait::Until(deadline), move |env, _| wake(env, cookie))
}

/// Ends a blocked wait with the server's wake-up, or blocks again.
fn wake(env: &mut Env<'_>, cookie: u64) -> Outcome {
    match server::wake_up(env) {
        Err(error) => status::finish(Err(error)),
        // The thread was killed while it waited.
        Ok(Some(wake_up)) if wake_up.cookie == 0 => Outcome::ExitThread(wake_up.signaled),
        Ok(Some(wake_up)) if wake_up.cookie != cookie => Outcome::Fail(format!(
            "a wake-up for wait {:#x} ended wait {cookie:#x}",
            wake_up.cookie
        )),
        Ok(Some(wake_up)) => completed(wake_up.signaled),
        Ok(None) => {
            let retry = env.sys.clock.now_us() + RETRY_US;
            Outcome::block(Wait::Until(retry), move |env, _| wake(env, cookie))
        }
    }
}

fn completed(status: Status) -> Outcome {
    match status {
        USER_APC | KERNEL_APC => status::finish(Err(Error::Unsupported(
            "asynchronous procedure calls".into(),
        ))),
        status => Outcome::Return(status),
    }
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

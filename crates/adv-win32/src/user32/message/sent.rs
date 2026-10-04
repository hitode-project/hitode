//! Nonqueued messages: owned by the receiving guest thread, never a host worker.
//! All queue/result transitions happen on the API owner. Waiting senders may
//! reenter their own window procedures, without retaining a Rust borrow.
use super::*;
use std::cell::Cell;
use std::rc::Rc;

const MAX_PENDING: usize = 10_000;
const ERROR_TIMEOUT: u32 = 1460;
const ERROR_WINDOW: u32 = 1400;
pub(super) const QS_SEND: u32 = 0x40;
pub(super) type Done = Box<dyn FnOnce(&mut Env<'_>, Result<u32, u32>) -> Outcome>;

#[derive(Default)]
pub(in crate::user32) struct Queue {
    pending: VecDeque<Rc<Call>>,
    active: Vec<Rc<Call>>,
    event: u32,
    next_id: u64,
    last_pump: u64,
}

struct Call {
    id: u64,
    sender: u32,
    receiver: u32,
    window: u32,
    created: u64,
    args: [u32; 4],
    done: u32,
    thread: u32,
    result: Cell<Option<Result<u32, u32>>>,
    cancelled: Cell<bool>,
    started: Cell<bool>,
}

#[derive(Clone, Copy, Default)]
pub(super) struct Options {
    pub deadline: Option<u64>,
    pub block: bool,
    pub no_timeout_if_responsive: bool,
    pub abort_if_hung: bool,
    pub error_on_exit: bool,
}

pub(super) fn pending(env: &Env<'_>, context: u32) -> bool {
    user(env)
        .queues
        .get(&context)
        .is_some_and(|q| !q.sent.pending.is_empty())
}

fn notification(env: &mut Env<'_>, context: u32) -> u32 {
    let old = user_mut(env).queue(context).sent.event;
    if old != 0 {
        return old;
    }
    let event = env.sys.objects.insert(Object::Event {
        manual_reset: true,
        signaled: pending(env, context),
    });
    user_mut(env).queue(context).sent.event = event;
    event
}

pub(super) fn send(
    env: &mut Env<'_>,
    receiver: u32,
    args: [u32; 4],
    options: Options,
    then: impl FnOnce(&mut Env<'_>, Result<u32, u32>) -> Outcome + 'static,
) -> Outcome {
    let Some(window) = user(env).windows.get(&env.sys.objects, &args[0]) else {
        return then(env, Err(ERROR_WINDOW));
    };
    let created = window.created_us;
    if env
        .sys
        .contexts
        .get(receiver as usize)
        .is_none_or(|c| c.is_done())
    {
        return then(env, Err(ERROR_WINDOW));
    }
    let sender = env.context;
    if user_mut(env).queue(receiver).sent.pending.len() >= MAX_PENDING {
        return then(env, Err(1816));
    }
    let Some(id) = user_mut(env).queue(sender).sent.next_id.checked_add(1) else {
        return then(env, Err(1816));
    };
    let signal = notification(env, sender);
    let done = env.sys.objects.insert(Object::Event {
        manual_reset: true,
        signaled: false,
    });
    // A private thread reference survives the application's CloseHandle.
    let thread = env.sys.objects.insert(Object::Thread { ctx: receiver });
    if signal == 0 || done == 0 || thread == 0 {
        if done != 0 {
            env.sys.objects.close(done);
        }
        if thread != 0 {
            env.sys.objects.close(thread);
        }
        return then(env, Err(8));
    }
    user_mut(env).queue(sender).sent.next_id = id;
    let call = Rc::new(Call {
        id,
        sender,
        receiver,
        window: args[0],
        created,
        args,
        done,
        thread,
        result: Cell::new(None),
        cancelled: Cell::new(false),
        started: Cell::new(false),
    });
    let now = env.sys.clock.now_us();
    let target = &mut user_mut(env).queue(receiver).sent;
    if target.last_pump == 0 {
        target.last_pump = now;
    }
    target.pending.push_back(call.clone());
    let signal = target.event;
    if signal != 0 {
        env.sys.set_event(signal);
    }
    user_mut(env).queue(receiver).changed_status |= QS_SEND;
    queue_changed(env, receiver);
    wait_reply(env, call, options, Box::new(then))
}

fn finish(env: &mut Env<'_>, call: Rc<Call>, result: Result<u32, u32>, then: Done) -> Outcome {
    call.cancelled.set(true);
    let queue = &mut user_mut(env).queue(call.receiver).sent;
    queue.pending.retain(|other| !Rc::ptr_eq(other, &call));
    let event = queue.event;
    if queue.pending.is_empty() && event != 0 {
        env.sys.reset_event(event);
    }
    env.sys.objects.close(call.done);
    env.sys.objects.close(call.thread);
    then(env, result)
}

fn wait_reply(env: &mut Env<'_>, call: Rc<Call>, options: Options, then: Done) -> Outcome {
    debug_assert_eq!(env.context, call.sender);
    let exists = user(env)
        .windows
        .get(&env.sys.objects, &call.window)
        .is_some_and(|w| w.created_us == call.created && !w.destroying);
    if options.error_on_exit && !exists {
        return finish(env, call, Err(ERROR_WINDOW), then);
    }
    if let Some(result) = call.result.get() {
        return finish(env, call, result, then);
    }
    if (!exists && !call.started.get()) || env.sys.contexts[call.receiver as usize].is_done() {
        return finish(env, call, Err(ERROR_WINDOW), then);
    }
    let now = env.sys.clock.now_us();
    let responsive_until = user_mut(env)
        .queue(call.receiver)
        .sent
        .last_pump
        .saturating_add(5_000_000);
    let mut deadline = options.deadline;
    if options.no_timeout_if_responsive && now < responsive_until {
        deadline = deadline.map(|at| at.max(responsive_until));
    }
    if options.abort_if_hung {
        deadline = Some(deadline.map_or(responsive_until, |at| at.min(responsive_until)));
    }
    if deadline.is_some_and(|at| now >= at) {
        return finish(env, call, Err(ERROR_TIMEOUT), then);
    }
    if !options.block && pending(env, env.context) {
        return drain(env, move |env| wait_reply(env, call, options, then));
    }
    let notification = (!options.block).then(|| notification(env, env.context));
    if let Some(event) = notification {
        env.sys.reset_event(event);
    }
    // Never time-skip this wait: another real guest context must complete it.
    Outcome::block(
        Wait::Objects {
            handles: vec![call.done, call.thread],
            all: false,
            deadline,
            notification,
        },
        move |env, _| wait_reply(env, call, options, then),
    )
}

fn reply(env: &mut Env<'_>, call: &Call, result: Result<u32, u32>) {
    if !call.cancelled.get() && call.result.get().is_none() {
        call.result.set(Some(result));
        env.sys.set_event(call.done);
    }
}

pub(in crate::user32) fn drain(
    env: &mut Env<'_>,
    then: impl FnOnce(&mut Env<'_>) -> Outcome + 'static,
) -> Outcome {
    let context = env.context;
    let now = env.sys.clock.now_us();
    let queue = &mut user_mut(env).queue(context).sent;
    queue.last_pump = now;
    let call = loop {
        match queue.pending.pop_front() {
            Some(call) if !call.cancelled.get() => break Some(call),
            Some(_) => continue,
            None => break None,
        }
    };
    if queue.pending.is_empty() {
        let event = queue.event;
        if event != 0 {
            env.sys.reset_event(event);
        }
    }
    let Some(call) = call else {
        return then(env);
    };
    if env.sys.contexts[call.sender as usize].is_done() {
        return finish(
            env,
            call,
            Err(ERROR_WINDOW),
            Box::new(move |env, _| drain(env, then)),
        );
    }
    let Some(proc) = user(env)
        .windows
        .get(&env.sys.objects, &call.window)
        .filter(|w| w.created_us == call.created && w.thread == context && !w.destroying)
        .map(|w| w.proc)
    else {
        reply(env, &call, Err(ERROR_WINDOW));
        return drain(env, then);
    };
    user_mut(env).queue(context).sent.active.push(call.clone());
    call.started.set(true);
    let [window, message, wp, lp] = call.args;
    call_proc(proc, window, message, wp, lp, move |env, value| {
        let active = user_mut(env)
            .queue(context)
            .sent
            .active
            .pop()
            .expect("live sent callback");
        debug_assert_eq!((active.sender, active.id), (call.sender, call.id));
        reply(env, &call, Ok(value));
        drain(env, then)
    })
}

pub(super) fn in_send(env: &mut Env<'_>) -> Outcome {
    let context = env.context;
    Outcome::Return(u32::from(
        !user_mut(env).queue(context).sent.active.is_empty(),
    ))
}

pub(super) fn reply_message(env: &mut Env<'_>) -> Outcome {
    let value = env.arg(0);
    let context = env.context;
    let call = user_mut(env).queue(context).sent.active.last().cloned();
    let Some(call) = call else {
        return Outcome::Return(0);
    };
    let fresh = !call.cancelled.get() && call.result.get().is_none();
    if fresh {
        reply(env, &call, Ok(value));
    }
    Outcome::Return(u32::from(fresh))
}

/// Destruction cancels undelivered messages, not a WndProc already on the stack.
pub(in crate::user32) fn cancel_window(env: &mut Env<'_>, hwnd: u32) {
    let mut removed = Vec::new();
    for queue in user_mut(env).queues.values_mut() {
        queue.sent.pending.retain(|call| {
            if call.window == hwnd {
                removed.push(call.clone());
                false
            } else {
                true
            }
        });
    }
    for call in removed {
        reply(env, &call, Err(ERROR_WINDOW));
    }
}

pub(crate) fn exit_thread(env: &mut Env<'_>) {
    let context = env.context;
    let queue = std::mem::take(&mut user_mut(env).queue(context).sent);
    if queue.event != 0 {
        env.sys.objects.close(queue.event);
    }
    for call in queue.pending.into_iter().chain(queue.active) {
        reply(env, &call, Err(ERROR_WINDOW));
    }
    let mut abandoned = Vec::new();
    for queue in user_mut(env).queues.values_mut() {
        queue.sent.pending.retain(|call| {
            if call.sender == context {
                abandoned.push(call.clone());
                false
            } else {
                true
            }
        });
        abandoned.extend(
            queue
                .sent
                .active
                .iter()
                .filter(|call| call.sender == context)
                .cloned(),
        );
    }
    for call in abandoned {
        if !call.cancelled.replace(true) {
            env.sys.objects.close(call.done);
            env.sys.objects.close(call.thread);
        }
    }
}

#[cfg(test)]
mod tests;

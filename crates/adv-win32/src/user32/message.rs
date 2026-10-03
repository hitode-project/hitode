//! Messages: sending (a guest call into the window procedure), per-thread posted-message
//! queues, GetMessage/PeekMessage, timers, and DefWindowProc.
//!
//! SendMessage always calls the procedure on the *calling* context, also for windows of
//! other threads; Win32 would switch to the owner thread. PAL-era games only send across
//! threads to PAL's own window, whose procedure is host code, so the difference is not
//! observable there.
//!
//! Retrieval order follows Win32: posted messages, then WM_QUIT once nothing is posted,
//! then WM_PAINT for invalid windows, then due WM_TIMERs. An empty queue yields
//! (PeekMessage) or blocks on the queue's event (GetMessage), so waiting for input costs
//! no host CPU.

use std::collections::VecDeque;
use std::collections::{BTreeMap, HashMap};

use adv_cpu::backend::GuestExt;
use adv_machine::{Env, Object, Outcome, Registry, Wait};

use super::window::{self, Rect, destroy_window, make_long, wm};
use super::{USER32, user, user_mut};
use crate::util::{cstr, put_cstr};
use crate::{Win32, stdcall};

#[derive(Clone, Copy, Debug, Default)]
pub struct Msg {
    pub hwnd: u32,
    pub msg: u32,
    pub wparam: u32,
    pub lparam: u32,
    pub time: u32,
    pub pt: (i32, i32),
    /// Host-delivered physical input, rather than an application PostMessage.
    /// This metadata is never written to the guest's MSG structure.
    pub input: bool,
    /// Identity of this posted host message, never part of the guest MSG bytes.
    pub input_receipt: u64,
}

#[derive(Default)]
struct InputReceipt {
    dispatched: bool,
    dispatching: bool,
    dequeued_frame: Option<u32>,
    observed_frame: Option<u32>,
    key: Option<(usize, u64)>,
    context: adv_machine::CtxId,
}

/// Optional observation of host-delivered message instances and physical key
/// generations. It does not affect queue ordering, waits or guest dispatch.
pub struct InputTracking {
    latest: u64,
    receipts: BTreeMap<u64, InputReceipt>,
    retrieved: HashMap<(adv_machine::CtxId, u32), Msg>,
    key_receipts: [u64; 256],
    key_observations: [Option<u64>; 256],
    polled: [Option<(u64, u32)>; 256],
}
impl Default for InputTracking {
    fn default() -> Self {
        Self {
            latest: 0,
            receipts: BTreeMap::new(),
            retrieved: HashMap::new(),
            key_receipts: [0; 256],
            key_observations: [None; 256],
            polled: [None; 256],
        }
    }
}
impl InputTracking {
    pub fn latest(&self) -> u64 {
        self.latest
    }
    fn issue(&mut self, key: Option<(usize, u8, Option<u64>)>) -> u64 {
        self.latest = self
            .latest
            .checked_add(1)
            .expect("host input receipt overflow");
        let receipt = self.latest;
        let key =
            key.map(|(vk, value, host)| (vk, host.unwrap_or((receipt << 8) | u64::from(value))));
        let mut status = InputReceipt {
            key,
            ..Default::default()
        };
        if let Some((vk, observation)) = key {
            // A physical release may be polled before its queued WM_KEYUP is
            // drained. Only that new generation can acknowledge it in advance;
            // an old identical value cannot acknowledge a new synthetic input.
            if self.key_observations[vk].unwrap_or(0) != observation
                && let Some((seen, frame)) = self.polled[vk]
                && seen == observation
            {
                status.observed_frame = Some(frame);
            }
            self.key_observations[vk] = Some(observation);
            self.key_receipts[vk] = receipt;
        }
        self.receipts.insert(receipt, status);
        receipt
    }
    pub(super) fn key_observation(&self, vk: usize, value: u8) -> u64 {
        (self.key_receipts[vk] << 8) | u64::from(value)
    }
    pub(super) fn observe_key(&mut self, vk: usize, observation: u64, frame: u32) {
        self.polled[vk] = Some((observation, frame));
        if let Some(receipt) = self.receipts.get_mut(&self.key_receipts[vk])
            && receipt.key == Some((vk, observation))
        {
            receipt.observed_frame.get_or_insert(frame);
        }
    }
    fn remember(&mut self, ctx: adv_machine::CtxId, address: u32, message: Msg, removed: bool) {
        // Every retrieval rebinds the MSG output slot, including non-input.
        self.retrieved.remove(&(ctx, address));
        if removed && message.input_receipt != 0 {
            self.retrieved.insert((ctx, address), message);
        }
    }
    fn take(&mut self, ctx: adv_machine::CtxId, address: u32, message: Msg) -> Msg {
        match self.retrieved.remove(&(ctx, address)) {
            Some(original) if original.guest_fields() == message.guest_fields() => original,
            _ => message,
        }
    }
    fn complete(&mut self, receipt: u64) {
        if let Some(status) = self.receipts.get_mut(&receipt) {
            status.dispatched = true;
        }
    }
    fn dequeued(&mut self, receipt: u64, frame: u32) {
        if let Some(status) = self.receipts.get_mut(&receipt) {
            status.dequeued_frame.get_or_insert(frame);
        }
    }
    fn dispatching(&mut self, receipt: u64) {
        if let Some(status) = self.receipts.get_mut(&receipt) {
            status.dispatching = true;
        }
    }
    pub fn set_context(&mut self, receipt: u64, context: adv_machine::CtxId) {
        if let Some(status) = self.receipts.get_mut(&receipt) {
            status.context = context;
        }
    }
    pub fn context(&self, receipt: u64) -> Option<adv_machine::CtxId> {
        self.receipts.get(&receipt).map(|status| status.context)
    }
    /// A dispatch receipt completes only when its window procedure returns.
    /// A polling receipt additionally needs a subsequent frame, or an explicit
    /// ordinary full-slice/idle boundary supplied by the execution owner.
    pub fn completed(&mut self, receipt: u64, frame: u32, full_or_idle_boundary: bool) -> bool {
        self.fence_reached(receipt, frame, full_or_idle_boundary, false)
    }
    /// A non-dispatching application can form a dequeue/physical-observation
    /// fence. An unobserved receipt needs its owning context to complete a full
    /// allowance or block; that means an opportunity, not an application ACK.
    pub fn fence_reached(
        &mut self,
        receipt: u64,
        frame: u32,
        full_or_idle_boundary: bool,
        target_boundary: bool,
    ) -> bool {
        if receipt == 0 {
            return true;
        }
        let ready = self.receipts.get(&receipt).is_some_and(|status| {
            status.dispatched
                || status
                    .observed_frame
                    .is_some_and(|observed| observed != frame || full_or_idle_boundary)
                || (!status.dispatching
                    && status
                        .dequeued_frame
                        .is_some_and(|dequeued| dequeued != frame || full_or_idle_boundary))
                || (!status.dispatching && target_boundary)
        });
        if ready {
            self.receipts.retain(|&id, _| id > receipt);
            self.retrieved
                .retain(|_, message| message.input_receipt > receipt);
        }
        ready
    }
}

impl Msg {
    fn guest_fields(&self) -> [u32; 7] {
        [
            self.hwnd,
            self.msg,
            self.wparam,
            self.lparam,
            self.time,
            self.pt.0 as u32,
            self.pt.1 as u32,
        ]
    }
    fn write(&self, cpu: &mut dyn adv_cpu::backend::Guest, p: u32) -> bool {
        let f = self.guest_fields();
        let mut valid = true;
        for (i, v) in f.into_iter().enumerate() {
            valid &= cpu.write_u32(p + 4 * i as u32, v).is_ok();
        }
        valid
    }

    fn read(cpu: &dyn adv_cpu::backend::Guest, p: u32) -> Self {
        let r = |i: u32| cpu.read_u32(p + 4 * i).unwrap_or(0);
        Msg {
            hwnd: r(0),
            msg: r(1),
            wparam: r(2),
            lparam: r(3),
            time: r(4),
            pt: (r(5) as i32, r(6) as i32),
            input: false,
            input_receipt: 0,
        }
    }
}

pub struct Timer {
    pub hwnd: u32,
    pub id: u32,
    pub interval_us: u64,
    pub due_us: u64,
    pub proc: u32,
}

/// Call `hwnd`'s window procedure; `then` gets its result (0 if the window is gone).
pub fn send(
    env: &mut Env<'_>,
    hwnd: u32,
    msg: u32,
    wparam: u32,
    lparam: u32,
    then: impl FnOnce(&mut Env<'_>, u32) -> Outcome + 'static,
) -> Outcome {
    let Some(proc) = user(env).windows.get(&hwnd).map(|w| w.proc) else {
        return then(env, 0);
    };
    call_proc(proc, hwnd, msg, wparam, lparam, then)
}

pub fn call_proc(
    proc: u32,
    hwnd: u32,
    msg: u32,
    wparam: u32,
    lparam: u32,
    then: impl FnOnce(&mut Env<'_>, u32) -> Outcome + 'static,
) -> Outcome {
    if proc == 0 {
        return Outcome::Fail(format!(
            "message {msg:#x} to window {hwnd:#x}: null window procedure"
        ));
    }
    Outcome::call(proc, [hwnd, msg, wparam, lparam], move |env, r| {
        then(env, r.value())
    })
}

/// Send `[hwnd, msg, wparam, lparam]` messages in order (skipping destroyed windows).
pub fn send_all(
    env: &mut Env<'_>,
    mut msgs: VecDeque<[u32; 4]>,
    then: impl FnOnce(&mut Env<'_>) -> Outcome + 'static,
) -> Outcome {
    match msgs.pop_front() {
        None => then(env),
        Some([h, m, w, l]) => send(env, h, m, w, l, move |env, _| send_all(env, msgs, then)),
    }
}

/// Post to the queue of `hwnd`'s thread (or the current thread's for `hwnd` 0).
pub fn post(env: &mut Env<'_>, hwnd: u32, msg: u32, wparam: u32, lparam: u32) -> bool {
    post_kind(env, hwnd, msg, wparam, lparam, false)
}

/// Host physical input uses input queue categories; guest PostMessage and
/// TranslateMessage-generated characters remain ordinary posted messages.
pub fn post_input(env: &mut Env<'_>, hwnd: u32, msg: u32, wparam: u32, lparam: u32) -> bool {
    post_kind(env, hwnd, msg, wparam, lparam, true)
}

fn post_kind(
    env: &mut Env<'_>,
    hwnd: u32,
    msg: u32,
    wparam: u32,
    lparam: u32,
    input: bool,
) -> bool {
    let time = env.sys.clock.now_ms();
    let ctx = if hwnd == 0 {
        env.sys.current
    } else {
        match user(env).windows.get(&hwnd) {
            Some(w) => w.thread,
            None => return false,
        }
    };
    let input_receipt = if input {
        let u = user_mut(env);
        let vk = match msg {
            0x100 | 0x101 | 0x104 | 0x105 if wparam < 256 => Some(wparam as usize),
            0x201 | 0x202 => Some(1),
            0x204 | 0x205 => Some(2),
            0x207 | 0x208 => Some(4),
            _ => None,
        };
        let key = vk.map(|vk| {
            (
                vk,
                u.keys[vk],
                u.host_keys.as_ref().map(|keys| keys.observation(vk as u32)),
            )
        });
        u.input_tracking.as_mut().map_or(0, |tracking| {
            let receipt = tracking.issue(key);
            tracking.set_context(receipt, ctx);
            receipt
        })
    } else {
        0
    };
    let posted = post_to_thread(
        env,
        ctx,
        Msg {
            hwnd,
            msg,
            wparam,
            lparam,
            time,
            pt: user(env).cursor_pos,
            input,
            input_receipt,
        },
    );
    if posted && input {
        env.sys
            .prefer_ready_context(ctx, adv_machine::WakePriority::Input);
    }
    posted
}

fn post_to_thread(env: &mut Env<'_>, ctx: adv_machine::CtxId, m: Msg) -> bool {
    user_mut(env).queue(ctx).post(m);
    queue_changed(env, ctx);
    true
}

/// Notify both message retrieval and MsgWait. The latter needs a persistent,
/// mask-filtered input condition plus a separate invalidation for timer changes.
fn queue_changed(env: &mut Env<'_>, ctx: adv_machine::CtxId) {
    let changed = queue_status_for(env, ctx).1;
    let q = user_mut(env).queue(ctx);
    let (event, input, notification, mask) = (
        q.event,
        q.message_wait_event,
        q.message_wait_notification,
        q.message_wait_mask,
    );
    if input != 0 {
        if changed & mask != 0 {
            env.sys.set_event(input);
        } else {
            env.sys.reset_event(input);
        }
    }
    if event != 0 {
        env.sys.set_event(event);
    }
    if notification != 0 {
        env.sys.set_event(notification);
    }
}

/// Arm the input condition and deadline invalidation after MsgWait checks its
/// queue. Waiting does not itself mark any input old.
pub fn message_wait_events(env: &mut Env<'_>, mask: u32) -> (u32, u32) {
    let ctx = env.sys.current;
    for notification in [false, true] {
        let q = user_mut(env).queue(ctx);
        let handle = if notification {
            q.message_wait_notification
        } else {
            q.message_wait_event
        };
        if handle == 0 {
            let handle = env.sys.objects.insert(Object::Event {
                manual_reset: !notification,
                signaled: false,
            });
            let q = user_mut(env).queue(ctx);
            if notification {
                q.message_wait_notification = handle;
            } else {
                q.message_wait_event = handle;
            }
        }
    }
    let changed = queue_status_for(env, ctx).1;
    let q = user_mut(env).queue(ctx);
    q.message_wait_mask = mask;
    let (input, notification) = (q.message_wait_event, q.message_wait_notification);
    env.sys.reset_event(notification);
    if changed & mask != 0 {
        env.sys.set_event(input);
    } else {
        env.sys.reset_event(input);
    }
    (input, notification)
}

const QS_POSTMESSAGE: u32 = 0x8;
const QS_TIMER: u32 = 0x10;
const QS_PAINT: u32 = 0x20;
const QS_ALLPOSTMESSAGE: u32 = 0x100;

pub(super) fn posted_status(m: &Msg) -> u32 {
    if !m.input {
        return QS_POSTMESSAGE | QS_ALLPOSTMESSAGE;
    }
    match m.msg {
        wm::KEYDOWN | wm::KEYUP | wm::SYSKEYDOWN | 0x105 => 1, // WM_SYSKEYUP
        0x200 => 2,
        0x201..=0x20e => 4,
        _ => QS_POSTMESSAGE | QS_ALLPOSTMESSAGE,
    }
}

/// Current pending and new-input bits. Observing these internally does not make
/// input old; MsgWaitForMultipleObjects can report the same new post repeatedly.
pub fn queue_status(env: &mut Env<'_>) -> (u32, u32) {
    queue_status_for(env, env.sys.current)
}

fn queue_status_for(env: &mut Env<'_>, ctx: adv_machine::CtxId) -> (u32, u32) {
    let now = env.sys.clock.now_us();
    let u = user_mut(env);
    let paint = u
        .windows
        .values()
        .any(|w| w.thread == ctx && w.needs_paint && w.visible() && !w.destroying);
    let q = u.queue(ctx);
    let generated = (u32::from(paint) * QS_PAINT)
        | (u32::from(q.timers.iter().any(|t| t.due_us <= now)) * QS_TIMER);
    let mut pending = generated;
    for m in &q.posted {
        pending |= posted_status(m);
    }
    if q.quit.is_some() {
        pending |= QS_POSTMESSAGE | QS_ALLPOSTMESSAGE;
    }
    q.changed_status |= generated & !q.generated_status;
    q.generated_status = generated;
    q.changed_status &= pending;
    (pending, q.changed_status)
}

fn observe_retrieval(env: &mut Env<'_>, f: Filter) {
    queue_status(env);
    let ctx = env.sys.current;
    // Even a filtered miss observes QS_POSTMESSAGE. QS_ALLPOSTMESSAGE is
    // preserved until retrieval without a message-range filter (or GQS).
    user_mut(env).queue(ctx).changed_status &= if f.min == 0 && f.max == 0 {
        0
    } else {
        QS_ALLPOSTMESSAGE
    };
}

#[derive(Clone, Copy)]
struct Filter {
    hwnd: u32,
    min: u32,
    max: u32,
}

impl Filter {
    fn matches(&self, u: &window::User, m: &Msg) -> bool {
        self.matches_hwnd(u, m.hwnd) && self.range(m.msg)
    }
    fn matches_hwnd(&self, u: &window::User, hwnd: u32) -> bool {
        match self.hwnd {
            0 => true,
            u32::MAX => hwnd == 0,
            h => hwnd == h || (hwnd != 0 && is_descendant(u, hwnd, h)),
        }
    }
    fn range(&self, msg: u32) -> bool {
        (self.min == 0 && self.max == 0) || (self.min <= msg && msg <= self.max)
    }
}

fn is_descendant(u: &window::User, mut h: u32, ancestor: u32) -> bool {
    while let Some(w) = u.windows.get(&h) {
        if w.parent == ancestor {
            return true;
        }
        h = w.parent;
    }
    false
}

/// The next message for the current thread, removed from the queue with `remove`.
fn next_message(env: &mut Env<'_>, f: Filter, remove: bool) -> Option<Msg> {
    observe_retrieval(env, f);
    let ctx = env.sys.current;
    let now = env.sys.clock.now_us();
    let now_ms = env.sys.clock.now_ms();
    let frame = env.sys.frame;
    let u = user_mut(env);
    let mut found = None;
    {
        let posted = &u.queues.get(&ctx).map(|q| &q.posted);
        if let Some(posted) = posted {
            found = posted.iter().position(|m| f.matches(u, m));
        }
    }
    if let Some(i) = found {
        let q = u.queue(ctx);
        let m = if remove {
            q.posted.remove(i).unwrap()
        } else {
            q.posted[i]
        };
        q.last_time = m.time;
        q.last_pos = m.pt;
        if remove
            && m.input_receipt != 0
            && let Some(tracking) = &mut u.input_tracking
        {
            tracking.dequeued(m.input_receipt, frame);
        }
        return Some(m);
    }
    let pos = u.cursor_pos;
    let q = u.queue(ctx);
    // GetMessage/PeekMessage always retrieve WM_QUIT, regardless of their filters.
    if let Some(code) = q.quit {
        if remove {
            q.quit = None;
        }
        return Some(Msg {
            hwnd: 0,
            msg: wm::QUIT,
            wparam: code,
            lparam: 0,
            time: now_ms,
            pt: pos,
            input: false,
            input_receipt: 0,
        });
    }
    if f.range(wm::PAINT) {
        let paint = u.windows.values().find(|w| {
            w.thread == ctx
                && w.needs_paint
                && w.visible()
                && !w.destroying
                && (f.hwnd == 0 || w.hwnd == f.hwnd || is_descendant(u, w.hwnd, f.hwnd))
        });
        if let Some(w) = paint {
            return Some(Msg {
                hwnd: w.hwnd,
                msg: wm::PAINT,
                wparam: 0,
                lparam: 0,
                time: now_ms,
                pt: pos,
                input: false,
                input_receipt: 0,
            });
        }
    }
    if f.range(wm::TIMER) {
        let index = u.queues[&ctx]
            .timers
            .iter()
            .enumerate()
            .filter(|(_, t)| t.due_us <= now && f.matches_hwnd(u, t.hwnd))
            .min_by_key(|(_, t)| t.due_us)
            .map(|(i, _)| i);
        if let Some(i) = index {
            let t = &mut u.queue(ctx).timers[i];
            let m = Msg {
                hwnd: t.hwnd,
                msg: wm::TIMER,
                wparam: t.id,
                lparam: t.proc,
                time: now_ms,
                pt: pos,
                input: false,
                input_receipt: 0,
            };
            if remove {
                t.due_us = now + t.interval_us;
                // A future expiry must become new even without an intervening
                // Peek/GQS while no timers were due.
                let q = u.queue(ctx);
                if !q.timers.iter().any(|t| t.due_us <= now) {
                    q.generated_status &= !QS_TIMER;
                }
            }
            return Some(m);
        }
    }
    None
}

/// Earliest time something may appear in the current thread's queue on its own.
fn next_deadline(env: &mut Env<'_>, f: Filter) -> Option<u64> {
    let ctx = env.sys.current;
    let u = user(env);
    let timers = u
        .queues
        .get(&ctx)
        .into_iter()
        .flat_map(|q| &q.timers)
        .filter(|t| f.range(wm::TIMER) && f.matches_hwnd(u, t.hwnd))
        .map(|t| t.due_us)
        .min();
    let dialog = headless_deadline(u, ctx);
    [timers, dialog].into_iter().flatten().min()
}

// ---- headless dialog policy ----

const AUTO_CLICK_MAX: u32 = 3;

/// A dialog-like window of `ctx` waiting for input, and its button to press: the default
/// push button, else IDOK, else the first push button.
fn headless_target(u: &window::User, ctx: adv_machine::CtxId) -> Option<(u32, u32)> {
    u.auto_dialog_ms?;
    for &top in &u.top {
        let w = &u.windows[&top];
        if w.thread != ctx
            || !w.visible()
            || !w.enabled()
            || w.destroying
            || w.auto_clicks >= AUTO_CLICK_MAX
        {
            continue;
        }
        let buttons: Vec<&window::Window> = w
            .children
            .iter()
            .map(|c| &u.windows[c])
            .filter(|c| {
                u.classes[c.class].name.eq_ignore_ascii_case("button")
                    && c.style & 0xf <= 1
                    && c.visible()
                    && c.enabled()
            })
            .collect();
        let pick = buttons
            .iter()
            .find(|b| b.style & 0xf == 1)
            .or_else(|| buttons.iter().find(|b| b.id == 1))
            .or_else(|| buttons.first());
        if let Some(b) = pick {
            return Some((top, b.hwnd));
        }
    }
    None
}

fn headless_deadline(u: &window::User, ctx: adv_machine::CtxId) -> Option<u64> {
    let (top, _) = headless_target(u, ctx)?;
    let w = &u.windows[&top];
    let delay = u64::from(u.auto_dialog_ms?) * 1000;
    Some(w.created_us + delay * u64::from(w.auto_clicks + 1))
}

/// Headless mode: press the button of a dialog that has been waiting long enough, by
/// posting a real click to it. Returns whether something was posted.
fn headless_idle(env: &mut Env<'_>) -> bool {
    let ctx = env.sys.current;
    let now = env.sys.clock.now_us();
    let u = user_mut(env);
    let Some((top, button)) = headless_target(u, ctx) else {
        return false;
    };
    if headless_deadline(u, ctx).is_none_or(|d| now < d) {
        return false;
    }
    let w = u.windows.get_mut(&top).unwrap();
    w.auto_clicks += 1;
    let title = w.text.clone();
    let b = &u.windows[&button];
    let (bw, bh) = b.client_size();
    log::info!("headless: pressing [{}] in window \"{title}\"", b.text);
    let lp = make_long(bw / 2, bh / 2);
    post_input(env, button, wm::LBUTTONDOWN, 1, lp);
    post_input(env, button, wm::LBUTTONUP, 0, lp);
    true
}

// ---- retrieval API ----

const PM_REMOVE: u32 = 1;

fn write_message(env: &mut Env<'_>, message: Msg, out: u32, removed: bool) {
    let written = message.write(env.cpu, out);
    let ctx = env.sys.current;
    if let Some(tracking) = &mut user_mut(env).input_tracking {
        tracking.remember(ctx, out, message, removed && written);
    }
}

fn peek_message_a(env: &mut Env<'_>) -> Outcome {
    let (out, hwnd, min, max, flags) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3), env.arg(4));
    let f = Filter { hwnd, min, max };
    let remove = flags & PM_REMOVE != 0;
    if let Some(m) = next_message(env, f, remove).or_else(|| {
        headless_idle(env)
            .then(|| next_message(env, f, remove))
            .flatten()
    }) {
        write_message(env, m, out, remove);
        return Outcome::Return(1);
    }
    // Nothing: let other contexts run (the caller is a polling loop).
    Outcome::block_then_return(Wait::Yield, 0)
}

fn get_message_a(env: &mut Env<'_>) -> Outcome {
    let (out, hwnd, min, max) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3));
    get_message(env, out, Filter { hwnd, min, max })
}

fn get_message(env: &mut Env<'_>, out: u32, f: Filter) -> Outcome {
    if let Some(m) = next_message(env, f, true) {
        write_message(env, m, out, true);
        return Outcome::Return(u32::from(m.msg != wm::QUIT));
    }
    if headless_idle(env) {
        return get_message(env, out, f);
    }
    let event = queue_event(env);
    let deadline = next_deadline(env, f);
    Outcome::block(
        Wait::Object {
            handle: event,
            deadline,
        },
        move |env, _| get_message(env, out, f),
    )
}

/// Block until the current thread's queue may have something (WaitMessage, modal loops).
pub fn wait_for_message(
    env: &mut Env<'_>,
    then: impl FnOnce(&mut Env<'_>) -> Outcome + 'static,
) -> Outcome {
    let event = queue_event(env);
    let deadline = next_deadline(
        env,
        Filter {
            hwnd: 0,
            min: 0,
            max: 0,
        },
    );
    Outcome::block(
        Wait::Object {
            handle: event,
            deadline,
        },
        move |env, _| then(env),
    )
}

/// Arm the wake-up event after the caller found no matching message. Unmatched posted
/// messages must not keep a filtered GetMessage runnable; a new post signals it again.
fn queue_event(env: &mut Env<'_>) -> u32 {
    let ctx = env.sys.current;
    let q = user_mut(env).queue(ctx);
    let ev = q.event;
    let ev = if ev == 0 {
        let h = env.sys.objects.insert(Object::Event {
            manual_reset: true,
            signaled: false,
        });
        user_mut(env).queue(ctx).event = h;
        h
    } else {
        ev
    };
    env.sys.reset_event(ev);
    ev
}

/// Next message for modal loops run by the host (DialogBoxParam, MessageBox).
pub fn next_any(env: &mut Env<'_>) -> Option<Msg> {
    next_message(
        env,
        Filter {
            hwnd: 0,
            min: 0,
            max: 0,
        },
        true,
    )
    .or_else(|| {
        headless_idle(env)
            .then(|| {
                next_message(
                    env,
                    Filter {
                        hwnd: 0,
                        min: 0,
                        max: 0,
                    },
                    true,
                )
            })
            .flatten()
    })
}

fn wait_message(env: &mut Env<'_>) -> Outcome {
    if next_message(
        env,
        Filter {
            hwnd: 0,
            min: 0,
            max: 0,
        },
        false,
    )
    .is_some()
    {
        return Outcome::Return(1);
    }
    wait_for_message(env, |_| Outcome::Return(1))
}

fn get_queue_status(env: &mut Env<'_>) -> Outcome {
    let flags = env.arg(0) & 0xffff;
    let (pending, changed) = queue_status(env);
    let ctx = env.sys.current;
    user_mut(env).queue(ctx).changed_status &= !flags;
    Outcome::Return((pending & flags) << 16 | (changed & flags))
}

fn post_message_a(env: &mut Env<'_>) -> Outcome {
    let (hwnd, msg, wp, lp) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3));
    if hwnd == 0xffff {
        // HWND_BROADCAST: to every top-level window.
        let tops = user(env).top.clone();
        for t in tops {
            post(env, t, msg, wp, lp);
        }
        return Outcome::Return(1);
    }
    Outcome::Return(u32::from(post(env, hwnd, msg, wp, lp)))
}

fn post_thread_message_a(env: &mut Env<'_>) -> Outcome {
    let (tid, msg, wp, lp) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3));
    // A terminated context keeps its record for GetExitCodeThread, but its thread
    // id no longer names a live message target. Returning success here can make
    // a caller wait forever for a reply that the exited thread cannot produce.
    let Some(ctx) = env
        .sys
        .contexts
        .iter()
        .find(|c| c.tid == tid && !c.is_done())
        .map(|c| c.id)
    else {
        crate::util::set_last_error(env, 1444); // ERROR_INVALID_THREAD_ID (winerror.h)
        return Outcome::Return(0);
    };
    let time = env.sys.clock.now_ms();
    Outcome::Return(u32::from(post_to_thread(
        env,
        ctx,
        Msg {
            hwnd: 0,
            msg,
            wparam: wp,
            lparam: lp,
            time,
            pt: (0, 0),
            input: false,
            input_receipt: 0,
        },
    )))
}

fn post_quit_message(env: &mut Env<'_>) -> Outcome {
    let code = env.arg(0);
    post_quit(env, code);
    Outcome::Return(0)
}

/// PostQuitMessage for the current thread.
pub fn post_quit(env: &mut Env<'_>, code: u32) {
    let ctx = env.sys.current;
    let q = user_mut(env).queue(ctx);
    q.quit = Some(code);
    q.changed_status |= QS_POSTMESSAGE | QS_ALLPOSTMESSAGE;
    queue_changed(env, ctx);
}

fn send_message_a(env: &mut Env<'_>) -> Outcome {
    let (hwnd, msg, wp, lp) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3));
    if hwnd == 0xffff {
        let tops: VecDeque<[u32; 4]> = user(env).top.iter().map(|&t| [t, msg, wp, lp]).collect();
        return send_all(env, tops, |_| Outcome::Return(1));
    }
    send(env, hwnd, msg, wp, lp, |_, r| Outcome::Return(r))
}

fn send_message_timeout_a(env: &mut Env<'_>) -> Outcome {
    let (hwnd, msg, wp, lp, out) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3), env.arg(6));
    send(env, hwnd, msg, wp, lp, move |env, r| {
        if out != 0 {
            let _ = env.cpu.write_u32(out, r);
        }
        Outcome::Return(1)
    })
}

fn dispatch_message_a(env: &mut Env<'_>) -> Outcome {
    let address = env.arg(0);
    let m = Msg::read(env.cpu, address);
    let ctx = env.sys.current;
    let m = user_mut(env)
        .input_tracking
        .as_mut()
        .map_or(m, |tracking| tracking.take(ctx, address, m));
    dispatch(env, m, |_, r| Outcome::Return(r))
}

/// DispatchMessage: timer callbacks, else the window procedure.
pub fn dispatch(
    env: &mut Env<'_>,
    m: Msg,
    then: impl FnOnce(&mut Env<'_>, u32) -> Outcome + 'static,
) -> Outcome {
    if m.input_receipt != 0
        && let Some(tracking) = &mut user_mut(env).input_tracking
    {
        tracking.dispatching(m.input_receipt);
    }
    let then = move |env: &mut Env<'_>, result| {
        if m.input_receipt != 0
            && let Some(tracking) = &mut user_mut(env).input_tracking
        {
            tracking.complete(m.input_receipt);
        }
        then(env, result)
    };
    if m.msg == wm::TIMER && m.lparam != 0 {
        return call_proc(m.lparam, m.hwnd, wm::TIMER, m.wparam, m.time, then);
    }
    if m.hwnd == 0 {
        return then(env, 0);
    }
    send(env, m.hwnd, m.msg, m.wparam, m.lparam, then)
}

fn translate_message(env: &mut Env<'_>) -> Outcome {
    let m = Msg::read(env.cpu, env.arg(0));
    Outcome::Return(u32::from(translate(env, &m)))
}

/// TranslateMessage: WM_CHAR for character keys. Returns whether one was posted.
pub fn translate(env: &mut Env<'_>, m: &Msg) -> bool {
    if m.msg != wm::KEYDOWN && m.msg != wm::SYSKEYDOWN {
        return false;
    }
    let shift = user(env).keys[0x10] & 0x80 != 0;
    let ch = match m.wparam {
        v @ 0x41..=0x5a => Some(if shift { v } else { v + 0x20 }),
        v @ (0x30..=0x39 | 0x20 | 0x0d | 0x08 | 0x1b | 0x09) => Some(v),
        _ => None,
    };
    let Some(ch) = ch else { return false };
    let msg = if m.msg == wm::KEYDOWN {
        wm::CHAR
    } else {
        wm::SYSCHAR
    };
    post(env, m.hwnd, msg, ch, m.lparam)
}

fn get_message_time(env: &mut Env<'_>) -> Outcome {
    let ctx = env.sys.current;
    Outcome::Return(user_mut(env).queue(ctx).last_time)
}

fn get_message_pos(env: &mut Env<'_>) -> Outcome {
    let ctx = env.sys.current;
    let (x, y) = user_mut(env).queue(ctx).last_pos;
    Outcome::Return(make_long(x, y))
}

// ---- timers ----

fn set_timer(env: &mut Env<'_>) -> Outcome {
    let (hwnd, id, ms, proc) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3));
    let ctx = if hwnd == 0 {
        env.sys.current
    } else {
        let Some(w) = user(env).windows.get(&hwnd) else {
            return Outcome::Return(0);
        };
        w.thread
    };
    let now = env.sys.clock.now_us();
    // USER_TIMER_MINIMUM is 10 ms.
    let interval_us = u64::from(ms.max(10)) * 1000;
    let u = user_mut(env);
    let q = u.queue(ctx);
    let id = if hwnd == 0 {
        0x7fff_0000 + q.timers.len() as u32 + 1
    } else {
        id
    };
    q.timers.retain(|t| !(t.hwnd == hwnd && t.id == id));
    q.timers.push(Timer {
        hwnd,
        id,
        interval_us,
        due_us: now + interval_us,
        proc,
    });
    // PAL's game thread installs timers on the main thread's window. Wake an
    // already-blocked GetMessage so it recomputes its next timer deadline.
    queue_changed(env, ctx);
    Outcome::Return(if hwnd == 0 { id } else { id.max(1) })
}

fn kill_timer(env: &mut Env<'_>) -> Outcome {
    let (hwnd, id) = (env.arg(0), env.arg(1));
    let ctx = if hwnd == 0 {
        env.sys.current
    } else {
        let Some(w) = user(env).windows.get(&hwnd) else {
            return Outcome::Return(0);
        };
        w.thread
    };
    let q = user_mut(env).queue(ctx);
    let before = q.timers.len();
    q.timers.retain(|t| !(t.hwnd == hwnd && t.id == id));
    let removed = q.timers.len() != before;
    if removed {
        queue_changed(env, ctx);
    }
    Outcome::Return(u32::from(removed))
}

// ---- default processing ----

fn def_window_proc_a(env: &mut Env<'_>) -> Outcome {
    let (hwnd, msg, wp, lp) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3));
    def_window_proc(env, hwnd, msg, wp, lp, |_, r| Outcome::Return(r))
}

pub fn def_window_proc(
    env: &mut Env<'_>,
    hwnd: u32,
    msg: u32,
    wp: u32,
    lp: u32,
    then: impl FnOnce(&mut Env<'_>, u32) -> Outcome + 'static,
) -> Outcome {
    let r = match msg {
        wm::NCCREATE => 1,
        wm::CLOSE => return destroy_window(env, hwnd, move |env, _| then(env, 0)),
        wm::SETTEXT => {
            let text = if lp == 0 {
                String::new()
            } else {
                env.ext.get::<Win32>().codec.decode(&cstr(env.cpu, lp))
            };
            if let Some(w) = user_mut(env).windows.get_mut(&hwnd) {
                w.text = text;
                w.needs_paint = true;
            }
            1
        }
        wm::GETTEXT => {
            let Some(w) = user(env).windows.get(&hwnd) else {
                return then(env, 0);
            };
            let bytes = env.ext.get::<Win32>().codec.encode(&w.text);
            if wp == 0 {
                0
            } else {
                put_cstr(env.cpu, lp, wp, &bytes).unwrap_or(wp - 1)
            }
        }
        wm::GETTEXTLENGTH => {
            let Some(w) = user(env).windows.get(&hwnd) else {
                return then(env, 0);
            };
            env.ext.get::<Win32>().codec.encode(&w.text).len() as u32
        }
        wm::SETFONT => {
            if let Some(w) = user_mut(env).windows.get_mut(&hwnd) {
                w.font = wp;
            }
            0
        }
        wm::GETFONT => user(env).windows.get(&hwnd).map_or(0, |w| w.font),
        wm::PAINT => {
            validate(env, hwnd);
            0
        }
        wm::ERASEBKGND => 1,
        wm::NCHITTEST => 1, // HTCLIENT
        wm::MOUSEACTIVATE => 1,
        wm::ACTIVATE if wp & 0xffff != 0 => {
            return window::set_focus(env, hwnd, move |env, _| then(env, 0));
        }
        wm::WINDOWPOSCHANGED => {
            let flags = env.cpu.read_u32(lp + 24).unwrap_or(0);
            let msgs = window::window_pos_changed_msgs(user(env), hwnd, flags);
            return send_all(env, msgs.into(), move |env| then(env, 0));
        }
        wm::SYSCOMMAND if wp & 0xfff0 == 0xf060 => {
            // SC_CLOSE
            return send(env, hwnd, wm::CLOSE, 0, 0, move |env, _| then(env, 0));
        }
        wm::SYSKEYDOWN if wp == 0x73 => {
            // Alt+F4
            return send(env, hwnd, wm::SYSCOMMAND, 0xf060, 0, move |env, _| {
                then(env, 0)
            });
        }
        m if (wm::CTLCOLORMSGBOX..=wm::CTLCOLORSTATIC).contains(&m) => {
            crate::gdi32::stock_object(0) // WHITE_BRUSH
        }
        0x11 => 1, // WM_QUERYENDSESSION
        _ => 0,
    };
    then(env, r)
}

pub fn validate(env: &mut Env<'_>, hwnd: u32) {
    let ctx = if let Some(w) = user_mut(env).windows.get_mut(&hwnd) {
        w.needs_paint = false;
        w.thread
    } else {
        return;
    };
    // Do not hide a validate -> re-invalidate transition between queue queries.
    queue_status_for(env, ctx);
}

fn call_window_proc_a(env: &mut Env<'_>) -> Outcome {
    let (proc, hwnd, msg, wp, lp) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3), env.arg(4));
    call_proc(proc, hwnd, msg, wp, lp, |_, r| Outcome::Return(r))
}

// ---- painting ----

fn invalidate_rect(env: &mut Env<'_>) -> Outcome {
    let hwnd = env.arg(0);
    let u = user_mut(env);
    if hwnd == 0 {
        for w in u.windows.values_mut() {
            w.needs_paint = true;
        }
        return Outcome::Return(1);
    }
    match u.windows.get_mut(&hwnd) {
        Some(w) => {
            w.needs_paint = true;
            Outcome::Return(1)
        }
        None => Outcome::Return(0),
    }
}

fn validate_rect(env: &mut Env<'_>) -> Outcome {
    let hwnd = env.arg(0);
    validate(env, hwnd);
    Outcome::Return(1)
}

fn begin_paint(env: &mut Env<'_>) -> Outcome {
    let (hwnd, ps) = (env.arg(0), env.arg(1));
    let Some(w) = user(env).windows.get(&hwnd) else {
        return Outcome::Return(0);
    };
    let (cw, ch) = w.client_size();
    validate(env, hwnd);
    let hdc = crate::gdi32::window_dc(env, hwnd);
    // PAINTSTRUCT: hdc, fErase, rcPaint, fRestore, fIncUpdate, rgbReserved[32].
    crate::util::zero(env.cpu, ps, 64);
    let _ = env.cpu.write_u32(ps, hdc);
    Rect::new(0, 0, cw, ch).write(env.cpu, ps + 8);
    Outcome::Return(hdc)
}

fn end_paint(env: &mut Env<'_>) -> Outcome {
    let ps = env.arg(1);
    let hdc = env.cpu.read_u32(ps).unwrap_or(0);
    crate::gdi32::release_dc(env, hdc);
    Outcome::Return(1)
}

fn redraw_window(env: &mut Env<'_>) -> Outcome {
    let hwnd = env.arg(0);
    if let Some(w) = user_mut(env).windows.get_mut(&hwnd) {
        w.needs_paint = true;
    }
    Outcome::Return(1)
}

pub fn register(reg: &mut Registry) {
    let s = |reg: &mut Registry, name: &str, argc: u8, h| stdcall(reg, USER32, name, argc, h);
    s(reg, "PeekMessageA", 5, peek_message_a);
    s(reg, "GetMessageA", 4, get_message_a);
    s(reg, "WaitMessage", 0, wait_message);
    s(reg, "GetQueueStatus", 1, get_queue_status);
    s(reg, "PostMessageA", 4, post_message_a);
    s(reg, "PostThreadMessageA", 4, post_thread_message_a);
    s(reg, "PostQuitMessage", 1, post_quit_message);
    s(reg, "SendMessageA", 4, send_message_a);
    s(reg, "SendMessageTimeoutA", 7, send_message_timeout_a);
    s(reg, "DispatchMessageA", 1, dispatch_message_a);
    s(reg, "TranslateMessage", 1, translate_message);
    s(reg, "GetMessageTime", 0, get_message_time);
    s(reg, "GetMessagePos", 0, get_message_pos);
    s(reg, "SetTimer", 4, set_timer);
    s(reg, "KillTimer", 2, kill_timer);
    s(reg, "DefWindowProcA", 4, def_window_proc_a);
    s(reg, "CallWindowProcA", 5, call_window_proc_a);
    s(reg, "InvalidateRect", 3, invalidate_rect);
    s(reg, "ValidateRect", 2, validate_rect);
    s(reg, "BeginPaint", 2, begin_paint);
    s(reg, "EndPaint", 2, end_paint);
    s(reg, "RedrawWindow", 4, redraw_window);
}

#[cfg(test)]
mod receipt_tests {
    use super::*;

    fn message(receipt: u64) -> Msg {
        Msg {
            hwnd: 1,
            msg: wm::KEYUP,
            wparam: 17,
            lparam: 0xc0000001,
            time: 10,
            pt: (2, 3),
            input: receipt != 0,
            input_receipt: receipt,
        }
    }

    #[test]
    fn reused_msg_slot_keeps_callback_completions_bound_to_their_instances() {
        let mut tracking = InputTracking::default();
        let first = tracking.issue(None);
        tracking.remember(0, 0x10000, message(first), true);
        let dispatched_first = tracking.take(0, 0x10000, message(0));
        assert_eq!(dispatched_first.input_receipt, first);
        // Identical guest bytes and the same MSG address, but a different
        // physical message instance. The older suspended callback cannot ack it.
        let second = tracking.issue(None);
        tracking.remember(0, 0x10000, message(second), true);
        let dispatched_second = tracking.take(0, 0x10000, message(0));
        tracking.complete(dispatched_first.input_receipt);
        assert!(!tracking.completed(second, 99, true));
        tracking.complete(dispatched_second.input_receipt);
        assert!(tracking.completed(second, 99, false));
        let third = tracking.issue(None);
        tracking.complete(dispatched_second.input_receipt);
        assert!(!tracking.completed(third, 100, true));
    }

    #[test]
    fn copied_changed_and_nonremoving_msg_slots_never_guess_a_receipt() {
        let mut tracking = InputTracking::default();
        let receipt = tracking.issue(None);
        tracking.remember(0, 0x10000, message(receipt), true);
        // Copying identical MSG bytes to an unregistered output slot does not
        // create an identity. Another context cannot consume the binding either.
        assert_eq!(tracking.take(0, 0x20000, message(0)).input_receipt, 0);
        assert_eq!(tracking.take(1, 0x10000, message(0)).input_receipt, 0);
        let mut changed = message(0);
        changed.wparam = 18;
        assert_eq!(tracking.take(0, 0x10000, changed).input_receipt, 0);
        tracking.remember(0, 0x10000, message(receipt), true);
        tracking.remember(0, 0x10000, message(0), false);
        assert_eq!(tracking.take(0, 0x10000, message(0)).input_receipt, 0);
        assert!(!tracking.completed(receipt, 100, true));
    }

    #[test]
    fn polling_receipt_needs_the_new_generation_and_a_subsequent_boundary() {
        let mut tracking = InputTracking::default();
        tracking.observe_key(17, 0, 3);
        let synthetic = tracking.issue(Some((17, 0, Some(0))));
        assert!(
            !tracking.completed(synthetic, 4, true),
            "old identical value is insufficient"
        );
        let down = tracking.issue(Some((17, 0x80, Some(0x180))));
        tracking.observe_key(17, 0x180, 5);
        assert!(!tracking.completed(down, 5, false));
        assert!(tracking.completed(down, 5, true));
        // LIVE_INPUT can observe hardware release before the executor queues
        // WM_KEYUP. Its new generation carries that observation to the receipt.
        tracking.observe_key(17, 0x200, 7);
        let up = tracking.issue(Some((17, 0, Some(0x200))));
        assert!(!tracking.completed(up, 7, false));
        tracking.observe_key(17, 0x200, 8);
        assert!(
            tracking.completed(up, 8, false),
            "repeated polls must preserve the first observation frame"
        );
    }

    #[test]
    fn dequeue_fence_cannot_complete_a_suspended_dispatch_callback() {
        let mut tracking = InputTracking::default();
        let receipt = tracking.issue(None);
        tracking.dequeued(receipt, 10);
        assert!(!tracking.fence_reached(receipt, 10, false, false));
        tracking.dispatching(receipt);
        assert!(!tracking.fence_reached(receipt, 11, true, true));
        tracking.complete(receipt);
        assert!(tracking.fence_reached(receipt, 11, false, false));
        // An application that retrieves and handles MSG itself has no dispatch
        // callback. Its receipt is an observation fence after the next boundary.
        let copied = tracking.issue(None);
        tracking.dequeued(copied, 12);
        assert!(tracking.fence_reached(copied, 13, false, false));
        let ignored = tracking.issue(None);
        assert!(!tracking.fence_reached(ignored, 13, true, false));
        assert!(tracking.fence_reached(ignored, 13, true, true));
    }
}

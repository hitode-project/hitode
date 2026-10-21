//! Messages: sending (a guest call into the window procedure), per-thread posted-message
//! queues, GetMessage/PeekMessage, timers, and DefWindowProc.
//!
//! Cross-thread sends run on the receiving guest thread at message retrieval,
//! including reentrant sends while the sender waits. Host workers never own a WndProc.
//!
//! Retrieval order follows Win32: posted messages, then WM_QUIT once nothing is posted,
//! then WM_PAINT for invalid windows, then due WM_TIMERs. An empty queue yields
//! (PeekMessage) or blocks on the queue's event (GetMessage), so waiting for input costs
//! no host CPU.

use std::collections::VecDeque;
use std::collections::{BTreeMap, HashMap};

use adv_abi::{AnsiStr, GuestPtr, Handle as AbiHandle, Registry, WideStr, win32};
use adv_cpu::backend::GuestExt;
use adv_machine::{Env, Object, Outcome, Wait};

use super::window::{self, destroy_window, make_long, wm};
use super::{USER32, user, user_mut};
use crate::{Win32, stdcall};
pub mod frame_cap;
pub(crate) mod sent;

/// Internal procedure for built-in classes, whose encoding follows the window.
pub const DEFAULT_PROC: &str = "<DefaultWndProc>";

pub(super) fn read_text(env: &Env<'_>, pointer: u32, wide: bool) -> Option<String> {
    read_text_limit(env, pointer, wide, crate::util::MAX_STR)
}

fn read_text_limit(env: &Env<'_>, pointer: u32, wide: bool, limit: usize) -> Option<String> {
    if pointer == 0 {
        return Some(String::new());
    }
    if wide {
        WideStr(GuestPtr::new(pointer))
            .read(env.cpu, limit)
            .ok()
            .map(|units| String::from_utf16_lossy(&units))
    } else {
        AnsiStr(GuestPtr::new(pointer))
            .read(env.cpu, limit)
            .ok()
            .map(|bytes| env.ext.get::<Win32>().codec.decode(&bytes))
    }
}

pub(super) fn text_length(env: &Env<'_>, text: &str, wide: bool) -> u32 {
    if wide {
        text.encode_utf16().count() as u32
    } else {
        env.ext.get::<Win32>().codec.encode(text).len() as u32
    }
}

pub(super) fn write_text(
    env: &mut Env<'_>,
    pointer: u32,
    capacity: u32,
    text: &str,
    wide: bool,
) -> Option<u32> {
    if capacity == 0 {
        return Some(0);
    }
    let (length, bytes) = if wide {
        let units: Vec<_> = text.encode_utf16().take(capacity as usize - 1).collect();
        let count = units.len() as u32;
        (
            count,
            units
                .into_iter()
                .flat_map(u16::to_le_bytes)
                .chain([0, 0])
                .collect::<Vec<_>>(),
        )
    } else {
        let codec = env.ext.get::<Win32>().codec;
        let mut bytes = codec.encode(text);
        let limit = bytes.len().min(capacity as usize - 1);
        let mut end = 0;
        if codec.acp == 65001 {
            end = limit;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
        } else {
            // DefWindowProcA truncates complete DBCS characters, not a lead
            // byte followed by NUL. The returned count remains an ACP byte count.
            while end < limit {
                let next = end
                    + if codec.is_lead_byte(0, bytes[end]) {
                        2
                    } else {
                        1
                    };
                if next > limit {
                    break;
                }
                end = next;
            }
        }
        bytes.truncate(end);
        bytes.push(0);
        (end as u32, bytes)
    };
    adv_abi::guest::check_range(
        env.cpu,
        pointer,
        bytes.len() as u32,
        adv_cpu::backend::Prot::WRITE,
    )
    .ok()?;
    env.cpu.write(pointer, &bytes).ok()?;
    Some(length)
}

pub(super) fn guest_text(env: &mut Env<'_>, text: &str, wide: bool) -> u32 {
    let length = text_length(env, text, wide) + 1;
    let pointer = window::temp_alloc(env, length * if wide { 2 } else { 1 });
    if pointer != 0 && write_text(env, pointer, length, text, wide).is_none() {
        window::temp_free(env, pointer);
        return 0;
    }
    pointer
}

#[cfg(test)]
mod dwm_tests;
#[cfg(test)]
mod frame_cap_tests;
#[cfg(test)]
mod keyboard_tests;

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
    /// Queue-local key update captured from this input message, independent of
    /// the live physical state which may already include later releases.
    pub input_key: Option<KeyTransition>,
    /// Character wParam encoding supplied by the posting API, independently of
    /// the destination class and physical-input category. Never guest MSG data.
    pub(super) character_wide: bool,
    /// USER-generated post: QS_POSTMESSAGE without QS_ALLPOSTMESSAGE. This
    /// origin is private queue metadata, never inferred from the message number.
    pub(super) system: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct KeyTransition {
    pub key: u8,
    /// Left or right key behind a generic Shift, Ctrl or Alt transition.
    pub side: Option<u8>,
    pub pressed: bool,
    pub toggle: bool,
}

fn key_transition(message: u32, wparam: u32, lparam: u32) -> Option<KeyTransition> {
    let (key, pressed) = match message {
        0x100 | 0x104 if wparam < 256 => (wparam as u8, true),
        0x101 | 0x105 if wparam < 256 => (wparam as u8, false),
        0x201 | 0x203 => (1, true),
        0x202 => (1, false),
        0x204 | 0x206 => (2, true),
        0x205 => (2, false),
        0x207 | 0x209 => (4, true),
        0x208 => (4, false),
        0x20b..=0x20d if matches!(wparam >> 16, 1 | 2) => {
            ((wparam >> 16) as u8 + 4, message != 0x20c)
        }
        _ => return None,
    };
    Some(KeyTransition {
        key,
        side: matches!(message, 0x100 | 0x101 | 0x104 | 0x105)
            .then(|| adv_os::keyboard::side_virtual_key(wparam, lparam))
            .flatten()
            .map(|side| side as u8),
        pressed,
        // Only the first keyboard down toggles; autorepeat has bit 30 set.
        toggle: pressed && matches!(key, 0x14 | 0x90 | 0x91) && lparam & (1 << 30) == 0,
    })
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
            input_key: None,
            character_wide: false,
            system: false,
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
    let Some((proc, thread)) = user(env)
        .windows
        .get(&env.sys.objects, &hwnd)
        .map(|w| (w.proc, w.thread))
    else {
        return then(env, 0);
    };
    if thread == env.context {
        call_proc(proc, hwnd, msg, wparam, lparam, then)
    } else {
        sent::send(
            env,
            thread,
            [hwnd, msg, wparam, lparam],
            sent::Options::default(),
            move |env, result| {
                if let Err(error) = result {
                    crate::util::set_last_error(env, error);
                }
                then(env, result.unwrap_or(0))
            },
        )
    }
}

/// Send a public A/W message, converting supported text messages to the receiving
/// procedure's encoding. The callback and its result still use the common send path.
pub(super) fn send_encoded(
    env: &mut Env<'_>,
    hwnd: u32,
    msg: u32,
    wp: u32,
    lp: u32,
    wide: bool,
    then: impl FnOnce(&mut Env<'_>, u32) -> Outcome + 'static,
) -> Outcome {
    encoded_to(env, None, hwnd, msg, wp, lp, wide, move |env, result| {
        if let Err(error) = result {
            crate::util::set_last_error(env, error);
        }
        then(env, result.unwrap_or(0))
    })
}

/// Share bounded text conversion while calling an immutable saved procedure.
/// This deliberately bypasses SendMessage/current-procedure/thread dispatch.
pub(super) fn call_proc_encoded(
    env: &mut Env<'_>,
    proc: u32,
    hwnd: u32,
    msg: u32,
    wp: u32,
    lp: u32,
    wide: bool,
) -> Outcome {
    let api = if wide {
        "user32.dll!CallWindowProcW"
    } else {
        "user32.dll!CallWindowProcA"
    };
    if !matches!(env.ext.get::<Win32>().codec.acp, 932 | 936 | 1252 | 65001) {
        adv_abi::warn_unavailable(
            env,
            api,
            "saved-procedure ANSI text conversion requires guest ACP 932/936/1252/65001",
        );
        crate::util::set_last_error(env, 50);
        return Outcome::Return(0);
    }
    if msg == wm::GETTEXT && wp != 0 {
        let size = wp.checked_mul(if wide { 2 } else { 1 });
        if wp as usize > crate::util::MAX_STR || size.is_none() {
            adv_abi::warn_unavailable(
                env,
                api,
                "saved-procedure text capacity exceeds the bounded buffer profile",
            );
            crate::util::set_last_error(env, 87);
            return Outcome::Return(0);
        }
        if adv_abi::guest::check_range(env.cpu, lp, size.unwrap(), adv_cpu::backend::Prot::WRITE)
            .is_err()
        {
            crate::util::set_last_error(env, 998);
            return Outcome::Return(0);
        }
    }
    encoded_to(env, Some(proc), hwnd, msg, wp, lp, wide, |env, result| {
        if let Err(error) = result {
            crate::util::set_last_error(env, error);
        }
        Outcome::Return(result.unwrap_or(0))
    })
}

#[allow(clippy::too_many_arguments)]
fn dispatch_encoded(
    env: &mut Env<'_>,
    proc: Option<u32>,
    hwnd: u32,
    msg: u32,
    wp: u32,
    lp: u32,
    then: impl FnOnce(&mut Env<'_>, Result<u32, u32>) -> Outcome + 'static,
) -> Outcome {
    if let Some(proc) = proc {
        call_proc(proc, hwnd, msg, wp, lp, move |env, value| {
            then(env, Ok(value))
        })
    } else {
        send(env, hwnd, msg, wp, lp, move |env, value| {
            then(env, Ok(value))
        })
    }
}

#[allow(clippy::too_many_arguments)]
fn encoded_to(
    env: &mut Env<'_>,
    proc: Option<u32>,
    hwnd: u32,
    msg: u32,
    wp: u32,
    lp: u32,
    wide: bool,
    then: impl FnOnce(&mut Env<'_>, Result<u32, u32>) -> Outcome + 'static,
) -> Outcome {
    let Some(target_wide) = user(env)
        .windows
        .get(&env.sys.objects, &hwnd)
        .map(|w| w.unicode)
    else {
        return then(env, Err(1400));
    };
    if wide == target_wide {
        return dispatch_encoded(env, proc, hwnd, msg, wp, lp, then);
    }
    if character_message(msg) {
        return match character_param(env, wp, wide, target_wide) {
            Ok(parameter) => dispatch_encoded(env, proc, hwnd, msg, parameter, lp, then),
            Err(reason) => {
                adv_abi::warn_unavailable(env, "user32.dll!A/W message conversion", reason);
                then(env, Err(50))
            }
        };
    }
    let combo = user(env)
        .windows
        .get(&env.sys.objects, &hwnd)
        .is_some_and(|w| {
            user(env).classes[w.class]
                .name
                .eq_ignore_ascii_case("ComboBox")
        });
    if msg == wm::SETTEXT || (combo && matches!(msg, 0x143 | 0x14a | 0x14c | 0x14d | 0x158)) {
        if msg == wm::SETTEXT && lp == 0 {
            return dispatch_encoded(env, proc, hwnd, msg, wp, 0, then);
        }
        let Some(text) = read_text(env, lp, wide) else {
            return then(env, Err(87));
        };
        let pointer = guest_text(env, &text, target_wide);
        if pointer == 0 {
            return then(env, Err(8));
        }
        return dispatch_encoded(env, proc, hwnd, msg, wp, pointer, move |env, result| {
            window::temp_free(env, pointer);
            then(env, result)
        });
    }
    if msg == wm::GETTEXT {
        if wp == 0 {
            return then(env, Ok(0));
        }
        // ACP936 native W->A sends wp*2 bytes. Follow the shared GetCPInfo
        // profile for other guest encoders, not the host's active code page.
        let cp = env.ext.get::<Win32>().codec.acp;
        let max_char_size = if cp == 65001 {
            4
        } else if adv_os::nls::lead_byte_ranges(cp).is_empty() {
            1
        } else {
            2
        };
        let Some(capacity) = wp.checked_mul(if target_wide { 1 } else { max_char_size }) else {
            return then(env, Err(87));
        };
        return converted_text(
            env,
            proc,
            hwnd,
            msg,
            capacity,
            0,
            lp,
            wp,
            wide,
            target_wide,
            then,
        );
    }
    if msg == wm::GETTEXTLENGTH || (combo && matches!(msg, 0x148 | 0x149)) {
        let query = if combo && msg != wm::GETTEXTLENGTH {
            0x149
        } else {
            wm::GETTEXTLENGTH
        };
        return dispatch_encoded(env, proc, hwnd, query, wp, 0, move |env, length| {
            let length = match length {
                Ok(value) => value,
                Err(error) => return then(env, Err(error)),
            };
            if length == u32::MAX {
                return then(env, Ok(length));
            }
            let Some(capacity) = length.checked_add(1) else {
                return then(env, Ok(0));
            };
            converted_text(
                env,
                proc,
                hwnd,
                if query == 0x149 { 0x148 } else { wm::GETTEXT },
                capacity,
                wp,
                lp,
                if msg == 0x148 { u32::MAX } else { 0 },
                wide,
                target_wide,
                then,
            )
        });
    }
    dispatch_encoded(env, proc, hwnd, msg, wp, lp, then)
}

#[allow(clippy::too_many_arguments)]
fn converted_text(
    env: &mut Env<'_>,
    proc: Option<u32>,
    hwnd: u32,
    msg: u32,
    capacity: u32,
    index: u32,
    output: u32,
    output_capacity: u32,
    wide: bool,
    target_wide: bool,
    then: impl FnOnce(&mut Env<'_>, Result<u32, u32>) -> Outcome + 'static,
) -> Outcome {
    // Bound temporary guest allocation even if an untrusted procedure reports a
    // bogus text length. Normal text uses the same limit as string marshalling.
    let Some(size) = capacity
        .checked_mul(if target_wide { 2 } else { 1 })
        .filter(|size| *size <= crate::util::MAX_STR as u32 * 4)
    else {
        adv_abi::warn_unavailable(
            env,
            "user32.dll!A/W text conversion",
            "temporary text capacity exceeds the supported bounded buffer profile",
        );
        return then(env, Err(87));
    };
    let pointer = window::temp_alloc(env, size);
    if pointer == 0 {
        return then(env, Err(8));
    }
    dispatch_encoded(
        env,
        proc,
        hwnd,
        msg,
        if msg == wm::GETTEXT { capacity } else { index },
        pointer,
        move |env, result| {
            let text = read_text_limit(env, pointer, target_wide, capacity as usize);
            window::temp_free(env, pointer);
            let result = match result {
                Ok(value) => value,
                Err(error) => return then(env, Err(error)),
            };
            if result == u32::MAX {
                return then(env, Ok(result));
            }
            let Some(text) = text else {
                return then(env, Err(87));
            };
            let length = text_length(env, &text, wide);
            if output_capacity == 0 {
                return then(env, Ok(length));
            }
            let Some(length) =
                write_text(env, output, output_capacity.min(length + 1), &text, wide)
            else {
                return then(env, Err(87));
            };
            then(env, Ok(length))
        },
    )
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
    post_kind(env, hwnd, msg, wparam, lparam, PostSource::Ansi)
}

/// Host physical input uses input queue categories; guest PostMessage and
/// TranslateMessage-generated characters remain ordinary posted messages.
pub fn post_input(env: &mut Env<'_>, hwnd: u32, msg: u32, wparam: u32, lparam: u32) -> bool {
    post_kind(env, hwnd, msg, wparam, lparam, PostSource::Input)
}

#[derive(Clone, Copy)]
enum PostSource {
    Ansi,
    Unicode,
    Input,
}

fn post_kind(
    env: &mut Env<'_>,
    hwnd: u32,
    msg: u32,
    wparam: u32,
    lparam: u32,
    source: PostSource,
) -> bool {
    let input = matches!(source, PostSource::Input);
    let time = env.sys.clock.now_ms();
    let ctx = if hwnd == 0 {
        env.context
    } else {
        match user(env).windows.get(&env.sys.objects, &hwnd) {
            Some(w) => w.thread,
            None => {
                crate::util::set_last_error(env, 1400);
                return false;
            }
        }
    };
    // Fail before issuing a physical-input receipt or changing queue metadata.
    if !reserve_post(env, ctx) {
        return false;
    }
    let input_receipt = if input {
        let u = &mut env.ext.get_mut::<crate::Win32>().user;
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
            input_key: input.then(|| key_transition(msg, wparam, lparam)).flatten(),
            character_wide: !matches!(source, PostSource::Ansi),
            system: false,
        },
    );
    if posted && input {
        env.sys
            .prefer_ready_context(ctx, adv_machine::WakePriority::Input);
    }
    posted
}

const POST_MESSAGE_LIMIT: usize = 10_000;

fn reserve_post(env: &mut Env<'_>, ctx: adv_machine::CtxId) -> bool {
    let queue = user_mut(env).queue(ctx);
    let error = if queue.posted.len() >= POST_MESSAGE_LIMIT {
        Some(1816) // ERROR_NOT_ENOUGH_QUOTA; Win32's default posted-message limit.
    } else if queue.posted.try_reserve(1).is_err() {
        Some(8)
    } else {
        None
    };
    if let Some(error) = error {
        crate::util::set_last_error(env, error);
        return false;
    }
    true
}

fn post_to_thread(env: &mut Env<'_>, ctx: adv_machine::CtxId, m: Msg) -> bool {
    if !reserve_post(env, ctx) {
        return false;
    }
    user_mut(env).queue(ctx).post(m);
    queue_changed(env, ctx);
    true
}

/// Lifecycle notifications use the existing owning thread's posted queue.
/// They carry no physical-input receipt and do not manufacture keyboard state.
pub(super) fn post_system(env: &mut Env<'_>, hwnd: u32, msg: u32, wparam: u32) {
    let Some(ctx) = user(env)
        .windows
        .get(&env.sys.objects, &hwnd)
        .map(|w| w.thread)
    else {
        return;
    };
    let message = Msg {
        hwnd,
        msg,
        wparam,
        time: env.sys.clock.now_ms(),
        pt: user(env).cursor_pos,
        system: true,
        ..Default::default()
    };
    post_to_thread(env, ctx, message);
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
pub fn message_wait_events(env: &mut Env<'_>, mask: u32) -> Option<(u32, u32)> {
    let ctx = env.context;
    let q = user_mut(env).queue(ctx);
    let original = [q.message_wait_event, q.message_wait_notification];
    let mut handles = original;
    for index in 0..2 {
        if handles[index] == 0 {
            let handle = env.sys.objects.insert(Object::Event {
                manual_reset: index == 0,
                signaled: false,
            });
            if handle == 0 {
                for previous in 0..index {
                    if original[previous] == 0 {
                        env.sys.objects.close(handles[previous]);
                    }
                }
                return None;
            }
            handles[index] = handle;
        }
    }
    let changed = queue_status_for(env, ctx).1;
    let q = user_mut(env).queue(ctx);
    q.message_wait_event = handles[0];
    q.message_wait_notification = handles[1];
    q.message_wait_mask = mask;
    let (input, notification) = (q.message_wait_event, q.message_wait_notification);
    env.sys.reset_event(notification);
    if changed & mask != 0 {
        env.sys.set_event(input);
    } else {
        env.sys.reset_event(input);
    }
    Some((input, notification))
}

const QS_POSTMESSAGE: u32 = 0x8;
const QS_TIMER: u32 = 0x10;
const QS_PAINT: u32 = 0x20;
const QS_ALLPOSTMESSAGE: u32 = 0x100;

pub(super) fn posted_status(m: &Msg) -> u32 {
    if !m.input {
        return QS_POSTMESSAGE | if m.system { 0 } else { QS_ALLPOSTMESSAGE };
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
    queue_status_for(env, env.context)
}

pub(super) fn queue_status_for(env: &mut Env<'_>, ctx: adv_machine::CtxId) -> (u32, u32) {
    super::mouse_tracking::poll(env);
    let sent = u32::from(sent::pending(env, ctx)) * sent::QS_SEND;
    let now = env.sys.clock.now_us();
    let u = &mut env.ext.get_mut::<crate::Win32>().user;
    let paint = u
        .windows
        .values(&env.sys.objects)
        .any(|w| w.thread == ctx && u.paints(w));
    let q = u.queue(ctx);
    let generated = (u32::from(paint) * QS_PAINT)
        | (u32::from(q.timers.iter().any(|t| t.due_us <= now)) * QS_TIMER);
    let mut pending = generated | sent;
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
    let ctx = env.context;
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
    fn matches(&self, u: &window::User, objects: &adv_machine::Objects, m: &Msg) -> bool {
        self.matches_hwnd(u, objects, m.hwnd) && self.range(m.msg)
    }
    fn matches_hwnd(&self, u: &window::User, objects: &adv_machine::Objects, hwnd: u32) -> bool {
        match self.hwnd {
            0 => true,
            u32::MAX => hwnd == 0,
            h => hwnd == h || (hwnd != 0 && is_descendant(u, objects, hwnd, h)),
        }
    }
    fn range(&self, msg: u32) -> bool {
        (self.min == 0 && self.max == 0) || (self.min <= msg && msg <= self.max)
    }
}

fn is_descendant(
    u: &window::User,
    objects: &adv_machine::Objects,
    mut h: u32,
    ancestor: u32,
) -> bool {
    while let Some(w) = u.windows.get(objects, &h) {
        if w.parent == ancestor {
            return true;
        }
        h = w.parent;
    }
    false
}

/// The next message for the current thread, removed from the queue with `remove`.
fn next_message(env: &mut Env<'_>, f: Filter, remove: bool) -> Option<Msg> {
    super::mouse_tracking::poll(env);
    let message = next_message_inner(env, f, remove);
    if message.is_some() {
        // Posted/generated messages and the supported host-input path carry no
        // driver tag. PM_NOREMOVE updates metadata too; empty/filtered peeks do not.
        let context = env.context;
        user_mut(env).queue(context).extra_info = 0;
    }
    message
}
fn next_message_inner(env: &mut Env<'_>, f: Filter, remove: bool) -> Option<Msg> {
    observe_retrieval(env, f);
    let ctx = env.context;
    let now = env.sys.clock.now_us();
    let now_ms = env.sys.clock.now_ms();
    let frame = env.sys.frame;
    let u = &mut env.ext.get_mut::<crate::Win32>().user;
    // Native retrieval discards lifecycle posts whose HWND was destroyed,
    // including WM_CREATE failure. Before retrieval their category remains
    // observable through GetQueueStatus; the post never owns the window.
    if let Some(q) = u.queues.get_mut(&ctx) {
        q.posted
            .retain(|m| !m.system || u.windows.contains_key(&env.sys.objects, &m.hwnd));
    }
    let mut found = None;
    {
        let posted = &u.queues.get(&ctx).map(|q| &q.posted);
        if let Some(posted) = posted {
            found = posted
                .iter()
                .position(|m| f.matches(u, &env.sys.objects, m));
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
            && m.input
            && let Some(key) = m.input_key
        {
            let previous = q.keyboard.0[key.key as usize];
            q.keyboard.0[key.key as usize] =
                ((previous & 1) ^ u8::from(key.toggle)) | if key.pressed { 0x80 } else { 0 };
            if let Some(side) = key.side {
                let previous = q.keyboard.0[side as usize];
                q.keyboard.0[side as usize] = (previous & 1) | if key.pressed { 0x80 } else { 0 };
            }
        }
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
            input_key: None,
            character_wide: false,
            system: false,
        });
    }
    if f.range(wm::PAINT) {
        let paint = u.windows.values(&env.sys.objects).find(|w| {
            w.thread == ctx
                && u.paints(w)
                && (f.hwnd == 0
                    || w.hwnd == f.hwnd
                    || is_descendant(u, &env.sys.objects, w.hwnd, f.hwnd))
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
                input_key: None,
                character_wide: false,
                system: false,
            });
        }
    }
    if f.range(wm::TIMER) {
        let index = u.queues[&ctx]
            .timers
            .iter()
            .enumerate()
            .filter(|(_, t)| t.due_us <= now && f.matches_hwnd(u, &env.sys.objects, t.hwnd))
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
                input_key: None,
                character_wide: false,
                system: false,
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
    let ctx = env.context;
    let u = user(env);
    let timers = u
        .queues
        .get(&ctx)
        .into_iter()
        .flat_map(|q| &q.timers)
        .filter(|t| f.range(wm::TIMER) && f.matches_hwnd(u, &env.sys.objects, t.hwnd))
        .map(|t| t.due_us)
        .min();
    let dialog = headless_deadline(u, &env.sys.objects, ctx);
    let hover = u
        .queues
        .get(&ctx)
        .and_then(|q| q.mouse_tracking)
        .filter(|t| f.range(0x2a1) && f.matches_hwnd(u, &env.sys.objects, t.hwnd))
        .and_then(|t| t.due_us);
    [timers, dialog, hover].into_iter().flatten().min()
}

// ---- headless dialog policy ----

const AUTO_CLICK_MAX: u32 = 3;

/// A dialog-like window of `ctx` waiting for input, and its button to press: the default
/// push button, else IDOK, else the first push button.
fn headless_target(
    u: &window::User,
    objects: &adv_machine::Objects,
    ctx: adv_machine::CtxId,
) -> Option<(u32, u32)> {
    u.auto_dialog_ms?;
    for &top in &u.top {
        let w = &u.windows.get(objects, &top).unwrap();
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
            .map(|c| u.windows.get(objects, c).unwrap())
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

fn headless_deadline(
    u: &window::User,
    objects: &adv_machine::Objects,
    ctx: adv_machine::CtxId,
) -> Option<u64> {
    let (top, _) = headless_target(u, objects, ctx)?;
    let w = &u.windows.get(objects, &top).unwrap();
    let delay = u64::from(u.auto_dialog_ms?) * 1000;
    Some(w.created_us + delay * u64::from(w.auto_clicks + 1))
}

/// Headless mode: press the button of a dialog that has been waiting long enough, by
/// posting a real click to it. Returns whether something was posted.
fn headless_idle(env: &mut Env<'_>) -> bool {
    let ctx = env.context;
    let now = env.sys.clock.now_us();
    let u = &mut env.ext.get_mut::<crate::Win32>().user;
    let Some((top, button)) = headless_target(u, &env.sys.objects, ctx) else {
        return false;
    };
    if headless_deadline(u, &env.sys.objects, ctx).is_none_or(|d| now < d) {
        return false;
    }
    let w = u.windows.get_mut(&mut env.sys.objects, &top).unwrap();
    w.auto_clicks += 1;
    let title = w.text.clone();
    let b = &u.windows.get(&env.sys.objects, &button).unwrap();
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
    let ctx = env.context;
    if let Some(tracking) = &mut user_mut(env).input_tracking {
        tracking.remember(ctx, out, message, removed && written);
    }
}

fn finish_hooked_message(env: &mut Env<'_>, mut original: Msg, out: u32, removed: bool) -> Msg {
    let changed = Msg::read(env.cpu, out);
    original.hwnd = changed.hwnd;
    original.msg = changed.msg;
    original.wparam = changed.wparam;
    original.lparam = changed.lparam;
    original.time = changed.time;
    original.pt = changed.pt;
    let ctx = env.context;
    if let Some(tracking) = &mut user_mut(env).input_tracking {
        tracking.remember(ctx, out, original, removed);
    }
    original
}

fn peek_message_a(env: &mut Env<'_>) -> Outcome {
    let (out, hwnd, min, max, flags) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3), env.arg(4));
    peek_message_front(env, out, hwnd, min, max, flags, false)
}

#[win32(user32, name = "PeekMessageW", level = implement)]
fn peek_message_w(
    env: &mut Env<'_>,
    out: GuestPtr<()>,
    hwnd: AbiHandle,
    min: u32,
    max: u32,
    flags: u32,
) -> Outcome {
    peek_message_front(env, out.addr(), hwnd.0, min, max, flags, true)
}

fn peek_message_front(
    env: &mut Env<'_>,
    out: u32,
    hwnd: u32,
    min: u32,
    max: u32,
    flags: u32,
    wide: bool,
) -> Outcome {
    let f = Filter { hwnd, min, max };
    if let Some(out) =
        crate::gdi32::raster::front::flush_software(env, adv_device::surface::FlushReason::Message)
    {
        return out;
    }
    let remove = flags & PM_REMOVE != 0;
    if flags >> 16 == 0 || flags & (sent::QS_SEND << 16) != 0 {
        sent::drain(env, move |env| peek_message(env, out, f, remove, wide))
    } else {
        peek_message(env, out, f, remove, wide)
    }
}

// Public retrieval owns a checked 28-byte x86 MSG. Recheck after draining sent
// callbacks or waking a blocked GetMessage: guest code can change mappings.
fn retrieval_output(env: &mut Env<'_>, out: u32, f: Filter) -> bool {
    if f.hwnd != 0
        && f.hwnd != u32::MAX
        && !user(env)
            .windows
            .get(&env.sys.objects, &f.hwnd)
            .is_some_and(|window| window.thread == env.context)
    {
        crate::util::set_last_error(env, 1400);
        return false;
    }
    if adv_abi::guest::check_range(env.cpu, out, 28, adv_cpu::backend::Prot::WRITE).is_err() {
        crate::util::set_last_error(env, 998);
        return false;
    }
    true
}

fn character_message(message: u32) -> bool {
    matches!(message, wm::CHAR | wm::SYSCHAR | 0x103 | 0x107)
}

/// Queue producers retain the A/W posting API's character encoding; physical
/// host input supplies UTF-16. TranslateMessage generates ASCII, common to both. A/W
/// thunks support whole single-byte characters; DBCS/UTF-8 split-message state
/// and IME composition are not implemented, so they must not be guessed.
fn character_param(
    env: &Env<'_>,
    parameter: u32,
    source_wide: bool,
    target_wide: bool,
) -> Result<u32, &'static str> {
    if source_wide == target_wide {
        return Ok(parameter);
    }
    if parameter < 0x80 {
        return Ok(parameter);
    }
    let codec = &env.ext.get::<Win32>().codec;
    if !matches!(codec.acp, 932 | 936 | 1252 | 65001) {
        return Err("character conversion requires a supported guest ANSI encoder");
    }
    if source_wide {
        let unit = u16::try_from(parameter)
            .ok()
            .filter(|unit| !(0xd800..=0xdfff).contains(unit))
            .ok_or("cross-encoding UTF-16 surrogate character messages are unsupported")?;
        let (bytes, _) = codec.from_utf16(0, &[unit]);
        if bytes.len() != 1 {
            return Err(
                "multibyte character dispatch requires unsupported split-message conversion",
            );
        }
        Ok(u32::from(bytes[0]))
    } else {
        let byte = u8::try_from(parameter)
            .map_err(|_| "packed or split multibyte character retrieval is unsupported")?;
        let (units, invalid) = codec.to_utf16_checked(0, &[byte]);
        if invalid || units.len() != 1 {
            return Err(
                "multibyte character retrieval requires unsupported split-message conversion",
            );
        }
        Ok(u32::from(units[0]))
    }
}

fn next_public_message(
    env: &mut Env<'_>,
    filter: Filter,
    remove: bool,
    wide: bool,
) -> Result<Option<Msg>, &'static str> {
    let Some(mut message) = next_message(env, filter, false) else {
        return Ok(None);
    };
    let parameter = if character_message(message.msg) {
        character_param(
            env,
            message.wparam,
            message.character_wide || message.input,
            wide,
        )?
    } else {
        message.wparam
    };
    // No guest callback or scheduling occurs between these observations. Only
    // remove after conversion succeeds, retaining unsupported queued input.
    if remove {
        message = next_message(env, filter, true).expect("peeked message remains queued");
    }
    message.wparam = parameter;
    Ok(Some(message))
}

fn character_failure(env: &mut Env<'_>, api: &str, reason: &str, result: u32) -> Outcome {
    adv_abi::warn_unavailable(env, api, reason);
    crate::util::set_last_error(env, 50);
    Outcome::Return(result)
}

fn peek_message(env: &mut Env<'_>, out: u32, f: Filter, remove: bool, wide: bool) -> Outcome {
    if !retrieval_output(env, out, f) {
        return Outcome::Return(0);
    }
    let mut next = next_public_message(env, f, remove, wide);
    if matches!(next, Ok(None)) && headless_idle(env) {
        next = next_public_message(env, f, remove, wide);
    }
    let m = match next {
        Ok(message) => message,
        Err(reason) => {
            return character_failure(
                env,
                if wide {
                    "user32.dll!PeekMessageW"
                } else {
                    "user32.dll!PeekMessageA"
                },
                reason,
                0,
            );
        }
    };
    if let Some(m) = m {
        write_message(env, m, out, remove);
        return super::hooks::retrieved(env, out, remove, move |env| {
            finish_hooked_message(env, m, out, remove);
            Outcome::Return(1)
        });
    }
    // A frame above the display rate waits for the vsync or a message.
    if let Some(until) = frame_cap::take(env) {
        let event = queue_event(env);
        if event != 0 {
            let deadline = next_deadline(env, f).map_or(until, |d| d.min(until));
            return Outcome::block_timed(
                Wait::Object {
                    handle: event,
                    deadline: Some(deadline),
                },
                |_, _| Outcome::Return(0),
            );
        }
    }
    // Nothing: let other contexts run (the caller is a polling loop).
    Outcome::block_then_return(Wait::Yield, 0)
}

fn get_message_a(env: &mut Env<'_>) -> Outcome {
    let (out, hwnd, min, max) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3));
    get_message(env, out, Filter { hwnd, min, max }, false)
}

#[win32(user32, name = "GetMessageW", level = implement)]
fn get_message_w(
    env: &mut Env<'_>,
    out: GuestPtr<()>,
    hwnd: AbiHandle,
    min: u32,
    max: u32,
) -> Outcome {
    get_message(
        env,
        out.addr(),
        Filter {
            hwnd: hwnd.0,
            min,
            max,
        },
        true,
    )
}

fn get_message(env: &mut Env<'_>, out: u32, f: Filter, wide: bool) -> Outcome {
    if let Some(out) =
        crate::gdi32::raster::front::flush_software(env, adv_device::surface::FlushReason::Message)
    {
        return out;
    }
    sent::drain(env, move |env| get_message_inner(env, out, f, wide))
}

fn get_message_inner(env: &mut Env<'_>, out: u32, f: Filter, wide: bool) -> Outcome {
    if !retrieval_output(env, out, f) {
        return Outcome::Return(u32::MAX);
    }
    let next = match next_public_message(env, f, true, wide) {
        Ok(message) => message,
        Err(reason) => {
            return character_failure(
                env,
                if wide {
                    "user32.dll!GetMessageW"
                } else {
                    "user32.dll!GetMessageA"
                },
                reason,
                u32::MAX,
            );
        }
    };
    if let Some(m) = next {
        write_message(env, m, out, true);
        return super::hooks::retrieved(env, out, true, move |env| {
            let message = finish_hooked_message(env, m, out, true);
            Outcome::Return(u32::from(message.msg != wm::QUIT))
        });
    }
    if headless_idle(env) {
        return get_message(env, out, f, wide);
    }
    let event = queue_event(env);
    if event == 0 {
        crate::util::set_last_error(env, crate::util::err::NOT_ENOUGH_MEMORY);
        return Outcome::Return(u32::MAX);
    }
    let deadline = next_deadline(env, f);
    Outcome::block_timed(
        Wait::Object {
            handle: event,
            deadline,
        },
        move |env, _| get_message(env, out, f, wide),
    )
}

/// Block until the current thread's queue may have something (WaitMessage, modal loops).
pub fn wait_for_message(
    env: &mut Env<'_>,
    then: impl FnOnce(&mut Env<'_>) -> Outcome + 'static,
) -> Outcome {
    let event = queue_event(env);
    if event == 0 {
        return Outcome::Fail("message wake allocation exhausted the object table".into());
    }
    let deadline = next_deadline(
        env,
        Filter {
            hwnd: 0,
            min: 0,
            max: 0,
        },
    );
    Outcome::block_timed(
        Wait::Object {
            handle: event,
            deadline,
        },
        move |env, _| sent::drain(env, then),
    )
}

/// Arm the wake-up event after the caller found no matching message. Unmatched posted
/// messages must not keep a filtered GetMessage runnable; a new post signals it again.
fn queue_event(env: &mut Env<'_>) -> u32 {
    let ctx = env.context;
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
    sent::drain(env, wait_message_inner)
}

fn wait_message_inner(env: &mut Env<'_>) -> Outcome {
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
    if queue_event(env) == 0 {
        crate::util::set_last_error(env, crate::util::err::NOT_ENOUGH_MEMORY);
        return Outcome::Return(0);
    }
    wait_for_message(env, |_| Outcome::Return(1))
}

fn get_queue_status(env: &mut Env<'_>) -> Outcome {
    let flags = env.arg(0) & 0xffff;
    let (pending, changed) = queue_status(env);
    let ctx = env.context;
    user_mut(env).queue(ctx).changed_status &= !flags;
    Outcome::Return((pending & flags) << 16 | (changed & flags))
}

// These system/control messages carry pointer parameters in their documented
// contracts. Asynchronous marshaling is not provided by this virtual process;
// never queue a pointer for later text decoding or structure access. Custom
// WM_USER+ messages retain the application's ordinary integer/pointer protocol.
fn asynchronous_pointer_message(msg: u32) -> bool {
    matches!(
        msg,
        0x0001 | 0x000c | 0x000d | 0x001a | 0x001b | 0x0024 | 0x002b | 0x002c
            | 0x002d | 0x0039 | 0x0046 | 0x0047 | 0x004a | 0x004e | 0x0053
            | 0x007c | 0x007d | 0x0081 | 0x0083
            // EDIT, COMBOBOX and LISTBOX string/array/rectangle parameters.
            | 0x00b0 | 0x00b2 | 0x00b3 | 0x00b4 | 0x00c2 | 0x00c4 | 0x00cb
            | 0x0143 | 0x0145 | 0x0148 | 0x014a | 0x014c | 0x014d | 0x0152
            | 0x0158 | 0x0180 | 0x0181 | 0x0189 | 0x018c | 0x018d | 0x018f
            | 0x0191 | 0x0192 | 0x0198 | 0x01a2 | 0x0213 | 0x0220 | 0x0309 | 0x030b
    )
}

fn post_parameters(env: &mut Env<'_>, msg: u32, api: &str) -> bool {
    if asynchronous_pointer_message(msg) {
        adv_abi::warn_unavailable(
            env,
            api,
            "pointer-bearing system/control messages require synchronous delivery; asynchronous marshaling is unavailable",
        );
        crate::util::set_last_error(env, 1159); // ERROR_MESSAGE_SYNC_ONLY
        return false;
    }
    true
}

fn post_message_front(env: &mut Env<'_>, hwnd: u32, msg: u32, wp: u32, lp: u32, wide: bool) -> i32 {
    if !post_parameters(
        env,
        msg,
        if wide {
            "user32.dll!PostMessageW"
        } else {
            "user32.dll!PostMessageA"
        },
    ) {
        return 0;
    }
    let source = if wide {
        PostSource::Unicode
    } else {
        PostSource::Ansi
    };
    if hwnd == 0xffff {
        // Existing virtual-process HWND_BROADCAST profile: top-level guest
        // windows only. Preserve each target's actual queue and source encoding.
        let tops = user(env).top.clone();
        let mut success = true;
        for t in tops {
            success &= post_kind(env, t, msg, wp, lp, source);
        }
        return i32::from(success);
    }
    i32::from(post_kind(env, hwnd, msg, wp, lp, source))
}

fn post_thread_message_front(
    env: &mut Env<'_>,
    tid: u32,
    msg: u32,
    wp: u32,
    lp: u32,
    wide: bool,
) -> i32 {
    if !post_parameters(
        env,
        msg,
        if wide {
            "user32.dll!PostThreadMessageW"
        } else {
            "user32.dll!PostThreadMessageA"
        },
    ) {
        return 0;
    }
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
        return 0;
    };
    // Native self-post initializes the caller's queue; posting from another
    // thread must not invent a queue for a live kernel-only target.
    if ctx != env.context && !user(env).queues.contains_key(&ctx) {
        crate::util::set_last_error(env, 1444);
        return 0;
    }
    let time = env.sys.clock.now_ms();
    i32::from(post_to_thread(
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
            input_key: None,
            character_wide: wide,
            system: false,
        },
    ))
}

#[win32(user32, name = "PostMessageA", level = implement)]
fn post_message_a(env: &mut Env<'_>, hwnd: AbiHandle, msg: u32, wp: u32, lp: i32) -> i32 {
    post_message_front(env, hwnd.0, msg, wp, lp as u32, false)
}

#[win32(user32, name = "PostMessageW", level = implement)]
fn post_message_w(env: &mut Env<'_>, hwnd: AbiHandle, msg: u32, wp: u32, lp: i32) -> i32 {
    post_message_front(env, hwnd.0, msg, wp, lp as u32, true)
}

#[win32(user32, name = "PostThreadMessageA", level = implement)]
fn post_thread_message_a(env: &mut Env<'_>, tid: u32, msg: u32, wp: u32, lp: i32) -> i32 {
    post_thread_message_front(env, tid, msg, wp, lp as u32, false)
}

#[win32(user32, name = "PostThreadMessageW", level = implement)]
fn post_thread_message_w(env: &mut Env<'_>, tid: u32, msg: u32, wp: u32, lp: i32) -> i32 {
    post_thread_message_front(env, tid, msg, wp, lp as u32, true)
}

fn post_quit_message(env: &mut Env<'_>) -> Outcome {
    let code = env.arg(0);
    post_quit(env, code);
    Outcome::Return(0)
}

/// PostQuitMessage for the current thread.
pub fn post_quit(env: &mut Env<'_>, code: u32) {
    let ctx = env.context;
    let q = user_mut(env).queue(ctx);
    q.quit = Some(code);
    q.changed_status |= QS_POSTMESSAGE | QS_ALLPOSTMESSAGE;
    queue_changed(env, ctx);
}

fn send_message_a(env: &mut Env<'_>) -> Outcome {
    let (hwnd, msg, wp, lp) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3));
    send_message(env, hwnd, msg, wp, lp, false)
}

#[win32(user32, name = "SendMessageW", level = implement)]
fn send_message_w(env: &mut Env<'_>, hwnd: AbiHandle, msg: u32, wp: u32, lp: i32) -> Outcome {
    send_message(env, hwnd.0, msg, wp, lp as u32, true)
}

fn send_message(env: &mut Env<'_>, hwnd: u32, msg: u32, wp: u32, lp: u32, wide: bool) -> Outcome {
    if hwnd == 0xffff {
        let tops = user(env).top.iter().copied().collect();
        return broadcast_encoded(env, tops, msg, wp, lp, wide);
    }
    send_encoded(env, hwnd, msg, wp, lp, wide, |_, r| Outcome::Return(r))
}

fn broadcast_encoded(
    env: &mut Env<'_>,
    mut windows: VecDeque<u32>,
    msg: u32,
    wp: u32,
    lp: u32,
    wide: bool,
) -> Outcome {
    let Some(hwnd) = windows.pop_front() else {
        return Outcome::Return(1);
    };
    // A broadcast can reach both class encodings. Each receiver uses the same
    // conversion, callback and cross-thread send lifetime as an ordinary send.
    send_encoded(env, hwnd, msg, wp, lp, wide, move |env, _| {
        broadcast_encoded(env, windows, msg, wp, lp, wide)
    })
}

fn send_message_timeout_a(env: &mut Env<'_>) -> Outcome {
    send_message_timeout(env, false)
}
#[win32(user32, name = "SendMessageTimeoutW", level = implement)]
#[allow(clippy::too_many_arguments)]
fn send_message_timeout_w(
    env: &mut Env<'_>,
    _hwnd: AbiHandle,
    _msg: u32,
    _wp: u32,
    _lp: i32,
    _flags: u32,
    _timeout: u32,
    _out: GuestPtr<u32>,
) -> Outcome {
    send_message_timeout(env, true)
}
fn send_message_timeout(env: &mut Env<'_>, wide: bool) -> Outcome {
    let (hwnd, msg, wp, lp, flags, timeout, out) = (
        env.arg(0),
        env.arg(1),
        env.arg(2),
        env.arg(3),
        env.arg(4),
        env.arg(5),
        env.arg(6),
    );
    if flags & !0x2b != 0 {
        crate::util::set_last_error(env, 87);
        return Outcome::Return(0);
    }
    let then = move |env: &mut Env<'_>, result: Result<u32, u32>| match result {
        Ok(value) => {
            if out != 0 && env.cpu.write_u32(out, value).is_err() {
                crate::util::set_last_error(env, 998);
                return Outcome::Return(0);
            }
            Outcome::Return(1)
        }
        Err(error) => {
            crate::util::set_last_error(env, error);
            Outcome::Return(0)
        }
    };
    if hwnd == 0xffff {
        let targets = user(env)
            .top
            .iter()
            .map(|&window| [window, msg, wp, lp])
            .collect();
        return timeout_broadcast(env, targets, flags, timeout, wide, Ok(0), Box::new(then));
    }
    timeout_to(
        env,
        [hwnd, msg, wp, lp],
        flags,
        timeout,
        wide,
        Box::new(then),
    )
}

fn timeout_broadcast(
    env: &mut Env<'_>,
    mut targets: VecDeque<[u32; 4]>,
    flags: u32,
    timeout: u32,
    wide: bool,
    result: Result<u32, u32>,
    then: sent::Done,
) -> Outcome {
    let Some(target) = targets.pop_front() else {
        return then(env, result);
    };
    timeout_to(
        env,
        target,
        flags,
        timeout,
        wide,
        Box::new(move |env, next| {
            timeout_broadcast(env, targets, flags, timeout, wide, result.and(next), then)
        }),
    )
}

fn timeout_to(
    env: &mut Env<'_>,
    args: [u32; 4],
    flags: u32,
    timeout: u32,
    wide: bool,
    then: sent::Done,
) -> Outcome {
    let [hwnd, msg, mut wp, lp] = args;
    let Some(thread) = user(env)
        .windows
        .get(&env.sys.objects, &hwnd)
        .map(|w| w.thread)
    else {
        return then(env, Err(1400));
    };
    if thread == env.context {
        return encoded_to(env, None, hwnd, msg, wp, lp, wide, then);
    }
    let window = user(env).windows.get(&env.sys.objects, &hwnd).unwrap();
    let target_wide = window.unicode;
    let combo = user(env).classes[window.class]
        .name
        .eq_ignore_ascii_case("ComboBox");
    let text_message = matches!(msg, wm::SETTEXT | wm::GETTEXT | wm::GETTEXTLENGTH)
        || (combo && matches!(msg, 0x143 | 0x148 | 0x149 | 0x14a | 0x14c | 0x14d | 0x158));
    if target_wide != wide && character_message(msg) {
        wp = match character_param(env, wp, wide, target_wide) {
            Ok(parameter) => parameter,
            Err(reason) => {
                adv_abi::warn_unavailable(
                    env,
                    if wide {
                        "user32.dll!SendMessageTimeoutW"
                    } else {
                        "user32.dll!SendMessageTimeoutA"
                    },
                    reason,
                );
                return then(env, Err(50));
            }
        };
    }
    if target_wide != wide && text_message {
        // A timed sender may return while its receiver still runs. Do not hand
        // that receiver a converted temporary buffer with sender-only lifetime.
        adv_abi::warn_unavailable(
            env,
            if wide {
                "user32.dll!SendMessageTimeoutW"
            } else {
                "user32.dll!SendMessageTimeoutA"
            },
            "cross-thread A/W timed text requires receiver-owned conversion storage",
        );
        return then(env, Err(50));
    }
    let options = sent::Options {
        deadline: Some(
            env.sys
                .clock
                .now_us()
                .saturating_add(u64::from(timeout) * 1000),
        ),
        block: flags & 1 != 0,
        abort_if_hung: flags & 2 != 0,
        no_timeout_if_responsive: flags & 8 != 0,
        error_on_exit: flags & 0x20 != 0,
    };
    sent::send(env, thread, [hwnd, msg, wp, lp], options, then)
}

fn dispatch_message_a(env: &mut Env<'_>) -> Outcome {
    let address = env.arg(0);
    dispatch_message(env, address, false)
}

#[win32(user32, name = "DispatchMessageW", level = implement)]
fn dispatch_message_w(env: &mut Env<'_>, message: GuestPtr<()>) -> Outcome {
    dispatch_message(env, message.addr(), true)
}

fn dispatch_message(env: &mut Env<'_>, address: u32, wide: bool) -> Outcome {
    if adv_abi::guest::check_range(env.cpu, address, 28, adv_cpu::backend::Prot::READ).is_err() {
        crate::util::set_last_error(env, 998);
        return Outcome::Return(0);
    }
    let m = Msg::read(env.cpu, address);
    let target_wide = user(env)
        .windows
        .get(&env.sys.objects, &m.hwnd)
        .map(|w| w.unicode);
    let parameter = if character_message(m.msg)
        && let Some(target) = target_wide
    {
        match character_param(env, m.wparam, wide, target) {
            Ok(parameter) => parameter,
            Err(reason) => {
                return character_failure(
                    env,
                    if wide {
                        "user32.dll!DispatchMessageW"
                    } else {
                        "user32.dll!DispatchMessageA"
                    },
                    reason,
                    0,
                );
            }
        }
    } else {
        m.wparam
    };
    let ctx = env.context;
    let mut m = user_mut(env)
        .input_tracking
        .as_mut()
        .map_or(m, |tracking| tracking.take(ctx, address, m));
    m.wparam = parameter;
    let encoding = if character_message(m.msg) {
        target_wide.unwrap_or(wide)
    } else {
        wide
    };
    dispatch_common(env, m, Some(encoding), |_, r| Outcome::Return(r))
}

/// DispatchMessage: timer callbacks, else the window procedure.
pub fn dispatch(
    env: &mut Env<'_>,
    m: Msg,
    then: impl FnOnce(&mut Env<'_>, u32) -> Outcome + 'static,
) -> Outcome {
    dispatch_common(env, m, None, then)
}

fn dispatch_common(
    env: &mut Env<'_>,
    m: Msg,
    wide: Option<bool>,
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
    if let Some(wide) = wide {
        send_encoded(env, m.hwnd, m.msg, m.wparam, m.lparam, wide, then)
    } else {
        send(env, m.hwnd, m.msg, m.wparam, m.lparam, then)
    }
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
    let shift = super::queued_key_state(env, 0x10) & 0x80 != 0;
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
    let ctx = env.context;
    Outcome::Return(user_mut(env).queue(ctx).last_time)
}

fn get_message_pos(env: &mut Env<'_>) -> Outcome {
    let ctx = env.context;
    let (x, y) = user_mut(env).queue(ctx).last_pos;
    Outcome::Return(make_long(x, y))
}

// ---- timers ----

fn set_timer(env: &mut Env<'_>) -> Outcome {
    let (hwnd, id, ms, proc) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3));
    let ctx = if hwnd == 0 {
        env.context
    } else {
        let Some(w) = user(env).windows.get(&env.sys.objects, &hwnd) else {
            return Outcome::Return(0);
        };
        w.thread
    };
    let now = env.sys.clock.now_us();
    // USER_TIMER_MINIMUM is 10 ms.
    let interval_us = u64::from(ms.max(10)) * 1000;
    let u = &mut env.ext.get_mut::<crate::Win32>().user;
    let q = u.queue(ctx);
    // A thread timer keeps an ID that names one of its own timers; any other
    // ID is ignored for a fresh one, so re-arming never adds a timer.
    // Generated IDs cycle through 0x7fff..=0xffff: callers that keep only a
    // WORD (Delphi's hint timer) must be able to kill them.
    let live =
        |q: &super::window::Queue, id: u32| q.timers.iter().any(|t| t.hwnd == 0 && t.id == id);
    let id = if hwnd != 0 || live(q, id) {
        Some(id)
    } else {
        let next = |id: u32| if id >= 0xffff { 0x7fff } else { id + 1 };
        let start = q.next_timer_id.max(0x7fff);
        let mut candidate = start;
        while live(q, candidate) {
            candidate = next(candidate);
            if candidate == start {
                break;
            }
        }
        (!live(q, candidate)).then(|| {
            q.next_timer_id = next(candidate);
            candidate
        })
    };
    let Some(id) = id else {
        // Every generated thread timer ID is live.
        crate::util::set_last_error(env, 8);
        return Outcome::Return(0);
    };
    let q = env.ext.get_mut::<crate::Win32>().user.queue(ctx);
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
        env.context
    } else {
        let Some(w) = user(env).windows.get(&env.sys.objects, &hwnd) else {
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
    def_window_proc_encoded(env, hwnd, msg, wp, lp, false, |_, r| Outcome::Return(r))
}

#[win32(user32, name="DefWindowProcW", level=state)]
fn def_window_proc_w(env: &mut Env<'_>, hwnd: AbiHandle, msg: u32, wp: u32, lp: i32) -> Outcome {
    def_window_proc_encoded(env, hwnd.0, msg, wp, lp as u32, true, |_, r| {
        Outcome::Return(r)
    })
}

fn default_proc(env: &mut Env<'_>) -> Outcome {
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
    let wide = user(env)
        .windows
        .get(&env.sys.objects, &hwnd)
        .is_some_and(|w| w.unicode);
    def_window_proc_encoded(env, hwnd, msg, wp, lp, wide, then)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn def_window_proc_encoded(
    env: &mut Env<'_>,
    hwnd: u32,
    msg: u32,
    wp: u32,
    lp: u32,
    wide: bool,
    then: impl FnOnce(&mut Env<'_>, u32) -> Outcome + 'static,
) -> Outcome {
    let r = match msg {
        wm::NCCREATE => 1,
        wm::CLOSE => return destroy_window(env, hwnd, move |env, _| then(env, 0)),
        wm::SHOWWINDOW => return window::parent_show(env, hwnd, wp != 0, lp, then),
        wm::SETTEXT => {
            let Some(text) = read_text(env, lp, wide) else {
                crate::util::set_last_error(env, 87);
                return then(env, 0);
            };
            if let Some(w) = env
                .ext
                .get::<crate::Win32>()
                .user
                .windows
                .get_mut(&mut env.sys.objects, &hwnd)
            {
                w.text = text;
                w.invalidate_client(true);
            }
            1
        }
        wm::GETTEXT => {
            let Some(w) = user(env).windows.get(&env.sys.objects, &hwnd) else {
                return then(env, 0);
            };
            let text = w.text.clone();
            write_text(env, lp, wp, &text, wide).unwrap_or_else(|| {
                crate::util::set_last_error(env, 87);
                0
            })
        }
        wm::GETTEXTLENGTH => {
            let Some(w) = user(env).windows.get(&env.sys.objects, &hwnd) else {
                return then(env, 0);
            };
            text_length(env, &w.text, wide)
        }
        wm::SETFONT => {
            if let Some(w) = env
                .ext
                .get::<crate::Win32>()
                .user
                .windows
                .get_mut(&mut env.sys.objects, &hwnd)
            {
                w.font = wp;
            }
            0
        }
        wm::GETFONT => user(env)
            .windows
            .get(&env.sys.objects, &hwnd)
            .map_or(0, |w| w.font),
        wm::PAINT => {
            validate(env, hwnd);
            0
        }
        wm::ERASEBKGND => {
            let Some(w) = user(env).windows.get(&env.sys.objects, &hwnd) else {
                return then(env, 0);
            };
            let (width, height) = w.client_size();
            let brush = user(env).classes[w.class].background;
            if brush == 0 {
                return then(env, 0);
            }
            let brush = if brush <= 31 {
                super::system_colors::brush(env, brush - 1)
            } else {
                brush
            };
            let api = if wide {
                "user32.dll!DefWindowProcW"
            } else {
                "user32.dll!DefWindowProcA"
            };
            return match crate::gdi32::raster::fill::erase_background(
                env, wp, width, height, brush, api,
            ) {
                Outcome::Return(result) => then(env, result),
                other => other,
            };
        }
        wm::NCHITTEST => 1, // HTCLIENT
        wm::IME_CHAR => {
            // DefWindowProcA posts a packed DBCS character as lead then trail byte.
            let (source, characters) = if wide {
                (PostSource::Unicode, vec![wp & 0xffff])
            } else if wp & 0xff00 != 0 {
                (PostSource::Ansi, vec![wp >> 8 & 0xff, wp & 0xff])
            } else {
                (PostSource::Ansi, vec![wp & 0xff])
            };
            for character in characters {
                post_kind(env, hwnd, wm::CHAR, character, lp, source);
            }
            0
        }
        wm::MOUSEWHEEL => {
            let parent = user(env)
                .windows
                .get(&env.sys.objects, &hwnd)
                .map_or(0, |window| window.parent);
            if parent != 0 {
                return send(env, parent, msg, wp, lp, then);
            }
            0
        }
        wm::MOUSEACTIVATE => 1,
        wm::ACTIVATE if wp & 0xffff != 0 => {
            return window::set_focus(env, hwnd, move |env, _| then(env, 0));
        }
        wm::WINDOWPOSCHANGED => {
            let flags = env.cpu.read_u32(lp + 24).unwrap_or(0);
            let msgs = window::window_pos_changed_msgs(user(env), &env.sys.objects, hwnd, flags);
            return send_all(env, msgs.into(), move |env| then(env, 0));
        }
        wm::SYSCOMMAND if wp & 0xfff0 == 0xf060 => {
            // SC_CLOSE
            return send(env, hwnd, wm::CLOSE, 0, 0, move |env, _| then(env, 0));
        }
        wm::SYSCOMMAND if wp & 0xfff0 == 0xf120 => {
            // SC_RESTORE
            return window::show_window(env, hwnd, window::SW_RESTORE, move |env, _| then(env, 0));
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
    let ctx = if let Some(w) = env
        .ext
        .get::<crate::Win32>()
        .user
        .windows
        .get_mut(&mut env.sys.objects, &hwnd)
    {
        super::update::clear(w);
        w.thread
    } else {
        return;
    };
    // Do not hide a validate -> re-invalidate transition between queue queries.
    queue_status_for(env, ctx);
}

fn call_window_proc_a(env: &mut Env<'_>) -> Outcome {
    let (proc, hwnd, msg, wp, lp) = (env.arg(0), env.arg(1), env.arg(2), env.arg(3), env.arg(4));
    window::call_window_proc(env, proc, hwnd, msg, wp, lp, false)
}

// ---- painting ----

fn end_paint(env: &mut Env<'_>) -> Outcome {
    crate::gdi32::raster::front::end_paint_value(env, env.arg(0), env.arg(1))
}

fn redraw_window(env: &mut Env<'_>) -> Outcome {
    let hwnd = env.arg(0);
    if let Some(w) = env
        .ext
        .get::<crate::Win32>()
        .user
        .windows
        .get_mut(&mut env.sys.objects, &hwnd)
    {
        w.invalidate_client(true);
    }
    Outcome::Return(1)
}

#[win32(user32, name = "GetMessageExtraInfo", level = state)]
fn get_message_extra_info(env: &mut Env<'_>) -> i32 {
    let context = env.context;
    user_mut(env).queue(context).extra_info
}
#[win32(user32, name = "SetMessageExtraInfo", level = state)]
fn set_message_extra_info(env: &mut Env<'_>, value: i32) -> i32 {
    let context = env.context;
    std::mem::replace(&mut user_mut(env).queue(context).extra_info, value)
}
pub fn register(reg: &mut Registry) {
    reg.register(def_window_proc_w_ABI);
    reg.register(get_message_extra_info_ABI);
    reg.register(set_message_extra_info_ABI);
    stdcall(reg, USER32, DEFAULT_PROC, 4, default_proc);
    let s = |reg: &mut Registry, name: &str, argc: u8, h| stdcall(reg, USER32, name, argc, h);
    s(reg, "PeekMessageA", 5, peek_message_a);
    reg.register(peek_message_w_ABI);
    s(reg, "GetMessageA", 4, get_message_a);
    reg.register(get_message_w_ABI);
    s(reg, "WaitMessage", 0, wait_message);
    s(reg, "GetQueueStatus", 1, get_queue_status);
    reg.register(post_message_a_ABI);
    reg.register(post_message_w_ABI);
    reg.register(post_thread_message_a_ABI);
    reg.register(post_thread_message_w_ABI);
    s(reg, "PostQuitMessage", 1, post_quit_message);
    s(reg, "SendMessageA", 4, send_message_a);
    reg.register(send_message_w_ABI);
    s(reg, "SendMessageTimeoutA", 7, send_message_timeout_a);
    reg.register(send_message_timeout_w_ABI);
    s(reg, "InSendMessage", 0, sent::in_send);
    s(reg, "ReplyMessage", 1, sent::reply_message);
    s(reg, "DispatchMessageA", 1, dispatch_message_a);
    reg.register(dispatch_message_w_ABI);
    s(reg, "TranslateMessage", 1, translate_message);
    s(reg, "GetMessageTime", 0, get_message_time);
    s(reg, "GetMessagePos", 0, get_message_pos);
    s(reg, "SetTimer", 4, set_timer);
    s(reg, "KillTimer", 2, kill_timer);
    s(reg, "DefWindowProcA", 4, def_window_proc_a);
    s(reg, "CallWindowProcA", 5, call_window_proc_a);
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
            input_key: None,
            character_wide: false,
            system: false,
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

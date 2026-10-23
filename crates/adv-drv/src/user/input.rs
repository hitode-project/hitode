//! Host events wait in the owning guest thread's queue. Its pipe is Wine's
//! queue fd: readability sets QS_DRIVER, and ProcessEvents sends hardware
//! input through win32u on that thread, as the Mac driver does.
use std::cell::Cell;
use std::collections::{HashMap, VecDeque};
use std::io::{PipeReader, PipeWriter, Read, Write};
use std::os::fd::AsRawFd;

use adv_abi::{Conv, Env, Outcome, Registry};
use adv_machine::{CtxId, Ext};
use adv_nt::{protocol::SetQueueFdRequest, server};

use super::{MODULE, win32u, window};
use crate::{free_slot, slot, write_words};

const QS_KEY: u32 = 1;
const QS_MOUSEMOVE: u32 = 2;
const QS_MOUSEBUTTON: u32 = 4;
const QS_ALLINPUT: u32 = 0x1cff;
/// NtUserCallHwndParam_SendHardwareInput from Wine's ntuser.h.
const SEND_HARDWARE_INPUT: u32 = 26;

/// The window and screen origin of the frame being presented, then the
/// last completed presentation. Geometry callbacks take shared extensions.
#[derive(Default)]
pub(super) struct Presentation {
    candidate: Cell<Option<(u32, (i32, i32))>>,
    last: Cell<Option<(u32, (i32, i32))>>,
}

/// Associate the next presentation with this window and its screen origin.
pub fn prepare(ext: &Ext, hwnd: u32, origin: (i32, i32)) {
    ext.get::<Presentation>()
        .candidate
        .set(Some((hwnd, origin)));
}

/// A frame has been presented using the recorded window geometry.
pub fn presented(ext: &Ext) {
    let presentation = ext.get::<Presentation>();
    presentation.last.set(presentation.candidate.get());
}

struct Input {
    hwnd: u32,
    mask: u32,
    /// i386 INPUT, including its type and largest union member.
    words: [u32; 7],
}

struct Queue {
    read: PipeReader,
    write: PipeWriter,
    events: VecDeque<Input>,
    processing: bool,
}

#[derive(Default)]
struct Queues(HashMap<CtxId, Queue>);

pub(super) fn register(registry: &mut Registry) {
    registry.add(MODULE, "ProcessEvents", Conv::Cdecl { argc: 1 }, process);
}

pub(super) fn init(env: &mut Env<'_>) -> Result<(), String> {
    if env
        .ext
        .get_or_default::<Queues>()
        .0
        .contains_key(&env.context)
    {
        return Ok(());
    }
    let (read, write) = std::io::pipe().map_err(|e| e.to_string())?;
    // set_queue_display_fd: make a server handle, store it in the thread's
    // queue, and close the temporary handle. The server keeps its own fd.
    let set = |env: &mut Env<'_>| {
        let handle = server::fd_to_handle(env, read.as_raw_fd(), 0x8010_0000)?;
        let mut request = SetQueueFdRequest {
            handle,
            ..Default::default()
        };
        let answer = server::request(env, &mut request, &[], &mut []);
        server::close(env, handle)?;
        adv_nt::status::check(answer?.0.header.error)
    };
    set(env).map_err(|e| format!("{MODULE}: {e:?}"))?;
    env.ext.get_mut::<Queues>().0.insert(
        env.context,
        Queue {
            read,
            write,
            events: VecDeque::new(),
            processing: false,
        },
    );
    Ok(())
}

/// Queue a host Win32 mouse or key message. Mouse points are relative to
/// the last presented frame, and become screen coordinates here.
pub fn post(ext: &mut Ext, message: u32, wparam: u32, lparam: u32) -> Result<(), String> {
    let Some((hwnd, (left, top))) = ext.get::<Presentation>().last.get() else {
        return Ok(());
    };
    let Some(window) = window(ext, hwnd) else {
        return Ok(());
    };
    let (mask, words) = match message {
        0x100 | 0x101 | 0x104 | 0x105 => {
            let scan = (lparam >> 16) & 0xff;
            let flags = ((lparam >> 24) & 1) | if message & 1 != 0 { 2 } else { 0 };
            (
                QS_KEY,
                [1, (wparam & 0xffff) | scan << 16, flags, 0, 0, 0, 0],
            )
        }
        0x200..=0x20a => {
            let (flag, data) = match message {
                0x200 => (0, 0),
                0x201 | 0x203 => (0x0002, 0),
                0x202 => (0x0004, 0),
                0x204 | 0x206 => (0x0008, 0),
                0x205 => (0x0010, 0),
                0x207 | 0x209 => (0x0020, 0),
                0x208 => (0x0040, 0),
                0x20a => (0x0800, (wparam as i32 >> 16) as u32),
                _ => unreachable!(),
            };
            let x = left + i32::from(lparam as u16 as i16);
            let y = top + i32::from((lparam >> 16) as u16 as i16);
            let mask = if message == 0x200 {
                QS_MOUSEMOVE
            } else {
                QS_MOUSEBUTTON
            };
            (mask, [0, x as u32, y as u32, data, flag | 0x8001, 0, 0])
        }
        _ => return Ok(()),
    };
    let queue = ext.get_mut::<Queues>().0.get_mut(&window.context).unwrap();
    // One byte marks a nonempty queue. Neither end can block: writes only
    // happen on empty queues, and reads only after their marker was written.
    if queue.events.is_empty() {
        queue.write.write_all(&[1]).map_err(|e| e.to_string())?;
    }
    queue.events.push_back(Input { hwnd, mask, words });
    Ok(())
}

fn process(env: &mut Env<'_>) -> Outcome {
    let mask = env.arg(0);
    let queue = env.ext.get_or_default::<Queues>().0.get_mut(&env.context);
    let Some(queue) = queue else {
        return Outcome::Return(0);
    };
    if queue.processing {
        return Outcome::Return(0);
    }
    queue.processing = true;
    next(env, mask)
}

fn next(env: &mut Env<'_>, mask: u32) -> Outcome {
    let queue = env.ext.get_mut::<Queues>().0.get_mut(&env.context).unwrap();
    let Some(index) = queue.events.iter().position(|input| input.mask & mask != 0) else {
        queue.processing = false;
        return Outcome::Return(u32::from(mask == QS_ALLINPUT && queue.events.is_empty()));
    };
    let input = queue.events.remove(index).unwrap();
    if queue.events.is_empty()
        && let Err(error) = queue.read.read_exact(&mut [0])
    {
        return Outcome::Fail(error.to_string());
    }
    let send = match win32u(env, "NtUserCallHwndParam") {
        Ok(send) => send,
        Err(error) => return Outcome::Fail(error),
    };
    let scratch = match slot(env) {
        Ok(scratch) => scratch,
        Err(error) => return Outcome::Fail(error),
    };
    // NtUserSendHardwareInput's inline wrapper uses these three words:
    // send_hardware_input_params { flags, input, lparam }.
    if let Err(error) = write_words(env, scratch, &input.words)
        .and_then(|()| write_words(env, scratch + 28, &[0, scratch, 0]))
    {
        return Outcome::Fail(error);
    }
    Outcome::call_aligned(
        env,
        send,
        [input.hwnd, scratch + 28, SEND_HARDWARE_INPUT],
        move |env, _| {
            free_slot(env, scratch);
            next(env, mask)
        },
    )
}

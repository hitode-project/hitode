//! The runtime's existing USER32 compatibility personality and diagnostic wrappers.
use crate::{Win32, util::set_last_error};
use adv_abi::{
    Conv, GuestPtr, Handle, Registry,
    types::{POINT, RECT},
    win32,
};
use adv_cpu::backend::GuestExt;
use adv_machine::{Env, Outcome};

pub fn register(registry: &mut Registry) {
    if std::env::var_os("ADV_PRINTF_DIAG_PATH").is_some() {
        registry.add(
            "user32.dll",
            "wsprintfA",
            Conv::CdeclVariadic { fixed: 2 },
            diagnostic_wsprintf,
        );
        registry.add(
            "user32.dll",
            "PeekMessageA",
            Conv::Stdcall { argc: 5 },
            diagnostic_peek,
        );
    }
    for api in [
        point_in_rect_ABI,
        open_icon_ABI,
        close_window_ABI,
        wait_objects_ABI,
    ] {
        registry.register(api);
    }
}

#[win32(user32, name = "MsgWaitForMultipleObjects", level = implement)]
fn wait_objects(
    e: &mut Env<'_>,
    count: u32,
    handles: GuestPtr<u32>,
    all: i32,
    timeout: u32,
    mask: u32,
) -> Outcome {
    if count > 63 {
        set_last_error(e, 87);
        return Outcome::Return(u32::MAX);
    }
    let mut values = Vec::new();
    for i in 0..count {
        let Ok(value) = e.cpu.read_u32(handles.addr().wrapping_add(i * 4)) else {
            set_last_error(e, 87);
            return Outcome::Return(u32::MAX);
        };
        if values.contains(&value) {
            set_last_error(e, 87);
            return Outcome::Return(u32::MAX);
        }
        values.push(value);
    }
    let deadline = (timeout != u32::MAX).then(|| e.sys.clock.now_us() + u64::from(timeout) * 1000);
    message_wait(e, values, all != 0, deadline, mask)
}

#[win32(user32, name = "PtInRect", level = implement)]
fn point_in_rect(e: &mut Env<'_>, rect: GuestPtr<RECT>, point: POINT) -> i32 {
    let Some(rect) = crate::user32::window::Rect::read(e.cpu, rect.addr()) else {
        return 0;
    };
    i32::from(
        point.x >= rect.left
            && point.x < rect.right
            && point.y >= rect.top
            && point.y < rect.bottom,
    )
}

#[win32(user32, name = "OpenIcon", level = state)]
fn open_icon(e: &mut Env<'_>, window: Handle) -> i32 {
    icon_state(e, window, false)
}
#[win32(user32, name = "CloseWindow", level = state)]
fn close_window(e: &mut Env<'_>, window: Handle) -> i32 {
    icon_state(e, window, true)
}
fn icon_state(e: &mut Env<'_>, window: Handle, close: bool) -> i32 {
    let user = &mut e.ext.get_mut::<Win32>().user;
    if let Some(window) = user.windows.get_mut(&mut e.sys.objects, &window.0) {
        if close {
            window.style |= 0x20000000;
        } else {
            window.style &= !0x20000000;
        }
        1
    } else {
        0
    }
}

fn diagnostic_wsprintf(e: &mut Env<'_>) -> Outcome {
    static ORIGINAL: std::sync::LazyLock<adv_machine::Handler> = std::sync::LazyLock::new(|| {
        let mut registry = Registry::default();
        crate::register(&mut registry);
        registry.get("user32.dll", "wsprintfA").unwrap().handler
    });
    let diagnostic = std::env::var_os("ADV_PRINTF_DIAG_PATH").map(|path| {
        use adv_cpu::backend::Reg;
        let mut args = crate::printf::StackArgs { cpu: e.cpu, addr: e.cpu.reg(Reg::Esp) + 12 };
        let format = crate::util::cstr(e.cpu, e.arg(1));
        let out = crate::printf::format(e.cpu, &e.ext.get::<Win32>().codec, &format, &mut args);
        let mut strings = Vec::new();
        for i in 2..5 {
            let pointer = e.arg(i);
            let bytes = e.cpu.read_cstr(pointer, crate::util::MAX_STR).unwrap_or_default();
            strings.push((pointer, bytes.len(), bytes.into_iter().take(64).collect::<Vec<_>>()));
        }
        let words: Vec<_> = std::env::var("ADV_PRINTF_DIAG_WORDS").unwrap_or_default().split(',').filter_map(|value| {
            let value = value.trim().trim_start_matches("0x");
            u32::from_str_radix(value, 16).ok().map(|address| (address, e.cpu.read_u32(address).ok()))
        }).collect();
        (path, format!("ctx={} caller={:#x} dst={:#x} fmt={:#x} format={:?} out_len={} out_prefix={:?} return_offset_bytes={:?} args={strings:?} words={words:?}", e.context, e.caller(), e.arg(0), e.arg(1), format, out.len(), &out[..out.len().min(64)], out.get(296..300)))
    });
    let result = ORIGINAL(e);
    if let Some((path, message)) = diagnostic {
        use std::io::Write;
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let returned = match &result {
                Outcome::Return(value) => Some(*value),
                _ => None,
            };
            let _ = writeln!(file, "{message} returned={returned:?}");
        }
    }
    result
}
fn diagnostic_peek(e: &mut Env<'_>) -> Outcome {
    static ORIGINAL: std::sync::LazyLock<adv_machine::Handler> = std::sync::LazyLock::new(|| {
        let mut registry = Registry::default();
        crate::register(&mut registry);
        registry.get("user32.dll", "PeekMessageA").unwrap().handler
    });
    let pointer = e.arg(0);
    let result = ORIGINAL(e);
    if matches!(result, Outcome::Return(1))
        && let Some(path) = std::env::var_os("ADV_PRINTF_DIAG_PATH")
    {
        use std::io::Write;
        let words: Vec<_> = (0..7)
            .map(|i| e.cpu.read_u32(pointer + i * 4).ok())
            .collect();
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = writeln!(
                file,
                "peek ctx={} frame={} caller={:#x} ptr={pointer:#x} msg_words={words:?}",
                e.context,
                e.sys.frame,
                e.caller()
            );
        }
    }
    result
}
fn message_wait(
    e: &mut Env<'_>,
    handles: Vec<u32>,
    all: bool,
    deadline: Option<u64>,
    mask: u32,
) -> Outcome {
    use adv_machine::{Object, Resume, Wait, WakeReason};
    let now = e.sys.clock.now_us();
    let ctx = e.context;
    for &h in &handles {
        if !matches!(
            e.sys.objects.get(h),
            Some(
                Object::Event { .. }
                    | Object::Mutex { .. }
                    | Object::Semaphore { .. }
                    | Object::Thread { .. }
            )
        ) {
            set_last_error(e, 6);
            return Outcome::Return(u32::MAX);
        }
    }
    let Some((input, notification)) = crate::user32::message::message_wait_events(e, mask) else {
        set_last_error(e, 8);
        return Outcome::Return(u32::MAX);
    };
    // The native fixture establishes input priority over ready handles for
    // MsgWait; ordinary WaitForMultipleObjects preserves handle-array order.
    let wait_handles: Vec<_> = std::iter::once(input)
        .chain(handles.iter().copied())
        .collect();
    let result = |index: u32, abandoned: bool| {
        if abandoned {
            0x80 + index - 1
        } else if all {
            0
        } else if index == 0 {
            handles.len() as u32
        } else {
            index - 1
        }
    };
    if let Some(WakeReason::Objects { index, abandoned }) =
        e.sys.try_acquire_objects(&wait_handles, all)
    {
        return Outcome::Return(result(index, abandoned));
    }
    let user = &mut e.ext.get_mut::<Win32>().user;
    let q = user.queue(ctx);
    let timer = if mask & 0x10 != 0 {
        // Old unread WM_TIMER input must not continuously wake at a deadline
        // already in the past. Removing it rearms the timer's next expiry.
        q.timers.iter().map(|t| t.due_us).filter(|&t| t > now).min()
    } else {
        None
    };
    if deadline.is_some_and(|t| now >= t) {
        return Outcome::Return(0x102);
    }
    let wait = Wait::Objects {
        handles: wait_handles,
        all,
        deadline: [deadline, timer].into_iter().flatten().min(),
        notification: Some(notification),
    };
    let count = handles.len() as u32;
    Outcome::block_timed(wait, move |e, wake| {
        match wake {
            // The object was already acquired when the wait became ready.
            // A later ResetEvent or waiter must not revoke/steal that grant.
            Resume::Woke(WakeReason::Objects { index, abandoned }) => {
                Outcome::Return(if abandoned {
                    0x80 + index - 1
                } else if all {
                    0
                } else if index == 0 {
                    count
                } else {
                    index - 1
                })
            }
            _ => message_wait(e, handles, all, deadline, mask),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use adv_cpu::JitCpu;
    use adv_cpu::backend::{Guest, Prot, asm::Asm};
    use adv_machine::{Clock, Machine, Object, RunResult};

    fn waiter(jit: bool, timeout: u32, event: bool) -> (Machine<JitCpu>, Option<u32>) {
        let mut registry = Registry::default();
        crate::register(&mut registry);
        register(&mut registry);
        let mut m = Machine::new(
            JitCpu::new(adv_machine::layout::PARK_ADDR, jit).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        Win32::install(
            &mut m,
            crate::Win32Config::new(".", "wait-test.exe", "target/wait-test"),
        )
        .unwrap();
        let code = 0x400000;
        let data = 0x500000;
        m.cpu.map(code, 4096, Prot::RX).unwrap();
        m.cpu.map(data, 4096, Prot::RW).unwrap();
        let event = event.then(|| {
            m.sys.objects.insert(Object::Event {
                manual_reset: false,
                signaled: true,
            })
        });
        if let Some(handle) = event {
            m.cpu.write_u32(data, handle).unwrap();
        }
        let wait = m
            .resolve_import("user32.dll", "MsgWaitForMultipleObjects")
            .unwrap();
        let exit = m.resolve_import("kernel32.dll", "ExitProcess").unwrap();
        let mut a = Asm::new(code);
        a.push_imm(1)
            .push_imm(timeout)
            .push_imm(0)
            .push_imm(data)
            .push_imm(u32::from(event.is_some()))
            .call(wait)
            .push_eax()
            .call(exit);
        m.cpu.write(code, &a.bytes).unwrap();
        m.spawn("wait", code, &[], 65536).unwrap();
        (m, event)
    }

    #[test]
    fn message_wait_sleeps_until_matching_input_without_busy_polling() {
        for jit in [false, true] {
            let (mut m, _) = waiter(jit, u32::MAX, false);
            assert!(matches!(
                m.run(),
                RunResult::Idle {
                    next_deadline: None
                }
            ));
            let calls = m.sys.host_calls;
            for _ in 0..3 {
                assert!(matches!(
                    m.run(),
                    RunResult::Idle {
                        next_deadline: None
                    }
                ));
            }
            assert_eq!(m.sys.host_calls, calls);
            let mut env = Env {
                context: m.sys.current,
                cpu: &mut m.cpu,
                sys: &mut m.sys,
                ext: &mut m.ext,
                thunk: 0,
            };
            assert!(crate::user32::message::post_input(&mut env, 0, 0x200, 0, 0));
            assert!(
                matches!(
                    m.run(),
                    RunResult::Idle {
                        next_deadline: None
                    }
                ),
                "mouse movement must not satisfy QS_KEY"
            );
            let mut env = Env {
                context: m.sys.current,
                cpu: &mut m.cpu,
                sys: &mut m.sys,
                ext: &mut m.ext,
                thunk: 0,
            };
            assert!(crate::user32::message::post_input(
                &mut env, 0, 0x100, 65, 0
            ));
            assert!(matches!(m.run(), RunResult::Exited(0)));
        }
    }

    #[test]
    fn message_wait_timeout_and_auto_reset_handle_follow_scheduler() {
        for jit in [false, true] {
            let (mut m, _) = waiter(jit, 7, false);
            let start = m.sys.clock.now_us();
            assert!(matches!(m.run_virtual(10_000), RunResult::Exited(0x102)));
            assert_eq!(m.sys.clock.now_us() - start, 7000);
            let (mut m, handle) = waiter(jit, u32::MAX, true);
            assert!(matches!(m.run(), RunResult::Exited(0)));
            assert!(matches!(
                m.sys.objects.get(handle.unwrap()),
                Some(Object::Event {
                    signaled: false,
                    ..
                })
            ));
        }
    }

    #[test]
    fn active_time_skip_advances_message_timeout_but_not_an_input_only_wait() {
        for jit in [false, true] {
            let (mut m, _) = waiter(jit, 7, false);
            m.set_time_skip_source(Some(std::sync::Arc::new(|| true)));
            let before = m.sys.clock.now_us();
            assert!(matches!(m.run(), RunResult::Exited(0x102)));
            assert_eq!(m.sys.clock.now_us() - before, 7000);
            assert_eq!(m.sys.scheduling.active_time_skips, 1);
            let (mut waiting, _) = waiter(jit, u32::MAX, false);
            waiting.set_time_skip_source(Some(std::sync::Arc::new(|| true)));
            assert!(matches!(
                waiting.run(),
                RunResult::Idle {
                    next_deadline: None
                }
            ));
            assert_eq!(waiting.sys.scheduling.active_time_skips, 0);
        }
    }
}

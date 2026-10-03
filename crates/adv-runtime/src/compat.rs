//! Small non-rendering compatibility services needed by the first generic ADV sample.
use adv_abi::Registry;
use adv_cpu::backend::GuestExt;
use adv_machine::{Conv, Env, Outcome};
use adv_win32::{Win32, util::set_last_error};

pub fn register(r: &mut Registry) {
    crate::mmio::register(r);
    crate::acm::register(r);
    crate::avi::register(r);
    if std::env::var_os("ADV_PRINTF_DIAG_PATH").is_some() {
        r.add(
            "user32.dll",
            "wsprintfA",
            Conv::CdeclVariadic { fixed: 2 },
            diagnostic_wsprintf,
        );
        r.add(
            "user32.dll",
            "PeekMessageA",
            Conv::Stdcall { argc: 5 },
            diagnostic_peek,
        );
    }
    for (module, name, argc) in [
        ("user32.dll", "CharUpperA", 1),
        ("user32.dll", "CharLowerA", 1),
        ("user32.dll", "PtInRect", 3),
        ("user32.dll", "OpenIcon", 1),
        ("user32.dll", "CloseWindow", 1),
        ("user32.dll", "MsgWaitForMultipleObjects", 5),
        ("imm32.dll", "ImmGetDefaultIMEWnd", 1),
        ("ole32.dll", "CoInitialize", 1),
        ("ole32.dll", "CoInitializeEx", 2),
        ("ole32.dll", "CoUninitialize", 0),
        ("ole32.dll", "CoCreateInstance", 5),
        ("dsound.dll", "#1", 3),
        ("dsound.dll", "DirectSoundCreate", 3),
        ("dsound.dll", "DirectSoundCreate8", 3),
        ("kernel32.dll", "CreateProcessA", 10),
    ] {
        r.add(module, name, Conv::Stdcall { argc }, call);
    }
}

fn diagnostic_wsprintf(e: &mut Env<'_>) -> Outcome {
    static ORIGINAL: std::sync::LazyLock<adv_machine::Handler> = std::sync::LazyLock::new(|| {
        let mut registry = Registry::default();
        adv_win32::register(&mut registry);
        registry.get("user32.dll", "wsprintfA").unwrap().handler
    });
    let diagnostic = std::env::var_os("ADV_PRINTF_DIAG_PATH").map(|path| {
        use adv_cpu::backend::Reg;
        let mut args = adv_win32::printf::StackArgs { cpu: e.cpu, addr: e.cpu.reg(Reg::Esp) + 12 };
        let format = adv_win32::util::cstr(e.cpu, e.arg(1));
        let out = adv_win32::printf::format(e.cpu, &e.ext.get::<Win32>().codec, &format, &mut args);
        let mut strings = Vec::new();
        for i in 2..5 {
            let pointer = e.arg(i);
            let bytes = e.cpu.read_cstr(pointer, adv_win32::util::MAX_STR).unwrap_or_default();
            strings.push((pointer, bytes.len(), bytes.into_iter().take(64).collect::<Vec<_>>()));
        }
        let words: Vec<_> = std::env::var("ADV_PRINTF_DIAG_WORDS").unwrap_or_default().split(',').filter_map(|value| {
            let value = value.trim().trim_start_matches("0x");
            u32::from_str_radix(value, 16).ok().map(|address| (address, e.cpu.read_u32(address).ok()))
        }).collect();
        (path, format!("ctx={} caller={:#x} dst={:#x} fmt={:#x} format={:?} out_len={} out_prefix={:?} return_offset_bytes={:?} args={strings:?} words={words:?}", e.sys.current, e.caller(), e.arg(0), e.arg(1), format, out.len(), &out[..out.len().min(64)], out.get(296..300)))
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
        adv_win32::register(&mut registry);
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
                e.sys.current,
                e.sys.frame,
                e.caller()
            );
        }
    }
    result
}
fn call(e: &mut Env<'_>) -> Outcome {
    let name = e.sys.thunks.get(e.thunk).name.clone();
    let a = e.arg(0);
    let value = match name.as_str() {
        "MsgWaitForMultipleObjects" => {
            let count = a;
            let p = e.arg(1);
            let all = e.arg(2) != 0;
            let timeout = e.arg(3);
            let mask = e.arg(4);
            if count > 63 {
                set_last_error(e, 87);
                return Outcome::Return(u32::MAX);
            }
            let mut handles = Vec::new();
            for i in 0..count {
                let Ok(h) = e.cpu.read_u32(p.wrapping_add(i * 4)) else {
                    set_last_error(e, 87);
                    return Outcome::Return(u32::MAX);
                };
                if handles.contains(&h) {
                    set_last_error(e, 87);
                    return Outcome::Return(u32::MAX);
                }
                handles.push(h);
            }
            let deadline =
                (timeout != u32::MAX).then(|| e.sys.clock.now_us() + u64::from(timeout) * 1000);
            return message_wait(e, handles, all, deadline, mask);
        }
        "CharUpperA" | "CharLowerA" => {
            let upper = name == "CharUpperA";
            if a >> 16 == 0 {
                // A packed DBCS character is already outside our ASCII case map.
                if a > 255 {
                    return Outcome::Return(a);
                }
                let c = a as u8;
                u32::from(if upper {
                    c.to_ascii_uppercase()
                } else {
                    c.to_ascii_lowercase()
                })
            } else {
                let Ok(mut bytes) = e.cpu.read_cstr(a, 16 * 1024 * 1024) else {
                    return Outcome::Fail("CharUpper/Lower: invalid string".into());
                };
                let codec = &e.ext.get::<Win32>().codec;
                // DBCS trail bytes must never be mistaken for ASCII letters.
                let mut i = 0;
                while i < bytes.len() {
                    if codec.is_lead_byte(0, bytes[i]) && i + 1 < bytes.len() {
                        i += 2;
                    } else {
                        bytes[i] = if upper {
                            bytes[i].to_ascii_uppercase()
                        } else {
                            bytes[i].to_ascii_lowercase()
                        };
                        i += 1;
                    }
                }
                if let Err(err) = e.cpu.write(a, &bytes) {
                    return Outcome::Fail(err.to_string());
                }
                a
            }
        }
        "PtInRect" => {
            let Some(r) = adv_win32::user32::window::Rect::read(e.cpu, a) else {
                return Outcome::Return(0);
            };
            let (x, y) = (e.arg(1) as i32, e.arg(2) as i32);
            u32::from(x >= r.left && x < r.right && y >= r.top && y < r.bottom)
        }
        "OpenIcon" | "CloseWindow" => {
            let user = &mut e.ext.get_mut::<Win32>().user;
            if let Some(w) = user.windows.get_mut(&a) {
                if name == "CloseWindow" {
                    w.style |= 0x20000000;
                } else {
                    w.style &= !0x20000000;
                }
                1
            } else {
                0
            }
        }
        "ImmGetDefaultIMEWnd" => 0,
        "CoInitialize" | "CoInitializeEx" => 0,
        "CoUninitialize" => 0,
        "CoCreateInstance" => {
            let out = e.arg(4);
            if let Err(err) = e.cpu.write_u32(out, 0) {
                return Outcome::Fail(err.to_string());
            }
            0x80040154
        }
        "#1" | "DirectSoundCreate" | "DirectSoundCreate8" => {
            let out = e.arg(1);
            if let Err(err) = e.cpu.write_u32(out, 0) {
                return Outcome::Fail(err.to_string());
            }
            0x88780078
        }
        "CreateProcessA" => {
            set_last_error(e, 120);
            0
        }
        _ => return Outcome::Fail(format!("unknown compatibility service {name}")),
    };
    Outcome::Return(value)
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
    let ctx = e.sys.current;
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
    let (input, notification) = adv_win32::user32::message::message_wait_events(e, mask);
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
    Outcome::block(wait, move |e, wake| {
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
        adv_win32::register(&mut registry);
        register(&mut registry);
        let mut m = Machine::new(
            JitCpu::new(adv_machine::layout::PARK_ADDR, jit).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        Win32::install(
            &mut m,
            adv_win32::Win32Config::new(".", "wait-test.exe", "target/wait-test"),
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
                cpu: &mut m.cpu,
                sys: &mut m.sys,
                ext: &mut m.ext,
                thunk: 0,
            };
            assert!(adv_win32::user32::message::post_input(
                &mut env, 0, 0x200, 0, 0
            ));
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
                cpu: &mut m.cpu,
                sys: &mut m.sys,
                ext: &mut m.ext,
                thunk: 0,
            };
            assert!(adv_win32::user32::message::post_input(
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
}

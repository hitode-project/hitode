//! Keeps a report of every panic in the session's debug folder, beside the
//! debugger's snapshots, whether or not the debugger runs. A panic a worker
//! recovers from, such as a failed compile, leaves the game running and
//! would otherwise show only on stderr. Nothing runs until a panic.
use std::path::PathBuf;
use std::sync::{Mutex, Once};

/// The folder reports go to and the session they describe.
static SESSION: Mutex<Option<(PathBuf, String)>> = Mutex::new(None);
/// Sanitized metadata of the latest panic in the current session, for anonymous reports.
static LAST_PANIC: Mutex<Option<(adv_report::HostPanic, Vec<String>)>> = Mutex::new(None);
/// Bounded history of panics observed during the current session (including recovered ones).
static SESSION_PANICS: Mutex<Vec<adv_report::HostPanic>> = Mutex::new(Vec::new());

/// Sends later panics' reports to this session's debug folder.
pub(crate) fn watch(options: &crate::Options, windowed: bool) {
    static HOOK: Once = Once::new();
    HOOK.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            previous(info);
            report(info);
        }));
    });
    let session = (
        options.data_dir.join("debug"),
        crate::debug_info(options, windowed),
    );
    *SESSION
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(session);
    *LAST_PANIC
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    SESSION_PANICS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
}

/// Returns and clears the latest sanitized host panic from the current run.
pub(crate) fn take_last_panic() -> Option<(adv_report::HostPanic, Vec<String>)> {
    LAST_PANIC
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()
}

/// Returns up to 4 sanitized panics seen during the current run without clearing them.
pub(crate) fn session_panics() -> Vec<adv_report::HostPanic> {
    SESSION_PANICS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

fn sanitize_location(location: &std::panic::Location<'_>) -> Option<String> {
    let normalized = location.file().replace('\\', "/");
    let relative = normalized
        .find("crates/")
        .map(|index| &normalized[index..])
        .or_else(|| normalized.find("apps/").map(|index| &normalized[index..]))?;
    if relative.contains("..") || !relative.bytes().all(|b| b.is_ascii_graphic()) {
        return None;
    }
    let formatted = format!("{relative}:{}", location.line());
    (formatted.len() <= 160).then_some(formatted)
}

fn sanitize_reason(info: &std::panic::PanicHookInfo<'_>) -> Option<String> {
    let raw = info
        .payload()
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| info.payload().downcast_ref::<String>().map(String::as_str))?;
    let cleaned: Vec<&str> = raw
        .split_whitespace()
        .filter(|word| !word.contains(['/', '\\']))
        .collect();
    if cleaned.is_empty() {
        return None;
    }
    let joined = cleaned.join(" ");
    let bounded: String = joined
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(200)
        .collect();
    let trimmed = bounded.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn sanitize_translating(context: Option<&str>) -> Option<String> {
    let stage = context?.split_whitespace().next()?;
    adv_report::is_safe_token(stage, 96).then(|| stage.to_owned())
}

/// Captures up to 12 relative offsets (`+0x...`) inside the current image so
/// stripped mobile release builds can be symbolicated offline against CI symbols.
#[cfg(all(unix, target_arch = "aarch64"))]
fn relative_host_frames() -> Vec<String> {
    let mut base_info: libc::Dl_info = unsafe { std::mem::zeroed() };
    let anchor = relative_host_frames as *const () as *const libc::c_void;
    if unsafe { libc::dladdr(anchor, &mut base_info) } == 0 || base_info.dli_fbase.is_null() {
        return Vec::new();
    }
    let image_base = base_info.dli_fbase as usize;
    let mut fp: usize;
    // SAFETY: reading the current frame pointer register x29 is side-effect free.
    unsafe {
        std::arch::asm!("mov {}, x29", out(reg) fp, options(nomem, nostack, preserves_flags));
    }
    let stack_lo = fp;
    let stack_hi = stack_lo.saturating_add(1024 * 1024);
    let mut frames = Vec::new();
    for _ in 0..32 {
        if !fp.is_multiple_of(8) || fp < stack_lo || fp + 16 > stack_hi {
            break;
        }
        // SAFETY: fp lies on the current thread's call stack inside valid frame records.
        let (next, lr) = unsafe { (*(fp as *const usize), *((fp + 8) as *const usize)) };
        if lr != 0 {
            let mut info: libc::Dl_info = unsafe { std::mem::zeroed() };
            if unsafe { libc::dladdr(lr as *const libc::c_void, &mut info) } != 0
                && info.dli_fbase as usize == image_base
                && lr >= image_base
            {
                frames.push(format!("+0x{:x}", lr - image_base));
                if frames.len() == 12 {
                    break;
                }
            }
        }
        if next <= fp {
            break;
        }
        fp = next;
    }
    frames
}

#[cfg(not(all(unix, target_arch = "aarch64")))]
fn relative_host_frames() -> Vec<String> {
    Vec::new()
}

fn report(info: &std::panic::PanicHookInfo<'_>) {
    let translating = adv_runtime::panic_context();
    if let Some(text) = &translating {
        eprint!("a64: {text}");
    }
    let thread = std::thread::current();
    let name = thread.name().unwrap_or("unnamed");
    let safe_thread: String = name
        .chars()
        .filter(|c| (c.is_ascii_graphic() || *c == ' ') && !matches!(c, '/' | '\\'))
        .take(64)
        .collect();
    let panic_meta = adv_report::HostPanic {
        thread: if safe_thread.is_empty() {
            "unnamed".into()
        } else {
            safe_thread
        },
        location: info.location().and_then(sanitize_location),
        reason: sanitize_reason(info),
        translating: sanitize_translating(translating.as_deref()),
    };
    let host_frames = relative_host_frames();
    if let Ok(mut list) = SESSION_PANICS.try_lock()
        && list.len() < 4
    {
        list.push(panic_meta.clone());
    }
    if let Ok(mut slot) = LAST_PANIC.try_lock() {
        *slot = Some((panic_meta, host_frames));
    }
    // A panic while the session is being replaced writes no report.
    let Ok(session) = SESSION.try_lock() else {
        return;
    };
    let Some((folder, session)) = session.as_ref() else {
        return;
    };
    let text = format!(
        "== info\n{session}\n\n== panic\nthread {name}\n{info}\n\n== translating\n{}\n== backtrace\n{}\n",
        translating.as_deref().unwrap_or("nothing\n"),
        std::backtrace::Backtrace::force_capture(),
    );
    let file: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .take(32)
        .collect();
    let path = folder.join(format!(
        "panic-{}-{file}.txt",
        crate::debug::unix_us() / 1000
    ));
    if std::fs::create_dir_all(folder)
        .and_then(|()| std::fs::write(&path, text))
        .is_ok()
    {
        eprintln!("panic report: {}", path.display());
    }
}

#[cfg(test)]
mod tests {
    // spec: WINE-UI-11
    #[test]
    fn a_panic_on_any_thread_leaves_a_report_in_the_debug_folder() {
        let data = std::env::temp_dir().join(format!("adv-panic-report-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&data);
        let options = crate::Options::new("game.exe".into(), data.clone());
        super::watch(&options, false);
        let worker = std::thread::Builder::new()
            .name("guest compile".into())
            .spawn(|| panic!("a compile failed at /secret/path/file.rs"))
            .unwrap();
        assert!(worker.join().is_err());
        let (panic_meta, _frames) = super::take_last_panic().expect("sanitized panic recorded");
        assert_eq!(panic_meta.thread, "guest compile");
        assert!(
            panic_meta
                .location
                .as_deref()
                .is_some_and(|loc| loc.starts_with("crates/adv-shell/src/panic_report.rs:")),
            "{panic_meta:?}"
        );
        assert_eq!(panic_meta.reason.as_deref(), Some("a compile failed at"));
        let reports: Vec<_> = std::fs::read_dir(data.join("debug"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.to_string_lossy().ends_with("-guest-compile.txt"))
            .collect();
        assert_eq!(reports.len(), 1, "{reports:?}");
        let text = std::fs::read_to_string(&reports[0]).unwrap();
        assert!(text.contains("thread guest compile"), "{text}");
        assert!(text.contains("a compile failed"), "{text}");
        assert!(text.contains("== translating\nnothing\n"), "{text}");
        assert!(
            text.contains(&format!("data_dir={}", data.display())),
            "{text}"
        );
        std::fs::remove_dir_all(data).unwrap();
    }
}

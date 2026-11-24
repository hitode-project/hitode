//! Keeps a report of every panic in the session's debug folder, beside the
//! debugger's snapshots, whether or not the debugger runs. A panic a worker
//! recovers from, such as a failed compile, leaves the game running and
//! would otherwise show only on stderr. Nothing runs until a panic.
use std::path::PathBuf;
use std::sync::{Mutex, Once};

/// The folder reports go to and the session they describe.
static SESSION: Mutex<Option<(PathBuf, String)>> = Mutex::new(None);

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
}

fn report(info: &std::panic::PanicHookInfo<'_>) {
    let translating = adv_runtime::panic_context();
    if let Some(text) = &translating {
        eprint!("a64: {text}");
    }
    // A panic while the session is being replaced writes no report.
    let Ok(session) = SESSION.try_lock() else {
        return;
    };
    let Some((folder, session)) = session.as_ref() else {
        return;
    };
    let thread = std::thread::current();
    let name = thread.name().unwrap_or("unnamed");
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
            .spawn(|| panic!("a compile failed"))
            .unwrap();
        assert!(worker.join().is_err());
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

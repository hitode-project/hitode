//! Process diagnostics are owned by the shell entry point, not the audio device.
use std::sync::Mutex;

struct ShellLogger {
    last_audio_warning: Mutex<Option<String>>,
}

static LOGGER: ShellLogger = ShellLogger {
    last_audio_warning: Mutex::new(None),
};

pub(crate) fn init() {
    // An embedding host owns both its logger and its level filter.
    if log::set_logger(&LOGGER).is_ok() {
        log::set_max_level(log::LevelFilter::Warn);
    }
}

/// Mobile apps have no console, so records at info and above go to the
/// system log: bionic's syslog writes to logcat, and iOS's to the unified
/// log. Call it first; an embedding host that has a logger keeps it.
#[cfg(mobile)]
pub fn init_system_log() {
    struct SystemLog;
    impl log::Log for SystemLog {
        fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
            metadata.level() <= log::Level::Info
        }

        fn log(&self, record: &log::Record<'_>) {
            if !self.enabled(record.metadata()) {
                return;
            }
            let priority = match record.level() {
                log::Level::Error => libc::LOG_ERR,
                log::Level::Warn => libc::LOG_WARNING,
                _ => libc::LOG_NOTICE,
            };
            let text = format!("{}: {}", record.target(), record.args()).replace('\0', " ");
            let text = std::ffi::CString::new(text).unwrap();
            // SAFETY: a constant format consuming one C string.
            unsafe { libc::syslog(priority, c"%s".as_ptr(), text.as_ptr()) };
        }

        fn flush(&self) {}
    }
    static LOG: SystemLog = SystemLog;
    if log::set_logger(&LOG).is_ok() {
        log::set_max_level(log::LevelFilter::Info);
    }
}

pub(crate) fn reset_audio_warning() {
    *LOGGER.last_audio_warning.lock().unwrap() = None;
}

impl log::Log for ShellLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::Level::Warn
    }

    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        if record.target() == "adv_device::device" && record.level() == log::Level::Warn {
            let message = record.args().to_string();
            let mut last = self.last_audio_warning.lock().unwrap();
            if last.as_ref() != Some(&message) {
                eprintln!("audio: {message}");
                *last = Some(message);
            }
        } else {
            eprintln!("{} {}: {}", record.level(), record.target(), record.args());
        }
    }

    fn flush(&self) {}
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    // A fresh process keeps the global logger out of other shell tests.
    fn child_process(test: &str) -> Option<std::process::Output> {
        const CHILD: &str = "ADV_SHELL_LOG_TEST_CHILD";
        if std::env::var(CHILD).is_ok_and(|value| value == test) {
            return None;
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                &format!("logging::tests::{test}"),
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD, test)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("1 passed"),
            "{stdout}{}",
            String::from_utf8_lossy(&output.stderr)
        );
        Some(output)
    }

    #[test]
    fn shell_logs_all_targets_without_starting_audio() {
        if let Some(output) = child_process("shell_logs_all_targets_without_starting_audio") {
            let stderr = String::from_utf8_lossy(&output.stderr);
            for (message, count) in [
                ("audio: audio warning", 2),
                ("WARN adv_dx::d3d9::shaders: shader warning", 2),
                ("ERROR adv_nt::loader: module error", 1),
                ("WARN adv_device::text::windows: font warning", 2),
                ("ERROR adv_device::device: audio error", 2),
                ("filtered info", 0),
            ] {
                assert_eq!(stderr.matches(message).count(), count, "{stderr}");
            }
            return;
        }

        // Test-harness arguments stop CLI parsing before any runtime/device starts.
        let _ = crate::run_cli();
        let _ = crate::run_cli();
        assert_eq!(log::max_level(), log::LevelFilter::Warn);
        log::warn!(target: "adv_device::device", "audio warning");
        log::warn!(target: "adv_device::device", "audio warning");
        log::warn!(target: "adv_dx::d3d9::shaders", "shader warning");
        log::warn!(target: "adv_dx::d3d9::shaders", "shader warning");
        log::error!(target: "adv_nt::loader", "module error");
        log::warn!(target: "adv_device::text::windows", "font warning");
        log::warn!(target: "adv_device::text::windows", "font warning");
        log::error!(target: "adv_device::device", "audio error");
        log::error!(target: "adv_device::device", "audio error");
        log::info!(target: "adv_dx::d3d9::shaders", "filtered info");
        log::warn!(target: "adv_device::device", "audio warning");
        super::reset_audio_warning();
        log::warn!(target: "adv_device::device", "audio warning");
        log::logger().flush();
    }

    struct EmbeddedLogger(AtomicUsize);

    impl log::Log for EmbeddedLogger {
        fn enabled(&self, _metadata: &log::Metadata<'_>) -> bool {
            true
        }

        fn log(&self, _record: &log::Record<'_>) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }

        fn flush(&self) {}
    }

    #[test]
    fn shell_preserves_embedded_logger_and_filter() {
        if child_process("shell_preserves_embedded_logger_and_filter").is_some() {
            return;
        }

        static EMBEDDED: EmbeddedLogger = EmbeddedLogger(AtomicUsize::new(0));
        log::set_logger(&EMBEDDED).unwrap();
        log::set_max_level(log::LevelFilter::Debug);
        log::debug!(target: "embedding", "before shell entry");
        let _ = crate::run_cli();
        let _ = crate::run_cli();
        assert_eq!(log::max_level(), log::LevelFilter::Debug);
        log::debug!(target: "embedding", "after shell entry");
        for _ in 0..2 {
            log::warn!(target: "adv_device::device", "audio warning");
            log::warn!(target: "adv_dx::d3d9::shaders", "shader warning");
        }
        assert_eq!(EMBEDDED.0.load(Ordering::Relaxed), 6);
    }
}

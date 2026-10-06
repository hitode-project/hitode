//! Best-effort priority for the one guest execution owner.
//! Call after creating neutral background services and compiler pools so those
//! threads do not inherit this preference. Dedicated compilers reset their own
//! nice value in adv-cpu. No affinity or realtime scheduling policy is changed.
//!
//! ADPF is deliberately absent: the current 8 ms execution quantum is a soft
//! interrupt budget, not a periodic frame deadline. Reporting every quantum as
//! a completed ADPF cycle would misrepresent idle waits, pause and guest work.

use serde_json::{Value, json};

const EXECUTION_NICE: i32 = -2;

/// Configure only the calling execution thread and return startup diagnostics.
/// Permission/OS failures leave execution enabled at its observed priority.
pub fn configure_execution_thread() -> Value {
    #[cfg(target_os = "android")]
    {
        // SAFETY: gettid returns the calling thread's kernel ID. Both resource
        // calls below target only that thread, not the process or other lanes.
        let tid = unsafe { libc::gettid() };
        let before = current_nice(tid);
        // -2 is Android's foreground nice level. We never request audio's
        // -16/-19 levels or sched_setscheduler's realtime policies.
        // SAFETY: tid is live on this calling thread and priority is in range.
        let result =
            unsafe { libc::setpriority(libc::PRIO_PROCESS, tid as libc::id_t, EXECUTION_NICE) };
        let error = (result != 0).then(|| std::io::Error::last_os_error().to_string());
        let after = current_nice(tid);
        json!({
            "policy":"best_effort_thread_nice",
            "tid":tid,
            "requested_nice":EXECUTION_NICE,
            "nice_before":before.as_ref().ok(),
            "nice_after":after.as_ref().ok(),
            "applied":result == 0 && after.as_ref().ok() == Some(&EXECUTION_NICE),
            "error":error,
            "query_error":after.err().or_else(|| before.err()).map(|error| error.to_string()),
            "cpu_affinity_changed":false,
            "adpf_enabled":false,
            "adpf_reason":"execution quantum is not a periodic work deadline"
        })
    }
    #[cfg(not(target_os = "android"))]
    {
        json!({"policy":"unsupported_platform", "requested_nice":EXECUTION_NICE,
               "applied":false, "cpu_affinity_changed":false, "adpf_enabled":false})
    }
}

#[cfg(target_os = "android")]
fn current_nice(tid: libc::pid_t) -> std::io::Result<i32> {
    // SAFETY: bionic errno is valid thread-local storage. Clear it because -1
    // is both a valid nice value and getpriority's error sentinel.
    let (priority, error) = unsafe {
        *libc::__errno() = 0;
        let priority = libc::getpriority(libc::PRIO_PROCESS, tid as libc::id_t);
        (priority, *libc::__errno())
    };
    if error == 0 {
        Ok(priority)
    } else {
        Err(std::io::Error::from_raw_os_error(error))
    }
}

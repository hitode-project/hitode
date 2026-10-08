//! Opt-in bounded native polling. The owner's existing AtomicBool is the
//! polling word: release stores request a yield, acquire loads observe it.
//! No timer owns or clears another producer's request.
use std::sync::OnceLock;

pub(crate) fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("ADV_SAFEPOINT").is_ok_and(|s| s == "poll"))
}

//! The engine-neutral guest interface plus the Cranelift engine behind it.
pub use adv_cpu_core::backend::*;

pub mod jit;
mod jit_engine;

/// Keep image metadata cheap while bounding snapshots waiting for the owner.
#[derive(Clone)]
pub(crate) struct CodeImageSink {
    sender: std::sync::mpsc::SyncSender<crate::AotImage>,
    bytes: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    rejected: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// Images sent since the receiver last looked: lets it skip the channel.
    sent: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl CodeImageSink {
    pub(crate) fn channel() -> (Self, std::sync::mpsc::Receiver<crate::AotImage>) {
        let (sender, receiver) = std::sync::mpsc::sync_channel(32);
        (
            Self {
                sender,
                bytes: Default::default(),
                rejected: Default::default(),
                sent: Default::default(),
            },
            receiver,
        )
    }

    pub(crate) fn try_send(&self, mut image: crate::AotImage) {
        use std::sync::atomic::Ordering;
        let bytes = image.image.len();
        // Reserve one maximum-sized snapshot for the main executable, which
        // loads after its initial DLL dependencies have registered their images.
        let limit = if image.primary { 32 } else { 24 } * 1024 * 1024;
        if bytes != 0
            && self
                .bytes
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                    used.checked_add(bytes).filter(|&total| total <= limit)
                })
                .is_err()
        {
            image.image.clear();
            image.image.shrink_to_fit();
            image.executable.clear();
            image.seeds.clear();
            self.rejected.fetch_add(1, Ordering::Relaxed);
        }
        match self.sender.try_send(image) {
            // After the send, so a receiver that sees the count sees the image.
            Ok(()) => drop(self.sent.fetch_add(1, Ordering::Release)),
            Err(
                std::sync::mpsc::TrySendError::Full(image)
                | std::sync::mpsc::TrySendError::Disconnected(image),
            ) => {
                self.release(image.image.len());
                self.rejected.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    pub(crate) fn release(&self, bytes: usize) {
        self.bytes
            .fetch_sub(bytes, std::sync::atomic::Ordering::AcqRel);
    }

    /// A plain load first: the common answer is zero and needs no locked swap.
    pub(crate) fn take_rejections(&self) -> u64 {
        use std::sync::atomic::Ordering;
        if self.rejected.load(Ordering::Relaxed) == 0 {
            return 0;
        }
        self.rejected.swap(0, Ordering::Relaxed)
    }

    /// Whether images may wait in the channel since the last call.
    pub(crate) fn take_sent(&self) -> bool {
        use std::sync::atomic::Ordering;
        self.sent.load(Ordering::Relaxed) != 0 && self.sent.swap(0, Ordering::Acquire) != 0
    }
}

//! Three slots: one owned by the writer, one by the reader, one atomic exchange.
//! Publishing replaces an unread frame; the writer never waits for presentation.
use std::cell::{Cell, UnsafeCell};
use std::marker::PhantomData;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
const DIRTY: usize = 4;
struct Shared<T> {
    slots: [UnsafeCell<T>; 3],
    middle: AtomicUsize,
}
// AcqRel exchanges transfer exclusive slot ownership. A consumer reference is
// tied to its &mut borrow, so another swap cannot occur while that reference lives.
unsafe impl<T: Send> Sync for Shared<T> {}
unsafe impl<T: Send> Send for Shared<T> {}
pub struct Producer<T> {
    shared: Arc<Shared<T>>,
    slot: usize,
    latest: Option<usize>,
    _single: PhantomData<Cell<()>>,
}
pub struct Consumer<T> {
    shared: Arc<Shared<T>>,
    slot: usize,
    _single: PhantomData<Cell<()>>,
}
/// Read-only scheduling view. It never clears the dirty bit or accesses a slot,
/// so a consumer can move to an owned background job while UI observes updates.
pub struct Observer<T> {
    shared: Arc<Shared<T>>,
}
impl<T> Clone for Observer<T> {
    fn clone(&self) -> Self {
        Self {
            shared: self.shared.clone(),
        }
    }
}
impl<T> Observer<T> {
    pub fn has_update(&self) -> bool {
        self.shared.middle.load(Ordering::Acquire) & DIRTY != 0
    }
}
pub fn channel<T>(slots: [T; 3]) -> (Producer<T>, Consumer<T>) {
    let shared = Arc::new(Shared {
        slots: slots.map(UnsafeCell::new),
        middle: AtomicUsize::new(2),
    });
    (
        Producer {
            shared: shared.clone(),
            slot: 0,
            latest: None,
            _single: PhantomData,
        },
        Consumer {
            shared,
            slot: 1,
            _single: PhantomData,
        },
    )
}
impl<T> Producer<T> {
    /// The closure fills the private slot, then publishes the complete value.
    /// Returns whether an unread value was replaced.
    pub fn publish(&mut self, fill: impl FnOnce(&mut T)) -> bool {
        fill(unsafe { &mut *self.shared.slots[self.slot].get() });
        self.latest = Some(self.slot);
        let old = self.shared.middle.swap(self.slot | DIRTY, Ordering::AcqRel);
        self.slot = old & !DIRTY;
        old & DIRTY != 0
    }
}
impl<T: Sync> Producer<T> {
    /// Diagnostic access to the last published value. The consumer never writes;
    /// the immutable borrow prevents this producer from recycling any slot.
    pub fn latest(&self) -> Option<&T> {
        self.latest
            .map(|slot| unsafe { &*self.shared.slots[slot].get() })
    }
}
impl<T> Consumer<T> {
    pub fn observer(&self) -> Observer<T> {
        Observer {
            shared: self.shared.clone(),
        }
    }
    pub fn has_update(&self) -> bool {
        self.shared.middle.load(Ordering::Acquire) & DIRTY != 0
    }
    pub fn update(&mut self) -> bool {
        if !self.has_update() {
            return false;
        }
        self.slot = self.shared.middle.swap(self.slot, Ordering::AcqRel) & !DIRTY;
        true
    }
    pub fn current(&self) -> &T {
        unsafe { &*self.shared.slots[self.slot].get() }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn concurrent_frames_are_complete_monotonic_and_latest_survives() {
        let (mut tx, mut rx) = channel([[0u64; 257]; 3]);
        let producer = std::thread::spawn(move || {
            for n in 1..=100_000 {
                tx.publish(|slot| slot.fill(n));
            }
        });
        let mut last = 0;
        while last != 100_000 {
            if rx.update() {
                let frame = rx.current();
                assert!(frame[0] >= last);
                assert!(frame.iter().all(|&x| x == frame[0]));
                last = frame[0];
            } else {
                std::thread::yield_now();
            }
        }
        producer.join().unwrap();
        assert!(!rx.update());
    }

    #[test]
    fn observer_does_not_consume_updates_and_covers_publication_during_a_job() {
        let (mut producer, mut consumer) = channel([0_u32; 3]);
        let observer = consumer.observer();
        let second = observer.clone();
        assert!(!observer.has_update());
        producer.publish(|slot| *slot = 1);
        for _ in 0..10 {
            assert!(observer.has_update());
        }
        // A failed enqueue/retry observes dirty without clearing the frame.
        producer.publish(|slot| *slot = 2);
        assert!(second.has_update());
        assert!(consumer.update());
        assert_eq!(*consumer.current(), 2);
        assert!(!observer.has_update());
        // Publication while the worker renders is still pending after its job
        // completes; UI's observer can schedule the next ownership transfer.
        producer.publish(|slot| *slot = 3);
        assert!(observer.has_update());
        assert_eq!(*consumer.current(), 2);
        assert!(consumer.update());
        assert_eq!(*consumer.current(), 3);
        assert!(!second.has_update());
    }

    #[test]
    fn observer_is_send_sync_without_requiring_clone_or_sync_payloads() {
        struct NotClone(std::cell::Cell<u32>);
        fn send_sync<T: Send + Sync>() {}
        send_sync::<Observer<NotClone>>();
        let (mut producer, consumer) =
            channel(std::array::from_fn(|_| NotClone(std::cell::Cell::new(0))));
        let observer = consumer.observer();
        let duplicate = observer.clone();
        producer.publish(|slot| slot.0.set(5));
        assert!(observer.has_update() && duplicate.has_update());
        assert!(consumer.has_update());
    }
}

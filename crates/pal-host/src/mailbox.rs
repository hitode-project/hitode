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
pub fn channel<T>(slots: [T; 3]) -> (Producer<T>, Consumer<T>) {
    let shared = Arc::new(Shared { slots: slots.map(UnsafeCell::new), middle: AtomicUsize::new(2) });
    (
        Producer { shared: shared.clone(), slot: 0, latest: None, _single: PhantomData },
        Consumer { shared, slot: 1, _single: PhantomData },
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
        self.latest.map(|slot| unsafe { &*self.shared.slots[slot].get() })
    }
}
impl<T> Consumer<T> {
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
}

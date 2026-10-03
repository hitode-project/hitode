//! Bounded single-producer/single-consumer ownership transfer.
use std::cell::{Cell, UnsafeCell};
use std::marker::PhantomData;
use std::mem::MaybeUninit;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

struct Shared<T> {
    slots: Box<[UnsafeCell<MaybeUninit<T>>]>,
    head: AtomicUsize,
    tail: AtomicUsize,
}
// A slot has exactly one owner. Release/Acquire transfers initialized values;
// neither endpoint is Clone or Sync, so its index cannot have concurrent writers.
unsafe impl<T: Send> Sync for Shared<T> {}
unsafe impl<T: Send> Send for Shared<T> {}
impl<T> Drop for Shared<T> {
    fn drop(&mut self) {
        let mut head = *self.head.get_mut();
        let tail = *self.tail.get_mut();
        while head != tail {
            unsafe {
                self.slots[head].get_mut().assume_init_drop();
            }
            head = (head + 1) % self.slots.len();
        }
    }
}
pub struct Producer<T> {
    shared: Arc<Shared<T>>,
    _single: PhantomData<Cell<()>>,
}
pub struct Consumer<T> {
    shared: Arc<Shared<T>>,
    _single: PhantomData<Cell<()>>,
}

pub fn channel<T>(capacity: usize) -> (Producer<T>, Consumer<T>) {
    assert!(capacity > 0 && capacity < usize::MAX);
    let shared = Arc::new(Shared {
        slots: (0..=capacity)
            .map(|_| UnsafeCell::new(MaybeUninit::uninit()))
            .collect(),
        head: AtomicUsize::new(0),
        tail: AtomicUsize::new(0),
    });
    (
        Producer {
            shared: shared.clone(),
            _single: PhantomData,
        },
        Consumer {
            shared,
            _single: PhantomData,
        },
    )
}
impl<T> Producer<T> {
    /// Never waits or allocates. On saturation ownership stays with the caller.
    pub fn push(&mut self, value: T) -> Result<(), T> {
        let tail = self.shared.tail.load(Ordering::Relaxed);
        let next = (tail + 1) % self.shared.slots.len();
        if next == self.shared.head.load(Ordering::Acquire) {
            return Err(value);
        }
        unsafe {
            (*self.shared.slots[tail].get()).write(value);
        }
        self.shared.tail.store(next, Ordering::Release);
        Ok(())
    }
    pub fn is_full(&self) -> bool {
        (self.shared.tail.load(Ordering::Relaxed) + 1) % self.shared.slots.len()
            == self.shared.head.load(Ordering::Acquire)
    }
    /// Free slots observed by the producer. The consumer can only increase this
    /// count before the next push, so it can conservatively reserve capacity.
    pub fn available(&self) -> usize {
        let tail = self.shared.tail.load(Ordering::Relaxed);
        let head = self.shared.head.load(Ordering::Acquire);
        if tail >= head {
            self.shared.slots.len() - (tail - head) - 1
        } else {
            head - tail - 1
        }
    }
}
impl<T> Consumer<T> {
    pub fn pop(&mut self) -> Option<T> {
        let head = self.shared.head.load(Ordering::Relaxed);
        if head == self.shared.tail.load(Ordering::Acquire) {
            return None;
        }
        let value = unsafe { (*self.shared.slots[head].get()).assume_init_read() };
        self.shared
            .head
            .store((head + 1) % self.shared.slots.len(), Ordering::Release);
        Some(value)
    }
    pub fn is_empty(&self) -> bool {
        self.shared.head.load(Ordering::Relaxed) == self.shared.tail.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn producer_capacity_reservation_survives_full_queue_and_wraparound() {
        let (mut tx, mut rx) = channel(3);
        assert_eq!(tx.available(), 3);
        for n in 0..24 {
            assert!(tx.available() >= 2);
            tx.push(n).unwrap();
            tx.push(n + 1).unwrap();
            assert_eq!(tx.available(), 1);
            tx.push(n + 2).unwrap();
            assert_eq!(tx.available(), 0);
            assert!(tx.push(999).is_err());
            for expected in n..n + 3 {
                assert_eq!(rx.pop(), Some(expected));
            }
            assert_eq!(tx.available(), 3);
        }
    }
    #[test]
    fn threaded_wraparound_preserves_order_and_drops_pending_values() {
        let (mut tx, mut rx) = channel(7);
        let producer = std::thread::spawn(move || {
            for i in 0..200_000 {
                let mut value = i;
                loop {
                    match tx.push(value) {
                        Ok(()) => break,
                        Err(v) => {
                            value = v;
                            std::thread::yield_now();
                        }
                    }
                }
            }
        });
        for expected in 0..200_000 {
            loop {
                if let Some(value) = rx.pop() {
                    assert_eq!(value, expected);
                    break;
                }
                std::thread::yield_now();
            }
        }
        producer.join().unwrap();
        let value = Arc::new(());
        let (mut tx, rx) = channel(1);
        tx.push(value.clone()).unwrap();
        assert!(tx.push(value.clone()).is_err());
        drop(tx);
        drop(rx);
        assert_eq!(Arc::strong_count(&value), 1);
    }
}

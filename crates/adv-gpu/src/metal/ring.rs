use super::*;

pub(crate) struct Ring {
    buffer: crate::Buffer,
    state: Arc<Mutex<State>>,
    completed: Object<dyn MTLSharedEvent>,
    _not_sync: std::marker::PhantomData<std::cell::Cell<()>>,
}

pub(super) struct State {
    capacity: u64,
    head: u64,
    tail: u64,
    reservations: VecDeque<Arc<Reservation>>,
}

struct Reservation {
    end: u64,
    status: Mutex<Status>,
}

#[derive(Default)]
struct Status {
    dropped: bool,
    last: Option<u64>,
}

pub(crate) struct Span {
    buffer: crate::Buffer,
    offset: u64,
    len: u64,
    reservation: Arc<Reservation>,
}

impl Device {
    pub(crate) fn ring(&self, size: u64) -> Result<crate::Ring> {
        if size == 0 {
            return Err(Error::Invalid("ring size must be nonzero".into()));
        }
        let buffer = self.create_buffer(&BufferDesc {
            size,
            memory: Memory::Upload,
            usage: BufferUsage::COPY_SRC
                | BufferUsage::VERTEX
                | BufferUsage::INDEX
                | BufferUsage::UNIFORM
                | BufferUsage::STORAGE,
            label: "upload ring",
        })?;
        let state = Arc::new(Mutex::new(State {
            capacity: size,
            head: 0,
            tail: 0,
            reservations: VecDeque::new(),
        }));
        self.0.rings.lock().unwrap().push(Arc::downgrade(&state));
        Ok(crate::Ring {
            inner: Ring {
                buffer,
                state,
                completed: self.0.event.clone(),
                _not_sync: std::marker::PhantomData,
            },
        })
    }
}

impl State {
    pub(super) fn submitted(&mut self, token: u64) {
        for reservation in &self.reservations {
            let mut status = reservation.status.lock().unwrap();
            if status.dropped && status.last.is_none() {
                status.last = Some(token);
            }
        }
    }
    pub(super) fn retire(&mut self, completed: u64) {
        while let Some(reservation) = self.reservations.front() {
            let status = reservation.status.lock().unwrap();
            if !status.dropped || status.last.is_none_or(|token| token > completed) {
                break;
            }
            self.tail = reservation.end;
            drop(status);
            self.reservations.pop_front();
        }
    }
    fn reserve(&mut self, size: u64, align: u64) -> Result<(u64, Arc<Reservation>)> {
        if align == 0 || !align.is_power_of_two() {
            return Err(Error::Invalid(
                "ring alignment must be a power of two".into(),
            ));
        }
        if size > self.capacity {
            return Err(Error::Busy);
        }
        let base = self.head / self.capacity * self.capacity;
        let offset = (self.head % self.capacity).next_multiple_of(align);
        let start = if offset + size > self.capacity {
            base + self.capacity
        } else {
            base + offset
        };
        let end = start + size;
        let tail = if self.reservations.is_empty() {
            start
        } else {
            self.tail
        };
        if end - tail > self.capacity {
            return Err(Error::Busy);
        }
        self.tail = tail;
        self.head = end;
        let reservation = Arc::new(Reservation {
            end,
            status: Mutex::new(Status::default()),
        });
        self.reservations.push_back(reservation.clone());
        Ok((start % self.capacity, reservation))
    }
}

impl Ring {
    pub(crate) fn reserve(&mut self, size: u64, align: u64) -> Result<crate::Span> {
        if let Some(reason) = self.buffer.inner.0.lost.get() {
            return Err(Error::DeviceLost(reason.clone()));
        }
        let mut state = self.state.lock().unwrap();
        state.retire(self.completed.signaledValue());
        let (offset, reservation) = state.reserve(size, align)?;
        Ok(crate::Span {
            inner: Span {
                buffer: self.buffer.clone(),
                offset,
                len: size,
                reservation,
            },
        })
    }
}
impl Span {
    pub(crate) fn bytes(&mut self) -> &mut [u8] {
        // The reservation owns a disjoint, mapped interval until retirement.
        unsafe {
            std::slice::from_raw_parts_mut(
                self.buffer
                    .inner
                    .0
                    .raw
                    .contents()
                    .as_ptr()
                    .cast::<u8>()
                    .add(self.offset as usize),
                self.len as usize,
            )
        }
    }
    pub(crate) fn buffer(&self) -> &crate::Buffer {
        &self.buffer
    }
    pub(crate) fn offset(&self) -> u64 {
        self.offset
    }
    pub(crate) fn len(&self) -> u64 {
        self.len
    }
    pub(super) fn submitted(&self, token: u64) {
        self.reservation.status.lock().unwrap().last = Some(token);
    }
}
impl Drop for Span {
    fn drop(&mut self) {
        self.reservation.status.lock().unwrap().dropped = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn state() -> State {
        State {
            capacity: 64,
            head: 0,
            tail: 0,
            reservations: VecDeque::new(),
        }
    }
    #[test]
    fn busy_preserves_the_reservation_cursor() {
        let mut state = state();
        let (_, first) = state.reserve(48, 16).unwrap();
        assert!(matches!(state.reserve(32, 16), Err(Error::Busy)));
        assert_eq!(state.head, 48);
        *first.status.lock().unwrap() = Status {
            dropped: true,
            last: Some(2),
        };
        state.retire(1);
        assert!(matches!(state.reserve(32, 16), Err(Error::Busy)));
        state.retire(2);
        assert_eq!(state.reserve(32, 16).unwrap().0, 0);
    }
    #[test]
    fn wrapping_aligns_offsets_when_capacity_is_not_a_power_of_two() {
        let mut state = State {
            capacity: 60,
            head: 48,
            tail: 48,
            reservations: VecDeque::new(),
        };
        assert_eq!(state.reserve(24, 16).unwrap().0, 0);
        assert_eq!(state.reserve(16, 16).unwrap().0, 32);
    }
    #[test]
    fn retirement_follows_reservations_and_the_last_submission() {
        let mut state = state();
        let (_, first) = state.reserve(16, 4).unwrap();
        let (_, second) = state.reserve(16, 4).unwrap();
        *first.status.lock().unwrap() = Status {
            dropped: true,
            last: Some(3),
        };
        *second.status.lock().unwrap() = Status {
            dropped: true,
            last: Some(1),
        };
        state.retire(2);
        assert_eq!(state.tail, 0);
        state.retire(3);
        assert_eq!(state.tail, 32);
        let (_, unused) = state.reserve(16, 4).unwrap();
        unused.status.lock().unwrap().dropped = true;
        state.retire(3);
        assert_eq!(state.tail, 32);
        state.submitted(4);
        state.retire(4);
        assert_eq!(state.tail, 48);
    }
}

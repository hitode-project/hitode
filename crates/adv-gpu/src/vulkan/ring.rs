use super::*;

pub(crate) struct Ring(
    pub(super) Arc<RingCore>,
    std::marker::PhantomData<std::cell::Cell<()>>,
);
pub(super) struct RingCore {
    pub(super) context: Arc<Context>,
    pub(super) buffer: crate::Buffer,
    pub(super) state: Mutex<RingState>,
}
pub(super) struct RingState {
    capacity: u64,
    head: u64,
    tail: u64,
    base_id: u64,
    unsubmitted_dropped: u32,
    spans: VecDeque<Reservation>,
}
struct Reservation {
    end: u64,
    dropped: bool,
    last: Option<u64>,
}
pub(crate) struct Span {
    ring: Arc<RingCore>,
    id: u64,
    offset: u64,
    len: u64,
    retained: bool,
}
impl Device {
    pub(crate) fn ring(&self, size: u64, capacity: u64) -> Result<(crate::Ring, crate::Buffer)> {
        let buffer = self.create_buffer(&BufferDesc {
            size: capacity,
            usage: BufferUsage::VERTEX
                | BufferUsage::INDEX
                | BufferUsage::UNIFORM
                | BufferUsage::STORAGE
                | BufferUsage::COPY_SRC
                | BufferUsage::INDIRECT
                | if size != capacity {
                    BufferUsage::COPY_DST
                } else {
                    BufferUsage::empty()
                },
            memory: Memory::Upload,
            label: "upload ring",
        })?;
        let ring = Arc::new(RingCore {
            context: self.0.clone(),
            buffer: buffer.clone(),
            state: Mutex::new(RingState {
                capacity: size,
                head: 0,
                tail: 0,
                base_id: 0,
                unsubmitted_dropped: 0,
                spans: VecDeque::new(),
            }),
        });
        self.0.rings.lock().unwrap().push(Arc::downgrade(&ring));
        Ok((
            crate::Ring {
                inner: Ring(ring, std::marker::PhantomData),
            },
            buffer,
        ))
    }
}
impl RingState {
    fn retire_locked(&mut self, completed: u64) {
        while let Some(span) = self.spans.front() {
            if !span.dropped || span.last.is_none_or(|t| t > completed) {
                break;
            }
            self.tail = span.end;
            self.spans.pop_front();
            self.base_id += 1;
        }
    }
    fn try_reserve(&mut self, size: u64, align: u64) -> Result<(u64, u64)> {
        if align == 0 || !align.is_power_of_two() {
            return Err(Error::Invalid(
                "ring alignment must be a power of two".into(),
            ));
        }
        let capacity = self.capacity;
        if size > capacity {
            return Err(Error::Busy);
        }
        let offset = (self.head % capacity).next_multiple_of(align);
        let start = if offset + size <= capacity {
            self.head - self.head % capacity + offset
        } else {
            self.head - self.head % capacity + capacity
        };
        // Empty rings may discard padding left at the end of the previous lap.
        let tail = if self.spans.is_empty() {
            start
        } else {
            self.tail
        };
        if start + size - tail > capacity {
            return Err(Error::Busy);
        }
        let end = start + size;
        let id = self.base_id + self.spans.len() as u64;
        self.head = end;
        self.tail = tail;
        self.spans.push_back(Reservation {
            end,
            dropped: false,
            last: None,
        });
        Ok((start % capacity, id))
    }
}
impl Ring {
    pub(crate) fn reserve(&mut self, size: u64, align: u64) -> Result<crate::Span> {
        self.0.context.check()?;
        let (offset, id) = {
            let mut state = self.0.state.lock().unwrap();
            match state.try_reserve(size, align) {
                Ok(reserved) => reserved,
                Err(Error::Busy) => {
                    let cached = self.0.context.completed.load(Ordering::Acquire);
                    state.retire_locked(cached);
                    match state.try_reserve(size, align) {
                        Ok(reserved) => reserved,
                        Err(Error::Busy) => {
                            // Release the ring lock before querying the GPU so
                            // Shared::poll cannot deadlock on RingCore::state.
                            drop(state);
                            let completed =
                                if let Some(completed) = self.0.context.poll_timeline()? {
                                    completed
                                } else if let Some(shared) =
                                    self.0.context.queue.get().and_then(Weak::upgrade)
                                {
                                    shared.query_completed_fences()?
                                } else {
                                    return Err(Error::Busy);
                                };
                            let mut state = self.0.state.lock().unwrap();
                            state.retire_locked(completed);
                            state.try_reserve(size, align)?
                        }
                        Err(err) => return Err(err),
                    }
                }
                Err(err) => return Err(err),
            }
        };
        Ok(crate::Span {
            inner: Span {
                ring: self.0.clone(),
                id,
                offset,
                len: size,
                retained: false,
            },
        })
    }
}
impl Span {
    pub(crate) fn bytes(&mut self) -> &mut [u8] {
        // SAFETY: reserve gives this span exclusive CPU access until it is retained.
        unsafe {
            std::slice::from_raw_parts_mut(
                self.ring
                    .buffer
                    .mapped()
                    .unwrap()
                    .as_ptr()
                    .cast::<u8>()
                    .add(self.offset as usize),
                self.len as usize,
            )
        }
    }
    pub(crate) fn buffer(&self) -> &crate::Buffer {
        &self.ring.buffer
    }
    pub(crate) fn offset(&self) -> u64 {
        self.offset
    }
    pub(crate) fn len(&self) -> u64 {
        self.len
    }
    pub(super) fn submit_batch(spans: &mut [Span], token: u64) {
        let mut i = 0;
        while i < spans.len() {
            let ring = spans[i].ring.clone();
            let mut state = ring.state.lock().unwrap();
            while i < spans.len() && Arc::ptr_eq(&spans[i].ring, &ring) {
                let index = (spans[i].id - state.base_id) as usize;
                let reservation = &mut state.spans[index];
                reservation.last = Some(token);
                reservation.dropped = true;
                spans[i].retained = true;
                i += 1;
            }
        }
    }
}
impl Drop for Span {
    fn drop(&mut self) {
        if !self.retained {
            let mut state = self.ring.state.lock().unwrap();
            let index = (self.id - state.base_id) as usize;
            let reservation = &mut state.spans[index];
            reservation.dropped = true;
            if reservation.last.is_none() {
                state.unsubmitted_dropped += 1;
            }
        }
    }
}
impl RingCore {
    pub(super) fn submit_unused(&self, token: u64) {
        let mut state = self.state.lock().unwrap();
        if state.unsubmitted_dropped == 0 {
            return;
        }
        for span in &mut state.spans {
            if span.dropped && span.last.is_none() {
                span.last = Some(token);
            }
        }
        state.unsubmitted_dropped = 0;
    }
    pub(super) fn retire(&self, completed: u64) {
        self.state.lock().unwrap().retire_locked(completed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> RingState {
        RingState {
            capacity: 64,
            head: 0,
            tail: 0,
            base_id: 0,
            unsubmitted_dropped: 0,
            spans: VecDeque::new(),
        }
    }

    #[test]
    fn tight_space_recovers_after_retiring_completed_spans() {
        let mut state = state();
        let (_, first) = state.try_reserve(48, 16).unwrap();
        assert!(matches!(state.try_reserve(32, 16), Err(Error::Busy)));
        let idx = (first - state.base_id) as usize;
        state.spans[idx].last = Some(2);
        state.spans[idx].dropped = true;
        state.retire_locked(1);
        assert!(matches!(state.try_reserve(32, 16), Err(Error::Busy)));
        state.retire_locked(2);
        assert_eq!(state.try_reserve(32, 16).unwrap().0, 0);
    }
}

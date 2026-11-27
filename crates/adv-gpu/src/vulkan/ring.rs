use super::*;

pub(crate) struct Ring(
    pub(super) Arc<RingCore>,
    std::marker::PhantomData<std::cell::Cell<()>>,
);
pub(super) struct RingCore {
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
impl Ring {
    pub(crate) fn reserve(&mut self, size: u64, align: u64) -> Result<crate::Span> {
        debug_assert!(align.is_power_of_two(), "ring alignment {align}");
        let mut state = self.0.state.lock().unwrap();
        let capacity = state.capacity;
        if size > capacity {
            return Err(Error::Busy);
        }
        let offset = (state.head % capacity).next_multiple_of(align);
        let start = if offset + size <= capacity {
            state.head - state.head % capacity + offset
        } else {
            state.head - state.head % capacity + capacity
        };
        // Empty rings may discard padding left at the end of the previous lap.
        let tail = if state.spans.is_empty() {
            start
        } else {
            state.tail
        };
        if start + size - tail > capacity {
            return Err(Error::Busy);
        }
        let end = start + size;
        let id = state.base_id + state.spans.len() as u64;
        state.head = end;
        state.tail = tail;
        state.spans.push_back(Reservation {
            end,
            dropped: false,
            last: None,
        });
        Ok(crate::Span {
            inner: Span {
                ring: self.0.clone(),
                id,
                offset: start % capacity,
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
        let mut state = self.state.lock().unwrap();
        while let Some(span) = state.spans.front() {
            if !span.dropped || span.last.is_none_or(|t| t > completed) {
                break;
            }
            state.tail = span.end;
            state.spans.pop_front();
            state.base_id += 1;
        }
    }
}

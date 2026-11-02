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
    head: u64,
    tail: u64,
    spans: VecDeque<Arc<SpanState>>,
}
pub(super) struct SpanState {
    end: u64,
    pub(super) status: Mutex<SpanStatus>,
}
pub(super) enum SpanStatus {
    Reserved,
    Unused,
    Submitted(u64),
}
pub(crate) struct Span {
    ring: Arc<RingCore>,
    pub(super) state: Arc<SpanState>,
    offset: u64,
    len: u64,
    retained: bool,
}
impl Device {
    pub(crate) fn ring(&self, size: u64) -> Result<crate::Ring> {
        let buffer = self.create_buffer(&BufferDesc {
            size,
            usage: BufferUsage::VERTEX
                | BufferUsage::INDEX
                | BufferUsage::UNIFORM
                | BufferUsage::STORAGE
                | BufferUsage::COPY_SRC
                | BufferUsage::INDIRECT,
            memory: Memory::Upload,
            label: "upload ring",
        })?;
        let ring = Arc::new(RingCore {
            buffer,
            state: Mutex::new(RingState {
                head: 0,
                tail: 0,
                spans: VecDeque::new(),
            }),
        });
        self.0.rings.lock().unwrap().push(Arc::downgrade(&ring));
        Ok(crate::Ring {
            inner: Ring(ring, std::marker::PhantomData),
        })
    }
}
impl Ring {
    pub(crate) fn reserve(&mut self, size: u64, align: u64) -> Result<crate::Span> {
        debug_assert!(align.is_power_of_two(), "ring alignment {align}");
        let capacity = self.0.buffer.desc().size;
        if size > capacity {
            return Err(Error::Busy);
        }
        let mut state = self.0.state.lock().unwrap();
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
        let span = Arc::new(SpanState {
            end,
            status: Mutex::new(SpanStatus::Reserved),
        });
        state.head = end;
        state.tail = tail;
        state.spans.push_back(span.clone());
        Ok(crate::Span {
            inner: Span {
                ring: self.0.clone(),
                state: span,
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
    pub(super) fn submitted(&mut self, token: u64) {
        *self.state.status.lock().unwrap() = SpanStatus::Submitted(token);
        self.retained = true;
    }
}
impl Drop for Span {
    fn drop(&mut self) {
        if !self.retained {
            *self.state.status.lock().unwrap() = SpanStatus::Unused;
        }
    }
}
impl RingCore {
    pub(super) fn submit_unused(&self, token: u64) {
        for span in &self.state.lock().unwrap().spans {
            let mut status = span.status.lock().unwrap();
            if matches!(*status, SpanStatus::Unused) {
                *status = SpanStatus::Submitted(token);
            }
        }
    }
    pub(super) fn retire(&self, completed: u64) {
        let mut state = self.state.lock().unwrap();
        while state.spans.front().is_some_and(
            |s| matches!(*s.status.lock().unwrap(),SpanStatus::Submitted(t) if t<=completed),
        ) {
            state.tail = state.spans.pop_front().unwrap().end;
        }
    }
}

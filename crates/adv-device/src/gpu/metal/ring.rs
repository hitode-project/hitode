use super::*;
pub(super) const CAPACITY: u64 = 64 * 1024 * 1024;
pub(super) const MAX_DRAW: u64 = 32 * 1024 * 1024 + program::UNIFORM_STRIDE + 256;
pub(super) struct Ring {
    pub(super) buffer: Object<dyn MTLBuffer>,
    pub(super) head: u64,
    tail: u64,
    waiting: Arc<AtomicBool>,
    completions: VecDeque<(u64, Arc<AtomicBool>)>,
}
impl Ring {
    pub(super) fn new(device: &ProtocolObject<dyn MTLDevice>) -> Result<Self, Error> {
        Ok(Self {
            buffer: device
                .newBufferWithLength_options(
                    CAPACITY as usize,
                    MTLResourceOptions::StorageModeShared,
                )
                .ok_or_else(|| Error::Unavailable("Metal shared ring allocation failed".into()))?,
            head: 0,
            tail: 0,
            waiting: Default::default(),
            completions: VecDeque::new(),
        })
    }
    pub(super) fn plan(&mut self, size: u64) -> Result<u64, Error> {
        // Arm before observing completions, so a Busy retry cannot lose its wake.
        self.waiting.store(true, Ordering::Release);
        while let Some((end, done)) = self.completions.front() {
            if !done.load(Ordering::Acquire) {
                break;
            }
            self.tail = *end;
            self.completions.pop_front();
        }
        let start = if self.head % CAPACITY + size > CAPACITY {
            self.head.next_multiple_of(CAPACITY)
        } else {
            self.head
        };
        if self.head == self.tail {
            self.head = start;
            self.tail = start;
        }
        if start + size - self.tail > CAPACITY {
            return Err(Error::Busy);
        }
        self.waiting.store(false, Ordering::Release);
        Ok(start)
    }
    pub(super) fn write(&self, offset: u64, bytes: &[u8]) {
        // The reservation excludes GPU-owned intervals; Shared storage is coherent.
        unsafe {
            std::ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                self.buffer
                    .contents()
                    .as_ptr()
                    .cast::<u8>()
                    .add(offset as usize),
                bytes.len(),
            );
        }
    }
    pub(super) fn equal(&self, offset: u64, block: &[u8]) -> bool {
        let bytes = unsafe {
            std::slice::from_raw_parts(
                self.buffer
                    .contents()
                    .as_ptr()
                    .cast::<u8>()
                    .add(offset as usize),
                block.len(),
            )
        };
        bytes == block
    }
    pub(super) fn submitted(
        &mut self,
        command: &ProtocolObject<dyn MTLCommandBuffer>,
        gate: Arc<submission::Gate>,
        failure: Arc<OnceLock<String>>,
    ) {
        let done = Arc::new(AtomicBool::new(false));
        self.completions.push_back((self.head, done.clone()));
        let waiting = self.waiting.clone();
        let block = block2::RcBlock::new(
            move |command: std::ptr::NonNull<ProtocolObject<dyn MTLCommandBuffer>>| {
                if let Some(error) = unsafe { command.as_ref().error() } {
                    let _ = failure.set(format!("Metal command failed: {error}"));
                }
                done.store(true, Ordering::Release);
                if waiting.swap(false, Ordering::AcqRel) {
                    gate.notify_execution();
                }
            },
        );
        unsafe {
            command.addCompletedHandler(block2::RcBlock::as_ptr(&block));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // spec: GFX-METAL-1, GFX-REC-7
    fn sparse_constants_still_reserve_a_complete_window_before_wrapping() {
        let gpu = pollster::block_on(Gpu::request_backend(false, GpuBackend::Metal)).unwrap();
        let state = State::new(&gpu.context).unwrap();
        let mut ring = Ring::new(&state.device).unwrap();
        ring.head = CAPACITY - 256;
        ring.tail = ring.head;
        let start = ring.plan(program::UNIFORM_STRIDE).unwrap();
        assert_eq!(start, CAPACITY);
        let base = start % CAPACITY;
        assert_eq!(base % 256, 0);
        assert!(base + 2 * std::mem::size_of::<ConstantBlock>() as u64 + 640 <= CAPACITY);
    }
}

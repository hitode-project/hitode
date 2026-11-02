use super::*;
use std::cell::Cell;
use std::marker::PhantomData;

pub(crate) struct Queue {
    device: crate::Device,
    raw: Object<dyn MTLCommandQueue>,
    event: Object<dyn MTLSharedEvent>,
    listener: Retained<MTLSharedEventListener>,
    next: u64,
    // The public queue has one submitting owner.
    _not_sync: PhantomData<Cell<()>>,
}

impl Device {
    pub(crate) fn open(options: &DeviceOptions) -> Result<(crate::Device, crate::Queue)> {
        let raw = MTLCreateSystemDefaultDevice()
            .ok_or_else(|| Error::Unavailable("no system Metal device".into()))?;
        let supported = if cfg!(target_os = "ios") {
            raw.supportsFamily(MTLGPUFamily::Apple5)
        } else {
            raw.supportsFamily(MTLGPUFamily::Apple7) || raw.supportsFamily(MTLGPUFamily::Mac2)
        };
        if !supported {
            return Err(Error::Unavailable(
                "Metal GPU family below the platform baseline".into(),
            ));
        }
        let caps = caps::query(&raw);
        let queue = raw.newCommandQueue().ok_or(Error::OutOfMemory)?;
        queue.setLabel(Some(&NSString::from_str(options.label)));
        let event = raw.newSharedEvent().ok_or(Error::OutOfMemory)?;
        let device = crate::Device {
            inner: Device(Arc::new(DeviceState {
                raw,
                caps,
                event: event.clone(),
                lost: Arc::new(OnceLock::new()),
                samplers: Mutex::new(HashMap::new()),
                rings: Mutex::new(Vec::new()),
                clears: Mutex::new(HashMap::new()),
            })),
        };
        let queue = crate::Queue {
            inner: Queue {
                device: device.clone(),
                raw: queue,
                event,
                listener: MTLSharedEventListener::new(),
                next: 1,
                _not_sync: PhantomData,
            },
        };
        Ok((device, queue))
    }
}

impl Queue {
    pub(crate) fn device(&self) -> &crate::Device {
        &self.device
    }
    pub(crate) fn encoder(&mut self, label: &'static str) -> crate::Encoder {
        // commandBuffer retains every referenced Metal resource until completion.
        let command = self
            .raw
            .commandBuffer()
            .expect("Metal command buffer allocation");
        command.setLabel(Some(&NSString::from_str(label)));
        crate::Encoder {
            inner: Encoder::new(self.device.inner.clone(), command),
        }
    }
    pub(crate) fn submit(&mut self, encoder: crate::Encoder) -> Result<Token> {
        self.device.inner.check()?;
        let token = Token(self.next);
        self.next += 1;
        for span in encoder.inner.spans {
            span.inner.submitted(token.0);
        }
        self.rings(|ring| ring.submitted(token.0));
        let lost = self.device.inner.0.lost.clone();
        let event = self.event.clone();
        let block = block2::RcBlock::new(
            move |command: NonNull<ProtocolObject<dyn MTLCommandBuffer>>| {
                if let Some(error) = unsafe { command.as_ref() }.error() {
                    // Faulted work cannot signal its encoded event. Publish the loss
                    // before waking shared-event waiters so they return DeviceLost.
                    if matches!(error.code(), 1..=4 | 11) {
                        let _ = lost.set(format!("Metal command failed: {error}"));
                    }
                    event.setSignaledValue(event.signaledValue().max(token.0));
                }
            },
        );
        unsafe {
            encoder
                .inner
                .command
                .addCompletedHandler(block2::RcBlock::as_ptr(&block));
            encoder
                .inner
                .command
                .encodeSignalEvent_value(ProtocolObject::from_ref(&*self.event), token.0);
        }
        encoder.inner.command.commit();
        Ok(token)
    }
    fn rings(&self, mut f: impl FnMut(&mut ring::State)) {
        self.device.inner.0.rings.lock().unwrap().retain(|weak| {
            if let Some(ring) = weak.upgrade() {
                f(&mut ring.lock().unwrap());
                true
            } else {
                false
            }
        });
    }
    pub(crate) fn poll(&mut self) -> Result<Token> {
        let completed = self.event.signaledValue();
        self.rings(|ring| ring.retire(completed));
        self.device.inner.check()?;
        Ok(Token(completed))
    }
    pub(crate) fn wait(&mut self, token: Token, timeout: Duration) -> Result<bool> {
        self.device.inner.check()?;
        if self.event.signaledValue() < token.0 {
            let (sender, receiver) = std::sync::mpsc::sync_channel(1);
            self.on_complete(
                token,
                Box::new(move || {
                    let _ = sender.send(());
                }),
            );
            let _ = receiver.recv_timeout(timeout);
        }
        Ok(self.poll()? >= token)
    }
    pub(crate) fn on_complete(&mut self, token: Token, callback: Box<dyn FnOnce() + Send>) {
        let callback = Mutex::new(Some(callback));
        let block = block2::RcBlock::new(
            move |_: NonNull<ProtocolObject<dyn MTLSharedEvent>>, _: u64| {
                if let Some(callback) = callback.lock().unwrap().take() {
                    callback();
                }
            },
        );
        unsafe {
            self.event.notifyListener_atValue_block(
                &self.listener,
                token.0,
                block2::RcBlock::as_ptr(&block),
            );
        }
    }
    pub(crate) fn export_fence(&mut self, _token: Token) -> Result<crate::queue::Fence> {
        todo!("B3")
    }
    pub(crate) fn wait_fence(&mut self, _fence: crate::queue::Fence) -> Result<()> {
        todo!("B3")
    }
}

#[derive(Clone)]
pub(crate) struct QuerySet(pub(super) Arc<QueryData>);
pub(super) enum QueryData {
    Timestamp {
        raw: Object<dyn MTLCounterSampleBuffer>,
        period: f64,
        /// Indices whose pass ended without work. Apple GPUs sample counters
        /// at stage boundaries only, so such a pass writes nothing; resolves
        /// report it as zero duration instead (see `RenderPassDesc::timestamps`).
        empty: Mutex<Vec<bool>>,
    },
    Occlusion(Object<dyn MTLBuffer>),
}
impl Device {
    pub(crate) fn create_query_set(
        &self,
        kind: QueryKind,
        count: u32,
    ) -> Result<crate::queue::QuerySet> {
        self.check()?;
        let data = match kind {
            QueryKind::Occlusion => QueryData::Occlusion(
                self.0
                    .raw
                    .newBufferWithLength_options(
                        count as usize * 8,
                        MTLResourceOptions::StorageModePrivate
                            | MTLResourceOptions::HazardTrackingModeTracked,
                    )
                    .ok_or(Error::OutOfMemory)?,
            ),
            QueryKind::Timestamp => {
                if !self.caps().features.contains(Features::TIMESTAMPS) {
                    return Err(Error::Invalid(
                        "stage-boundary timestamps unavailable".into(),
                    ));
                }
                let d = MTLCounterSampleBufferDescriptor::new();
                d.setCounterSet(caps::timestamp_counter(&self.0.raw).as_deref());
                d.setStorageMode(MTLStorageMode::Private);
                unsafe {
                    d.setSampleCount(count as usize);
                }
                let raw = self
                    .0
                    .raw
                    .newCounterSampleBufferWithDescriptor_error(&d)
                    .map_err(|e| Error::Invalid(e.to_string()))?;
                let period = if self.0.raw.supportsFamily(MTLGPUFamily::Apple5) {
                    1.0
                } else {
                    // Immediate-mode GPUs have a separate timestamp clock. CPU
                    // samples are nanoseconds, as specified by sampleTimestamps.
                    let (mut cpu0, mut gpu0, mut cpu1, mut gpu1) = (0, 0, 0, 0);
                    unsafe {
                        self.0.raw.sampleTimestamps_gpuTimestamp(
                            NonNull::from(&mut cpu0),
                            NonNull::from(&mut gpu0),
                        );
                    }
                    std::thread::sleep(Duration::from_millis(1));
                    unsafe {
                        self.0.raw.sampleTimestamps_gpuTimestamp(
                            NonNull::from(&mut cpu1),
                            NonNull::from(&mut gpu1),
                        );
                    }
                    (cpu1 - cpu0) as f64 / (gpu1 - gpu0) as f64
                };
                QueryData::Timestamp {
                    raw,
                    period,
                    empty: Mutex::new(vec![false; count as usize]),
                }
            }
        };
        Ok(crate::queue::QuerySet {
            inner: QuerySet(Arc::new(data)),
        })
    }
}
impl QueryData {
    /// Record whether the pass that owns timestamps `first` and `first + 1`
    /// ended without work.
    pub(super) fn mark(&self, first: u32, empty: bool) {
        if let Self::Timestamp { empty: flags, .. } = self {
            let mut flags = flags.lock().unwrap();
            flags[first as usize] = empty;
            flags[first as usize + 1] = empty;
        }
    }
}
impl QuerySet {
    pub(crate) fn period(&self) -> f64 {
        match &*self.0 {
            QueryData::Timestamp { period, .. } => *period,
            QueryData::Occlusion(_) => 1.0,
        }
    }
}

// Query allocation handles are immutable; the queue alone encodes their writes.
unsafe impl Send for QueryData {}
unsafe impl Sync for QueryData {}

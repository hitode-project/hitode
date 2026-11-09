use super::*;
use std::sync::mpsc;
use std::time::Duration;

pub(crate) struct Queue {
    device: crate::Device,
    _owner: std::marker::PhantomData<std::cell::Cell<()>>,
    pub(super) shared: Arc<Shared>,
    sender: Option<mpsc::Sender<Message>>,
    waiter: Option<std::thread::JoinHandle<()>>,
}
pub(super) struct Shared {
    pub(super) context: Arc<Context>,
    handle: vk::Queue,
    timeline: vk::Semaphore,
    pub(super) state: Mutex<State>,
    pub(super) descriptors: Mutex<Vec<binding::DescriptorArena>>,
}
pub(super) struct State {
    next: u64,
    completed: u64,
    pending: VecDeque<Submission>,
    callbacks: Vec<(u64, Callback)>,
    pub(super) pools: Vec<encode::Commands>,
    fences: Vec<vk::Fence>,
    /// The fence the waiter thread blocks on. Fences are externally
    /// synchronized, so a retire of it leaves the reset to the waiter.
    waiting: vk::Fence,
    retired: bool,
    #[cfg(target_os = "android")]
    sync: present::Semaphores,
}
struct Submission {
    token: u64,
    fence: vk::Fence,
    commands: encode::Commands,
    _resources: encode::Resources,
}
enum Message {
    Wake,
    Stop,
}
type Callback = Box<dyn FnOnce() + Send>;
impl Queue {
    pub(super) fn new(device: crate::Device) -> Result<Self> {
        let context = device.inner.0.clone();
        let timeline = if context.caps.modern.contains(Modern::TIMELINE_SEMAPHORES) {
            let mut ty =
                vk::SemaphoreTypeCreateInfo::default().semaphore_type(vk::SemaphoreType::TIMELINE);
            context.result(unsafe {
                context
                    .device
                    .create_semaphore(&vk::SemaphoreCreateInfo::default().push_next(&mut ty), None)
            })?
        } else {
            vk::Semaphore::null()
        };
        let shared = Arc::new(Shared {
            handle: unsafe { context.device.get_device_queue(context.family, 0) },
            context,
            timeline,
            descriptors: Mutex::new(Vec::new()),
            state: Mutex::new(State {
                next: 1,
                completed: 0,
                pending: VecDeque::new(),
                callbacks: Vec::new(),
                pools: Vec::new(),
                fences: Vec::new(),
                waiting: vk::Fence::null(),
                retired: false,
                #[cfg(target_os = "android")]
                sync: present::Semaphores::default(),
            }),
        });
        let (sender, receiver) = mpsc::channel();
        let backend = shared.clone();
        let waiter = std::thread::Builder::new()
            .name("adv-gpu waiter".into())
            .spawn(move || {
                // Sleeps on the GPU, never on a timer: blocked on the oldest
                // pending submission, or on messages when none is pending.
                let mut ready = Vec::new();
                'waiter: loop {
                    if !backend.dispatch_callbacks(&mut ready) {
                        return;
                    }
                    loop {
                        match receiver.try_recv() {
                            Ok(Message::Wake) => {}
                            Ok(Message::Stop) | Err(mpsc::TryRecvError::Disconnected) => {
                                break 'waiter;
                            }
                            Err(mpsc::TryRecvError::Empty) => break,
                        }
                    }
                    if !backend.wait_oldest() && !matches!(receiver.recv(), Ok(Message::Wake)) {
                        break;
                    }
                }
                if backend
                    .context
                    .result(unsafe { backend.context.device.device_wait_idle() })
                    .is_ok()
                {
                    backend.dispatch_callbacks(&mut ready);
                }
                backend.cancel_callbacks();
            })
            .map_err(|e| Error::Unavailable(format!("GPU waiter thread: {e}")))?;
        Ok(Self {
            _owner: std::marker::PhantomData,
            device,
            shared,
            sender: Some(sender),
            waiter: Some(waiter),
        })
    }
    pub(crate) fn device(&self) -> &crate::Device {
        &self.device
    }
    pub(crate) fn encoder(&mut self, _label: &'static str) -> crate::Encoder {
        crate::Encoder {
            inner: Encoder::new(self.shared.clone()),
        }
    }
    pub(crate) fn submit(&mut self, encoder: crate::Encoder) -> Result<Token> {
        Ok(self.submit_present(encoder, &[])?.0)
    }
    /// Releases each buffer to the compositor and exports the same batch's
    /// binary semaphore once; the shared pool duplicates its fd per image.
    pub(crate) fn submit_present(
        &mut self,
        mut encoder: crate::Encoder,
        buffers: &[(&super::Image, &super::PresentBuffer)],
    ) -> Result<(Token, Option<super::Fence>)> {
        let context = &self.shared.context;
        context.check()?;
        if let Some(error) = encoder.inner.error.take() {
            return Err(error);
        }
        encoder.inner.memory_barrier();
        #[cfg(target_os = "android")]
        {
            for (image, _) in buffers {
                encoder.inner.release(&image.core);
            }
            encoder.inner.release_borrowed();
        }
        #[cfg(not(target_os = "android"))]
        let _ = buffers;
        context.result(unsafe { context.device.end_command_buffer(encoder.inner.command) })?;
        let mut state = self.shared.state.lock().unwrap();
        let token = state.next;
        let fence = if self.shared.timeline == vk::Semaphore::null() {
            match state.fences.pop() {
                Some(f) => f,
                None => context.result(unsafe {
                    context
                        .device
                        .create_fence(&vk::FenceCreateInfo::default(), None)
                })?,
            }
        } else {
            vk::Fence::null()
        };
        #[cfg(target_os = "android")]
        let signal = if buffers.is_empty() {
            vk::Semaphore::null()
        } else {
            match context.result(state.sync.semaphore(context)) {
                Ok(semaphore) => semaphore,
                Err(error) => {
                    if fence != vk::Fence::null() {
                        state.fences.push(fence);
                    }
                    return Err(error);
                }
            }
        };
        let mut commands = std::mem::take(&mut encoder.inner.commands);
        commands.push(encoder.inner.command);
        let mut semaphores = arrayvec::ArrayVec::<_, 2>::new();
        let mut values = arrayvec::ArrayVec::<_, 2>::new();
        if self.shared.timeline != vk::Semaphore::null() {
            semaphores.push(self.shared.timeline);
            values.push(token);
        }
        #[cfg(target_os = "android")]
        if signal != vk::Semaphore::null() {
            semaphores.push(signal);
            values.push(0);
        }
        let mut timeline_info =
            vk::TimelineSemaphoreSubmitInfo::default().signal_semaphore_values(&values);
        let mut submit = vk::SubmitInfo::default()
            .command_buffers(&commands)
            .signal_semaphores(&semaphores);
        if self.shared.timeline != vk::Semaphore::null() {
            submit = submit.push_next(&mut timeline_info);
        }
        #[cfg(target_os = "android")]
        {
            submit = submit
                .wait_semaphores(&state.sync.waits)
                .wait_dst_stage_mask(&state.sync.stages);
        }
        if let Err(error) = context.result(unsafe {
            context
                .device
                .queue_submit(self.shared.handle, &[submit], fence)
        }) {
            if fence != vk::Fence::null() {
                state.fences.push(fence);
            }
            #[cfg(target_os = "android")]
            if signal != vk::Semaphore::null() {
                state.sync.free.push(signal);
            }
            commands.pop();
            encoder.inner.commands = commands;
            return Err(error);
        }
        for span in &mut encoder.inner.resources.spans {
            span.submitted(token);
        }
        context.rings.lock().unwrap().retain(|weak| {
            if let Some(ring) = weak.upgrade() {
                ring.submit_unused(token);
                true
            } else {
                false
            }
        });
        let resources = std::mem::take(&mut encoder.inner.resources);
        state.pending.push_back(Submission {
            token,
            fence,
            commands: encoder.inner.pool.take().unwrap(),
            _resources: resources,
        });
        state.next += 1;
        context.next.store(state.next, Ordering::Release);
        context.open.fetch_sub(1, Ordering::Release);
        encoder.inner.submitted = true;
        let _ = self.sender.as_ref().unwrap().send(Message::Wake);
        #[cfg(target_os = "android")]
        let acquire = {
            state.sync.submitted(token);
            if signal != vk::Semaphore::null() {
                state.sync.export(context, signal, token)?
            } else {
                None
            }
        };
        #[cfg(not(target_os = "android"))]
        let acquire = None;
        Ok((Token(token), acquire))
    }
    pub(crate) fn poll(&mut self) -> Result<Token> {
        self.shared.poll()
    }
    pub(crate) fn wait(&mut self, token: Token, timeout: Duration) -> Result<bool> {
        self.shared.context.check()?;
        let state = self.shared.state.lock().unwrap();
        if token.0 <= state.completed {
            return Ok(true);
        }
        if token.0 >= state.next {
            return Err(Error::Invalid("unknown submission token".into()));
        }
        let timeout = timeout.as_nanos().min(u64::MAX as u128) as u64;
        let context = &self.shared.context;
        let result = unsafe {
            if self.shared.timeline != vk::Semaphore::null() {
                let semaphores = [self.shared.timeline];
                let values = [token.0];
                let info = vk::SemaphoreWaitInfo::default()
                    .semaphores(&semaphores)
                    .values(&values);
                if context.api >= vk::API_VERSION_1_2 {
                    context.device.wait_semaphores(&info, timeout)
                } else {
                    context.timeline.wait_semaphores(&info, timeout)
                }
            } else {
                let fence = state
                    .pending
                    .iter()
                    .find(|s| s.token == token.0)
                    .unwrap()
                    .fence;
                context.device.wait_for_fences(&[fence], true, timeout)
            }
        };
        drop(state);
        if result == Err(vk::Result::TIMEOUT) {
            return Ok(false);
        }
        context.result(result)?;
        self.shared.poll()?;
        Ok(true)
    }
    pub(crate) fn on_complete(&mut self, token: Token, callback: Callback) {
        let mut state = self.shared.state.lock().unwrap();
        if self.shared.context.check().is_err() {
            return;
        }
        if token.0 <= state.completed {
            // The waiter may already be asleep on a later submission.
            drop(state);
            callback();
        } else {
            state.callbacks.push((token.0, callback));
            drop(state);
            let _ = self.sender.as_ref().unwrap().send(Message::Wake);
        }
    }
    /// Makes the next submission wait on a compositor release fence.
    pub(crate) fn wait_next(&mut self, fence: super::Fence) -> Result<()> {
        #[cfg(target_os = "android")]
        return self
            .shared
            .state
            .lock()
            .unwrap()
            .sync
            .wait_next(&self.shared.context, fence);
        #[cfg(not(target_os = "android"))]
        match fence {}
    }
}
impl Shared {
    fn dispatch_callbacks(&self, ready: &mut Vec<Callback>) -> bool {
        if matches!(self.poll(), Err(Error::DeviceLost(_))) {
            self.cancel_callbacks();
            return false;
        }
        {
            let mut state = self.state.lock().unwrap();
            let completed = state.completed;
            ready.extend(
                state
                    .callbacks
                    .extract_if(.., |(token, _)| *token <= completed)
                    .map(|(_, callback)| callback),
            );
        }
        for callback in ready.drain(..) {
            callback();
        }
        true
    }
    fn cancel_callbacks(&self) {
        // Dropping a readback callback publishes failure without touching its
        // mapping. Never run a success callback for unfinished GPU work.
        let callbacks = std::mem::take(&mut self.state.lock().unwrap().callbacks);
        drop(callbacks);
    }
    /// Wait for the oldest submission, unless a callback became ready since
    /// the last drain. False means there is no GPU work to wait for.
    fn wait_oldest(&self) -> bool {
        let (token, fence) = {
            let mut state = self.state.lock().unwrap();
            if self.context.lost.load(Ordering::Relaxed)
                || state
                    .callbacks
                    .iter()
                    .any(|(token, _)| *token <= state.completed)
            {
                return true;
            }
            let Some(oldest) = state.pending.front() else {
                return false;
            };
            let oldest = (oldest.token, oldest.fence);
            state.waiting = oldest.1;
            oldest
        };
        let result = unsafe {
            if self.timeline != vk::Semaphore::null() {
                let semaphores = [self.timeline];
                let values = [token];
                let info = vk::SemaphoreWaitInfo::default()
                    .semaphores(&semaphores)
                    .values(&values);
                if self.context.api >= vk::API_VERSION_1_2 {
                    self.context.device.wait_semaphores(&info, u64::MAX)
                } else {
                    self.context.timeline.wait_semaphores(&info, u64::MAX)
                }
            } else {
                self.context
                    .device
                    .wait_for_fences(&[fence], true, u64::MAX)
            }
        };
        let _ = self.context.result(result);
        let mut state = self.state.lock().unwrap();
        state.waiting = vk::Fence::null();
        if std::mem::take(&mut state.retired) {
            let _ = unsafe { self.context.device.reset_fences(&[fence]) };
            state.fences.push(fence);
        }
        true
    }
    fn poll(&self) -> Result<Token> {
        self.context.check()?;
        let mut state = self.state.lock().unwrap();
        let completed = if self.timeline != vk::Semaphore::null() {
            self.context.result(unsafe {
                if self.context.api >= vk::API_VERSION_1_2 {
                    self.context
                        .device
                        .get_semaphore_counter_value(self.timeline)
                } else {
                    self.context
                        .timeline
                        .get_semaphore_counter_value(self.timeline)
                }
            })?
        } else {
            let mut completed = state.completed;
            for submission in &state.pending {
                if self
                    .context
                    .result(unsafe { self.context.device.get_fence_status(submission.fence) })?
                {
                    completed = submission.token;
                } else {
                    break;
                }
            }
            completed
        };
        state.completed = completed;
        while state.pending.front().is_some_and(|s| s.token <= completed) {
            let mut submission = state.pending.pop_front().unwrap();
            if let Some(arena) = submission._resources.descriptors.take() {
                self.descriptors.lock().unwrap().push(arena);
            }
            let fence = submission.fence;
            if fence != vk::Fence::null() && fence == state.waiting {
                state.retired = true;
            } else if fence != vk::Fence::null() {
                self.context
                    .result(unsafe { self.context.device.reset_fences(&[fence]) })?;
                state.fences.push(fence);
            }
            let mut commands = submission.commands;
            commands.reset(&self.context)?;
            state.pools.push(commands);
        }
        #[cfg(target_os = "android")]
        state.sync.retire(completed);
        self.context.rings.lock().unwrap().retain(|weak| {
            if let Some(ring) = weak.upgrade() {
                ring.retire(completed);
                true
            } else {
                false
            }
        });
        self.context.reap(completed);
        Ok(Token(completed))
    }
}
impl Drop for Queue {
    fn drop(&mut self) {
        let _ = self.sender.take().unwrap().send(Message::Stop);
        if let Some(waiter) = self.waiter.take() {
            let _ = waiter.join();
        }
    }
}
impl Drop for Shared {
    fn drop(&mut self) {
        let state = self.state.get_mut().unwrap();
        unsafe {
            let _ = self.context.device.device_wait_idle();
            for mut submission in state.pending.drain(..) {
                if let Some(mut arena) = submission._resources.descriptors.take() {
                    arena.destroy(&self.context);
                }
                if submission.fence != vk::Fence::null() {
                    self.context.device.destroy_fence(submission.fence, None);
                }
                submission.commands.destroy(&self.context);
            }
            for commands in state.pools.drain(..) {
                commands.destroy(&self.context);
            }
            for fence in state.fences.drain(..) {
                self.context.device.destroy_fence(fence, None);
            }
            #[cfg(target_os = "android")]
            state.sync.destroy(&self.context);
            if self.timeline != vk::Semaphore::null() {
                self.context.device.destroy_semaphore(self.timeline, None);
            }
            for arena in self.descriptors.get_mut().unwrap() {
                arena.destroy(&self.context);
            }
        }
    }
}

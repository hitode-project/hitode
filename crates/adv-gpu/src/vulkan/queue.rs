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
    pub(super) pool: vk::CommandPool,
    handle: vk::Queue,
    timeline: vk::Semaphore,
    pub(super) state: Mutex<State>,
    pub(super) descriptors: Mutex<Vec<binding::DescriptorArena>>,
}
pub(super) struct State {
    next: u64,
    completed: u64,
    pending: VecDeque<Submission>,
    pub(super) commands: Vec<vk::CommandBuffer>,
    fences: Vec<vk::Fence>,
}
struct Submission {
    token: u64,
    fence: vk::Fence,
    commands: Vec<vk::CommandBuffer>,
    _resources: encode::Resources,
}
enum Message {
    Wake,
    Callback(u64, Box<dyn FnOnce() + Send>),
    Stop,
}
impl Queue {
    pub(super) fn new(device: crate::Device) -> Result<Self> {
        let context = device.inner.0.clone();
        let pool = context.result(unsafe {
            context.device.create_command_pool(
                &vk::CommandPoolCreateInfo::default()
                    .queue_family_index(context.family)
                    .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
                None,
            )
        })?;
        let timeline = if context.caps.modern.contains(Modern::TIMELINE_SEMAPHORES) {
            let mut ty =
                vk::SemaphoreTypeCreateInfo::default().semaphore_type(vk::SemaphoreType::TIMELINE);
            match context.result(unsafe {
                context
                    .device
                    .create_semaphore(&vk::SemaphoreCreateInfo::default().push_next(&mut ty), None)
            }) {
                Ok(s) => s,
                Err(e) => {
                    unsafe {
                        context.device.destroy_command_pool(pool, None);
                    }
                    return Err(e);
                }
            }
        } else {
            vk::Semaphore::null()
        };
        let shared = Arc::new(Shared {
            handle: unsafe { context.device.get_device_queue(context.family, 0) },
            context,
            pool,
            timeline,
            descriptors: Mutex::new(Vec::new()),
            state: Mutex::new(State {
                next: 1,
                completed: 0,
                pending: VecDeque::new(),
                commands: Vec::new(),
                fences: Vec::new(),
            }),
        });
        let (sender, receiver) = mpsc::channel();
        let backend = shared.clone();
        let waiter = std::thread::Builder::new()
            .name("adv-gpu waiter".into())
            .spawn(move || {
                let mut callbacks: Vec<(u64, Box<dyn FnOnce() + Send>)> = Vec::new();
                loop {
                    let message = if callbacks.is_empty()
                        && backend.state.lock().unwrap().pending.is_empty()
                    {
                        receiver
                            .recv()
                            .map_err(|_| mpsc::RecvTimeoutError::Disconnected)
                    } else {
                        receiver.recv_timeout(Duration::from_millis(1))
                    };
                    match message {
                        Ok(Message::Wake) => {}
                        Ok(Message::Callback(token, callback)) => callbacks.push((token, callback)),
                        Ok(Message::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                    }
                    if let Ok(token) = backend.poll() {
                        let mut i = 0;
                        while i < callbacks.len() {
                            if callbacks[i].0 <= token.0 {
                                let (_, callback) = callbacks.remove(i);
                                callback();
                            } else {
                                i += 1;
                            }
                        }
                    }
                }
                let _ = backend
                    .context
                    .result(unsafe { backend.context.device.device_wait_idle() });
                let _ = backend.poll();
                for (_, callback) in callbacks {
                    callback();
                }
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
    pub(crate) fn submit(&mut self, mut encoder: crate::Encoder) -> Result<Token> {
        let context = &self.shared.context;
        context.check()?;
        if let Some(error) = encoder.inner.error.take() {
            return Err(error);
        }
        encoder.inner.memory_barrier();
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
        let mut commands = std::mem::take(&mut encoder.inner.commands);
        commands.push(encoder.inner.command);
        let semaphores = [self.shared.timeline];
        let values = [token];
        let mut timeline_info =
            vk::TimelineSemaphoreSubmitInfo::default().signal_semaphore_values(&values);
        let mut submit = vk::SubmitInfo::default().command_buffers(&commands);
        if self.shared.timeline != vk::Semaphore::null() {
            submit = submit
                .signal_semaphores(&semaphores)
                .push_next(&mut timeline_info);
        }
        if let Err(error) = context.result(unsafe {
            context
                .device
                .queue_submit(self.shared.handle, &[submit], fence)
        }) {
            if fence != vk::Fence::null() {
                state.fences.push(fence);
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
            commands,
            _resources: resources,
        });
        state.next += 1;
        context.next.store(state.next, Ordering::Release);
        context.open.fetch_sub(1, Ordering::Release);
        encoder.inner.submitted = true;
        let _ = self.sender.as_ref().unwrap().send(Message::Wake);
        Ok(Token(token))
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
    pub(crate) fn on_complete(&mut self, token: Token, callback: Box<dyn FnOnce() + Send>) {
        let _ = self
            .sender
            .as_ref()
            .unwrap()
            .send(Message::Callback(token.0, callback));
    }
    pub(crate) fn export_fence(&mut self, _token: Token) -> Result<crate::queue::Fence> {
        todo!("A3")
    }
    pub(crate) fn wait_fence(&mut self, _fence: crate::queue::Fence) -> Result<()> {
        todo!("A3")
    }
}
impl Shared {
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
            if submission.fence != vk::Fence::null() {
                self.context
                    .result(unsafe { self.context.device.reset_fences(&[submission.fence]) })?;
                state.fences.push(submission.fence);
            }
            state.commands.extend(submission.commands);
        }
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
            }
            for fence in state.fences.drain(..) {
                self.context.device.destroy_fence(fence, None);
            }
            if self.timeline != vk::Semaphore::null() {
                self.context.device.destroy_semaphore(self.timeline, None);
            }
            for arena in self.descriptors.get_mut().unwrap() {
                arena.destroy(&self.context);
            }
            self.context.device.destroy_command_pool(self.pool, None);
        }
    }
}

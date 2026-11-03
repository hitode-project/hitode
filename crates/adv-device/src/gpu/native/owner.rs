//! Package C2: the GPU owner thread. It drains command chunks, groups the
//! commands between `Target`s into passes with [`super::pass::plan`] once the
//! next target or a submission boundary is known, encodes them into one
//! encoder per submission, retains each chunk's ring spans with that encoder,
//! submits at `Present`, `Readback` and `Submit`, and hands frames to the
//! [`crate::frame::Presenter`] on this thread.

use super::stream::Consumer;
use crate::frame::Presenter;

#[cfg(test)]
pub(super) fn run(queue: adv_gpu::Queue, commands: Consumer, presenter: Box<dyn Presenter>) {
    high_priority().expect("GPU owner QoS");
    let (sender, receiver) = mpsc::channel();
    let owner = Owner {
        thread: Arc::new(OwnerThread {
            sender: Some(sender),
            join: None,
        }),
        failure: Arc::new(OnceLock::new()),
    };
    owner.attach(commands).expect("GPU stream thread");
    drop(owner);
    encode(queue, receiver, presenter).expect("GPU owner");
}

use super::resources::Storage;
use super::stream::{Binding, Bytes, Chunk, Command, Draw};
use super::{Depth, Image, backing, pass};
use crate::frame::{Base, Frame, SwapImage};
use adv_gpu::encode::{Bind, BufferCopy, ColorAttachment, DepthAttachment, ImageCopy};
use adv_gpu::resource::BufferSlice;
use adv_gpu::{Buffer, Encoder, Extent, Queue, Rect, RenderPassDesc, Token};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, OnceLock, mpsc};
use std::time::Instant;

enum Work {
    Chunk(Arc<Consumer>, Chunk),
    Closed(Arc<Consumer>),
}

/// All streams forward complete chunks to this single queue owner. Relays
/// sleep in Consumer::next; neither a recording draw nor an idle GPU polls.
#[derive(Clone)]
pub(super) struct Owner {
    thread: Arc<OwnerThread>,
    pub failure: Arc<OnceLock<String>>,
}
struct OwnerThread {
    sender: Option<mpsc::Sender<Work>>,
    join: Option<std::thread::JoinHandle<()>>,
}
impl Drop for OwnerThread {
    fn drop(&mut self) {
        // Gpu drops its producer first. Closing the final registration
        // sender lets all relays drain before the queue owner shuts down.
        self.sender.take();
        if let Some(join) = self.join.take() {
            join.join().expect("GPU owner panicked");
        }
    }
}

impl Owner {
    pub fn start(
        queue: Queue,
        commands: Consumer,
        presenter: Box<dyn Presenter>,
    ) -> Result<Self, super::super::Error> {
        let (sender, receiver) = mpsc::channel();
        let failure = Arc::new(OnceLock::new());
        let owner_failure = failure.clone();
        let join = std::thread::Builder::new()
            .name("GPU owner".into())
            .spawn(move || {
                if let Err(error) =
                    high_priority().and_then(|()| encode(queue, receiver, presenter))
                {
                    let _ = owner_failure.set(error.to_string());
                }
            })
            .map_err(|error| super::super::Error::Unavailable(error.to_string()))?;
        let owner = Self {
            thread: Arc::new(OwnerThread {
                sender: Some(sender),
                join: Some(join),
            }),
            failure,
        };
        owner.attach(commands)?;
        Ok(owner)
    }

    pub fn attach(&self, commands: Consumer) -> Result<(), super::super::Error> {
        let sender = self.thread.sender.as_ref().unwrap().clone();
        let commands = Arc::new(commands);
        std::thread::Builder::new()
            .name("GPU stream".into())
            .spawn(move || {
                while let Some(chunk) = commands.next() {
                    if sender.send(Work::Chunk(commands.clone(), chunk)).is_err() {
                        break;
                    }
                    while let Some(chunk) = commands.try_next() {
                        if sender.send(Work::Chunk(commands.clone(), chunk)).is_err() {
                            return;
                        }
                    }
                }
                let _ = sender.send(Work::Closed(commands));
            })
            .map_err(|error| super::super::Error::Unavailable(error.to_string()))?;
        Ok(())
    }
}

fn high_priority() -> adv_gpu::Result<()> {
    #[cfg(target_vendor = "apple")]
    unsafe {
        unsafe extern "C" {
            fn pthread_set_qos_class_self_np(class: u32, relative: i32) -> i32;
        }
        let status = pthread_set_qos_class_self_np(0x21, 0);
        if status != 0 {
            return Err(adv_gpu::Error::Unavailable(format!(
                "GPU owner QoS: {status}"
            )));
        }
    }
    Ok(())
}

struct State {
    queue: Queue,
    encoder: Option<Encoder>,
    presenter: Box<dyn Presenter>,
    targets: HashMap<usize, (Image, Option<Depth>)>,
    depths: HashMap<usize, Vec<Depth>>,
    pending: Option<Run>,
    participants: Vec<Arc<Consumer>>,
    #[cfg(test)]
    passes: Vec<PassRecord>,
}

struct Run {
    stream: usize,
    color: Image,
    depth: Option<Depth>,
    ops: Vec<pass::Op>,
    draws: Vec<Option<RecordedDraw>>,
}
struct RecordedDraw {
    pipeline: adv_gpu::RenderPipeline,
    vertices: std::ops::Range<u32>,
    base_vertex: i32,
    viewport: [f32; 6],
    scissor: Rect,
    stencil_reference: u32,
    blend_constant: [f32; 4],
    bindings: Vec<(u32, Resource)>,
    index: Option<(Resource, adv_gpu::encode::IndexFormat)>,
}
enum Resource {
    Uniform(Buffer, std::ops::Range<u64>),
    Storage(Buffer, std::ops::Range<u64>),
    Sampled(Image),
    Sampler(adv_gpu::Sampler),
}
impl Resource {
    fn capture(binding: &Binding, spans: &[(Buffer, u64)]) -> Self {
        match binding {
            Binding::Uniform(bytes) | Binding::Storage(bytes) => {
                let s = slice(*bytes, spans);
                let range = s.offset..s.offset + s.size;
                if matches!(binding, Binding::Uniform(_)) {
                    Self::Uniform(s.buffer.clone(), range)
                } else {
                    Self::Storage(s.buffer.clone(), range)
                }
            }
            Binding::Buffer(buffer, range) => Self::Storage(buffer.clone(), range.clone()),
            Binding::Sampled(image) => Self::Sampled(image.clone()),
            Binding::Sampler(sampler) => Self::Sampler(sampler.clone()),
        }
    }
    fn buffer(&self) -> BufferSlice<'_> {
        match self {
            Self::Uniform(buffer, range) | Self::Storage(buffer, range) => {
                buffer.slice(range.clone())
            }
            _ => unreachable!("buffer binding"),
        }
    }
}
#[cfg(test)]
struct PassRecord {
    area: Rect,
    color: (adv_gpu::encode::Load<[f64; 4]>, adv_gpu::encode::Store),
    depth: Option<(adv_gpu::encode::Load<f32>, adv_gpu::encode::Store)>,
    stencil: Option<(adv_gpu::encode::Load<u32>, adv_gpu::encode::Store)>,
    transient: bool,
}

fn encode(
    queue: Queue,
    receiver: mpsc::Receiver<Work>,
    presenter: Box<dyn Presenter>,
) -> adv_gpu::Result<()> {
    let mut state = State::new(queue, presenter);
    let mut ready = VecDeque::new();
    while let Ok(work) = receiver.recv() {
        ready.push_back(work);
        ready.extend(receiver.try_iter());
        while let Some(work) = ready.pop_front() {
            match work {
                Work::Chunk(consumer, mut chunk) => {
                    let stream = Arc::as_ptr(&consumer) as usize;
                    #[cfg(target_vendor = "apple")]
                    objc2::rc::autoreleasepool(|_| {
                        state.chunk(stream, &mut chunk, &ready, &consumer)
                    })?;
                    #[cfg(not(target_vendor = "apple"))]
                    state.chunk(stream, &mut chunk, &ready, &consumer)?;
                    consumer.recycle(chunk);
                }
                Work::Closed(consumer) => {
                    state.close(pass::NextDepth::Keep)?;
                    if state.encoder.is_some() {
                        state.submit(&consumer, None)?;
                    }
                    let stream = Arc::as_ptr(&consumer) as usize;
                    state.targets.remove(&stream);
                    state.depths.remove(&stream);
                }
            }
        }
        state.queue.poll()?;
    }
    state.close(pass::NextDepth::Keep)?;
    if let Some(encoder) = state.encoder.take() {
        state.queue.submit(encoder)?;
    }
    Ok(())
}

impl State {
    fn new(queue: Queue, presenter: Box<dyn Presenter>) -> Self {
        Self {
            queue,
            encoder: None,
            presenter,
            targets: HashMap::new(),
            depths: HashMap::new(),
            pending: None,
            participants: Vec::new(),
            #[cfg(test)]
            passes: Vec::new(),
        }
    }
    fn encoder(&mut self) -> &mut Encoder {
        self.encoder
            .get_or_insert_with(|| self.queue.encoder("guest commands"))
    }
    fn image(
        &mut self,
        image: &Image,
    ) -> adv_gpu::Result<(adv_gpu::Image, adv_gpu::ImageView, u32)> {
        self.encoder();
        backing::resolve(
            image,
            &mut self.queue,
            &mut *self.presenter,
            self.encoder.as_mut().unwrap(),
        )
    }
    fn close(&mut self, next: pass::NextDepth) -> adv_gpu::Result<()> {
        if let Some(run) = self.pending.take() {
            let plans = pass::plan(&run.ops, run.depth.is_some(), next);
            self.render(run, &plans)?;
        }
        Ok(())
    }
    fn chunk(
        &mut self,
        stream: usize,
        chunk: &mut Chunk,
        ready: &VecDeque<Work>,
        consumer: &Arc<Consumer>,
    ) -> adv_gpu::Result<()> {
        if self
            .pending
            .as_ref()
            .is_some_and(|run| run.stream != stream)
        {
            self.close(pass::NextDepth::Keep)?;
        }
        if !self
            .participants
            .iter()
            .any(|other| Arc::ptr_eq(other, consumer))
        {
            self.participants.push(consumer.clone());
        }
        let spans: Vec<_> = chunk
            .spans
            .iter()
            .map(|span| (span.buffer().clone(), span.offset()))
            .collect();
        for span in chunk.spans.drain(..) {
            self.encoder().retain(span);
        }
        for at in 0..chunk.commands.len() {
            let op = match &chunk.commands[at] {
                Command::Clear {
                    area,
                    color,
                    depth,
                    stencil,
                } => Some((
                    pass::Op {
                        area: *area,
                        kind: pass::OpKind::Clear {
                            color: *color,
                            depth: *depth,
                            stencil: *stencil,
                        },
                    },
                    None,
                )),
                Command::Draw(n) => {
                    let d = &chunk.draws[*n as usize];
                    let op = pass::Op {
                        area: d.area,
                        kind: pass::OpKind::Draw {
                            writes_color: d.writes_color,
                            uses_depth: d.uses_depth,
                            writes_depth: d.writes_depth,
                            opaque: d.opaque,
                        },
                    };
                    let bindings = chunk.bindings
                        [d.bindings.start as usize..d.bindings.end as usize]
                        .iter()
                        .map(|(slot, b)| (*slot, Resource::capture(b, &spans)))
                        .collect();
                    let index = d
                        .index
                        .as_ref()
                        .map(|(b, f)| (Resource::capture(b, &spans), *f));
                    Some((
                        op,
                        Some(RecordedDraw {
                            pipeline: d.pipeline.clone(),
                            vertices: d.vertices.clone(),
                            base_vertex: d.base_vertex,
                            viewport: d.viewport,
                            scissor: d.scissor,
                            stencil_reference: d.stencil_reference,
                            blend_constant: d.blend_constant,
                            bindings,
                            index,
                        }),
                    ))
                }
                _ => None,
            };
            if let Some((op, draw)) = op {
                let run = self.pending.get_or_insert_with(|| {
                    let (color, depth) = self.targets[&stream].clone();
                    Run {
                        stream,
                        color,
                        depth,
                        ops: Vec::new(),
                        draws: Vec::new(),
                    }
                });
                run.ops.push(op);
                run.draws.push(draw);
                continue;
            }
            let next = self.pending.as_ref().map_or(pass::NextDepth::Keep, |run| {
                next_depth(
                    run.depth.as_ref(),
                    chunk.commands[at..]
                        .iter()
                        .map(|c| (c, chunk.draws.as_slice()))
                        .chain(
                            ready
                                .iter()
                                .filter_map(|work| match work {
                                    Work::Chunk(consumer, chunk)
                                        if Arc::as_ptr(consumer) as usize == stream =>
                                    {
                                        Some(chunk)
                                    }
                                    _ => None,
                                })
                                .flat_map(|c| {
                                    c.commands
                                        .iter()
                                        .map(|command| (command, c.draws.as_slice()))
                                }),
                        ),
                )
            });
            self.close(next)?;
            let command = std::mem::replace(&mut chunk.commands[at], Command::Submit);
            if let Command::Target { color, depth } = command {
                self.targets.insert(stream, (color, depth));
            } else {
                self.command(stream, command, &spans, consumer)?;
            }
        }
        Ok(())
    }

    fn command(
        &mut self,
        stream: usize,
        command: Command,
        spans: &[(Buffer, u64)],
        consumer: &Arc<Consumer>,
    ) -> adv_gpu::Result<()> {
        match command {
            Command::Upload {
                bytes,
                pitch,
                image,
                mip,
                rect,
            } => {
                let (image, _, _) = self.image(&image)?;
                let source = slice(bytes, spans);
                self.encoder().copy_buffer_to_image(
                    BufferCopy {
                        buffer: source.buffer,
                        offset: source.offset,
                        row_pitch: pitch,
                        rows: rect.h,
                    },
                    ImageCopy {
                        image: &image,
                        mip,
                        layer: 0,
                        origin: (rect.x, rect.y, 0),
                    },
                    extent(rect),
                );
            }
            Command::Copy {
                source,
                rect,
                destination,
                at,
            } => {
                let (source, _, mip) = self.image(&source)?;
                let (destination, _, destination_mip) = self.image(&destination)?;
                self.encoder().copy_image(
                    ImageCopy {
                        image: &source,
                        mip,
                        layer: 0,
                        origin: (rect.x, rect.y, 0),
                    },
                    ImageCopy {
                        image: &destination,
                        mip: destination_mip,
                        layer: 0,
                        origin: (at.0, at.1, 0),
                    },
                    extent(rect),
                );
            }
            Command::DiscardDepth(depth) => depth.0.owner.lock().unwrap().discard(),
            Command::Readback {
                image,
                rect,
                into,
                done,
            } => {
                let (image, _, mip) = self.image(&image)?;
                self.encoder().copy_image_to_buffer(
                    ImageCopy {
                        image: &image,
                        mip,
                        layer: 0,
                        origin: (rect.x, rect.y, 0),
                    },
                    BufferCopy {
                        buffer: &into,
                        offset: 0,
                        row_pitch: rect.w * 4,
                        rows: rect.h,
                    },
                    extent(rect),
                );
                let token = self.submit(consumer, None)?;
                self.queue.on_complete(token, done);
            }
            Command::Submit => {
                self.submit(consumer, None)?;
            }
            Command::Present {
                image,
                damage,
                serial,
            } => {
                let (raw, _, _) = self.image(&image)?;
                let mut swap = match &image.storage {
                    Storage::Fixed { .. } => SwapImage::Private(raw),
                    Storage::Swap(swap) => swap.owner.lock().unwrap().take_present(),
                };
                let presentable = match &mut swap {
                    SwapImage::Presentable(image) => Some(image),
                    _ => None,
                };
                let ready = self.submit(consumer, presentable)?;
                if let Storage::Swap(backing) = &image.storage {
                    backing.owner.lock().unwrap().presented(&swap, &damage);
                }
                self.presenter.present(
                    &mut self.queue,
                    Frame {
                        serial,
                        base: Base::Gpu {
                            image: swap,
                            damage,
                            ready,
                        },
                        video: Arc::from([]),
                        published_at: Instant::now(),
                    },
                );
                self.presenter.drain();
                if let Some(depths) = self.depths.remove(&stream) {
                    for depth in depths {
                        depth.0.owner.lock().unwrap().end_frame();
                    }
                }
            }
            Command::Target { .. } | Command::Clear { .. } | Command::Draw(_) => unreachable!(),
        }
        Ok(())
    }

    fn submit(
        &mut self,
        consumer: &Arc<Consumer>,
        image: Option<&mut adv_gpu::PresentImage>,
    ) -> adv_gpu::Result<Token> {
        let encoder = self
            .encoder
            .take()
            .unwrap_or_else(|| self.queue.encoder("submission boundary"));
        let token = if let Some(image) = image {
            self.queue.submit_present(encoder, &mut [image])?
        } else {
            self.queue.submit(encoder)?
        };
        if !self
            .participants
            .iter()
            .any(|other| Arc::ptr_eq(other, consumer))
        {
            self.participants.push(consumer.clone());
        }
        let consumers = std::mem::take(&mut self.participants);
        self.queue.on_complete(
            token,
            Box::new(move || {
                for consumer in consumers {
                    consumer.retired();
                }
            }),
        );
        Ok(token)
    }

    fn render(&mut self, run: Run, plans: &[pass::Pass]) -> adv_gpu::Result<()> {
        let (_, color, _) = self.image(&run.color)?;
        let depth_image = run
            .depth
            .as_ref()
            .map(|depth| {
                let depths = self.depths.entry(run.stream).or_default();
                if !depths.iter().any(|other| Arc::ptr_eq(&other.0, &depth.0)) {
                    depths.push(depth.clone());
                }
                depth
                    .0
                    .owner
                    .lock()
                    .unwrap()
                    .attachment(depth, self.queue.device(), plans)
            })
            .transpose()?;
        let mut sampled = HashMap::new();
        for draw in run.draws.iter().flatten() {
            for (_, binding) in &draw.bindings {
                if let Resource::Sampled(image) = binding
                    && let std::collections::hash_map::Entry::Vacant(entry) =
                        sampled.entry(image.id())
                {
                    let (_, view, _) = self.image(image)?;
                    entry.insert(view);
                }
            }
        }
        for plan in plans {
            let desc = RenderPassDesc {
                colors: [ColorAttachment {
                    view: &color,
                    load: plan.color.0,
                    store: plan.color.1,
                    resolve: None,
                }]
                .into_iter()
                .collect(),
                depth_stencil: depth_image.as_ref().map(|image| DepthAttachment {
                    view: image.view(),
                    depth: plan.depth,
                    stencil: plan.stencil,
                    read_only: false,
                }),
                area: plan.area,
                timestamps: None,
                label: "guest pass",
            };
            #[cfg(test)]
            self.passes.push(PassRecord {
                area: desc.area,
                color: (desc.colors[0].load, desc.colors[0].store),
                depth: desc.depth_stencil.as_ref().map(|d| d.depth),
                stencil: desc.depth_stencil.as_ref().map(|d| d.stencil),
                transient: depth_image
                    .as_ref()
                    .is_some_and(|d| d.desc().usage.contains(adv_gpu::resource::Usage::TRANSIENT)),
            });
            let mut pass = self.encoder().render_pass(&desc);
            let mut slots: HashMap<(u8, u32), BindingKey> = HashMap::new();
            let mut viewport = None;
            let mut scissor = None;
            let mut stencil = None;
            let mut blend = None;
            for recorded in run.draws[plan.ops.clone()].iter().flatten() {
                let d = recorded;
                pass.set_pipeline(&d.pipeline);
                if viewport != Some(d.viewport) {
                    pass.set_viewport(
                        d.viewport[0],
                        d.viewport[1],
                        d.viewport[2],
                        d.viewport[3],
                        d.viewport[4],
                        d.viewport[5],
                    );
                    viewport = Some(d.viewport);
                }
                if scissor != Some(d.scissor) {
                    pass.set_scissor(d.scissor);
                    scissor = Some(d.scissor);
                }
                if stencil != Some(d.stencil_reference) {
                    pass.set_stencil_reference(d.stencil_reference);
                    stencil = Some(d.stencil_reference);
                }
                if blend != Some(d.blend_constant) {
                    pass.set_blend_constant(d.blend_constant);
                    blend = Some(d.blend_constant);
                }
                for (slot, binding) in &recorded.bindings {
                    // Sampler identity is opaque in adv-gpu; only resources
                    // whose identity is exposed can be compared here.
                    if let Resource::Sampler(sampler) = binding {
                        pass.bind(*slot, Bind::Sampler(sampler));
                        continue;
                    }
                    let resource = match binding {
                        Resource::Uniform(buffer, range) => {
                            Bind::Uniform(buffer.slice(range.clone()))
                        }
                        Resource::Storage(buffer, range) => {
                            Bind::StorageBuffer(buffer.slice(range.clone()))
                        }
                        Resource::Sampled(image) => Bind::Sampled(&sampled[&image.id()]),
                        Resource::Sampler(_) => unreachable!(),
                    };
                    let (kind, key) = binding_key(resource);
                    if slots.get(&(kind, *slot)) != Some(&key) {
                        pass.bind(*slot, resource);
                        slots.insert((kind, *slot), key);
                    }
                }
                if let Some((index, format)) = &recorded.index {
                    pass.set_index_buffer(index.buffer(), *format);
                    pass.draw_indexed(d.vertices.clone(), d.base_vertex, 0..1);
                } else {
                    pass.draw(d.vertices.clone(), 0..1);
                }
            }
        }
        Ok(())
    }
}

#[derive(PartialEq, Eq)]
enum BindingKey {
    Buffer(usize, u64, u64),
    Image(u64),
}
fn binding_key(binding: Bind<'_>) -> (u8, BindingKey) {
    match binding {
        Bind::Uniform(s) | Bind::StorageBuffer(s) => {
            let kind = u8::from(matches!(binding, Bind::StorageBuffer(_))) * 3;
            let identity = s
                .buffer
                .mapped()
                .map_or(s.buffer as *const _ as usize, |p| {
                    p.as_ptr() as *mut u8 as usize
                });
            (kind, BindingKey::Buffer(identity, s.offset, s.size))
        }
        Bind::Sampled(image) => (1, BindingKey::Image(image as *const _ as u64)),
        _ => unreachable!(),
    }
}
fn slice(bytes: Bytes, spans: &[(Buffer, u64)]) -> BufferSlice<'_> {
    let (buffer, offset) = &spans[bytes.span as usize];
    buffer.slice(
        offset + u64::from(bytes.offset)..offset + u64::from(bytes.offset) + u64::from(bytes.len),
    )
}
fn extent(rect: Rect) -> Extent {
    Extent {
        width: rect.w,
        height: rect.h,
        depth_or_layers: 1,
    }
}

fn next_depth<'a>(
    depth: Option<&Depth>,
    commands: impl Iterator<Item = (&'a Command, &'a [Draw])>,
) -> pass::NextDepth {
    let Some(depth) = depth else {
        return pass::NextDepth::Keep;
    };
    let mut active = Some(depth.clone());
    for (command, draws) in commands {
        match command {
            Command::DiscardDepth(d) if Arc::ptr_eq(&d.0, &depth.0) => {
                return pass::NextDepth::Discarded;
            }
            Command::Target { depth, .. } => active = depth.clone(),
            Command::Clear {
                area,
                depth: clear_depth,
                stencil,
                ..
            } if active.as_ref().is_some_and(|d| Arc::ptr_eq(&d.0, &depth.0)) => {
                if clear_depth.is_some() && (stencil.is_some() || !depth.format().has_stencil()) {
                    return pass::NextDepth::Cleared(*area);
                }
                if clear_depth.is_some() || stencil.is_some() {
                    return pass::NextDepth::Keep;
                }
            }
            Command::Draw(n)
                if active.as_ref().is_some_and(|d| Arc::ptr_eq(&d.0, &depth.0))
                    && draws[*n as usize].uses_depth =>
            {
                return pass::NextDepth::Keep;
            }
            _ => {}
        }
    }
    pass::NextDepth::Keep
}

pub(super) struct Headless;
impl Presenter for Headless {
    fn presentable(&self, _: u32, _: u32) -> bool {
        false
    }
    fn present(&mut self, _: &mut Queue, _: Frame) {}
    fn drain(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::super::super::{AlphaMode, DepthFormat};
    use super::*;
    use adv_gpu::encode::{Load, Store};
    use adv_gpu::pipeline as p;
    use adv_gpu::resource::{BufferDesc, BufferUsage, Memory, Usage};
    use adv_gpu::shader::{ShaderDesc, Source, Stage};
    use adv_gpu::{Device, Format};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    const TIMEOUT: Duration = Duration::from_secs(20);
    fn open() -> Option<(Device, Queue)> {
        match Device::open(&adv_gpu::queue::DeviceOptions {
            software: !cfg!(target_vendor = "apple"),
            validation: true,
            label: "owner test",
            ..Default::default()
        }) {
            Ok(gpu) => Some(gpu),
            Err(adv_gpu::Error::Unavailable(reason))
                if std::env::var_os("ADV_GPU_REQUIRE_BACKEND").is_none() =>
            {
                eprintln!("skipped: {reason}");
                None
            }
            Err(error) => panic!("device: {error}"),
        }
    }
    fn rect(x: u32, y: u32, w: u32, h: u32) -> Rect {
        Rect { x, y, w, h }
    }
    fn fixed(image: adv_gpu::Image) -> Image {
        Image {
            storage: Storage::Fixed {
                view: image.view().clone(),
                image,
                level: None,
            },
            alpha: AlphaMode::Stored,
        }
    }
    fn image(device: &Device, w: u32, h: u32) -> Image {
        fixed(
            device
                .create_image(&backing::image_desc(
                    (w, h),
                    Format::Bgra8Unorm,
                    Usage::COLOR | Usage::COPY_SRC | Usage::COPY_DST | Usage::SAMPLED,
                ))
                .unwrap(),
        )
    }
    fn swap(w: u32, h: u32) -> Image {
        Image {
            storage: Storage::Swap(Arc::new(backing::Swap::new((w, h)))),
            alpha: AlphaMode::Stored,
        }
    }
    fn color(image: &Image) -> Command {
        Command::Target {
            color: image.clone(),
            depth: None,
        }
    }
    fn clear(area: Rect, rgba: [f64; 4]) -> Command {
        Command::Clear {
            area,
            color: Some(rgba),
            depth: None,
            stencil: None,
        }
    }
    fn present(image: &Image, damage: crate::frame::Damage, serial: u64) -> Command {
        Command::Present {
            image: image.clone(),
            damage,
            serial,
        }
    }

    #[derive(Default)]
    struct Display {
        presentable: AtomicBool,
        frames: Mutex<VecDeque<Frame>>,
    }
    struct RecordingPresenter {
        display: Arc<Display>,
        notify: Option<mpsc::Sender<u64>>,
    }
    impl Presenter for RecordingPresenter {
        fn presentable(&self, _: u32, _: u32) -> bool {
            self.display.presentable.load(Ordering::Acquire)
        }
        fn present(&mut self, _: &mut Queue, frame: Frame) {
            let serial = frame.serial;
            self.display.frames.lock().unwrap().push_back(frame);
            if let Some(notify) = &self.notify {
                notify.send(serial).unwrap();
            }
        }
        fn drain(&mut self) {}
    }
    struct Harness {
        device: Device,
        state: State,
        producer: super::super::stream::Producer,
        consumer: Arc<Consumer>,
        display: Arc<Display>,
    }
    impl Harness {
        fn new(device: Device, queue: Queue) -> Self {
            let (producer, consumer) = super::super::stream::channel(device.ring(4 << 20).unwrap());
            let display = Arc::new(Display::default());
            let state = State::new(
                queue,
                Box::new(RecordingPresenter {
                    display: display.clone(),
                    notify: None,
                }),
            );
            Self {
                device,
                state,
                producer,
                consumer: Arc::new(consumer),
                display,
            }
        }
        fn drain(&mut self) {
            let mut chunks = VecDeque::new();
            while let Some(chunk) = self.consumer.try_next() {
                chunks.push_back(Work::Chunk(self.consumer.clone(), chunk));
            }
            while let Some(Work::Chunk(consumer, mut chunk)) = chunks.pop_front() {
                self.state.chunk(1, &mut chunk, &chunks, &consumer).unwrap();
                consumer.recycle(chunk);
            }
        }
        fn read(&mut self, image: &Image) -> Vec<u8> {
            let buffer = self
                .device
                .create_buffer(&BufferDesc {
                    size: u64::from(image.width()) * u64::from(image.height()) * 4,
                    usage: BufferUsage::COPY_DST,
                    memory: Memory::Readback,
                    label: "test readback",
                })
                .unwrap();
            let (send, done) = mpsc::channel();
            self.producer.push(Command::Readback {
                image: image.clone(),
                rect: rect(0, 0, image.width(), image.height()),
                into: buffer.clone(),
                done: Box::new(move || send.send(()).unwrap()),
            });
            self.drain();
            done.recv_timeout(TIMEOUT).unwrap();
            self.state.queue.poll().unwrap();
            unsafe { buffer.mapped().unwrap().as_ref() }.to_vec()
        }
        fn frame_image(&self, n: usize) -> adv_gpu::Image {
            let frames = self.display.frames.lock().unwrap();
            let Base::Gpu { image, .. } = &frames[n].base;
            match image {
                SwapImage::Private(i) => i.clone(),
                SwapImage::Presentable(i) => i.image().clone(),
            }
        }
    }
    fn pipeline(device: &Device) -> adv_gpu::RenderPipeline {
        let shader = device
            .create_shader(ShaderDesc {
                identity: 17,
                label: "owner draw",
                source: Source::Wgsl(
                    r#"
struct Color {rgba:vec4<f32>}
@group(0) @binding(0) var<uniform> color:Color;
@vertex fn vertex_main(@builtin(vertex_index) n:u32)->@builtin(position) vec4<f32> {
    let xy=array<vec2<f32>,3>(vec2<f32>(-1.0,-1.0),vec2<f32>(3.0,-1.0),vec2<f32>(-1.0,3.0));
    return vec4<f32>(xy[n],0.0,1.0);
}
@fragment fn fragment_main()->@location(0) vec4<f32> {return color.rgba;}
"#,
                ),
            })
            .unwrap();
        device
            .create_render_pipeline(
                &p::RenderPipelineDesc {
                    vertex: shader.entry("vertex_main", Stage::Vertex).unwrap(),
                    fragment: Some(shader.entry("fragment_main", Stage::Fragment).unwrap()),
                    bindings: vec![p::Binding {
                        slot: 0,
                        kind: p::BindingKind::UniformBuffer,
                        stages: p::Stages::FRAGMENT,
                    }],
                    primitive: p::Primitive {
                        topology: p::Topology::Triangles,
                        cull: p::Cull::None,
                        front_ccw: true,
                        wireframe: false,
                        unclipped_depth: false,
                    },
                    colors: [p::ColorTarget {
                        format: Format::Bgra8Unorm,
                        blend: None,
                        write_mask: 15,
                    }]
                    .into_iter()
                    .collect(),
                    depth_stencil: None,
                    samples: 1,
                    alpha_to_coverage: false,
                    label: "owner draw",
                },
                None,
            )
            .unwrap()
    }
    fn draw(pipeline: adv_gpu::RenderPipeline, area: Rect) -> Draw {
        Draw {
            pipeline,
            bindings: 0..0,
            index: None,
            vertices: 0..3,
            base_vertex: 0,
            viewport: [0., 0., 4., 4., 0., 1.],
            scissor: area,
            stencil_reference: 0,
            blend_constant: [0.; 4],
            area,
            writes_color: true,
            uses_depth: false,
            writes_depth: false,
            opaque: None,
        }
    }

    // spec: GFX-PASS-1, GFX-PASS-2, GFX-REC-1
    #[test]
    fn chunks_encode_clear_draw_copy_readback_and_present() {
        let Some((device, queue)) = open() else {
            return;
        };
        let mut h = Harness::new(device, queue);
        let source = image(&h.device, 4, 4);
        let destination = image(&h.device, 4, 4);
        h.producer.push(color(&source));
        h.producer.push(clear(rect(0, 0, 4, 4), [1., 0., 0., 1.]));
        let bytes = h.producer.bytes(
            16,
            h.device.caps().limits.uniform_offset_alignment,
            |bytes| bytes.copy_from_slice(bytemuck::cast_slice(&[0f32, 0., 1., 1.])),
        );
        let pipeline = pipeline(&h.device);
        let index = h
            .device
            .create_buffer(&BufferDesc {
                size: 12,
                usage: BufferUsage::INDEX,
                memory: Memory::Upload,
                label: "indices",
            })
            .unwrap();
        unsafe { index.mapped().unwrap().as_mut() }
            .copy_from_slice(bytemuck::cast_slice(&[0u32, 1, 2]));
        let mut d = draw(pipeline, rect(1, 1, 2, 2));
        d.index = Some((
            Binding::Buffer(index, 0..12),
            adv_gpu::encode::IndexFormat::U32,
        ));
        h.producer.draw(d, [(0, Binding::Uniform(bytes))]);
        h.producer.push(color(&destination));
        h.producer.push(clear(rect(0, 0, 4, 4), [0., 1., 0., 1.]));
        h.producer.push(Command::Copy {
            source: source.clone(),
            rect: rect(1, 1, 2, 2),
            destination: destination.clone(),
            at: (0, 0),
        });
        let pixels = h.read(&destination);
        for y in 0..4 {
            for x in 0..4 {
                assert_eq!(
                    &pixels[(y * 4 + x) * 4..(y * 4 + x + 1) * 4],
                    if x < 2 && y < 2 {
                        &[255, 0, 0, 255]
                    } else {
                        &[0, 255, 0, 255]
                    }
                );
            }
        }
        let source_pixels = h.read(&source);
        assert_eq!(&source_pixels[..4], &[0, 0, 255, 255]);
        assert_eq!(&source_pixels[20..24], &[255, 0, 0, 255]);
        assert_eq!(h.state.passes[0].area, rect(0, 0, 4, 4));
        assert_eq!(
            h.state.passes[0].color,
            (Load::Clear([1., 0., 0., 1.]), Store::Store)
        );
        assert!(h.state.passes[0].depth.is_none());
        assert!(h.state.passes[0].stencil.is_none());
        assert!(!h.state.passes[0].transient);
        h.producer
            .push(present(&destination, crate::frame::Damage::Whole, 7));
        h.drain();
        let Base::Gpu {
            image: SwapImage::Private(frame),
            ..
        } = &h.display.frames.lock().unwrap()[0].base
        else {
            panic!("private frame");
        };
        assert_eq!(
            frame.id(),
            match &destination.storage {
                Storage::Fixed { image, .. } => image.id(),
                _ => unreachable!(),
            }
        );
    }

    // spec: GFX-PASS-1
    #[test]
    fn a_run_crosses_chunks_and_small_clears_have_their_own_area() {
        let Some((device, queue)) = open() else {
            return;
        };
        let mut h = Harness::new(device, queue);
        let image = image(&h.device, 4, 4);
        h.producer.push(color(&image));
        h.producer.push(clear(rect(0, 0, 4, 4), [1., 0., 0., 1.]));
        let pipeline = pipeline(&h.device);
        for _ in 0..127 {
            let bytes = h.producer.bytes(
                16,
                h.device.caps().limits.uniform_offset_alignment,
                |bytes| bytes.copy_from_slice(bytemuck::cast_slice(&[0f32, 0., 1., 1.])),
            );
            h.producer.draw(
                draw(pipeline.clone(), rect(0, 0, 4, 4)),
                [(0, Binding::Uniform(bytes))],
            );
        }
        assert!(
            h.read(&image)
                .as_chunks::<4>()
                .0
                .iter()
                .all(|p| *p == [255, 0, 0, 255])
        );
        assert_eq!(h.state.passes.len(), 1);
        h.producer.push(clear(rect(0, 0, 1, 1), [1., 0., 0., 1.]));
        let bytes = h.producer.bytes(
            16,
            h.device.caps().limits.uniform_offset_alignment,
            |bytes| bytes.copy_from_slice(bytemuck::cast_slice(&[0f32, 0., 1., 1.])),
        );
        h.producer.draw(
            draw(pipeline, rect(0, 0, 4, 4)),
            [(0, Binding::Uniform(bytes))],
        );
        h.read(&image);
        assert_eq!(h.state.passes.len(), 3);
        assert_eq!(h.state.passes[1].area, rect(0, 0, 1, 1));
        assert_eq!(h.state.passes[2].area, rect(0, 0, 4, 4));
    }

    // spec: GFX-REC-1
    #[test]
    fn a_full_ring_waits_for_owner_submission_then_completes() {
        let Some((device, queue)) = open() else {
            return;
        };
        let image = image(&device, 256, 128);
        let (mut producer, consumer) =
            super::super::stream::channel(device.ring(128 << 10).unwrap());
        let (send, events) = mpsc::channel();
        let producer_thread = std::thread::spawn(move || {
            let bytes = producer.bytes(128 << 10, 4, |bytes| bytes.fill(1));
            producer.push(Command::Upload {
                bytes,
                pitch: 1024,
                image: image.clone(),
                mip: 0,
                rect: rect(0, 0, 256, 128),
            });
            send.send(1).unwrap();
            let bytes = producer.bytes(128 << 10, 4, |bytes| bytes.fill(2));
            send.send(2).unwrap();
            producer.push(Command::Upload {
                bytes,
                pitch: 1024,
                image,
                mip: 0,
                rect: rect(0, 0, 256, 128),
            });
            producer.push(Command::Submit);
        });
        assert_eq!(events.recv_timeout(TIMEOUT).unwrap(), 1);
        assert!(events.recv_timeout(Duration::from_millis(20)).is_err());
        let owner = std::thread::spawn(move || run(queue, consumer, Box::new(Headless)));
        assert_eq!(events.recv_timeout(TIMEOUT).unwrap(), 2);
        producer_thread.join().unwrap();
        owner.join().unwrap();
    }

    // spec: GFX-REC-5
    #[test]
    fn swap_age_repair_and_role_changes_preserve_exact_content() {
        let Some((device, queue)) = open() else {
            return;
        };
        let mut h = Harness::new(device, queue);
        let image = swap(4, 4);
        h.display.presentable.store(true, Ordering::Release);
        h.producer.push(color(&image));
        h.producer.push(clear(rect(0, 0, 4, 4), [1., 0., 0., 1.]));
        h.producer.push(Command::Submit);
        h.drain();
        let rendered = h.state.image(&image).unwrap().0.id();
        h.producer
            .push(present(&image, crate::frame::Damage::Whole, 1));
        h.drain();
        let first = h.frame_image(0);
        assert_eq!(first.id(), rendered);
        h.producer.push(clear(rect(1, 1, 1, 1), [0., 0., 1., 1.]));
        h.producer.push(present(
            &image,
            crate::frame::Damage::Rects([rect(1, 1, 1, 1)].into_iter().collect()),
            2,
        ));
        h.drain();
        assert_ne!(first.id(), h.frame_image(1).id());
        drop(h.display.frames.lock().unwrap().pop_front());
        h.producer.push(clear(rect(0, 0, 1, 1), [0., 1., 0., 1.]));
        h.producer.push(present(
            &image,
            crate::frame::Damage::Rects([rect(0, 0, 1, 1)].into_iter().collect()),
            3,
        ));
        h.drain();
        let current = h.frame_image(1);
        assert_eq!(current.id(), first.id());
        let expected = h.read(&fixed(current));
        for y in 0..4 {
            for x in 0..4 {
                let p = &expected[(y * 4 + x) * 4..(y * 4 + x + 1) * 4];
                assert_eq!(
                    p,
                    if x == 0 && y == 0 {
                        &[0, 255, 0, 255]
                    } else if x == 1 && y == 1 {
                        &[255, 0, 0, 255]
                    } else {
                        &[0, 0, 255, 255]
                    }
                );
            }
        }
        h.display.presentable.store(false, Ordering::Release);
        assert_eq!(h.read(&image), expected);
        h.producer
            .push(present(&image, crate::frame::Damage::None, 4));
        h.drain();
        let private = h.frame_image(2);
        assert_eq!(h.read(&fixed(private.clone())), expected);
        h.display.presentable.store(true, Ordering::Release);
        assert_eq!(h.read(&image), expected);
        h.producer
            .push(present(&image, crate::frame::Damage::None, 5));
        h.drain();
        let last = h.frame_image(3);
        assert_ne!(last.id(), private.id());
        assert_eq!(h.read(&fixed(last)), expected);
    }

    // spec: GFX-REC-5
    #[test]
    fn an_exhausted_swap_pool_waits_for_compositor_release() {
        let Some((device, queue)) = open() else {
            return;
        };
        let image = swap(4, 4);
        let (mut producer, consumer) = super::super::stream::channel(device.ring(1 << 20).unwrap());
        let display = Arc::new(Display::default());
        display.presentable.store(true, Ordering::Release);
        let (notify, frames) = mpsc::channel();
        let presenter = RecordingPresenter {
            display: display.clone(),
            notify: Some(notify),
        };
        let owner = std::thread::spawn(move || run(queue, consumer, Box::new(presenter)));
        producer.push(color(&image));
        for serial in 1..=4 {
            producer.push(clear(rect(0, 0, 4, 4), [0., 0., serial as f64 / 4., 1.]));
            producer.push(present(&image, crate::frame::Damage::Whole, serial));
        }
        for serial in 1..=3 {
            assert_eq!(frames.recv_timeout(TIMEOUT).unwrap(), serial);
        }
        assert!(frames.recv_timeout(Duration::from_millis(20)).is_err());
        drop(display.frames.lock().unwrap().pop_front());
        assert_eq!(frames.recv_timeout(TIMEOUT).unwrap(), 4);
        drop(producer);
        owner.join().unwrap();
    }

    // spec: GFX-PASS-2
    #[test]
    fn depth_is_transient_until_stored_and_becomes_transient_again() {
        let Some((device, queue)) = open() else {
            return;
        };
        let mut h = Harness::new(device, queue);
        let image = image(&h.device, 4, 4);
        let depth = Depth(Arc::new(backing::DepthBuffer::new(
            (4, 4),
            DepthFormat::Depth24Stencil8,
        )));
        h.producer.push(Command::Target {
            color: image.clone(),
            depth: Some(depth.clone()),
        });
        let clear_depth = || Command::Clear {
            area: rect(0, 0, 4, 4),
            color: Some([1.; 4]),
            depth: Some(1.),
            stencil: Some(0),
        };
        h.producer.push(clear_depth());
        h.producer.push(Command::DiscardDepth(depth.clone()));
        h.producer
            .push(present(&image, crate::frame::Damage::Whole, 1));
        h.drain();
        assert!(h.state.passes[0].transient);
        assert_eq!(
            h.state.passes[0].depth,
            Some((Load::Clear(1.), Store::DontCare))
        );
        assert_eq!(
            h.state.passes[0].stencil,
            Some((Load::Clear(0), Store::DontCare))
        );
        h.producer.push(clear_depth());
        h.producer
            .push(present(&image, crate::frame::Damage::Whole, 2));
        h.drain();
        assert!(!h.state.passes[1].transient);
        assert_eq!(
            h.state.passes[1].depth,
            Some((Load::Clear(1.), Store::Store))
        );
        h.producer.push(clear_depth());
        h.producer.push(Command::DiscardDepth(depth));
        h.producer
            .push(present(&image, crate::frame::Damage::Whole, 3));
        h.drain();
        assert!(h.state.passes[2].transient);
    }
}

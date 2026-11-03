//! The GPU owner drains one stream, plans closed runs and encodes their
//! original chunks. Run storage and chunks are reused after warm-up; spans
//! stay with the encoder until its submission completes.

use super::resources::Storage;
use super::stream::{Binding, Bytes, Chunk, Command, Consumer, Draw, UploadSource};
use super::{Depth, Image, Target, backing, pass};
use crate::frame::{Base, Frame, Presenter, SwapImage};
use adv_gpu::encode::{Bind, BufferCopy, ColorAttachment, DepthAttachment, ImageCopy};
use adv_gpu::resource::BufferSlice;
use adv_gpu::{Encoder, Extent, Queue, Rect, RenderPassDesc, Span, Token};
use std::collections::VecDeque;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

pub(super) struct Owner {
    join: Option<std::thread::JoinHandle<()>>,
    pub failure: Arc<OnceLock<String>>,
    pub wake: Option<super::stream::Waker>,
}
impl Owner {
    #[cfg(test)]
    pub(super) fn unstarted() -> Self {
        Self {
            wake: None,
            join: None,
            failure: Arc::new(OnceLock::new()),
        }
    }
    pub fn start(
        queue: Queue,
        commands: Consumer,
        presenter: Box<dyn Presenter>,
    ) -> Result<Self, super::super::Error> {
        let wake = Some(commands.waker());
        let failure = Arc::new(OnceLock::new());
        let owner_failure = failure.clone();
        let join = std::thread::Builder::new()
            .name("GPU owner".into())
            .spawn(move || {
                if let Err(error) = run(queue, commands, presenter) {
                    let _ = owner_failure.set(error.to_string());
                }
            })
            .map_err(|error| super::super::Error::Unavailable(error.to_string()))?;
        Ok(Self {
            wake,
            join: Some(join),
            failure,
        })
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        // Gpu drops its producer before joining the queue owner.
        #[cfg(test)]
        if self.join.is_none() {
            return;
        }
        self.join
            .take()
            .unwrap()
            .join()
            .expect("GPU owner panicked");
    }
}
fn high_priority() -> adv_gpu::Result<()> {
    #[cfg(target_os = "android")]
    unsafe {
        // PRIORITY_DISPLAY, matching HWUI; leave core placement to the scheduler.
        if libc::setpriority(libc::PRIO_PROCESS, 0, -4) != 0 {
            return Err(adv_gpu::Error::Unavailable(format!(
                "GPU owner priority: {}",
                std::io::Error::last_os_error()
            )));
        }
    }
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

#[derive(Default)]
struct Run {
    ops: Vec<pass::Op>,
    /// Each summary points straight into a retained chunk's commands.
    locations: Vec<(usize, usize)>,
    passes: Vec<pass::Pass>,
}
struct State {
    wake: super::stream::Waker,
    queue: Queue,
    encoder: Option<Encoder>,
    presenter: Box<dyn Presenter>,
    target: Option<Target>,
    depths: Vec<Depth>,
    run: Run,
    chunks: VecDeque<Chunk>,
    cursor: (usize, usize),
    completed: Token,
    uploads: VecDeque<(Token, super::Staged)>,
    pending_uploads: usize,
    #[cfg(test)]
    passes: Vec<PassRecord>,
}
#[cfg(test)]
struct PassRecord {
    area: Rect,
    color: (adv_gpu::encode::Load<[f64; 4]>, adv_gpu::encode::Store),
    depth: Option<(adv_gpu::encode::Load<f32>, adv_gpu::encode::Store)>,
    stencil: Option<(adv_gpu::encode::Load<u32>, adv_gpu::encode::Store)>,
    transient: bool,
}

pub(super) fn run(
    queue: Queue,
    commands: Consumer,
    presenter: Box<dyn Presenter>,
) -> adv_gpu::Result<()> {
    high_priority()?;
    let mut state = State::new(queue, presenter, commands.waker());
    let wake = commands.waker();
    state.presenter.service(&mut state.queue, Some(&wake));
    loop {
        match commands.next() {
            super::stream::Next::Closed => break,
            super::stream::Next::Woken => {
                state.close(pass::NextDepth::Keep)?;
                if state.encoder.is_some() {
                    state.submit(None)?;
                }
                state.presenter.service(&mut state.queue, Some(&wake));
            }
            super::stream::Next::Chunk(chunk) => {
                #[cfg(feature = "test-support")]
                let _submissions = commands.submissions();
                state.chunks.push_back(chunk);
                #[cfg(target_vendor = "apple")]
                objc2::rc::autoreleasepool(|_| state.drain(&commands))?;
                #[cfg(not(target_vendor = "apple"))]
                state.drain(&commands)?;
                state.presenter.service(&mut state.queue, None);
            }
        }
    }
    state.close(pass::NextDepth::Keep)?;
    state.recycle(state.cursor.0, &commands);
    if state.encoder.is_some() {
        state.submit(None)?;
    }
    state.poll(&commands)?;
    Ok(())
}
impl State {
    fn new(queue: Queue, presenter: Box<dyn Presenter>, wake: super::stream::Waker) -> Self {
        Self {
            wake,
            queue,
            encoder: None,
            presenter,
            target: None,
            depths: Vec::new(),
            run: Run::default(),
            chunks: VecDeque::new(),
            cursor: (0, 0),
            completed: Token(0),
            uploads: VecDeque::new(),
            pending_uploads: 0,
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
        let mut blocked = false;
        loop {
            if blocked {
                if self.encoder.is_some() {
                    self.submit(None)?;
                }
                self.presenter.service(&mut self.queue, Some(&self.wake));
            }
            let result = backing::resolve(
                image,
                &mut self.queue,
                &mut *self.presenter,
                &mut self.encoder,
            );
            match result {
                Err(adv_gpu::Error::Busy) => {
                    if blocked && !self.wake.wait() {
                        return Err(adv_gpu::Error::DeviceLost(
                            "execution closed during swap acquisition".into(),
                        ));
                    }
                    blocked = true;
                }
                Ok(resolved) => {
                    self.encoder();
                    return Ok(resolved);
                }
                Err(error) => return Err(error),
            }
        }
    }

    fn poll(&mut self, consumer: &Consumer) -> adv_gpu::Result<()> {
        let completed = self.queue.poll()?;
        while self
            .uploads
            .front()
            .is_some_and(|(token, _)| *token <= completed)
        {
            self.uploads.pop_front();
        }
        if completed > self.completed {
            self.completed = completed;
            consumer.retired(completed);
        }
        Ok(())
    }
    fn close(&mut self, next: pass::NextDepth) -> adv_gpu::Result<()> {
        if self.run.ops.is_empty() {
            return Ok(());
        }
        let mut run = std::mem::take(&mut self.run);
        pass::plan_into(
            &run.ops,
            self.target.as_ref().unwrap().depth.is_some(),
            next,
            &mut run.passes,
        );
        let result = self.render(&run);
        run.ops.clear();
        run.locations.clear();
        run.passes.clear();
        self.run = run;
        result
    }
    fn retain(&mut self, chunk: usize) {
        if !self.chunks[chunk].spans.is_empty() {
            self.encoder();
            for span in self.chunks[chunk].spans.drain(..) {
                self.encoder.as_mut().unwrap().retain(span);
            }
        }
    }
    fn recycle(&mut self, count: usize, consumer: &Consumer) {
        debug_assert!(self.run.ops.is_empty());
        for _ in 0..count {
            self.retain(0);
            consumer.recycle(self.chunks.pop_front().unwrap());
        }
        self.cursor.0 -= count;
    }
    fn drain(&mut self, consumer: &Consumer) -> adv_gpu::Result<()> {
        while self.cursor.0 < self.chunks.len() {
            let (chunk, at) = self.cursor;
            if at == self.chunks[chunk].commands.len() {
                self.cursor = (chunk + 1, 0);
                if self.run.ops.is_empty() {
                    self.recycle(self.cursor.0, consumer);
                }
                continue;
            }
            let current = &self.chunks[chunk];
            let op = match &current.commands[at] {
                Command::Clear {
                    area,
                    color,
                    depth,
                    stencil,
                } => Some(pass::Op {
                    area: *area,
                    kind: pass::OpKind::Clear {
                        color: *color,
                        depth: *depth,
                        stencil: *stencil,
                    },
                }),
                Command::Draw(n) => {
                    let d = &current.draws[*n as usize];
                    Some(pass::Op {
                        area: d.area,
                        kind: pass::OpKind::Draw {
                            writes_color: d.writes_color,
                            uses_depth: d.uses_depth,
                            writes_depth: d.writes_depth,
                            opaque: d.opaque,
                        },
                    })
                }
                _ => None,
            };
            if let Some(op) = op {
                self.run.ops.push(op);
                self.run.locations.push(self.cursor);
                self.cursor.1 += 1;
                continue;
            }
            if !self.run.ops.is_empty() {
                while let Some(chunk) = consumer.try_next() {
                    self.chunks.push_back(chunk);
                }
                let next = next_depth(
                    self.target.as_ref().unwrap().depth.as_ref(),
                    self.chunks
                        .iter()
                        .skip(chunk)
                        .enumerate()
                        .flat_map(|(n, c)| {
                            c.commands
                                .iter()
                                .skip(if n == 0 { at } else { 0 })
                                .map(|command| (command, c.draws.as_slice()))
                        }),
                );
                self.close(next)?;
                self.recycle(chunk, consumer);
            }
            let (chunk, at) = self.cursor;
            let command = std::mem::replace(&mut self.chunks[chunk].commands[at], Command::Submit);
            if matches!(
                command,
                Command::Present { .. }
                    | Command::PresentSwap { .. }
                    | Command::Readback { .. }
                    | Command::Submit
            ) {
                debug_assert_eq!(at + 1, self.chunks[chunk].commands.len());
                self.retain(chunk);
            }
            self.command(command, chunk)?;
            self.cursor.1 += 1;
        }
        self.poll(consumer)
    }
    fn command(&mut self, command: Command, chunk: usize) -> adv_gpu::Result<()> {
        match command {
            Command::Target { color, depth } => {
                self.target = Some(Target {
                    image: color,
                    depth,
                })
            }
            Command::Upload {
                source,
                pitch,
                image,
                mip,
                rect,
            } => {
                let (image, _, _) = self.image(&image)?;
                let source_bytes = match &source {
                    UploadSource::Ring(bytes) => slice(*bytes, &self.chunks[chunk].spans),
                    UploadSource::Staged(staged) => staged.buffer().slice(0..staged.len() as u64),
                };
                self.encoder.as_mut().unwrap().copy_buffer_to_image(
                    BufferCopy {
                        buffer: source_bytes.buffer,
                        offset: source_bytes.offset,
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
                if let UploadSource::Staged(staged) = source {
                    // Submit assigns the completion token to these pending uploads.
                    self.uploads.push_back((Token(u64::MAX), staged));
                    self.pending_uploads += 1;
                }
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
                let token = self.submit(None)?;
                self.queue.on_complete(token, done);
            }
            Command::Submit => {
                self.submit(None)?;
            }
            Command::PresentSwap {
                image,
                damage,
                serial,
                video,
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
                let ready = self.submit(presentable)?;
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
                        video,
                        published_at: Instant::now(),
                    },
                );
                self.end_frame();
            }
            Command::Present {
                base,
                video,
                serial,
            } => {
                if self.encoder.is_some() {
                    self.submit(None)?;
                }
                self.presenter.present(
                    &mut self.queue,
                    Frame {
                        serial,
                        base,
                        video,
                        published_at: Instant::now(),
                    },
                );
                self.end_frame();
            }
            Command::Clear { .. } | Command::Draw(_) => unreachable!(),
        }
        Ok(())
    }
    fn end_frame(&mut self) {
        for depth in self.depths.drain(..) {
            depth.0.owner.lock().unwrap().end_frame();
        }
    }
    fn submit(&mut self, image: Option<&mut adv_gpu::PresentImage>) -> adv_gpu::Result<Token> {
        let encoder = self
            .encoder
            .take()
            .unwrap_or_else(|| self.queue.encoder("submission boundary"));
        let token = if let Some(image) = image {
            self.queue.submit_present(encoder, &mut [image])?
        } else {
            self.queue.submit(encoder)?
        };
        for (pending, _) in self.uploads.iter_mut().rev().take(self.pending_uploads) {
            *pending = token;
        }
        self.pending_uploads = 0;
        Ok(token)
    }
    fn render(&mut self, run: &Run) -> adv_gpu::Result<()> {
        let target = self.target.as_ref().unwrap().clone();
        let (_, color, _) = self.image(&target.image)?;
        let depth_image = target
            .depth
            .as_ref()
            .map(|depth| {
                if !self
                    .depths
                    .iter()
                    .any(|other| Arc::ptr_eq(&other.0, &depth.0))
                {
                    self.depths.push(depth.clone());
                }
                depth
                    .0
                    .owner
                    .lock()
                    .unwrap()
                    .attachment(depth, self.queue.device(), &run.passes)
            })
            .transpose()?;
        // Swap acquisition can repair age with a copy, so it precedes the
        // render pass. Each bind below resolves the current view in place.
        for &(chunk, at) in &run.locations {
            if let Command::Draw(n) = &self.chunks[chunk].commands[at] {
                let range = self.chunks[chunk].draws[*n as usize].bindings.clone();
                for index in range {
                    if let Binding::Sampled(image) = &self.chunks[chunk].bindings[index as usize].1
                        && matches!(image.storage, Storage::Swap(_))
                    {
                        let image = image.clone();
                        self.image(&image)?;
                    }
                }
            }
        }
        self.encoder();
        for plan in &run.passes {
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
            let mut pass = self.encoder.as_mut().unwrap().render_pass(&desc);
            let mut viewport = None;
            let mut scissor = None;
            let mut stencil = None;
            let mut blend = None;
            for &(chunk, at) in &run.locations[plan.ops.clone()] {
                let chunk = &self.chunks[chunk];
                let Command::Draw(n) = chunk.commands[at] else {
                    continue;
                };
                let d = &chunk.draws[n as usize];
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
                for (slot, binding) in
                    &chunk.bindings[d.bindings.start as usize..d.bindings.end as usize]
                {
                    match binding {
                        Binding::Uniform(bytes) => {
                            pass.bind(*slot, Bind::Uniform(slice(*bytes, &chunk.spans)))
                        }
                        Binding::Storage(bytes) => {
                            pass.bind(*slot, Bind::StorageBuffer(slice(*bytes, &chunk.spans)))
                        }
                        Binding::Buffer(buffer, range) => {
                            pass.bind(*slot, Bind::StorageBuffer(buffer.slice(range.clone())))
                        }
                        Binding::Sampler(sampler) => pass.bind(*slot, Bind::Sampler(sampler)),
                        Binding::Sampled(image) => match &image.storage {
                            Storage::Fixed { view, .. } => pass.bind(*slot, Bind::Sampled(view)),
                            Storage::Swap(swap) => pass.bind(
                                *slot,
                                Bind::Sampled(swap.owner.lock().unwrap().current_image().view()),
                            ),
                        },
                    }
                }
                if let Some((index, format)) = &d.index {
                    let bytes = match index {
                        Binding::Uniform(bytes) | Binding::Storage(bytes) => {
                            slice(*bytes, &chunk.spans)
                        }
                        Binding::Buffer(buffer, range) => buffer.slice(range.clone()),
                        _ => unreachable!("index binding"),
                    };
                    pass.set_index_buffer(bytes, *format);
                    pass.draw_indexed(d.vertices.clone(), d.base_vertex, d.instances.clone());
                } else {
                    pass.draw(d.vertices.clone(), d.instances.clone());
                }
            }
        }
        Ok(())
    }
}
fn slice(bytes: Bytes, spans: &[Span]) -> BufferSlice<'_> {
    let span = &spans[bytes.span as usize];
    let offset = span.offset() + u64::from(bytes.offset);
    span.buffer().slice(offset..offset + u64::from(bytes.len))
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
    fn service(&mut self, _: &mut Queue, _: Option<&crate::frame::Waker>) {}
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
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Mutex, mpsc};
    use std::time::Duration;

    const TIMEOUT: Duration = Duration::from_secs(20);

    // spec: GFX-SAMPLER-4
    #[test]
    fn staged_uploads_are_retained_until_their_submission_completes() {
        let Some((mut gpu, queue, consumer)) = super::super::resources::tests::open() else {
            return;
        };
        let image = gpu.image(1, 1).unwrap();
        let staged = gpu
            .stage(4, 1, |write| {
                write(0, &[11, 22, 33, 44]);
                Ok::<(), ()>(())
            })
            .unwrap()
            .unwrap();
        let mut state = State::new(queue, Box::new(Headless), consumer.waker());
        state
            .command(
                Command::Upload {
                    source: UploadSource::Staged(staged),
                    pitch: 4,
                    image,
                    mip: 0,
                    rect: rect(0, 0, 1, 1),
                },
                0,
            )
            .unwrap();
        state.poll(&consumer).unwrap();
        assert_eq!(state.uploads.len(), 1);
        assert_eq!(state.pending_uploads, 1);
        let token = state.submit(None).unwrap();
        assert_eq!(state.uploads[0].0, token);
        assert_eq!(state.pending_uploads, 0);
        assert!(state.queue.wait(token, TIMEOUT).unwrap());
        state.poll(&consumer).unwrap();
        assert!(state.uploads.is_empty());
    }

    fn open() -> Option<(Device, Queue)> {
        match Device::open(&adv_gpu::queue::DeviceOptions {
            software: !cfg!(any(target_vendor = "apple", target_os = "android")),
            validation: !cfg!(target_os = "android"),
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
        Command::PresentSwap {
            image: image.clone(),
            damage,
            serial,
            video: Arc::from([]),
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
        fn service(&mut self, queue: &mut Queue, wake: Option<&crate::frame::Waker>) {
            for reply in wake.map_or_else(Vec::new, |w| w.captures()) {
                let encoder = queue.encoder("owner poke");
                queue.submit(encoder).unwrap();
                reply.send(Err("owner serviced".into())).unwrap();
            }
        }
    }
    struct Harness {
        device: Device,
        state: State,
        producer: super::super::stream::Producer,
        consumer: Consumer,
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
                consumer.waker(),
            );
            Self {
                device,
                state,
                producer,
                consumer,
                display,
            }
        }
        fn drain(&mut self) {
            while let Some(chunk) = self.consumer.try_next() {
                self.state.chunks.push_back(chunk);
                self.state.drain(&self.consumer).unwrap();
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
            let Base::Gpu { image, .. } = &frames[n].base else {
                panic!("GPU frame")
            };
            match image {
                SwapImage::Private(i) => i.clone(),
                SwapImage::Presentable(i) => i.image().clone(),
            }
        }
    }
    fn pipeline(device: &Device) -> adv_gpu::RenderPipeline {
        pipeline_source(
            device,
            r#"
struct Color {rgba:vec4<f32>}
@group(0) @binding(0) var<uniform> color:Color;
@vertex fn vertex_main(@builtin(vertex_index) n:u32)->@builtin(position) vec4<f32> {
    let xy=array<vec2<f32>,3>(vec2<f32>(-1.0,-1.0),vec2<f32>(3.0,-1.0),vec2<f32>(-1.0,3.0));
    return vec4<f32>(xy[n],0.0,1.0);
}
@fragment fn fragment_main()->@location(0) vec4<f32> {return color.rgba;}
"#,
            vec![p::Binding {
                slot: 0,
                kind: p::BindingKind::UniformBuffer,
                stages: p::Stages::FRAGMENT,
            }],
        )
    }
    fn pipeline_source(
        device: &Device,
        source: &str,
        bindings: Vec<p::Binding>,
    ) -> adv_gpu::RenderPipeline {
        let shader = device
            .create_shader(ShaderDesc {
                identity: 17,
                label: "owner draw",
                source: Source::Wgsl(source),
            })
            .unwrap();
        device
            .create_render_pipeline(
                &p::RenderPipelineDesc {
                    vertex: shader.entry("vertex_main", Stage::Vertex).unwrap(),
                    fragment: Some(shader.entry("fragment_main", Stage::Fragment).unwrap()),
                    bindings,
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
            instances: 0..1,
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
    fn software_and_d3d9_presents_keep_one_stream_order_and_video() {
        let Some((device, queue)) = open() else {
            return;
        };
        let mut h = Harness::new(device, queue);
        let image = image(&h.device, 4, 4);
        let video: Arc<[crate::frame::VideoPlane]> = Arc::from([]);
        for serial in 1..=6 {
            if serial % 2 == 0 {
                h.producer.push(color(&image));
                h.producer.push(clear(rect(0, 0, 4, 4), [1., 0., 0., 1.]));
                let mut command = present(&image, crate::frame::Damage::Whole, serial);
                let Command::PresentSwap { video: planes, .. } = &mut command else {
                    unreachable!()
                };
                *planes = video.clone();
                h.producer.push(command);
            } else {
                let slot = crate::frame::Slot::new(
                    &h.device,
                    4,
                    4,
                    16,
                    crate::surface::PixelFormat::Bgra32,
                )
                .unwrap();
                h.producer.push(Command::Present {
                    base: Base::Software {
                        slot,
                        format: crate::surface::PixelFormat::Bgra32,
                        palette: None,
                        damage: crate::frame::Damage::Whole,
                    },
                    video: video.clone(),
                    serial,
                });
            }
        }
        h.drain();
        let frames = h.display.frames.lock().unwrap();
        assert_eq!(frames.len(), 6);
        for (i, frame) in frames.iter().enumerate() {
            assert_eq!(frame.serial, i as u64 + 1);
            assert_eq!(matches!(frame.base, Base::Software { .. }), i % 2 == 0);
            assert!(Arc::ptr_eq(&frame.video, &video));
        }
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
                source: UploadSource::Ring(bytes),
                pitch: 1024,
                image: image.clone(),
                mip: 0,
                rect: rect(0, 0, 256, 128),
            });
            send.send(1).unwrap();
            let bytes = producer.bytes(128 << 10, 4, |bytes| bytes.fill(2));
            send.send(2).unwrap();
            producer.push(Command::Upload {
                source: UploadSource::Ring(bytes),
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
        owner.join().unwrap().unwrap();
    }

    // spec: GFX-REC-5
    #[test]
    #[cfg_attr(
        not(any(target_vendor = "apple", target_os = "android")),
        ignore = "presentable pools exist only on Apple and Android"
    )]
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
    #[cfg_attr(
        not(any(target_vendor = "apple", target_os = "android")),
        ignore = "presentable pools exist only on Apple and Android"
    )]
    fn an_exhausted_swap_pool_services_pokes_and_waits_for_compositor_release() {
        let Some((device, queue)) = open() else {
            return;
        };
        let image = swap(4, 4);
        let (mut producer, consumer) = super::super::stream::channel(device.ring(1 << 20).unwrap());
        let wake = consumer.waker();
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
        assert_eq!(
            wake.capture()
                .recv_timeout(TIMEOUT)
                .unwrap()
                .err()
                .as_deref(),
            Some("owner serviced")
        );
        assert!(frames.recv_timeout(Duration::from_millis(20)).is_err());
        drop(display.frames.lock().unwrap().pop_front());
        wake.poke();
        assert_eq!(frames.recv_timeout(TIMEOUT).unwrap(), 4);
        drop(producer);
        owner.join().unwrap().unwrap();
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

    // spec: GFX-INPUT-7, GFX-INPUT-1
    #[test]
    fn indexed_and_nonindexed_draws_fetch_each_instances_stream_data() {
        let Some((device, queue)) = open() else {
            return;
        };
        let mut h = Harness::new(device, queue);
        let pipeline = pipeline_source(
            &h.device,
            r#"
struct Instance {offset:vec4<f32>,color:vec4<f32>}
@group(3) @binding(0) var<storage,read> positions:array<vec2<f32>>;
@group(3) @binding(1) var<storage,read> instances:array<Instance>;
struct Output {@builtin(position) position:vec4<f32>,@location(0) color:vec4<f32>}
@vertex fn vertex_main(@builtin(vertex_index) v:u32,@builtin(instance_index) i:u32)->Output {
    var out:Output;
    out.position=vec4<f32>(positions[v]*vec2<f32>(0.5,1.0)+instances[i].offset.xy,0.0,1.0);
    out.color=instances[i].color;return out;
}
@fragment fn fragment_main(in:Output)->@location(0) vec4<f32> {return in.color;}
"#,
            [0, 1]
                .map(|slot| p::Binding {
                    slot,
                    kind: p::BindingKind::StorageBuffer { writable: false },
                    stages: p::Stages::VERTEX,
                })
                .to_vec(),
        );
        let positions: [f32; 12] = [-1., -1., 1., -1., 1., 1., -1., -1., 1., 1., -1., 1.];
        let instances: [f32; 16] = [
            -0.5, 0., 0., 0., 1., 0., 0., 1., 0.5, 0., 0., 0., 0., 0., 1., 1.,
        ];
        for indexed in [false, true] {
            let target = image(&h.device, 4, 4);
            h.producer.push(color(&target));
            h.producer.push(clear(rect(0, 0, 4, 4), [0.; 4]));
            let align = h.device.caps().limits.storage_offset_alignment.max(32);
            let instance_at = 48u32.next_multiple_of(align);
            let index_at = instance_at + 64;
            let data = h.producer.bytes(index_at + 24, align, |bytes| {
                bytes[..48].copy_from_slice(bytemuck::cast_slice(&positions));
                bytes[instance_at as usize..index_at as usize]
                    .copy_from_slice(bytemuck::cast_slice(&instances));
                bytes[index_at as usize..]
                    .copy_from_slice(bytemuck::cast_slice(&[0u32, 1, 2, 3, 4, 5]));
            });
            let part = |offset, len| Bytes {
                span: data.span,
                offset: data.offset + offset,
                len,
            };
            let mut d = draw(pipeline.clone(), rect(0, 0, 4, 4));
            d.vertices = 0..6;
            d.instances = 0..2;
            if indexed {
                d.index = Some((
                    Binding::Storage(part(index_at, 24)),
                    adv_gpu::encode::IndexFormat::U32,
                ));
            }
            h.producer.draw(
                d,
                [
                    (0, Binding::Storage(part(0, 48))),
                    (1, Binding::Storage(part(instance_at, 64))),
                ],
            );
            let pixels = h.read(&target);
            for y in 0..4 {
                for x in 0..4 {
                    assert_eq!(
                        &pixels[(y * 4 + x) * 4..(y * 4 + x + 1) * 4],
                        if x < 2 {
                            &[0, 0, 255, 255]
                        } else {
                            &[255, 0, 0, 255]
                        },
                        "indexed={indexed}, ({x}, {y})"
                    );
                }
            }
        }
    }

    // spec: GFX-PASS-2
    #[test]
    fn a_queued_depth_clear_avoids_storing_the_preceding_chunk() {
        let Some((device, queue)) = open() else {
            return;
        };
        let mut h = Harness::new(device, queue);
        let image = image(&h.device, 4, 4);
        let depth = Depth(Arc::new(backing::DepthBuffer::new(
            (4, 4),
            DepthFormat::Depth24Stencil8,
        )));
        let clear_depth = || Command::Clear {
            area: rect(0, 0, 4, 4),
            color: Some([1.; 4]),
            depth: Some(1.),
            stencil: Some(0),
        };
        h.producer.push(Command::Target {
            color: image.clone(),
            depth: Some(depth.clone()),
        });
        h.producer.push(clear_depth());
        h.producer.push(Command::Submit);
        h.producer.push(clear_depth());
        h.producer.push(Command::DiscardDepth(depth));
        h.producer
            .push(present(&image, crate::frame::Damage::Whole, 1));
        h.drain();
        assert_eq!(h.state.passes.len(), 2);
        for pass in &h.state.passes {
            assert_eq!(pass.depth, Some((Load::Clear(1.), Store::DontCare)));
            assert_eq!(pass.stencil, Some((Load::Clear(0), Store::DontCare)));
            assert!(pass.transient);
        }
    }
}

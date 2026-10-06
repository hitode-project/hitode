use std::collections::{HashMap, VecDeque};
use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicU64, AtomicUsize, Ordering},
};

use super::*;

static NEXT_RESOURCE: AtomicU64 = AtomicU64::new(1);
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Bgra8Unorm;
const MAX_PIPELINES: usize = 512;
const MAX_UPLOAD: u64 = 16 * 1024 * 1024;
const MAX_NATIVE_FRAMES: usize = 8;
const MAX_READBACKS: usize = 8;

struct ReadbackReservation(Arc<AtomicUsize>);
impl Drop for ReadbackReservation {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Share this context with the native compositor, using the same queue. A frame
/// never requires a CPU readback to be sampled or copied into a window surface.
#[derive(Clone)]
pub struct NativeContext {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    queue: wgpu::Queue,
    id: u64,
    failure: Arc<OnceLock<String>>,
    gate: Arc<submission::Gate>,
}

impl NativeContext {
    pub fn check(&self) -> Result<(), Error> {
        match self.failure.get() {
            Some(error) => Err(Error::Device(error.clone())),
            None => Ok(()),
        }
    }
    pub fn submission(&self) -> Result<Submission<'_>, Error> {
        self.check()?;
        Ok(Submission {
            context: self,
            _lease: self.gate.enter()?,
        })
    }
    pub fn pause_submissions(&self) -> Result<SubmissionPause, Error> {
        self.gate.pause()
    }
    /// Install the existing execution wait object once during assembly.
    pub fn set_execution_wake(&self, wake: adv_sync::WakeEvent) -> Result<(), adv_sync::WakeEvent> {
        self.gate.set_wake(wake)
    }
}

/// A short nonblocking submission lease. The raw queue is never exposed, so
/// native consumers cannot accidentally bypass configuration exclusion.
pub struct Submission<'a> {
    context: &'a NativeContext,
    _lease: submission::Lease,
}
impl Submission<'_> {
    pub fn write_buffer(&self, buffer: &wgpu::Buffer, offset: u64, data: &[u8]) {
        self.context.queue.write_buffer(buffer, offset, data);
    }
    pub fn write_texture(
        &self,
        texture: wgpu::TexelCopyTextureInfo<'_>,
        data: &[u8],
        layout: wgpu::TexelCopyBufferLayout,
        size: wgpu::Extent3d,
    ) {
        self.context
            .queue
            .write_texture(texture, data, layout, size);
    }
    pub fn submit(
        &self,
        commands: impl IntoIterator<Item = wgpu::CommandBuffer>,
    ) -> wgpu::SubmissionIndex {
        self.context.queue.submit(commands)
    }
    pub fn present(&self, texture: wgpu::SurfaceTexture) {
        self.context.queue.present(texture);
    }
}

/// An owned color image, usable both as a texture and as a render target.
#[derive(Clone)]
pub struct Image {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    render_view: wgpu::TextureView,
    mip: u32,
    levels: u32,
    width: u32,
    height: u32,
    alpha: AlphaMode,
    context: u64,
    id: u64,
    lease: Option<Arc<()>>,
    frame_pins: Arc<AtomicUsize>,
    gate: Arc<submission::Gate>,
}
impl Image {
    pub fn width(&self) -> u32 {
        self.width
    }
    pub fn height(&self) -> u32 {
        self.height
    }
    pub fn alpha_mode(&self) -> AlphaMode {
        self.alpha
    }
    pub fn view(&self) -> &wgpu::TextureView {
        &self.view
    }
    pub fn mip_levels(&self) -> u32 {
        self.levels
    }
    /// A single-level view of the same allocation, sharing frame pins and lease.
    pub fn mip_level(&self, level: u32) -> Result<Self, Error> {
        if level >= self.levels {
            return Err(invalid("mip level is outside the image view"));
        }
        let mip = self.mip + level;
        let view = self.texture.create_view(&wgpu::TextureViewDescriptor {
            base_mip_level: mip,
            mip_level_count: Some(1),
            ..Default::default()
        });
        let mut image = self.clone();
        image.render_view = view.clone();
        image.view = view;
        image.mip = mip;
        image.levels = 1;
        image.width = (self.width >> level).max(1);
        image.height = (self.height >> level).max(1);
        Ok(image)
    }
    pub fn shares_storage(&self, other: &Self) -> bool {
        self.context == other.context && self.id == other.id
    }
    pub(crate) fn reusable(&self) -> bool {
        Arc::strong_count(self.lease.as_ref().unwrap()) == 1
            && self.frame_pins.load(Ordering::Acquire) == 0
    }
    fn writable(&self) -> Result<(), Error> {
        if self.frame_pins.load(Ordering::Acquire) != 0 {
            self.gate.wait_for_frame();
            // Release may precede waiter publication. Recheck after publishing
            // rather than sleeping with the final release already behind us.
            if self.frame_pins.load(Ordering::Acquire) != 0 {
                return Err(Error::Busy);
            }
        }
        Ok(())
    }
    pub(crate) fn same_context(&self, other: &Image) -> bool {
        self.context == other.context
    }
}
impl Drop for Image {
    fn drop(&mut self) {
        // Linearize the actual release before notification. Testing strong_count
        // before automatic field drop can miss two concurrent clone releases;
        // notifying before decrement can wake a retry while capacity is still full.
        drop(self.lease.take());
        if self.frame_pins.load(Ordering::Acquire) == 0 {
            self.gate.frame_available();
        }
    }
}

#[derive(Clone)]
pub struct Target {
    pub image: Image,
    depth: Option<Depth>,
}
impl Target {
    pub fn mip_level(&self, level: u32) -> Result<Self, Error> {
        if level != 0 && self.depth.is_some() {
            return Err(invalid("depth attachment has no color mip-level alias"));
        }
        let image = self.image.mip_level(level)?;
        Ok(Self {
            image,
            depth: self.depth.clone(),
        })
    }
    /// Declare the interpretation of an image before publishing its front-end view.
    pub fn with_alpha_mode(mut self, alpha: AlphaMode) -> Self {
        self.image.alpha = alpha;
        self
    }
    pub(crate) fn has_depth(&self) -> bool {
        self.depth.is_some()
    }
    pub fn depth(&self) -> Option<&Depth> {
        self.depth.as_ref()
    }
    /// Attachment views must have equal dimensions on the common GPU backend.
    /// Reject mismatches before encoding; never hide a resize or per-draw copy.
    pub fn with_depth(mut self, depth: Option<Depth>) -> Result<Self, Error> {
        if depth.as_ref().is_some_and(|d| {
            d.context() != self.image.context
                || d.width() != self.image.width
                || d.height() != self.image.height
        }) {
            return Err(invalid(
                "depth attachment context or dimensions do not match",
            ));
        }
        self.depth = depth;
        Ok(self)
    }
}

struct FramePin {
    pins: Arc<AtomicUsize>,
    gate: Arc<submission::Gate>,
}
impl FramePin {
    fn new(image: &Image) -> Result<Arc<Self>, Error> {
        image
            .frame_pins
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |pins| {
                pins.checked_add(1)
            })
            .map_err(|_| Error::Busy)?;
        Ok(Arc::new(Self {
            pins: image.frame_pins.clone(),
            gate: image.gate.clone(),
        }))
    }
}
impl Drop for FramePin {
    fn drop(&mut self) {
        if self.pins.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.gate.frame_available();
        }
    }
}

/// Immutable GPU snapshot of one presentation. Copies are texture-to-texture;
/// retaining this frame cannot observe the producer's next render target writes.
#[derive(Clone)]
pub struct NativeFrame {
    pub image: Image,
    pub serial: u64,
    context: NativeContext,
    _pin: Arc<FramePin>,
}
impl NativeFrame {
    pub fn context(&self) -> &NativeContext {
        &self.context
    }
}

/// Explicit readback ownership. Normal rendering/presentation never maps buffers.
/// Completion is delivered once through the common SPSC primitive and wake event.
pub struct Readback {
    buffer: wgpu::Buffer,
    completion: adv_sync::spsc::Consumer<Result<Vec<u8>, Error>>,
    pub width: u32,
    pub height: u32,
    context: NativeContext,
    submission: wgpu::SubmissionIndex,
    job_taken: bool,
}
impl Readback {
    /// Cancel an unconsumed transfer during owner teardown. Unmapping cancels
    /// its pending map; Poll only delivers ready callbacks and never waits.
    pub fn cancel(&self) {
        self.buffer.unmap();
        let _ = self.context.device.poll(wgpu::PollType::Poll);
    }
    pub fn poll(&mut self) -> Option<Result<Vec<u8>, Error>> {
        self.completion.pop()
    }

    /// Submit this once to the shared backend service. It never runs guest code
    /// and must never execute on UI or the machine's execution thread. The fence
    /// is this transfer's submission, not a continually moving latest frame.
    pub fn take_completion_job(&mut self) -> Option<Box<dyn FnOnce() + Send + 'static>> {
        if std::mem::replace(&mut self.job_taken, true) {
            return None;
        }
        let context = self.context.clone();
        let submission = self.submission.clone();
        Some(Box::new(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                context.device.poll(wgpu::PollType::Wait {
                    submission_index: Some(submission),
                    timeout: Some(std::time::Duration::from_secs(10)),
                })
            }));
            let failure = match result {
                Ok(Ok(_)) => None,
                Ok(Err(error)) => Some(error.to_string()),
                Err(_) => Some("GPU transfer completion panicked".into()),
            };
            if let Some(error) = failure {
                // A stalled/lost device is a failure, never a successful empty
                // readback. Destruction fails outstanding maps and invokes the
                // existing one-shot completion producer with its wake event.
                let _ = context
                    .failure
                    .set(format!("GPU transfer fence failed: {error}"));
                context.device.destroy();
                let _ = context.device.poll(wgpu::PollType::Poll);
            }
        }))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct PipelineKey {
    topology: Topology,
    cull: Cull,
    color_blend: Option<Blend>,
    alpha_blend: Option<Blend>,
    depth: Option<Compare>,
    depth_write: bool,
    depth_format: Option<DepthFormat>,
    stencil: Option<Stencil>,
    color_write: u8,
}

/// One graphics backend. Commands are submitted without waiting on the execution
/// thread. The shell may poll completions and sample NativeFrame on this context.
pub struct Gpu {
    context: NativeContext,
    shader: wgpu::ShaderModule,
    layout: wgpu::PipelineLayout,
    bindings: wgpu::BindGroupLayout,
    pipelines: HashMap<PipelineKey, wgpu::RenderPipeline>,
    pipeline_order: VecDeque<PipelineKey>,
    pipeline_limit: usize,
    samplers: HashMap<Sampler, wgpu::Sampler>,
    white: Image,
    upload: wgpu::Buffer,
    upload_capacity: u64,
    uniform: wgpu::Buffer,
    stats: Stats,
    frames: Vec<Image>,
    readbacks: Arc<AtomicUsize>,
    readback_limit: usize,
}

impl Gpu {
    /// Request a real host adapter. `fallback` requests a software adapter; an
    /// unavailable adapter is an error, never a no-op renderer.
    pub async fn request(fallback: bool) -> Result<Self, Error> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        Self::request_from(instance, None, fallback).await
    }

    /// The platform shell creates its native surface first. This request chooses
    /// an adapter capable of presenting to that surface, using the same device
    /// and Queue for rendering and composition. No platform types enter here.
    pub async fn request_presenting(
        instance: wgpu::Instance,
        surface: &wgpu::Surface<'_>,
        fallback: bool,
    ) -> Result<Self, Error> {
        Self::request_from(instance, Some(surface), fallback).await
    }

    async fn request_from(
        instance: wgpu::Instance,
        surface: Option<&wgpu::Surface<'_>>,
        fallback: bool,
    ) -> Result<Self, Error> {
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: fallback,
                compatible_surface: surface,
                ..Default::default()
            })
            .await
            .map_err(|error| Error::Unavailable(error.to_string()))?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("adv neutral graphics"),
                required_limits: wgpu::Limits::downlevel_defaults()
                    .using_resolution(adapter.limits()),
                ..Default::default()
            })
            .await
            .map_err(|error| Error::Unavailable(error.to_string()))?;
        let failure = Arc::new(OnceLock::<String>::new());
        let uncaptured = failure.clone();
        device.on_uncaptured_error(Arc::new(move |error: wgpu::Error| {
            let _ = uncaptured.set(error.to_string());
        }));
        let lost = failure.clone();
        device.set_device_lost_callback(move |reason, description| {
            let _ = lost.set(format!("graphics device lost ({reason:?}): {description}"));
        });
        let context = NativeContext {
            instance,
            adapter,
            device,
            queue,
            id: NEXT_RESOURCE.fetch_add(1, Ordering::Relaxed),
            failure,
            gate: submission::Gate::new().map_err(|error| Error::Unavailable(error.to_string()))?,
        };
        let error_scope = context
            .device
            .push_error_scope(wgpu::ErrorFilter::Validation);
        let shader = context
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("adv fixed drawing"),
                source: wgpu::ShaderSource::Wgsl(include_str!("fixed.wgsl").into()),
            });
        let bindings = context
            .device
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("adv drawing bindings"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: wgpu::BufferSize::new(16),
                        },
                        count: None,
                    },
                ],
            });
        let layout = context
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("adv fixed layout"),
                bind_group_layouts: &[Some(&bindings)],
                immediate_size: 0,
            });
        let uniform = context.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("adv draw parameters"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let upload = context.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("adv vertex upload"),
            size: 4096,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let white = Self::make_image(&context, 1, 1, 1);
        context.submission()?.write_texture(
            white.texture.as_image_copy(),
            &[255; 4],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(4),
                rows_per_image: Some(1),
            },
            extent(1, 1),
        );
        if let Some(error) = error_scope.pop().await {
            return Err(Error::Device(error.to_string()));
        }
        Ok(Self {
            context,
            shader,
            layout,
            bindings,
            pipelines: HashMap::new(),
            pipeline_order: VecDeque::new(),
            pipeline_limit: MAX_PIPELINES,
            samplers: HashMap::new(),
            white,
            upload,
            upload_capacity: 4096,
            uniform,
            stats: Stats {
                vertex_buffer_allocations: 1,
                ..Stats::default()
            },
            frames: Vec::new(),
            readbacks: Arc::new(AtomicUsize::new(0)),
            readback_limit: MAX_READBACKS,
        })
    }

    pub fn context(&self) -> &NativeContext {
        &self.context
    }
    pub fn stats(&self) -> Stats {
        self.stats
    }

    /// Bound explicit transfers to suit the host's staging-memory budget.
    pub fn set_readback_limit(&mut self, limit: usize) -> Result<(), Error> {
        if limit == 0 || limit > MAX_READBACKS {
            return Err(invalid("invalid readback budget"));
        }
        if self.readbacks.load(Ordering::Acquire) > limit {
            return Err(Error::Busy);
        }
        self.readback_limit = limit;
        Ok(())
    }

    pub fn check(&self) -> Result<(), Error> {
        self.context.check()
    }

    /// Nonblocking completion maintenance, for the shell/machine driver.
    pub fn poll(&self) -> Result<(), Error> {
        self.context
            .device
            .poll(wgpu::PollType::Poll)
            .map_err(|error| Error::Device(error.to_string()))?;
        self.check()
    }

    fn dimensions(&self, width: u32, height: u32) -> Result<(), Error> {
        self.check()?;
        let limit = self.context.device.limits().max_texture_dimension_2d;
        if width == 0 || height == 0 || width > limit || height > limit {
            return Err(Error::Invalid(
                "image dimensions exceed adapter limits".into(),
            ));
        }
        Ok(())
    }

    fn own(&self, image: &Image) -> Result<(), Error> {
        self.check()?;
        if image.context != self.context.id {
            return Err(Error::Invalid(
                "image belongs to another graphics context".into(),
            ));
        }
        Ok(())
    }

    pub(super) fn own_target(&self, target: &Target) -> Result<(), Error> {
        self.own(&target.image)?;
        if target.depth.as_ref().is_some_and(|depth| {
            depth.context() != target.image.context
                || depth.width() != target.image.width
                || depth.height() != target.image.height
        }) {
            return Err(invalid(
                "depth attachment context or dimensions do not match",
            ));
        }
        Ok(())
    }

    fn make_image(context: &NativeContext, width: u32, height: u32, levels: u32) -> Image {
        let texture = context.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("adv color image"),
            size: extent(width, height),
            mip_level_count: levels,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        let render_view = texture.create_view(&wgpu::TextureViewDescriptor {
            mip_level_count: Some(1),
            ..Default::default()
        });
        Image {
            texture,
            view,
            render_view,
            mip: 0,
            levels,
            width,
            height,
            alpha: AlphaMode::Stored,
            context: context.id,
            id: NEXT_RESOURCE.fetch_add(1, Ordering::Relaxed),
            lease: Some(Arc::new(())),
            frame_pins: Arc::new(AtomicUsize::new(0)),
            gate: context.gate.clone(),
        }
    }

    pub fn image(&self, width: u32, height: u32) -> Result<Image, Error> {
        self.image_levels(width, height, 1)
    }

    pub fn image_levels(&self, width: u32, height: u32, levels: u32) -> Result<Image, Error> {
        self.dimensions(width, height)?;
        let maximum = 32 - width.max(height).leading_zeros();
        if levels == 0 || levels > maximum {
            return Err(invalid("image mip count exceeds its dimensions"));
        }
        let image = Self::make_image(&self.context, width, height, levels);
        self.check()?;
        Ok(image)
    }

    pub fn target(&self, width: u32, height: u32, depth: bool) -> Result<Target, Error> {
        let image = self.image(width, height)?;
        let depth = depth
            .then(|| self.depth(width, height, DepthFormat::Float32))
            .transpose()?;
        Ok(Target { image, depth })
    }

    /// A color mip chain, with no per-level allocation or automatic generation.
    pub fn target_levels(&self, width: u32, height: u32, levels: u32) -> Result<Target, Error> {
        Ok(Target {
            image: self.image_levels(width, height, levels)?,
            depth: None,
        })
    }

    pub fn depth(&self, width: u32, height: u32, format: DepthFormat) -> Result<Depth, Error> {
        self.dimensions(width, height)?;
        let texture = self
            .context
            .device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("adv depth/stencil image"),
                size: extent(width, height),
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: format.format(),
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
        let view = texture.create_view(&Default::default());
        self.check()?;
        Ok(Depth::new(
            texture,
            view,
            self.context.id,
            width,
            height,
            format,
        ))
    }

    /// Upload a checked BGRA rectangle. Row padding stays outside the GPU image.
    pub fn upload(
        &self,
        image: &Image,
        rectangle: Rect,
        pitch: u32,
        bgra: &[u8],
    ) -> Result<(), Error> {
        let submission = self.context.submission()?;
        self.own(image)?;
        image.writable()?;
        rectangle_in(image, rectangle)?;
        let row = rectangle
            .w
            .checked_mul(4)
            .ok_or_else(|| invalid("upload row overflow"))?;
        let size = usize::try_from(u64::from(pitch) * u64::from(rectangle.h - 1) + u64::from(row))
            .map_err(|_| invalid("upload size overflow"))?;
        if pitch < row || pitch & 3 != 0 || bgra.len() < size {
            return Err(invalid("upload pitch or storage is too small"));
        }
        submission.write_texture(
            wgpu::TexelCopyTextureInfo {
                mip_level: image.mip,
                origin: wgpu::Origin3d {
                    x: rectangle.x,
                    y: rectangle.y,
                    z: 0,
                },
                ..image.texture.as_image_copy()
            },
            bgra,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(pitch),
                rows_per_image: Some(rectangle.h),
            },
            extent(rectangle.w, rectangle.h),
        );
        self.check()
    }

    pub fn clear(
        &mut self,
        target: &Target,
        color: Option<[f64; 4]>,
        depth: Option<f32>,
    ) -> Result<(), Error> {
        self.clear_with_stencil(target, color, depth, None)
    }

    pub fn clear_with_stencil(
        &mut self,
        target: &Target,
        color: Option<[f64; 4]>,
        depth: Option<f32>,
        stencil: Option<u32>,
    ) -> Result<(), Error> {
        let context = self.context.clone();
        let submission = context.submission()?;
        self.own_target(target)?;
        target.image.writable()?;
        if color.is_some_and(|c| c.iter().any(|v| !v.is_finite() || !(0.0..=1.0).contains(v)))
            || depth.is_some_and(|d| !d.is_finite() || !(0.0..=1.0).contains(&d))
            || (depth.is_some() && target.depth.is_none())
            || (stencil.is_some()
                && target
                    .depth
                    .as_ref()
                    .is_none_or(|d| !d.format().has_stencil()))
        {
            return Err(invalid("clear value or depth attachment is invalid"));
        }
        if color.is_none() && depth.is_none() && stencil.is_none() {
            return Err(invalid("clear has no attachment"));
        }
        let mut encoder = self
            .context
            .device
            .create_command_encoder(&Default::default());
        {
            let colors = [Some(wgpu::RenderPassColorAttachment {
                view: &target.image.render_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: color.map_or(wgpu::LoadOp::Load, |c| {
                        wgpu::LoadOp::Clear(wgpu::Color {
                            r: c[0],
                            g: c[1],
                            b: c[2],
                            a: if target.image.alpha == AlphaMode::Opaque {
                                1.0
                            } else {
                                c[3]
                            },
                        })
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })];
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                color_attachments: &colors,
                depth_stencil_attachment: target.depth.as_ref().map(|attachment| {
                    wgpu::RenderPassDepthStencilAttachment {
                        view: attachment.view(),
                        depth_ops: Some(wgpu::Operations {
                            load: depth.map_or(wgpu::LoadOp::Load, wgpu::LoadOp::Clear),
                            store: wgpu::StoreOp::Store,
                        }),
                        stencil_ops: attachment.format().has_stencil().then_some(
                            wgpu::Operations {
                                load: stencil
                                    .map_or(wgpu::LoadOp::Load, |v| wgpu::LoadOp::Clear(v & 255)),
                                store: wgpu::StoreOp::Store,
                            },
                        ),
                    }
                }),
                ..Default::default()
            });
        }
        self.submit(encoder, &submission);
        self.check()
    }

    fn pipeline(&mut self, key: PipelineKey) -> Result<wgpu::RenderPipeline, Error> {
        if let Some(pipeline) = self.pipelines.get(&key) {
            return Ok(pipeline.clone());
        }
        let pipeline = self.context.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("adv fixed pipeline"), layout: Some(&self.layout),
            vertex: wgpu::VertexState { module: &self.shader, entry_point: Some("vertex_main"),
                compilation_options: Default::default(), buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vertex>() as u64, step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4, 2 => Float32x2],
                })] },
            primitive: wgpu::PrimitiveState {
                topology: topology(key.topology),
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: match key.cull { Cull::None => None, Cull::Clockwise => Some(wgpu::Face::Back),
                    Cull::CounterClockwise => Some(wgpu::Face::Front) },
                ..Default::default()
            },
            depth_stencil: key.depth_format.map(|format| wgpu::DepthStencilState {
                format: format.format(), depth_write_enabled: Some(key.depth_write && key.depth.is_some()),
                depth_compare: Some(key.depth.map_or(wgpu::CompareFunction::Always, compare)),
                stencil: key.stencil.map(stencil_state).unwrap_or_default(), bias: Default::default(),
            }),
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState { module: &self.shader, entry_point: Some("fragment_main"),
                compilation_options: Default::default(), targets: &[Some(wgpu::ColorTargetState {
                    format: FORMAT, blend: key.color_blend.map(|color| wgpu::BlendState {
                        color: blend(color), alpha: blend(key.alpha_blend.unwrap_or(color)) }),
                    write_mask: wgpu::ColorWrites::from_bits_truncate(u32::from(key.color_write)),
                })] }),
            multiview_mask: None, cache: None,
        });
        self.check()?;
        // Submitted command buffers retain their own pipeline references. A
        // full cache can retire an entry immediately instead of waiting for a
        // capacity wake that could never occur. Hits leave FIFO order intact.
        if self.pipelines.len() >= self.pipeline_limit {
            let oldest = self
                .pipeline_order
                .pop_front()
                .expect("nonempty pipeline cache");
            self.pipelines.remove(&oldest);
        }
        self.pipelines.insert(key, pipeline.clone());
        self.pipeline_order.push_back(key);
        Ok(pipeline)
    }

    fn sampler(&mut self, spec: Sampler) -> wgpu::Sampler {
        self.samplers
            .entry(spec)
            .or_insert_with(|| {
                self.context
                    .device
                    .create_sampler(&wgpu::SamplerDescriptor {
                        label: Some("adv image sampler"),
                        address_mode_u: address(spec.u),
                        address_mode_v: address(spec.v),
                        mag_filter: filter(spec.mag),
                        min_filter: filter(spec.min),
                        mipmap_filter: match spec.mip {
                            Some(Filter::Linear) => wgpu::MipmapFilterMode::Linear,
                            _ => wgpu::MipmapFilterMode::Nearest,
                        },
                        lod_min_clamp: spec.first_level as f32,
                        lod_max_clamp: if spec.mip.is_none() {
                            spec.first_level as f32
                        } else {
                            32.0
                        },
                        ..Default::default()
                    })
            })
            .clone()
    }

    pub fn draw(
        &mut self,
        target: &Target,
        vertices: &[Vertex],
        image: Option<&Image>,
        mut state: DrawState,
    ) -> Result<(), Error> {
        let context = self.context.clone();
        let submission = context.submission()?;
        self.own_target(target)?;
        target.image.writable()?;
        if let Some(image) = image {
            self.own(image)?;
            if image.id == target.image.id {
                return Err(invalid("render target cannot sample itself"));
            }
        }
        validate(target, vertices, &state)?;
        let inclusive = line::prepare(vertices, image, &state)?;
        if state.stencil.is_some()
            && target
                .depth
                .as_ref()
                .is_none_or(|d| !d.format().has_stencil())
        {
            return Err(invalid("stencil state requires a stencil attachment"));
        }
        if let Some(stencil) = &mut state.stencil {
            // Every supported stencil attachment has eight bits. Ignored
            // high bits must not consume distinct pipeline cache entries.
            stencil.read_mask &= 255;
            stencil.write_mask &= 255;
        }
        if target.image.alpha == AlphaMode::Opaque {
            // RGBX has destination alpha one irrespective of unused storage bits.
            // Preserve source alpha for color blending and alpha testing.
            if let Some(blend) = &mut state.blend {
                let factor = |factor| match factor {
                    Factor::DestinationAlpha => Factor::One,
                    Factor::OneMinusDestinationAlpha | Factor::SourceAlphaSaturated => Factor::Zero,
                    factor => factor,
                };
                blend.source = factor(blend.source);
                blend.destination = factor(blend.destination);
            }
        }
        let size = std::mem::size_of_val(vertices) as u64;
        if size > MAX_UPLOAD {
            return Err(invalid("vertex upload exceeds command limit"));
        }
        if size > self.upload_capacity {
            self.upload_capacity = size.next_power_of_two().min(MAX_UPLOAD);
            self.upload = self.context.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("adv vertex upload"),
                size: self.upload_capacity,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.stats.vertex_buffer_allocations += 1;
        }
        let key = PipelineKey {
            topology: state.topology,
            cull: state.cull,
            color_blend: state.blend,
            alpha_blend: state.alpha_blend,
            depth: state.depth,
            depth_write: state.depth_write,
            depth_format: target.depth.as_ref().map(|d| d.format()),
            stencil: state.stencil,
            color_write: state.color_write,
        };
        let pipeline = self.pipeline(key)?;
        let endpoint_pipeline = inclusive
            .as_ref()
            .map(|_| {
                self.pipeline(PipelineKey {
                    topology: Topology::Points,
                    ..key
                })
            })
            .transpose()?;
        let image_levels = image.map_or(1, Image::mip_levels);
        let sampler = self.sampler(Sampler {
            first_level: state.sampler.first_level.min(image_levels - 1),
            ..state.sampler
        });
        submission.write_buffer(&self.upload, 0, bytemuck::cast_slice(vertices));
        let parameters = [
            texture_op(state.texture_op),
            texture_op(state.alpha_texture_op)
                | if image.is_some_and(|image| image.alpha == AlphaMode::Opaque) {
                    1 << 31
                } else {
                    0
                },
            compare_index(state.alpha_test),
            state.alpha_reference.to_bits(),
        ];
        submission.write_buffer(&self.uniform, 0, bytemuck::cast_slice(&parameters));
        let binding = self
            .context
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("adv draw image"),
                layout: &self.bindings,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(
                            &image.unwrap_or(&self.white).view,
                        ),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: self.uniform.as_entire_binding(),
                    },
                ],
            });
        let mut encoder = self
            .context
            .device
            .create_command_encoder(&Default::default());
        {
            let colors = [Some(wgpu::RenderPassColorAttachment {
                view: &target.image.render_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })];
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                color_attachments: &colors,
                depth_stencil_attachment: target.depth.as_ref().map(|attachment| {
                    wgpu::RenderPassDepthStencilAttachment {
                        view: attachment.view(),
                        depth_ops: Some(wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        }),
                        stencil_ops: attachment.format().has_stencil().then_some(
                            wgpu::Operations {
                                load: wgpu::LoadOp::Load,
                                store: wgpu::StoreOp::Store,
                            },
                        ),
                    }
                }),
                ..Default::default()
            });
            pass.set_pipeline(&pipeline);
            if state.stencil.is_some() {
                pass.set_stencil_reference(state.stencil_reference & 255);
            }
            pass.set_bind_group(0, &binding, &[]);
            let viewport = state.viewport;
            pass.set_viewport(
                viewport.x,
                viewport.y,
                viewport.width,
                viewport.height,
                viewport.minimum_depth,
                viewport.maximum_depth,
            );
            if let Some(r) = state.scissor {
                pass.set_scissor_rect(r.x, r.y, r.w, r.h);
            }
            pass.set_vertex_buffer(0, self.upload.slice(0..size));
            match inclusive {
                None => pass.draw(0..vertices.len() as u32, 0..1),
                Some(line::Inclusive::Strip { terminal }) => {
                    // A strip includes shared joins only once. Its terminal
                    // point follows the last real segment, never a clipped end.
                    pass.draw(0..vertices.len() as u32, 0..1);
                    if terminal {
                        pass.set_pipeline(endpoint_pipeline.as_ref().unwrap());
                        pass.draw(vertices.len() as u32 - 1..vertices.len() as u32, 0..1);
                    }
                }
                Some(line::Inclusive::List) => {
                    // Keep endpoint blending/depth/stencil in each segment's
                    // original order, including intersections with later lines.
                    for (index, pair) in vertices.chunks_exact(2).enumerate() {
                        let first = index as u32 * 2;
                        pass.set_pipeline(&pipeline);
                        pass.draw(first..first + 2, 0..1);
                        if !line::degenerate(&pair[0], &pair[1], state.viewport) {
                            pass.set_pipeline(endpoint_pipeline.as_ref().unwrap());
                            pass.draw(first + 1..first + 2, 0..1);
                        }
                    }
                }
            }
        }
        self.submit(encoder, &submission);
        self.stats.draws += 1;
        self.stats.vertices += vertices.len() as u64;
        self.check()
    }

    fn submit(
        &mut self,
        encoder: wgpu::CommandEncoder,
        guard: &Submission<'_>,
    ) -> wgpu::SubmissionIndex {
        let submission = guard.submit([encoder.finish()]);
        self.stats.submissions += 1;
        submission
    }

    pub fn copy(
        &mut self,
        source: &Image,
        rectangle: Rect,
        destination: &Image,
        x: u32,
        y: u32,
    ) -> Result<(), Error> {
        let context = self.context.clone();
        let submission = context.submission()?;
        self.copy_under(source, rectangle, destination, x, y, &submission)
    }

    fn copy_under(
        &mut self,
        source: &Image,
        rectangle: Rect,
        destination: &Image,
        x: u32,
        y: u32,
        submission: &Submission<'_>,
    ) -> Result<(), Error> {
        self.own(source)?;
        self.own(destination)?;
        destination.writable()?;
        rectangle_in(source, rectangle)?;
        rectangle_in(
            destination,
            Rect {
                x,
                y,
                w: rectangle.w,
                h: rectangle.h,
            },
        )?;
        if source.id == destination.id {
            return Err(invalid("image cannot copy onto itself"));
        }
        let mut encoder = self
            .context
            .device
            .create_command_encoder(&Default::default());
        encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                mip_level: source.mip,
                origin: wgpu::Origin3d {
                    x: rectangle.x,
                    y: rectangle.y,
                    z: 0,
                },
                ..source.texture.as_image_copy()
            },
            wgpu::TexelCopyTextureInfo {
                mip_level: destination.mip,
                origin: wgpu::Origin3d { x, y, z: 0 },
                ..destination.texture.as_image_copy()
            },
            extent(rectangle.w, rectangle.h),
        );
        self.submit(encoder, submission);
        self.check()
    }

    /// Present through a GPU snapshot. No readback, CPU conversion or upload.
    pub fn present(&mut self, image: &Image, serial: u64) -> Result<NativeFrame, Error> {
        let context = self.context.clone();
        let submission = context.submission()?;
        self.own(image)?;
        let mut reusable = self.frames.iter().position(Image::reusable);
        if reusable.is_none() && self.frames.len() >= MAX_NATIVE_FRAMES {
            self.wait_for_frame_release();
            reusable = self.frames.iter().position(Image::reusable);
            if reusable.is_none() {
                return Err(Error::Busy);
            }
        }
        let index = if let Some(index) = reusable {
            if self.frames[index].width != image.width || self.frames[index].height != image.height
            {
                self.frames[index] = self.image(image.width, image.height)?;
                self.stats.native_frame_allocations += 1;
            }
            index
        } else {
            if self.frames.len() >= MAX_NATIVE_FRAMES {
                return Err(Error::Busy);
            }
            self.frames.push(self.image(image.width, image.height)?);
            self.stats.native_frame_allocations += 1;
            self.frames.len() - 1
        };
        self.frames[index].alpha = image.alpha;
        let snapshot = self.frames[index].clone();
        self.copy_under(
            image,
            Rect {
                x: 0,
                y: 0,
                w: image.width,
                h: image.height,
            },
            &snapshot,
            0,
            0,
            &submission,
        )?;
        self.stats.native_frames += 1;
        let pin = FramePin::new(&snapshot)?;
        Ok(NativeFrame {
            image: snapshot,
            serial,
            context: self.context.clone(),
            _pin: pin,
        })
    }

    pub(crate) fn wait_for_frame_release(&self) {
        self.context.gate.wait_for_frame();
    }

    /// Freeze the completed image by ownership, without copying or submitting.
    /// The caller must rotate its writable target before another draw. A live
    /// frame rejects all ordinary GPU writes to this image, including clones.
    pub fn completed_frame(&mut self, image: Image, serial: u64) -> Result<NativeFrame, Error> {
        let context = self.context.clone();
        let submission = context.submission()?;
        self.completed_frame_under(image, serial, &submission)
    }

    pub(crate) fn completed_frame_under(
        &mut self,
        image: Image,
        serial: u64,
        submission: &Submission<'_>,
    ) -> Result<NativeFrame, Error> {
        if submission.context.id != self.context.id {
            return Err(invalid("submission lease belongs to another GPU context"));
        }
        self.own(&image)?;
        let pin = FramePin::new(&image)?;
        self.stats.native_frames += 1;
        Ok(NativeFrame {
            image,
            serial,
            context: self.context.clone(),
            _pin: pin,
        })
    }

    pub fn readback(
        &mut self,
        image: &Image,
        wake: Option<Arc<adv_sync::WakeEvent>>,
    ) -> Result<Readback, Error> {
        let context = self.context.clone();
        let submission_guard = context.submission()?;
        self.own(image)?;
        self.readbacks
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < self.readback_limit).then_some(count + 1)
            })
            .map_err(|_| Error::Busy)?;
        let reservation = ReadbackReservation(self.readbacks.clone());
        let row = image.width * 4;
        let aligned =
            row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let size = u64::from(aligned) * u64::from(image.height);
        let buffer = self.context.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("adv explicit readback"),
            size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .context
            .device
            .create_command_encoder(&Default::default());
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                mip_level: image.mip,
                ..image.texture.as_image_copy()
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(aligned),
                    rows_per_image: Some(image.height),
                },
            },
            extent(image.width, image.height),
        );
        let submission = self.submit(encoder, &submission_guard);
        let (mut producer, consumer) = adv_sync::spsc::channel(1);
        let completion_buffer = buffer.clone();
        let (width, height) = (image.width, image.height);
        buffer.map_async(wgpu::MapMode::Read, .., move |result| {
            let result = result
                .map_err(|error| Error::Device(error.to_string()))
                .and_then(|()| {
                    let mapped = completion_buffer
                        .get_mapped_range(..)
                        .map_err(|error| Error::Device(error.to_string()))?;
                    let mut pixels = Vec::with_capacity(row as usize * height as usize);
                    for y in 0..height as usize {
                        pixels.extend_from_slice(
                            &mapped[y * aligned as usize..y * aligned as usize + row as usize],
                        );
                    }
                    drop(mapped);
                    completion_buffer.unmap();
                    Ok(pixels)
                });
            let _ = producer.push(result);
            drop(reservation);
            if let Some(wake) = wake {
                wake.signal();
            }
        });
        self.stats.readbacks += 1;
        self.check()?;
        Ok(Readback {
            buffer,
            completion: consumer,
            width,
            height,
            context: self.context.clone(),
            submission,
            job_taken: false,
        })
    }
}

fn extent(width: u32, height: u32) -> wgpu::Extent3d {
    wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    }
}
fn invalid(message: &str) -> Error {
    Error::Invalid(message.into())
}
fn rectangle_in(image: &Image, r: Rect) -> Result<(), Error> {
    if r.w == 0
        || r.h == 0
        || r.x.checked_add(r.w).is_none_or(|right| right > image.width)
        || r.y
            .checked_add(r.h)
            .is_none_or(|bottom| bottom > image.height)
    {
        return Err(invalid("rectangle is outside the image"));
    }
    Ok(())
}
fn validate(target: &Target, vertices: &[Vertex], state: &DrawState) -> Result<(), Error> {
    let count = vertices.len();
    let valid_count = match state.topology {
        Topology::Points => count > 0,
        Topology::Lines => count > 0 && count.is_multiple_of(2),
        Topology::LineStrip => count >= 2,
        Topology::Triangles => count > 0 && count.is_multiple_of(3),
        Topology::TriangleStrip => count >= 3,
    };
    if !valid_count
        || vertices.iter().any(|v| {
            v.position
                .iter()
                .chain(&v.color)
                .chain(&v.uv)
                .any(|f| !f.is_finite())
        })
    {
        return Err(invalid("invalid primitive count or vertex"));
    }
    let v = state.viewport;
    if [
        v.x,
        v.y,
        v.width,
        v.height,
        v.minimum_depth,
        v.maximum_depth,
    ]
    .iter()
    .any(|f| !f.is_finite())
        || v.x < 0.0
        || v.y < 0.0
        || v.width <= 0.0
        || v.height <= 0.0
        || v.x + v.width > target.image.width as f32
        || v.y + v.height > target.image.height as f32
        || v.minimum_depth < 0.0
        || v.maximum_depth > 1.0
        || v.minimum_depth > v.maximum_depth
        || state.color_write & !15 != 0
        || !state.alpha_reference.is_finite()
        || !(0.0..=1.0).contains(&state.alpha_reference)
        || (state.depth.is_some() && target.depth.is_none())
    {
        return Err(invalid("invalid drawing state or viewport"));
    }
    if let Some(r) = state.scissor {
        rectangle_in(&target.image, r)?;
    }
    Ok(())
}
fn topology(t: Topology) -> wgpu::PrimitiveTopology {
    match t {
        Topology::Points => wgpu::PrimitiveTopology::PointList,
        Topology::Lines => wgpu::PrimitiveTopology::LineList,
        Topology::LineStrip => wgpu::PrimitiveTopology::LineStrip,
        Topology::Triangles => wgpu::PrimitiveTopology::TriangleList,
        Topology::TriangleStrip => wgpu::PrimitiveTopology::TriangleStrip,
    }
}
fn filter(f: Filter) -> wgpu::FilterMode {
    match f {
        Filter::Nearest => wgpu::FilterMode::Nearest,
        Filter::Linear => wgpu::FilterMode::Linear,
    }
}
fn address(a: Address) -> wgpu::AddressMode {
    match a {
        Address::Clamp => wgpu::AddressMode::ClampToEdge,
        Address::Repeat => wgpu::AddressMode::Repeat,
        Address::Mirror => wgpu::AddressMode::MirrorRepeat,
    }
}
fn compare(c: Compare) -> wgpu::CompareFunction {
    match c {
        Compare::Never => wgpu::CompareFunction::Never,
        Compare::Less => wgpu::CompareFunction::Less,
        Compare::Equal => wgpu::CompareFunction::Equal,
        Compare::LessEqual => wgpu::CompareFunction::LessEqual,
        Compare::Greater => wgpu::CompareFunction::Greater,
        Compare::NotEqual => wgpu::CompareFunction::NotEqual,
        Compare::GreaterEqual => wgpu::CompareFunction::GreaterEqual,
        Compare::Always => wgpu::CompareFunction::Always,
    }
}
fn stencil_state(s: Stencil) -> wgpu::StencilState {
    let operation = |op| match op {
        StencilOp::Keep => wgpu::StencilOperation::Keep,
        StencilOp::Zero => wgpu::StencilOperation::Zero,
        StencilOp::Replace => wgpu::StencilOperation::Replace,
        StencilOp::IncrementClamp => wgpu::StencilOperation::IncrementClamp,
        StencilOp::DecrementClamp => wgpu::StencilOperation::DecrementClamp,
        StencilOp::Invert => wgpu::StencilOperation::Invert,
        StencilOp::IncrementWrap => wgpu::StencilOperation::IncrementWrap,
        StencilOp::DecrementWrap => wgpu::StencilOperation::DecrementWrap,
    };
    let face = |f: StencilFace| wgpu::StencilFaceState {
        compare: compare(f.compare),
        fail_op: operation(f.fail),
        depth_fail_op: operation(f.depth_fail),
        pass_op: operation(f.pass),
    };
    wgpu::StencilState {
        front: face(s.front),
        back: face(s.back),
        read_mask: s.read_mask & 255,
        write_mask: s.write_mask & 255,
    }
}
fn factor(f: Factor) -> wgpu::BlendFactor {
    match f {
        Factor::Zero => wgpu::BlendFactor::Zero,
        Factor::One => wgpu::BlendFactor::One,
        Factor::SourceColor => wgpu::BlendFactor::Src,
        Factor::OneMinusSourceColor => wgpu::BlendFactor::OneMinusSrc,
        Factor::SourceAlpha => wgpu::BlendFactor::SrcAlpha,
        Factor::OneMinusSourceAlpha => wgpu::BlendFactor::OneMinusSrcAlpha,
        Factor::DestinationColor => wgpu::BlendFactor::Dst,
        Factor::OneMinusDestinationColor => wgpu::BlendFactor::OneMinusDst,
        Factor::DestinationAlpha => wgpu::BlendFactor::DstAlpha,
        Factor::OneMinusDestinationAlpha => wgpu::BlendFactor::OneMinusDstAlpha,
        Factor::SourceAlphaSaturated => wgpu::BlendFactor::SrcAlphaSaturated,
    }
}
fn blend(b: Blend) -> wgpu::BlendComponent {
    wgpu::BlendComponent {
        src_factor: factor(b.source),
        dst_factor: factor(b.destination),
        operation: match b.operation {
            BlendOp::Add => wgpu::BlendOperation::Add,
            BlendOp::Subtract => wgpu::BlendOperation::Subtract,
            BlendOp::ReverseSubtract => wgpu::BlendOperation::ReverseSubtract,
            BlendOp::Minimum => wgpu::BlendOperation::Min,
            BlendOp::Maximum => wgpu::BlendOperation::Max,
        },
    }
}
fn texture_op(op: TextureOp) -> u32 {
    match op {
        TextureOp::Color => 0,
        TextureOp::Texture => 1,
        TextureOp::Multiply => 2,
        TextureOp::Add => 3,
        TextureOp::Subtract => 4,
    }
}
fn compare_index(c: Compare) -> u32 {
    match c {
        Compare::Never => 0,
        Compare::Less => 1,
        Compare::Equal => 2,
        Compare::LessEqual => 3,
        Compare::Greater => 4,
        Compare::NotEqual => 5,
        Compare::GreaterEqual => 6,
        Compare::Always => 7,
    }
}

#[cfg(test)]
mod cache_tests {
    use super::*;

    #[test]
    fn full_pipeline_cache_evicts_fifo_and_submitted_commands_remain_valid() {
        let mut gpu = pollster::block_on(Gpu::request(true)).unwrap();
        assert_eq!(gpu.pipeline_limit, 512);
        assert_eq!(gpu.pipeline_limit, MAX_PIPELINES);
        gpu.pipeline_limit = 2;
        let depth = gpu.depth(2, 2, DepthFormat::Depth24Stencil8).unwrap();
        let target = gpu
            .target(2, 2, false)
            .unwrap()
            .with_depth(Some(depth))
            .unwrap();
        let snapshot = gpu.image(2, 2).unwrap();
        gpu.clear_with_stencil(&target, Some([0.; 4]), Some(1.), Some(0))
            .unwrap();
        let face = StencilFace {
            compare: Compare::Always,
            fail: StencilOp::Keep,
            depth_fail: StencilOp::Keep,
            pass: StencilOp::Keep,
        };
        let quad = |color| {
            let vertex = |x, y| Vertex {
                position: [x, y, 0., 1.],
                color,
                uv: [0.; 2],
            };
            [
                vertex(-1., 1.),
                vertex(-1., -1.),
                vertex(1., 1.),
                vertex(1., 1.),
                vertex(-1., -1.),
                vertex(1., -1.),
            ]
        };
        let mut state = DrawState::new(2, 2);
        state.stencil = Some(Stencil {
            front: face,
            back: face,
            read_mask: 1,
            write_mask: 1,
        });
        gpu.draw(&target, &quad([1., 0., 0., 1.]), None, state)
            .unwrap();
        gpu.copy(
            &target.image,
            Rect {
                x: 0,
                y: 0,
                w: 2,
                h: 2,
            },
            &snapshot,
            0,
            0,
        )
        .unwrap();
        state.stencil.as_mut().unwrap().read_mask = 2;
        gpu.draw(&target, &quad([0., 1., 0., 1.]), None, state)
            .unwrap();
        let insertion_order = gpu.pipeline_order.clone();
        state.stencil.as_mut().unwrap().read_mask = 0xab00_0001;
        state.stencil.as_mut().unwrap().write_mask = 0xcd00_0001;
        gpu.draw(&target, &quad([0., 1., 0., 1.]), None, state)
            .unwrap();
        assert_eq!(
            gpu.pipeline_order, insertion_order,
            "ignored high bits reuse the original key; hits do not reorder FIFO"
        );
        for mask in 3..=6 {
            state.stencil.as_mut().unwrap().read_mask = mask;
            gpu.draw(&target, &quad([0., 1., 0., 1.]), None, state)
                .unwrap();
            assert_eq!(gpu.pipelines.len(), 2);
            assert_eq!(gpu.pipeline_order.len(), 2);
        }
        assert!(!gpu.pipelines.contains_key(&insertion_order[0]));
        // No fence wait preceded eviction. Both submitted copies/draws must
        // retain their pipeline resources after the cache releases its handles.
        let mut old = gpu.readback(&snapshot, None).unwrap();
        let mut latest = gpu.readback(&target.image, None).unwrap();
        gpu.context
            .device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(std::time::Duration::from_secs(5)),
            })
            .unwrap();
        gpu.check().unwrap();
        assert_eq!(old.poll().unwrap().unwrap(), [0, 0, 255, 255].repeat(4));
        assert_eq!(latest.poll().unwrap().unwrap(), [0, 255, 0, 255].repeat(4));
    }
}

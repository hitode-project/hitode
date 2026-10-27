//! Background composition of device-owned GPU images and final software frames.
//! Configuration is deliberately separate: wgpu configure waits for queue idle,
//! so it belongs to startup or an owned background job, never the UI paint path.
use std::cell::Cell;
use std::collections::HashMap;
use std::sync::Arc;

use adv_device::gpu::{NativeContext, NativeFrame, Submission};
use adv_device::surface::{Rect, VideoCursorRegion, VideoFrame, VideoLayer, VideoLayout};
use adv_runtime::{FrameConsumer, Snapshot};
use adv_sync::{WakeEvent, background, mailbox::Observer, spsc};
use std::time::{Duration, Instant};

pub(crate) enum DrawResult {
    Presented,
    Skipped,
    Reconfigure,
}

#[cfg(target_os = "android")]
struct AndroidWindow(std::ptr::NonNull<ndk_sys::ANativeWindow>);
// ANativeWindow references are thread-safe; jobs own a reference until their surface drops.
#[cfg(target_os = "android")]
unsafe impl Send for AndroidWindow {}
#[cfg(target_os = "android")]
impl AndroidWindow {
    unsafe fn acquire(window: std::ptr::NonNull<ndk_sys::ANativeWindow>) -> Self {
        unsafe {
            ndk_sys::ANativeWindow_acquire(window.as_ptr());
        }
        Self(window)
    }
}
#[cfg(target_os = "android")]
impl Drop for AndroidWindow {
    fn drop(&mut self) {
        unsafe {
            ndk_sys::ANativeWindow_release(self.0.as_ptr());
        }
    }
}
pub struct WindowPresenter {
    surface: Option<wgpu::Surface<'static>>,
    #[cfg(target_os = "android")]
    android_window: Option<AndroidWindow>,
    context: NativeContext,
    configuration: wgpu::SurfaceConfiguration,
    compositor: Compositor,
    upscaler: crate::Upscaler,
    scaling: crate::Scaling,
    upscaling: Option<crate::upscaling::Upscaling>,
    max_acquire_ns: Cell<u64>,
    max_configure_ns: u64,
    /// Upscaler runs and the input pixels their last passes recomputed.
    upscaled: (u64, u64),
    #[cfg(target_os = "macos")]
    pub(crate) direct_layer: bool,
}

impl WindowPresenter {
    /// `layer` is the game view's frame `CAMetalLayer` (HOST-VIEW-9).
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    unsafe fn surface(
        layer: *mut std::ffi::c_void,
        instance: &wgpu::Instance,
    ) -> Result<wgpu::Surface<'static>, String> {
        if layer.is_null() {
            return Err("null GPU layer".into());
        }
        // SAFETY: the UI owner keeps the layer alive until all surface jobs finish.
        unsafe {
            instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::CoreAnimationLayer(layer))
        }
        .map_err(|error| error.to_string())
    }

    #[cfg(target_os = "android")]
    unsafe fn surface(
        window: *mut std::ffi::c_void,
        instance: &wgpu::Instance,
    ) -> Result<wgpu::Surface<'static>, String> {
        let window = std::ptr::NonNull::new(window).ok_or("null Android window")?;
        unsafe {
            instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                raw_display_handle: Some(wgpu::rwh::AndroidDisplayHandle::new().into()),
                raw_window_handle: wgpu::rwh::AndroidNdkWindowHandle::new(window).into(),
            })
        }
        .map_err(|error| error.to_string())
    }
    /// The coordinator has returned every old surface job before replacement.
    #[cfg(target_os = "android")]
    unsafe fn replace_window(
        &mut self,
        window: Option<std::ptr::NonNull<ndk_sys::ANativeWindow>>,
    ) -> Result<(), String> {
        self.surface.take();
        self.android_window.take();
        if let Some(window) = window {
            let owner = unsafe { AndroidWindow::acquire(window) };
            let surface = unsafe { Self::surface(window.as_ptr().cast(), &self.context.instance) }?;
            let mut replacement = Self::with_surface(
                surface,
                self.context.clone(),
                self.configuration.width,
                self.configuration.height,
            )?;
            replacement.android_window = Some(owner);
            replacement.upscaler = self.upscaler;
            replacement.scaling = self.scaling;
            *self = replacement;
        }
        Ok(())
    }
    pub(crate) async unsafe fn request(
        hwnd: *mut std::ffi::c_void,
        width: u32,
        height: u32,
        fallback: bool,
    ) -> Result<(adv_device::gpu::Gpu, Self), String> {
        let descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        let descriptor = wgpu::InstanceDescriptor {
            backends: wgpu::Backends::METAL,
            ..descriptor
        };
        #[cfg(target_os = "android")]
        let descriptor = wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..descriptor
        };
        let instance = wgpu::Instance::new(descriptor);
        let surface = unsafe { Self::surface(hwnd, &instance)? };
        let gpu =
            match adv_device::gpu::Gpu::request_presenting(instance.clone(), &surface, fallback)
                .await
            {
                Ok(gpu) => gpu,
                Err(error) if fallback => return Err(error.to_string()),
                Err(_) => adv_device::gpu::Gpu::request_presenting(instance, &surface, true)
                    .await
                    .map_err(|error| error.to_string())?,
            };
        let presenter = Self::with_surface(surface, gpu.context().clone(), width, height)?;
        #[cfg(target_os = "android")]
        let presenter = {
            let mut presenter = presenter;
            presenter.android_window = Some(unsafe {
                AndroidWindow::acquire(std::ptr::NonNull::new(hwnd.cast()).unwrap())
            });
            presenter
        };
        Ok((gpu, presenter))
    }

    fn with_surface(
        surface: wgpu::Surface<'static>,
        context: NativeContext,
        width: u32,
        height: u32,
    ) -> Result<Self, String> {
        let caps = surface.get_capabilities(&context.adapter);
        let mut configuration = surface
            .get_default_config(&context.adapter, width, height)
            .ok_or("GPU adapter cannot present to this native window")?;
        if let Some(format) = caps.formats.iter().find(|format| !format.is_srgb()) {
            configuration.format = *format;
        }
        // UI already selects the vblank opportunity. Never add a FIFO wait for
        // the guest or let a queue of old frames accumulate in the swapchain.
        // Phone compositors offer only FIFO; their display-link pacing presents
        // at most once per refresh, so no queue builds there either.
        configuration.present_mode = if caps.present_modes.contains(&wgpu::PresentMode::Mailbox) {
            wgpu::PresentMode::Mailbox
        } else if caps.present_modes.contains(&wgpu::PresentMode::Immediate) {
            wgpu::PresentMode::Immediate
        } else if cfg!(mobile) {
            wgpu::PresentMode::Fifo
        } else {
            return Err("native surface has no nonblocking presentation mode".into());
        };
        configuration.desired_maximum_frame_latency = 2;
        let compositor = Compositor::new(context.clone(), configuration.format);
        Ok(Self {
            surface: Some(surface),
            #[cfg(target_os = "android")]
            android_window: None,
            context,
            configuration,
            compositor,
            upscaler: crate::Upscaler::Off,
            scaling: crate::Scaling::Fit,
            upscaling: None,
            max_acquire_ns: Cell::new(0),
            max_configure_ns: 0,
            upscaled: (0, 0),
            #[cfg(target_os = "macos")]
            direct_layer: false,
        })
    }

    /// Only startup before guest submissions, or the background service after
    /// both queue submitters have acknowledged pause, may call this operation.
    pub(crate) fn configure_exclusive(&mut self, width: u32, height: u32) -> Result<(), String> {
        self.context.check().map_err(|error| error.to_string())?;
        if width == 0 || height == 0 {
            return Err("native configuration is empty".into());
        }
        self.configuration.width = width;
        self.configuration.height = height;
        let errors = self
            .context
            .device
            .push_error_scope(wgpu::ErrorFilter::Validation);
        let started = Instant::now();
        self.surface
            .as_ref()
            .ok_or("Android surface is absent")?
            .configure(&self.context.device, &self.configuration);
        let error = pollster::block_on(errors.pop());
        self.max_configure_ns = self.max_configure_ns.max(nanoseconds(started.elapsed()));
        if let Some(error) = error {
            return Err(format!("native surface configuration failed: {error}"));
        }
        Ok(())
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the operation takes explicit image layout and draw parameters"
    )]
    fn draw_view(
        &mut self,
        source: &wgpu::TextureView,
        size: (u32, u32),
        rectangle: [f32; 4],
        canvas: (u32, u32),
        changed: bool,
        damage: Option<Rect>,
        overlays: &[VideoOverlay],
        submission: &Submission<'_>,
    ) -> Result<DrawResult, String> {
        let (width, height) = size;
        let enabled = self.upscaler != crate::Upscaler::Off
            && rectangle[2] > canvas.0 as f32
            && rectangle[3] > canvas.1 as f32;
        let rebuild = enabled
            && self
                .upscaling
                .as_ref()
                .is_none_or(|u| u.choice != self.upscaler || u.size != canvas);
        if rebuild {
            self.upscaling = Some(crate::upscaling::Upscaling::new(
                &self.context,
                self.upscaler,
                canvas,
            )?);
            self.compositor
                .prepare_format(wgpu::TextureFormat::Rgba16Float);
        } else if !enabled {
            self.upscaling = None;
        }
        let refresh = changed || rebuild;
        if let Some(upscaling) = &self.upscaling {
            // Composed video changes without frame damage, so it redraws fully.
            let damage = damage.filter(|_| !rebuild && overlays.is_empty());
            if let Some(damage) = damage.filter(|_| refresh) {
                self.compositor.compose_view_region(
                    source,
                    &upscaling.input,
                    wgpu::TextureFormat::Rgba16Float,
                    CompositionRegion {
                        rectangle: [0., 0., canvas.0 as f32, canvas.1 as f32],
                        source_uv: [0., 0., 1., 1.],
                        scissor: Some(damage),
                    },
                    false,
                    submission,
                )?;
            } else if refresh {
                self.compositor.compose_frame(
                    source,
                    &upscaling.input,
                    wgpu::TextureFormat::Rgba16Float,
                    [0., 0., canvas.0 as f32, canvas.1 as f32],
                    canvas,
                    canvas,
                    overlays,
                    submission,
                )?;
            }
            if refresh {
                self.upscaled.0 += 1;
                self.upscaled.1 += upscaling.run(&self.context, submission, damage);
            }
        }
        if (width, height) != (self.configuration.width, self.configuration.height) {
            return Ok(DrawResult::Reconfigure);
        }
        let started = Instant::now();
        let Some(surface) = &self.surface else {
            return Ok(DrawResult::Skipped);
        };
        let output = surface.get_current_texture();
        self.max_acquire_ns.set(
            self.max_acquire_ns
                .get()
                .max(nanoseconds(started.elapsed())),
        );
        let output = match output {
            wgpu::CurrentSurfaceTexture::Success(output)
            | wgpu::CurrentSurfaceTexture::Suboptimal(output) => output,
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return Ok(DrawResult::Skipped);
            }
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                return Ok(DrawResult::Reconfigure);
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                return Err("native surface acquisition failed validation".into());
            }
        };
        let view = output.texture.create_view(&Default::default());
        if let Some(upscaling) = &self.upscaling {
            self.compositor.compose_view_region_sampled(
                upscaling.output(),
                &view,
                self.configuration.format,
                CompositionRegion {
                    rectangle,
                    source_uv: [0., 0., 1., 1.],
                    scissor: None,
                },
                true,
                true,
                submission,
            )?;
        } else {
            self.compositor.compose_frame(
                source,
                &view,
                self.configuration.format,
                rectangle,
                canvas,
                size,
                overlays,
                submission,
            )?;
        }
        submission.present(output);
        self.context
            .device
            .poll(wgpu::PollType::Poll)
            .map_err(|error| error.to_string())?;
        Ok(DrawResult::Presented)
    }
}

pub(crate) struct Compositor {
    context: NativeContext,
    layout: wgpu::BindGroupLayout,
    pipelines: HashMap<wgpu::TextureFormat, wgpu::RenderPipeline>,
    linear_sampler: wgpu::Sampler,
}

impl Compositor {
    /// Compile at startup or on the common backend service before publishing it
    /// to UI. The draw path only creates a binding and records/submits commands.
    pub(crate) fn new(context: NativeContext, format: wgpu::TextureFormat) -> Self {
        let layout = context
            .device
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("adv native composition"),
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
                        visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                ],
            });
        let linear_sampler = context.device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let mut result = Self {
            context,
            layout,
            pipelines: HashMap::new(),
            linear_sampler,
        };
        result.prepare_format(format);
        result
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the operation takes explicit image layout and draw parameters"
    )]
    fn compose_frame(
        &self,
        source: &wgpu::TextureView,
        view: &wgpu::TextureView,
        format: wgpu::TextureFormat,
        rectangle: [f32; 4],
        canvas: (u32, u32),
        size: (u32, u32),
        overlays: &[VideoOverlay],
        submission: &Submission<'_>,
    ) -> Result<(), String> {
        let (width, height) = size;
        self.compose_view(source, view, format, rectangle, submission)?;
        for overlay in overlays {
            if let Some((rect, _)) = overlay.layout.background {
                self.compose_view_region(
                    &overlay.border.as_ref().unwrap().view,
                    view,
                    format,
                    CompositionRegion::video(
                        rect,
                        [0., 0., 1., 1.],
                        rectangle,
                        canvas,
                        (width, height),
                    ),
                    false,
                    submission,
                )?;
            }
            if let Some(video) = overlay.layout.video {
                self.compose_view_region(
                    &overlay.image.as_ref().unwrap().view,
                    view,
                    format,
                    CompositionRegion::video(
                        video.destination,
                        video.source_uv,
                        rectangle,
                        canvas,
                        (width, height),
                    ),
                    false,
                    submission,
                )?;
            }
        }
        Ok(())
    }

    fn prepare_format(&mut self, format: wgpu::TextureFormat) {
        if self.pipelines.contains_key(&format) {
            return;
        }
        let source = SHADER.replace(
            "OUTPUT_SRGB",
            if format.is_srgb() { "true" } else { "false" },
        );
        let module = self
            .context
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("adv native frame composition"),
                source: wgpu::ShaderSource::Wgsl(source.into()),
            });
        let pipeline_layout =
            self.context
                .device
                .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("adv native composition layout"),
                    bind_group_layouts: &[Some(&self.layout)],
                    immediate_size: 0,
                });
        let pipeline =
            self.context
                .device
                .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("adv native opaque composition"),
                    layout: Some(&pipeline_layout),
                    vertex: wgpu::VertexState {
                        module: &module,
                        entry_point: Some("vertex_main"),
                        buffers: &[],
                        compilation_options: Default::default(),
                    },
                    fragment: Some(wgpu::FragmentState {
                        module: &module,
                        entry_point: Some("fragment_main"),
                        compilation_options: Default::default(),
                        targets: &[Some(wgpu::ColorTargetState {
                            format,
                            blend: None,
                            write_mask: wgpu::ColorWrites::ALL,
                        })],
                    }),
                    primitive: Default::default(),
                    depth_stencil: None,
                    multisample: Default::default(),
                    multiview_mask: None,
                    cache: None,
                });
        self.pipelines.insert(format, pipeline);
    }

    /// Destination is owned by UI; source stays owned through the NativeFrame.
    /// The rectangle is the same physical-pixel letterbox used to transform input.
    #[cfg(test)]
    pub(crate) fn compose(
        &self,
        frame: &NativeFrame,
        destination: &wgpu::TextureView,
        format: wgpu::TextureFormat,
        rectangle: [f32; 4],
    ) -> Result<(), String> {
        if frame.context().device != self.context.device {
            return Err("native frame belongs to another GPU context".into());
        }
        let context = self.context.clone();
        let submission = context.submission().map_err(|error| error.to_string())?;
        self.compose_view(
            frame.image.view(),
            destination,
            format,
            rectangle,
            &submission,
        )
    }

    pub(crate) fn compose_view(
        &self,
        source: &wgpu::TextureView,
        destination: &wgpu::TextureView,
        format: wgpu::TextureFormat,
        rectangle: [f32; 4],
        submission: &Submission<'_>,
    ) -> Result<(), String> {
        self.compose_view_load(source, destination, format, rectangle, true, submission)
    }

    fn compose_view_load(
        &self,
        source: &wgpu::TextureView,
        destination: &wgpu::TextureView,
        format: wgpu::TextureFormat,
        rectangle: [f32; 4],
        clear: bool,
        submission: &Submission<'_>,
    ) -> Result<(), String> {
        self.compose_view_region(
            source,
            destination,
            format,
            CompositionRegion {
                rectangle,
                source_uv: [0., 0., 1., 1.],
                scissor: None,
            },
            clear,
            submission,
        )
    }

    fn compose_view_region(
        &self,
        source: &wgpu::TextureView,
        destination: &wgpu::TextureView,
        format: wgpu::TextureFormat,
        region: CompositionRegion,
        clear: bool,
        submission: &Submission<'_>,
    ) -> Result<(), String> {
        self.compose_view_region_sampled(
            source,
            destination,
            format,
            region,
            clear,
            false,
            submission,
        )
    }
    #[allow(
        clippy::too_many_arguments,
        reason = "the operation takes explicit image layout and draw parameters"
    )]
    fn compose_view_region_sampled(
        &self,
        source: &wgpu::TextureView,
        destination: &wgpu::TextureView,
        format: wgpu::TextureFormat,
        region: CompositionRegion,
        clear: bool,
        linear: bool,
        submission: &Submission<'_>,
    ) -> Result<(), String> {
        let rectangle = region.rectangle;
        let pipeline = self
            .pipelines
            .get(&format)
            .ok_or("native destination format was not prepared")?;
        if rectangle.iter().any(|v| !v.is_finite() || *v < 0.0)
            || rectangle[2] == 0.0
            || rectangle[3] == 0.0
        {
            return Err("invalid native composition viewport".into());
        }
        let uv = self.context.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("adv composition sampling"),
            size: 32,
            usage: wgpu::BufferUsages::UNIFORM,
            mapped_at_creation: true,
        });
        {
            let mut bytes = uv
                .slice(..)
                .get_mapped_range_mut()
                .map_err(|error| error.to_string())?;
            let mut packed = [0u8; 32];
            for (out, value) in
                packed
                    .as_chunks_mut::<4>()
                    .0
                    .iter_mut()
                    .zip(region.source_uv.into_iter().chain([
                        if linear { 0.0 } else { rectangle[2] },
                        if linear { 0.0 } else { rectangle[3] },
                        0.0,
                        0.0,
                    ]))
            {
                out.copy_from_slice(&value.to_le_bytes());
            }
            bytes.copy_from_slice(&packed);
        }
        uv.unmap();
        let binding = self
            .context
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("adv native frame"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(source),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.linear_sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: uv.as_entire_binding(),
                    },
                ],
            });
        let mut encoder = self
            .context
            .device
            .create_command_encoder(&Default::default());
        {
            let colors = [Some(wgpu::RenderPassColorAttachment {
                view: destination,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: if clear {
                        wgpu::LoadOp::Clear(wgpu::Color::BLACK)
                    } else {
                        wgpu::LoadOp::Load
                    },
                    store: wgpu::StoreOp::Store,
                },
            })];
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                color_attachments: &colors,
                ..Default::default()
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &binding, &[]);
            pass.set_viewport(
                rectangle[0],
                rectangle[1],
                rectangle[2],
                rectangle[3],
                0.0,
                1.0,
            );
            if let Some(rect) = region.scissor {
                pass.set_scissor_rect(rect.x, rect.y, rect.w, rect.h);
            }
            pass.draw(0..3, 0..1);
        }
        submission.submit([encoder.finish()]);
        Ok(())
    }
}

struct CompositionRegion {
    rectangle: [f32; 4],
    source_uv: [f32; 4],
    scissor: Option<Rect>,
}
impl CompositionRegion {
    fn video(
        rect: Rect,
        source_uv: [f32; 4],
        viewport: [f32; 4],
        canvas: (u32, u32),
        output: (u32, u32),
    ) -> Self {
        let sx = viewport[2] / canvas.0 as f32;
        let sy = viewport[3] / canvas.1 as f32;
        let rectangle = [
            viewport[0] + rect.x as f32 * sx,
            viewport[1] + rect.y as f32 * sy,
            rect.w as f32 * sx,
            rect.h as f32 * sy,
        ];
        let left = rectangle[0].floor().max(0.) as u32;
        let top = rectangle[1].floor().max(0.) as u32;
        let right = (rectangle[0] + rectangle[2]).ceil().min(output.0 as f32) as u32;
        let bottom = (rectangle[1] + rectangle[3]).ceil().min(output.1 as f32) as u32;
        Self {
            rectangle,
            source_uv,
            scissor: Some(Rect {
                x: left,
                y: top,
                w: right - left,
                h: bottom - top,
            }),
        }
    }
}

struct VideoTexture {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    size: (u32, u32),
}
impl VideoTexture {
    fn new(context: &NativeContext, width: u32, height: u32) -> Self {
        let texture = context.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("adv decoded video texture"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Bgra8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        Self {
            texture,
            view,
            size: (width, height),
        }
    }
}
struct VideoOverlay {
    key: u64,
    image: Option<VideoTexture>,
    frame: Option<VideoFrame>,
    border: Option<VideoTexture>,
    color: Option<[u8; 4]>,
    layout: VideoLayout,
}
impl VideoOverlay {
    fn new(key: u64, layout: VideoLayout) -> Self {
        Self {
            key,
            image: None,
            frame: None,
            border: None,
            color: None,
            layout,
        }
    }
    fn update(
        &mut self,
        layer: &VideoLayer,
        layout: VideoLayout,
        context: &NativeContext,
        submission: &Submission<'_>,
    ) -> u64 {
        self.layout = layout;
        let mut uploads = 0;
        if layout.video.is_some() {
            let size = (layer.frame.width, layer.frame.height);
            if self.image.as_ref().is_none_or(|image| image.size != size) {
                self.image = Some(VideoTexture::new(context, size.0, size.1));
                self.frame = None;
            }
            if self.frame.as_ref().is_none_or(|frame| {
                frame.pitch != layer.frame.pitch || !Arc::ptr_eq(&frame.bgra, &layer.frame.bgra)
            }) {
                submission.write_texture(
                    self.image.as_ref().unwrap().texture.as_image_copy(),
                    &layer.frame.bgra,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(layer.frame.pitch),
                        rows_per_image: Some(layer.frame.height),
                    },
                    wgpu::Extent3d {
                        width: layer.frame.width,
                        height: layer.frame.height,
                        depth_or_array_layers: 1,
                    },
                );
                self.frame = Some(layer.frame.clone());
                uploads += 1;
            }
        } else {
            self.image = None;
            self.frame = None;
        }
        if let Some((_, color)) = layout.background {
            if self.border.is_none() {
                self.border = Some(VideoTexture::new(context, 1, 1));
            }
            if self.color != Some(color) {
                submission.write_texture(
                    self.border.as_ref().unwrap().texture.as_image_copy(),
                    &color,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(4),
                        rows_per_image: Some(1),
                    },
                    wgpu::Extent3d {
                        width: 1,
                        height: 1,
                        depth_or_array_layers: 1,
                    },
                );
                self.color = Some(color);
                uploads += 1;
            }
        } else {
            self.border = None;
            self.color = None;
        }
        uploads
    }
}

/// One presentation owner transfers this entire state into each accepted job.
/// The worker consumes the original three-slot mailbox only when it starts,
/// dropping intermediate producer frames before conversion or GPU submission.
pub(crate) struct Session {
    pub(crate) presenter: WindowPresenter,
    frames: FrameConsumer,
    native: Option<NativeFrame>,
    /// The native publication last handed to `draw_view`.
    publication: Option<u64>,
    /// Whether the current frame reached `draw_view`; damage is contiguous only then.
    contiguous: bool,
    /// What the current frame changed since the one before it reached `draw_view`.
    damage: Option<Rect>,
    software: Option<(wgpu::Texture, wgpu::TextureView)>,
    metal_upload: bool,
    snapshot: Snapshot,
    serial: u64,
    width: u32,
    height: u32,
    video: Arc<[(u64, VideoLayer)]>,
    overlays: Vec<VideoOverlay>,
    cursor_regions: Arc<[VideoCursorRegion]>,
    stats: crate::performance::NativePresentation,
}
/// How the frame reaches the screen (HOST-VIEW-9): the drawable size, the
/// frame's rectangle in client pixels, and whether the layer that shows it
/// there magnifies with nearest sampling.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Placement {
    pub(crate) drawable: (u32, u32),
    pub(crate) rectangle: [f32; 4],
    pub(crate) nearest: bool,
}
impl Placement {
    /// An upscaler doubles the frame. The layer magnifies that output when the
    /// viewport is at least as large; otherwise the drawable takes the
    /// viewport's size and the output is resampled into it, both linearly.
    fn upscaled(canvas: (u32, u32), viewport: [f32; 4]) -> Self {
        let output = (canvas.0 * 2, canvas.1 * 2);
        let [_, _, w, h] = viewport;
        Self {
            drawable: if w >= output.0 as f32 && h >= output.1 as f32 {
                output
            } else {
                (w as u32, h as u32)
            },
            rectangle: viewport,
            nearest: false,
        }
    }
    /// The drawable covers the client and the frame is composed into it.
    fn client(client: (u32, u32)) -> Self {
        Self {
            drawable: client,
            rectangle: [0.0, 0.0, client.0 as f32, client.1 as f32],
            nearest: false,
        }
    }
    /// The layer is placed at `viewport` and scales the drawable itself. An
    /// exact integer scale keeps the game resolution under nearest sampling,
    /// which equals sharp-bilinear there. Other scales prescale by the integer
    /// part with nearest sampling and leave the rest to linear filtering, which
    /// is the definition of sharp-bilinear.
    fn layer(canvas: (u32, u32), viewport: [f32; 4]) -> Self {
        let [_, _, w, h] = viewport;
        let exact =
            |size: f32, game: u32| size >= game as f32 && (size / game as f32).fract() == 0.0;
        let factor = |size: f32, game: u32| (size / game as f32).floor().max(1.0) as u32;
        let nearest = exact(w, canvas.0) && exact(h, canvas.1);
        Self {
            drawable: if nearest {
                canvas
            } else {
                (
                    canvas.0 * factor(w, canvas.0),
                    canvas.1 * factor(h, canvas.1),
                )
            },
            rectangle: viewport,
            nearest,
        }
    }
}
/// The bounding box of `rects`, if any.
fn bounds(rects: &[Rect]) -> Option<Rect> {
    let first = *rects.first()?;
    Some(rects.iter().fold(first, |a, b| {
        let x = a.x.min(b.x);
        let y = a.y.min(b.y);
        Rect {
            x,
            y,
            w: (a.x + a.w).max(b.x + b.w) - x,
            h: (a.y + a.h).max(b.y + b.h) - y,
        }
    }))
}
pub(crate) struct Rendered {
    pub(crate) result: DrawResult,
    /// Set when this render presented with it.
    #[cfg(target_vendor = "apple")]
    pub(crate) placement: Option<Placement>,
    pub(crate) serial: u64,
    #[cfg(host_view)]
    pub(crate) published_at: Option<Instant>,
    pub(crate) canvas: (u32, u32),
    pub(crate) cursor_regions: Arc<[VideoCursorRegion]>,
    #[cfg(target_os = "macos")]
    pub(crate) shared: Option<(NativeFrame, adv_device::gpu::DisplayCompletion)>,
}
impl Session {
    pub(crate) fn new(presenter: WindowPresenter, frames: FrameConsumer) -> Self {
        let metal_upload = cfg!(target_vendor = "apple")
            && presenter.context.adapter.get_info().backend == wgpu::Backend::Metal;
        Self {
            presenter,
            frames,
            native: None,
            publication: None,
            contiguous: false,
            damage: None,
            software: None,
            metal_upload,
            snapshot: Snapshot::default(),
            serial: 0,
            width: 0,
            height: 0,
            video: Arc::from([]),
            overlays: Vec::new(),
            cursor_regions: Arc::from([]),
            stats: crate::performance::NativePresentation {
                // Includes the real startup configure, which ran before any
                // guest submission. Its cost is distinct from max_job_ns.
                configurations: 1,
                ..Default::default()
            },
        }
    }
    pub(crate) fn context(&self) -> NativeContext {
        self.presenter.context.clone()
    }

    fn needs_configuration(&self, dimensions: (u32, u32)) -> bool {
        self.placement(dimensions).drawable
            != (
                self.presenter.configuration.width,
                self.presenter.configuration.height,
            )
    }

    /// The frame's rectangle in the client, in physical pixels.
    fn viewport(&self, client: (u32, u32)) -> [f32; 4] {
        if self.presenter.scaling == crate::Scaling::Fit {
            crate::window::native_viewport(client.0, client.1, self.width, self.height)
        } else {
            self.presenter
                .scaling
                .rectangle(
                    (f64::from(client.0), f64::from(client.1)),
                    (self.width, self.height),
                )
                .map(|v| v as f32)
        }
    }

    /// On macOS and iOS the game view's layer scales the frame, so no display-resolution
    /// pass runs. Composed video still draws at the client size.
    fn placement(&self, client: (u32, u32)) -> Placement {
        let canvas = (self.width, self.height);
        if !cfg!(any(target_os = "macos", target_os = "ios"))
            || !self.overlays.is_empty()
            || canvas.0 == 0
            || canvas.1 == 0
        {
            return Placement::client(client);
        }
        let viewport = self.viewport(client);
        if self.presenter.upscaler != crate::Upscaler::Off
            && viewport[2] > canvas.0 as f32
            && viewport[3] > canvas.1 as f32
        {
            Placement::upscaled(canvas, viewport)
        } else {
            Placement::layer(canvas, viewport)
        }
    }

    fn update_video(
        &mut self,
        context: &NativeContext,
        submission: &Submission<'_>,
    ) -> Result<(), String> {
        let mut used = 0;
        let mut cursor_regions = Vec::new();
        for (key, layer) in self.video.iter() {
            let layout = layer.layout((self.width, self.height))?;
            if let Some(region) = layout.cursor_region(layer.hide_cursor) {
                cursor_regions.push(region);
            }
            if layout.video.is_none() && layout.background.is_none() {
                continue;
            }
            if self.overlays.get(used).is_none_or(|old| old.key != *key) {
                let overlay = VideoOverlay::new(*key, layout);
                if used < self.overlays.len() {
                    self.overlays[used] = overlay;
                } else {
                    self.overlays.push(overlay);
                }
            }
            let overlay = &mut self.overlays[used];
            self.stats.uploads += overlay.update(layer, layout, context, submission);
            used += 1;
        }
        self.overlays.truncate(used);
        self.cursor_regions = Arc::from(cursor_regions);
        Ok(())
    }

    pub(crate) fn render(
        &mut self,
        width: u32,
        height: u32,
        repaint: bool,
        _wake: &WakeEvent,
    ) -> Result<Rendered, String> {
        let context = self.presenter.context.clone();
        let mut changed = false;
        if self.frames.update() {
            self.stats.consumed += 1;
            let video = self.frames.current().shared_video_layers();
            let video_changed = !Arc::ptr_eq(&self.video, &video);
            self.video = video;
            if let Some(frame) = self.frames.current().native() {
                if frame.context().device != context.device {
                    return Err("frame GPU context changed and needs presenter recovery".into());
                }
                let update_video = video_changed
                    || self.native.is_none()
                    || (self.width, self.height) != self.frames.current().extent();
                self.width = frame.image.width();
                self.height = frame.image.height();
                self.serial = frame.serial;
                self.damage = frame
                    .damage
                    .since(self.publication.filter(|_| self.contiguous));
                self.publication = Some(frame.damage.publication());
                self.contiguous = false;
                self.native = Some(frame.clone());
                if update_video {
                    let submission = context.submission().map_err(|error| error.to_string())?;
                    self.update_video(&context, &submission)?;
                }
                changed = true;
            } else {
                let update_video = video_changed
                    || self.native.is_some()
                    || (self.width, self.height) != self.frames.current().extent();
                let submission = context.submission().map_err(|error| error.to_string())?;
                self.cursor_regions = Arc::from(self.frames.current().video_cursor_regions()?);
                if self.native.is_some() {
                    self.snapshot.frames = 0;
                }
                let old_extent = (self.snapshot.width, self.snapshot.height);
                let frame = self.frames.current();
                let since = if old_extent == frame.extent() {
                    self.snapshot.frames
                } else {
                    0
                };
                let metal = self.metal_upload;
                let no_copy = metal;
                if no_copy {
                    (self.snapshot.width, self.snapshot.height) = frame.extent();
                    self.snapshot.frames = frame.serial();
                    self.snapshot.damage = frame.damage_since(since);
                } else {
                    frame.snapshot_base(&mut self.snapshot)?;
                }
                if self.snapshot.width == 0 || self.snapshot.height == 0 {
                    return Ok(Rendered {
                        #[cfg(target_os = "macos")]
                        shared: None,
                        result: DrawResult::Skipped,
                        #[cfg(target_vendor = "apple")]
                        placement: None,
                        serial: self.serial,
                        #[cfg(host_view)]
                        published_at: self.frames.current().published_at(),
                        canvas: (self.width, self.height),
                        cursor_regions: self.cursor_regions.clone(),
                    });
                }
                changed = !self.snapshot.damage.is_empty();
                self.width = self.snapshot.width;
                self.height = self.snapshot.height;
                self.serial = self.snapshot.frames;
                if changed {
                    self.damage = bounds(&self.snapshot.damage)
                        .filter(|_| self.contiguous && self.native.is_none());
                    self.publication = None;
                    self.contiguous = false;
                    #[cfg(target_vendor = "apple")]
                    let direct = if no_copy {
                        submission
                            .software_texture(self.frames.current())
                            .map_err(|error| error.to_string())?
                    } else {
                        None
                    };
                    #[cfg(not(target_vendor = "apple"))]
                    let direct: Option<wgpu::Texture> = None;
                    let sampled = direct.is_some();
                    let reset_upload = self.software.as_ref().is_none_or(|(texture, _)| {
                        !texture.usage().contains(wgpu::TextureUsages::COPY_DST)
                    }) || old_extent != (self.width, self.height);
                    if let Some(texture) = direct {
                        let view = texture.create_view(&Default::default());
                        self.software = Some((texture, view));
                    } else if reset_upload {
                        let texture = context.device.create_texture(&wgpu::TextureDescriptor {
                            label: Some("adv final software frame"),
                            size: wgpu::Extent3d {
                                width: self.width,
                                height: self.height,
                                depth_or_array_layers: 1,
                            },
                            mip_level_count: 1,
                            sample_count: 1,
                            dimension: wgpu::TextureDimension::D2,
                            format: wgpu::TextureFormat::Bgra8Unorm,
                            usage: wgpu::TextureUsages::TEXTURE_BINDING
                                | wgpu::TextureUsages::COPY_DST
                                | if metal {
                                    wgpu::TextureUsages::STORAGE_BINDING
                                        | wgpu::TextureUsages::RENDER_ATTACHMENT
                                } else {
                                    wgpu::TextureUsages::empty()
                                },
                            view_formats: &[],
                        });
                        let view = texture.create_view(&Default::default());
                        self.software = Some((texture, view));
                    }
                    let texture = &self.software.as_ref().unwrap().0;
                    if !sampled {
                        if no_copy {
                            #[cfg(target_vendor = "apple")]
                            {
                                submission
                                    .upload_software(
                                        texture,
                                        self.frames.current(),
                                        if reset_upload { 0 } else { since },
                                    )
                                    .map_err(|error| error.to_string())?;
                                self.stats.gpu_transfer_bytes += self
                                    .snapshot
                                    .damage
                                    .iter()
                                    .map(|r| u64::from(r.w) * u64::from(r.h) * 4)
                                    .sum::<u64>();
                            }
                        } else {
                            let bytes =
                                crate::software_frame::upload(&submission, texture, &self.snapshot);
                            self.stats.uploaded_bytes += bytes;
                            self.stats.converted_bytes += bytes;
                        }
                        self.stats.uploads += self.snapshot.damage.len() as u64;
                    }
                    self.stats.software_started.get_or_insert_with(Instant::now);
                }
                self.native = None;
                if update_video {
                    self.update_video(&context, &submission)?;
                }
            }
        }
        let canvas = (self.width, self.height);
        if (!changed && !repaint)
            || self.width == 0
            || self.height == 0
            || width == 0
            || height == 0
        {
            return Ok(Rendered {
                #[cfg(target_os = "macos")]
                shared: None,
                result: DrawResult::Skipped,
                #[cfg(target_vendor = "apple")]
                placement: None,
                serial: self.serial,
                #[cfg(host_view)]
                published_at: self.frames.current().published_at(),
                canvas,
                cursor_regions: self.cursor_regions.clone(),
            });
        }
        let placement = self.placement((width, height));
        #[cfg(target_os = "macos")]
        if self.presenter.direct_layer
            && self.overlays.is_empty()
            && placement.drawable == canvas
            && let Some(frame) = self
                .native
                .as_ref()
                .filter(|frame| frame.io_surface().is_some())
        {
            let completion = frame
                .display_completion(_wake.clone())
                .map_err(|e| e.to_string())?;
            self.contiguous = true;
            self.stats.presented += 1;
            self.stats.direct_presentations += 1;
            return Ok(Rendered {
                result: DrawResult::Presented,
                placement: Some(placement),
                serial: self.serial,
                #[cfg(host_view)]
                published_at: self.frames.current().published_at(),
                canvas,
                cursor_regions: self.cursor_regions.clone(),
                shared: Some((frame.clone(), completion)),
            });
        }
        let size = placement.drawable;
        let rectangle = if placement == Placement::client((width, height)) {
            self.viewport((width, height))
        } else {
            [0.0, 0.0, size.0 as f32, size.1 as f32]
        };
        let view = if let Some(frame) = &self.native {
            frame.image.view()
        } else {
            &self
                .software
                .as_ref()
                .ok_or("missing final software frame")?
                .1
        };
        let submission = context.submission().map_err(|error| error.to_string())?;
        let result = self.presenter.draw_view(
            view,
            size,
            rectangle,
            canvas,
            // A frame consumed without reaching `draw_view` still needs upscaling.
            changed || !self.contiguous,
            self.damage.filter(|_| !repaint && self.overlays.is_empty()),
            &self.overlays,
            &submission,
        )?;
        self.contiguous = true;
        self.stats.presented += u64::from(matches!(result, DrawResult::Presented));
        Ok(Rendered {
            #[cfg(target_os = "macos")]
            shared: None,
            #[cfg(target_vendor = "apple")]
            placement: matches!(result, DrawResult::Presented).then_some(placement),
            result,
            serial: self.serial,
            #[cfg(host_view)]
            published_at: self.frames.current().published_at(),
            canvas,
            cursor_regions: self.cursor_regions.clone(),
        })
    }
}

struct Completion {
    session: Session,
    result: Result<Rendered, String>,
    elapsed: Duration,
    at: Instant,
}
struct Job {
    task: Option<background::Task>,
    ticket: Option<background::Ticket>,
    completion: spsc::Consumer<Completion>,
}
pub(crate) struct Completed {
    pub(crate) rendered: Rendered,
    pub(crate) elapsed: Duration,
    pub(crate) at: Instant,
    pub(crate) stats: crate::performance::NativePresentation,
}

/// The UI owns only an observer and at most one transfer in flight. The common
/// worker owns Surface acquisition, configuration and the original consumer.
/// The HWND must outlive `shutdown` of the service and this coordinator.
pub(crate) struct UiPresenter {
    upscaler: crate::Upscaler,
    scaling: crate::Scaling,
    session: Option<Session>,
    observer: Observer<adv_device::surface::PendingFrame>,
    sender: background::Sender,
    wake: WakeEvent,
    job: Option<Job>,
    reconfigure: bool,
    #[cfg(target_os = "macos")]
    waiting: Option<Completed>,
    #[cfg(target_os = "android")]
    surface_live: Arc<std::sync::atomic::AtomicBool>,
}
impl UiPresenter {
    pub(crate) fn new(
        presenter: WindowPresenter,
        frames: FrameConsumer,
        sender: background::Sender,
        wake: WakeEvent,
    ) -> Self {
        let observer = frames.observer();
        Self {
            session: Some(Session::new(presenter, frames)),
            upscaler: crate::Upscaler::Off,
            scaling: crate::Scaling::Fit,
            observer,
            sender,
            wake,
            job: None,
            reconfigure: false,
            #[cfg(target_os = "macos")]
            waiting: None,
            #[cfg(target_os = "android")]
            surface_live: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        }
    }
    #[cfg(target_os = "android")]
    pub(crate) unsafe fn replace_window(
        &mut self,
        window: Option<std::ptr::NonNull<ndk_sys::ANativeWindow>>,
    ) -> Result<(), String> {
        use std::sync::atomic::Ordering;
        self.surface_live.store(false, Ordering::Release);
        // Pending tasks return their consumer without rendering. A task already
        // using the old surface finishes while the host still owns its window.
        let mut failure = None;
        while self.job.is_some() {
            if let Err(error) = self.poll() {
                failure = Some(error);
                break;
            }
            if self.job.is_some() {
                self.wake.wait(None).map_err(|error| error.to_string())?;
            }
        }
        if let Some(session) = self.session.as_mut() {
            unsafe {
                session.presenter.replace_window(window)?;
            }
        }
        self.reconfigure = window.is_some();
        self.surface_live.store(window.is_some(), Ordering::Release);
        failure.map_or(Ok(()), Err)
    }
    #[cfg(host_view)]
    pub(crate) fn set_scaling(&mut self, value: crate::Scaling) {
        self.scaling = value;
    }
    #[cfg(host_view)]
    pub(crate) fn set_upscaler(&mut self, value: crate::Upscaler) {
        self.upscaler = value;
    }
    pub(crate) fn has_update(&self) -> bool {
        self.observer.has_update() || self.reconfigure
    }
    pub(crate) fn in_flight(&self) -> bool {
        #[cfg(target_os = "macos")]
        if self.waiting.is_some() {
            return true;
        }
        self.job.is_some()
    }
    pub(crate) fn poll(&mut self) -> Result<Option<Completed>, String> {
        #[cfg(target_os = "macos")]
        if let Some(waiting) = &self.waiting {
            if !waiting
                .rendered
                .shared
                .as_ref()
                .unwrap()
                .1
                .ready()
                .map_err(|e| e.to_string())?
            {
                return Ok(None);
            }
            return Ok(self.waiting.take());
        }
        if let Some(job) = &mut self.job {
            if let Some(done) = job.completion.pop() {
                self.session = Some(done.session);
                self.job = None;
                let rendered = done.result?;
                self.reconfigure = matches!(rendered.result, DrawResult::Reconfigure);
                let completed = Completed {
                    rendered,
                    elapsed: done.elapsed,
                    at: done.at,
                    stats: self.session.as_ref().unwrap().stats,
                };
                #[cfg(target_os = "macos")]
                if let Some((_, fence)) = &completed.rendered.shared
                    && !fence.ready().map_err(|e| e.to_string())?
                {
                    self.waiting = Some(completed);
                    return Ok(None);
                }
                return Ok(Some(completed));
            }
            if let Some(ticket) = &job.ticket
                && matches!(
                    ticket.status(),
                    background::Status::Panicked | background::Status::Cancelled
                )
            {
                return Err("owned GPU presentation task failed before returning its state".into());
            }
            self.retry()?;
        }
        Ok(None)
    }
    fn retry(&mut self) -> Result<(), String> {
        let Some(job) = &mut self.job else {
            return Ok(());
        };
        let Some(task) = job.task.take() else {
            return Ok(());
        };
        match self.sender.try_submit(background::Priority::Urgent, task) {
            Ok(ticket) => job.ticket = Some(ticket),
            Err(std::sync::mpsc::TrySendError::Full(task)) => job.task = Some(task),
            Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                return Err("GPU background service closed before accepting presentation".into());
            }
        }
        Ok(())
    }
    /// False means the existing transfer is still owned by the worker. Dirty
    /// observation is never cleared here, including a full submission queue.
    pub(crate) fn schedule(
        &mut self,
        dimensions: (u32, u32),
        repaint: bool,
    ) -> Result<bool, String> {
        if !can_schedule(
            self.in_flight(),
            dimensions,
            self.session
                .as_ref()
                .is_none_or(|s| s.presenter.surface.is_some()),
        ) {
            return Ok(false);
        }
        let session = self
            .session
            .as_mut()
            .expect("idle presenter owns its session");
        session.presenter.upscaler = self.upscaler;
        session.presenter.scaling = self.scaling;
        let configure = self.reconfigure || session.needs_configuration(dimensions);
        let drawable = session.placement(dimensions).drawable;
        // Pause is nonblocking. The worker receives its token and waits for the
        // last short Queue lease before configure waits for GPU idle.
        let pause = configure
            .then(|| session.context().pause_submissions())
            .transpose()
            .map_err(|error| error.to_string())?;
        let mut session = self.session.take().unwrap();
        let (mut output, completion) = spsc::channel(1);
        let scheduled = Instant::now();
        let wake = self.wake.clone();
        #[cfg(target_os = "android")]
        let surface_live = self.surface_live.clone();
        let task = background::Task::new(move || {
            let started = Instant::now();
            session.stats.max_queue_ns = session
                .stats
                .max_queue_ns
                .max(nanoseconds(started.saturating_duration_since(scheduled)));
            let result = (|| {
                #[cfg(target_os = "android")]
                if !surface_live.load(std::sync::atomic::Ordering::Acquire) {
                    return Ok(Rendered {
                        result: DrawResult::Skipped,
                        serial: session.serial,
                        published_at: session.frames.current().published_at(),
                        canvas: (session.width, session.height),
                        cursor_regions: session.cursor_regions.clone(),
                    });
                }
                if let Some(pause) = pause {
                    pause.wait_drained().map_err(|error| error.to_string())?;
                    session
                        .presenter
                        .configure_exclusive(drawable.0, drawable.1)?;
                    session.stats.configurations += 1;
                    drop(pause);
                }
                session.render(dimensions.0, dimensions.1, repaint || configure, &wake)
            })();
            session.stats.max_job_ns = session.stats.max_job_ns.max(nanoseconds(started.elapsed()));
            session.stats.max_acquire_ns = session.presenter.max_acquire_ns.get();
            session.stats.max_configure_ns = session.presenter.max_configure_ns;
            (session.stats.upscaled_runs, session.stats.upscaled_pixels) =
                session.presenter.upscaled;
            let _ = output.push(Completion {
                session,
                result,
                elapsed: started.elapsed(),
                at: Instant::now(),
            });
        })
        .finish_on_shutdown()
        .with_wake(self.wake.clone());
        self.job = Some(Job {
            task: Some(task),
            ticket: None,
            completion,
        });
        self.retry()?;
        Ok(true)
    }
}

fn can_schedule(in_flight: bool, dimensions: (u32, u32), surface_live: bool) -> bool {
    !in_flight && dimensions.0 != 0 && dimensions.1 != 0 && surface_live
}
fn nanoseconds(duration: Duration) -> u64 {
    duration.as_nanos().min(u64::MAX as u128) as u64
}

const SHADER: &str = r#"
struct Fragment { @builtin(position) position: vec4<f32>, @location(0) uv: vec2<f32> }
@group(0) @binding(0) var image: texture_2d<f32>;
@group(0) @binding(1) var image_sampler: sampler;
struct Sampling { source_uv: vec4<f32>, viewport: vec2<f32> }
@group(0) @binding(2) var<uniform> sampling: Sampling;
@vertex fn vertex_main(@builtin(vertex_index) index: u32) -> Fragment {
    let positions = array(vec2(-1.0,-1.0),vec2(3.0,-1.0),vec2(-1.0,3.0));
    let position = positions[index];
    var output: Fragment;
    output.position = vec4(position,0.0,1.0);
    output.uv = sampling.source_uv.xy + vec2((position.x+1.0)*0.5,(1.0-position.y)*0.5) * sampling.source_uv.zw;
    return output;
}
@fragment fn fragment_main(input: Fragment) -> @location(0) vec4<f32> {
    // Sharp-bilinear remapping: rsn8887/TheMaister, public domain.
    // https://github.com/libretro/glsl-shaders/blob/master/interpolation/shaders/sharp-bilinear-simple.glsl
    // Emulate integer nearest prescaling followed by bilinear fitting, without
    // an intermediate image. A zero viewport selects ordinary linear sampling.
    let size = vec2<f32>(textureDimensions(image));
    let scale = max(floor(sampling.viewport / (size * sampling.source_uv.zw)), vec2(1.0));
    let texel = input.uv * size;
    let offset = fract(texel) - 0.5;
    let plateau = vec2(0.5) - 0.5 / scale;
    let uv = (floor(texel) + 0.5 + (offset - clamp(offset, -plateau, plateau)) * scale) / size;
    var rgb = textureSample(image,image_sampler,uv).rgb;
    // Legacy BGRA pixels already contain display-encoded values. Compensate an
    // sRGB attachment's encode; a second gamma transform would brighten them.
    if OUTPUT_SRGB {
        rgb = select(pow((rgb+vec3(0.055))/1.055,vec3(2.4)),rgb/12.92,rgb<=vec3(0.04045));
    }
    return vec4(rgb,1.0);
}
"#;

#[cfg(all(test, target_os = "macos"))]
#[path = "native_gpu/display_tests.rs"]
mod display_tests;

#[cfg(test)]
mod tests {
    // spec: AND-APP-10
    #[test]
    fn detached_surface_never_accepts_render_work() {
        assert!(!super::can_schedule(false, (640, 480), false));
        assert!(super::can_schedule(false, (640, 480), true));
        assert!(!super::can_schedule(true, (640, 480), true));
    }
    use super::*;
    use adv_device::gpu::Gpu;
    use adv_device::surface::{Rect, SurfaceMemory};
    use std::borrow::Cow;
    use std::time::Duration;

    struct Memory(Vec<u8>);
    impl SurfaceMemory for Memory {
        fn read(&self, at: u32, len: usize) -> Result<Cow<'_, [u8]>, String> {
            Ok(Cow::Borrowed(&self.0[at as usize..at as usize + len]))
        }
        fn write(&mut self, at: u32, data: &[u8]) -> Result<(), String> {
            self.0[at as usize..at as usize + data.len()].copy_from_slice(data);
            Ok(())
        }
        fn writable(&self, _: u32, _: usize) -> Result<(), String> {
            Ok(())
        }
        fn copy(&mut self, src: u32, dst: u32, len: usize) -> Result<(), String> {
            self.0
                .copy_within(src as usize..src as usize + len, dst as usize);
            Ok(())
        }
    }

    // spec: HOST-VIEW-1
    // spec: HOST-VIEW-9
    #[test]
    fn the_layer_scales_frames_from_their_integer_prescale() {
        let fit = |client: (u32, u32), canvas: (u32, u32)| {
            let viewport = crate::window::native_viewport(client.0, client.1, canvas.0, canvas.1);
            let placement = Placement::layer(canvas, viewport);
            assert_eq!(placement.rectangle, viewport);
            (placement.drawable, placement.nearest)
        };
        let game = (1920, 1080);
        // Exact integer scales keep the game resolution under nearest sampling.
        assert_eq!(fit((3840, 2160), game), (game, true));
        assert_eq!(fit((3840, 2400), game), (game, true));
        assert_eq!(fit((1920, 1080), game), (game, true));
        // Other scales prescale by the integer part; the layer filters linearly.
        assert_eq!(fit((2560, 1440), game), (game, false));
        assert_eq!(fit((3000, 2000), (1280, 720)), ((2560, 1440), false));
        // The layer sits on whole pixels, so an odd margin no longer matters.
        assert_eq!(fit((3840, 2161), game), (game, true));
        assert_eq!(fit((960, 540), game), (game, false));
        // Integer mode places an exact multiple, margins included.
        let integer = crate::Scaling::Integer
            .rectangle((4000.0, 2400.0), (1280, 720))
            .map(|v| v as f32);
        let placement = Placement::layer((1280, 720), integer);
        assert_eq!((placement.drawable, placement.nearest), ((1280, 720), true));
        assert_eq!(placement.rectangle, [80.0, 120.0, 3840.0, 2160.0]);
        // Upscaled output is magnified by the layer when it fits the viewport,
        // and resampled to the viewport's size when it is larger.
        let upscaled = |client: (u32, u32), canvas: (u32, u32)| {
            let viewport = crate::window::native_viewport(client.0, client.1, canvas.0, canvas.1);
            Placement::upscaled(canvas, viewport).drawable
        };
        assert_eq!(upscaled((3840, 2160), (1280, 720)), (2560, 1440));
        assert_eq!(upscaled((2560, 1440), (1280, 720)), (2560, 1440));
        assert_eq!(upscaled((2560, 1440), game), (2560, 1440));
        assert_eq!(upscaled((3000, 2000), game), (3000, 1687));
        // Stretch prescales each axis by its own integer part.
        let stretch = crate::Scaling::Stretch
            .rectangle((3000.0, 1500.0), (1280, 720))
            .map(|v| v as f32);
        assert_eq!(
            Placement::layer((1280, 720), stretch).drawable,
            (2560, 1440)
        );
    }
    #[test]
    fn sharp_bilinear_preserves_integer_pixels_and_blends_fractional_boundaries() {
        let mut gpu = pollster::block_on(Gpu::request(!cfg!(host_view))).unwrap();
        let source = gpu.image(2, 2).unwrap();
        let pixels: Vec<_> = [0, 255, 255, 0]
            .into_iter()
            .flat_map(|v| [v, v, v, 255])
            .collect();
        gpu.upload(
            &source,
            Rect {
                x: 0,
                y: 0,
                w: 2,
                h: 2,
            },
            8,
            &pixels,
        )
        .unwrap();
        let context = gpu.context().clone();
        let compositor = Compositor::new(context.clone(), wgpu::TextureFormat::Bgra8Unorm);
        let cases: &[(bool, &[u8])] = &[
            (false, &[128]),
            (false, &[0, 255]),
            (false, &[0, 128, 255]),
            (false, &[0, 0, 255, 255]),
            (false, &[0, 0, 128, 255, 255]),
            (false, &[0, 0, 0, 255, 255, 255]),
            (true, &[0, 64, 191, 255]),
        ];
        for &(linear, ramp) in cases {
            let n = ramp.len() as u32;
            let output = gpu.image(n + 2, n + 2).unwrap();
            compositor
                .compose_view_region_sampled(
                    source.view(),
                    output.view(),
                    wgpu::TextureFormat::Bgra8Unorm,
                    CompositionRegion {
                        rectangle: [1., 1., n as f32, n as f32],
                        source_uv: [0., 0., 1., 1.],
                        scissor: None,
                    },
                    true,
                    linear,
                    &context.submission().unwrap(),
                )
                .unwrap();
            let mut transfer = gpu.readback(&output, None).unwrap();
            transfer.take_completion_job().unwrap()();
            let bytes = transfer.poll().unwrap().unwrap();
            for y in 0..n + 2 {
                for x in 0..n + 2 {
                    let expected = if x == 0 || y == 0 || x == n + 1 || y == n + 1 {
                        0.0
                    } else {
                        let a = f32::from(ramp[(x - 1) as usize]) / 255.;
                        let b = f32::from(ramp[(y - 1) as usize]) / 255.;
                        255. * (a * (1. - b) + b * (1. - a))
                    };
                    let offset = ((y * (n + 2) + x) * 4) as usize;
                    for &value in &bytes[offset..offset + 3] {
                        assert!(
                            (f32::from(value) - expected).abs() <= 1.5,
                            "size={n}, linear={linear}, ({x},{y}): {value} != {expected}"
                        );
                    }
                    assert_eq!(bytes[offset + 3], 255);
                }
            }
        }
    }

    // spec: HOST-VIEW-3
    #[test]
    fn video_load_pass_preserves_native_base_and_restores_it_without_a_window() {
        use adv_device::surface::{BlitRect, VideoFrame};
        let mut gpu = pollster::block_on(Gpu::request(!cfg!(host_view))).unwrap();
        let target = gpu.target(6, 2, false).unwrap();
        gpu.clear(&target, Some([1., 0., 0., 1.]), None).unwrap();
        let base = gpu.present(&target.image, 1).unwrap();
        let destination = gpu.image(6, 2).unwrap();
        let layer = VideoLayer {
            frame: VideoFrame {
                width: 4,
                height: 1,
                pitch: 16,
                bgra: Arc::from([
                    7, 7, 7, 255, 255, 0, 0, 255, 0, 255, 0, 255, 255, 0, 255, 255,
                ]),
            },
            source: Rect {
                x: 1,
                y: 0,
                w: 2,
                h: 1,
            },
            destination: BlitRect {
                x: -160,
                y: 0,
                w: 640,
                h: 544,
            },
            background: Some((
                BlitRect {
                    x: 0,
                    y: 0,
                    w: 800,
                    h: 544,
                },
                [9, 0, 0, 255],
            )),
            clip: Rect {
                x: 0,
                y: 0,
                w: 800,
                h: 544,
            },
            canvas: (960, 544),
            visible: true,
            hide_cursor: false,
        };
        let mut patch = Snapshot::default();
        let bounds = layer.rasterize((6, 2), &mut patch).unwrap().unwrap();
        let layout = layer.layout((6, 2)).unwrap();
        let mut overlay = VideoOverlay::new(1, layout);
        let compositor = Compositor::new(gpu.context().clone(), wgpu::TextureFormat::Bgra8Unorm);
        compositor
            .compose(
                &base,
                destination.view(),
                wgpu::TextureFormat::Bgra8Unorm,
                [0., 0., 6., 2.],
            )
            .unwrap();
        {
            let context = gpu.context().clone();
            let submission = context.submission().unwrap();
            assert_eq!(overlay.update(&layer, layout, &context, &submission), 2);
            let mut moved = layer.clone();
            moved.destination.x += 160;
            assert_eq!(
                overlay.update(&moved, moved.layout((6, 2)).unwrap(), &context, &submission),
                0
            );
            assert_eq!(overlay.update(&layer, layout, &context, &submission), 0);
            let background = layout.background.unwrap().0;
            compositor
                .compose_view_region(
                    &overlay.border.as_ref().unwrap().view,
                    destination.view(),
                    wgpu::TextureFormat::Bgra8Unorm,
                    CompositionRegion::video(
                        background,
                        [0., 0., 1., 1.],
                        [0., 0., 6., 2.],
                        (6, 2),
                        (6, 2),
                    ),
                    false,
                    &submission,
                )
                .unwrap();
            let video = layout.video.unwrap();
            compositor
                .compose_view_region(
                    &overlay.image.as_ref().unwrap().view,
                    destination.view(),
                    wgpu::TextureFormat::Bgra8Unorm,
                    CompositionRegion::video(
                        video.destination,
                        video.source_uv,
                        [0., 0., 6., 2.],
                        (6, 2),
                        (6, 2),
                    ),
                    false,
                    &submission,
                )
                .unwrap();
        }
        assert_eq!(gpu.stats().readbacks, 0);
        let mut transfer = gpu.readback(&destination, None).unwrap();
        transfer.take_completion_job().unwrap()();
        let composed = transfer.poll().unwrap().unwrap();
        assert_eq!(
            composed,
            [
                255, 0, 0, 255, 0, 255, 0, 255, 0, 255, 0, 255, 9, 0, 0, 255, 9, 0, 0, 255, 0, 0,
                255, 255
            ]
            .repeat(2)
        );
        for y in 0..2usize {
            assert_eq!(
                &composed[y * 24..y * 24 + bounds.w as usize * 4],
                &patch.bgra[y * 20..(y + 1) * 20]
            );
        }
        compositor
            .compose(
                &base,
                destination.view(),
                wgpu::TextureFormat::Bgra8Unorm,
                [0., 0., 6., 2.],
            )
            .unwrap();
        let mut transfer = gpu.readback(&destination, None).unwrap();
        transfer.take_completion_job().unwrap()();
        assert_eq!(
            transfer.poll().unwrap().unwrap(),
            [0, 0, 255, 255].repeat(12)
        );
    }

    // spec: HOST-VIEW-3
    #[test]
    fn software_video_overlays_match_cpu_composition_without_a_window() {
        use adv_device::surface::{BlitRect, PixelFormat, Surfaces, frame_mailbox};
        let mut gpu = pollster::block_on(Gpu::request(!cfg!(host_view))).unwrap();
        let context = gpu.context().clone();
        let mut memory = Memory(vec![0; 1024]);
        let mut surfaces = Surfaces::new(6, 4, 16, 1008, Snapshot::default()).unwrap();
        let id = surfaces
            .create(6, 4, false, PixelFormat::Bgra32, &mut memory)
            .unwrap();
        memory
            .write(surfaces[id].pixels, &[31, 63, 95, 255].repeat(24))
            .unwrap();
        let (producer, consumer) = frame_mailbox();
        let wake = WakeEvent::new().unwrap();
        surfaces.set_frame_mailbox(producer, wake.clone());
        surfaces.present(id, &memory, surfaces[id].rect()).unwrap();
        let layer = VideoLayer {
            frame: VideoFrame {
                width: 2,
                height: 1,
                pitch: 8,
                bgra: Arc::from([255, 0, 0, 255, 0, 255, 0, 255]),
            },
            source: Rect {
                x: 0,
                y: 0,
                w: 2,
                h: 1,
            },
            destination: BlitRect {
                x: 2,
                y: 1,
                w: 2,
                h: 2,
            },
            background: Some((
                BlitRect {
                    x: 1,
                    y: 0,
                    w: 4,
                    h: 4,
                },
                [9, 0, 0, 255],
            )),
            clip: Rect {
                x: 1,
                w: 4,
                ..surfaces[id].rect()
            },
            canvas: (6, 4),
            visible: true,
            hide_cursor: false,
        };
        surfaces.set_video_layer(1, Some(layer), &memory).unwrap();
        let format = wgpu::TextureFormat::Bgra8Unorm;
        let presenter = WindowPresenter {
            surface: None,
            #[cfg(target_os = "android")]
            android_window: None,
            context: context.clone(),
            configuration: wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format,
                width: 6,
                height: 4,
                present_mode: wgpu::PresentMode::Fifo,
                desired_maximum_frame_latency: 2,
                alpha_mode: wgpu::CompositeAlphaMode::Opaque,
                color_space: wgpu::SurfaceColorSpace::Srgb,
                view_formats: vec![],
            },
            compositor: Compositor::new(context.clone(), format),
            upscaler: crate::Upscaler::Off,
            scaling: crate::Scaling::Fit,
            upscaling: None,
            max_acquire_ns: Cell::new(0),
            max_configure_ns: 0,
            upscaled: (0, 0),
            #[cfg(target_os = "macos")]
            direct_layer: false,
        };
        let mut session = Session::new(presenter, consumer);
        session.render(6, 4, false, &wake).unwrap();
        assert_eq!(session.overlays.len(), 1);
        assert!(session.native.is_none());
        assert_eq!(session.snapshot.bgra.is_empty(), session.metal_upload);
        let mut expected = Snapshot::default();
        session.frames.current().snapshot(&mut expected).unwrap();
        let output = gpu.image(6, 4).unwrap();
        session
            .presenter
            .compositor
            .compose_frame(
                &session.software.as_ref().unwrap().1,
                output.view(),
                format,
                [0., 0., 6., 4.],
                (6, 4),
                (6, 4),
                &session.overlays,
                &context.submission().unwrap(),
            )
            .unwrap();
        let mut transfer = gpu.readback(&output, None).unwrap();
        transfer.take_completion_job().unwrap()();
        assert_eq!(transfer.poll().unwrap().unwrap(), expected.bgra);
    }

    // spec: HOST-VIEW-7
    #[test]
    fn upscalers_process_odd_sized_rgb_frames_without_changing_the_source() {
        use crate::{Upscaler, upscaling::Upscaling};
        let mut gpu = pollster::block_on(Gpu::request(!cfg!(host_view))).unwrap();
        let (width, height) = (19, 13);
        let source = gpu.image(width, height).unwrap();
        let mut pixels = Vec::new();
        for y in 0..height {
            for x in 0..width {
                pixels.extend_from_slice(&[
                    if x % 5 == 0 || y % 5 == 0 { 20 } else { 180 },
                    (x * 11) as u8,
                    (y * 17) as u8,
                    255,
                ]);
            }
        }
        gpu.upload(
            &source,
            Rect {
                x: 0,
                y: 0,
                w: width,
                h: height,
            },
            width * 4,
            &pixels,
        )
        .unwrap();
        let context = gpu.context().clone();
        let mut compositor = Compositor::new(context.clone(), wgpu::TextureFormat::Rgba16Float);
        compositor.prepare_format(wgpu::TextureFormat::Bgra8Unorm);
        let destination = gpu.image(width * 2, height * 2).unwrap();
        let mut results = Vec::new();
        for choice in [
            Upscaler::Anime4k,
            Upscaler::Cunny,
            Upscaler::Artcnn,
            Upscaler::K7,
        ] {
            let upscaling = Upscaling::new(&context, choice, (width, height)).unwrap();
            {
                let submission = context.submission().unwrap();
                compositor
                    .compose_frame(
                        source.view(),
                        &upscaling.input,
                        wgpu::TextureFormat::Rgba16Float,
                        [0., 0., width as f32, height as f32],
                        (width, height),
                        (width, height),
                        &[],
                        &submission,
                    )
                    .unwrap();
                upscaling.run(&context, &submission, None);
                compositor
                    .compose_view(
                        upscaling.output(),
                        destination.view(),
                        wgpu::TextureFormat::Bgra8Unorm,
                        [0., 0., (width * 2) as f32, (height * 2) as f32],
                        &submission,
                    )
                    .unwrap();
            }
            let mut transfer = gpu.readback(&destination, None).unwrap();
            transfer.take_completion_job().unwrap()();
            let result = transfer.poll().unwrap().unwrap();
            assert!(result.as_chunks::<4>().0.iter().all(|p| p[3] == 255));
            assert!(result.as_chunks::<4>().0.iter().any(|p| p[0] > 100));
            assert!(result.as_chunks::<4>().0.iter().any(|p| p[0] < 80));
            assert_ne!(
                &result[..4],
                &result[result.len() - 4..],
                "{choice:?} lost the image extent"
            );
            assert!(
                !results.contains(&result),
                "{choice:?} must execute its own model"
            );
            results.push(result);
        }
        let mut transfer = gpu.readback(&source, None).unwrap();
        transfer.take_completion_job().unwrap()();
        assert_eq!(transfer.poll().unwrap().unwrap(), pixels);
    }

    // spec: HOST-VIEW-3
    #[test]
    fn native_composition_preserves_encoded_colors_opaque_alpha_and_letterbox_on_both_adapters() {
        for fallback in [false, true]
            .into_iter()
            .filter(|fallback| !cfg!(target_os = "macos") || !fallback)
        {
            let mut gpu = pollster::block_on(Gpu::request(fallback)).unwrap();
            let image = gpu.image(2, 2).unwrap();
            let colors = [
                [32, 64, 192, 0],
                [200, 100, 20, 255],
                [48, 160, 96, 128],
                [240, 16, 224, 7],
            ];
            gpu.upload(
                &image,
                Rect {
                    x: 0,
                    y: 0,
                    w: 2,
                    h: 2,
                },
                8,
                &colors.concat(),
            )
            .unwrap();
            let frame = gpu.present(&image, 1).unwrap();
            for format in [
                wgpu::TextureFormat::Bgra8Unorm,
                wgpu::TextureFormat::Bgra8UnormSrgb,
            ] {
                let compositor = Compositor::new(gpu.context().clone(), format);
                let destination = gpu
                    .context()
                    .device
                    .create_texture(&wgpu::TextureDescriptor {
                        label: Some("native compositor oracle"),
                        size: wgpu::Extent3d {
                            width: 32,
                            height: 24,
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format,
                        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                            | wgpu::TextureUsages::COPY_SRC,
                        view_formats: &[],
                    });
                compositor
                    .compose(
                        &frame,
                        &destination.create_view(&Default::default()),
                        format,
                        [6.0, 2.0, 20.0, 20.0],
                    )
                    .unwrap();
                let device = &gpu.context().device;
                let buffer = device.create_buffer(&wgpu::BufferDescriptor {
                    label: None,
                    size: 256 * 24,
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                });
                let mut encoder = device.create_command_encoder(&Default::default());
                encoder.copy_texture_to_buffer(
                    destination.as_image_copy(),
                    wgpu::TexelCopyBufferInfo {
                        buffer: &buffer,
                        layout: wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(256),
                            rows_per_image: Some(24),
                        },
                    },
                    wgpu::Extent3d {
                        width: 32,
                        height: 24,
                        depth_or_array_layers: 1,
                    },
                );
                gpu.context()
                    .submission()
                    .unwrap()
                    .submit([encoder.finish()]);
                let (mut tx, mut rx) = adv_sync::spsc::channel(1);
                buffer.map_async(wgpu::MapMode::Read, .., move |result| {
                    tx.push(result).unwrap();
                });
                device
                    .poll(wgpu::PollType::Wait {
                        submission_index: None,
                        timeout: Some(Duration::from_secs(10)),
                    })
                    .unwrap();
                rx.pop().unwrap().unwrap();
                gpu.check().unwrap();
                let bytes = buffer.get_mapped_range(..).unwrap();
                for y in 0..24usize {
                    for x in 0..32usize {
                        let pixel = &bytes[y * 256 + x * 4..y * 256 + x * 4 + 4];
                        if (6..26).contains(&x) && (2..22).contains(&y) {
                            let color = colors[((y - 2) / 10) * 2 + (x - 6) / 10];
                            assert!(
                                pixel[..3]
                                    .iter()
                                    .zip(color[..3].iter())
                                    .all(|(a, b)| a.abs_diff(*b) <= 1),
                                "{fallback:?} {format:?}: pixel ({x},{y}) {pixel:?}, expected {color:?}"
                            );
                            assert_eq!(
                                pixel[3], 255,
                                "source alpha is ignored by opaque presentation"
                            );
                        } else {
                            assert_eq!(pixel, &[0, 0, 0, 255]);
                        }
                    }
                }
                drop(bytes);
                buffer.unmap();
            }
        }
    }
}

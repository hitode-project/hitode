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

pub struct WindowPresenter {
    surface: wgpu::Surface<'static>,
    context: NativeContext,
    configuration: wgpu::SurfaceConfiguration,
    compositor: Compositor,
    upscaler: crate::Upscaler,
    scaling: crate::Scaling,
    upscaling: Option<crate::upscaling::Upscaling>,
    max_acquire_ns: Cell<u64>,
    max_configure_ns: u64,
}

impl WindowPresenter {
    /// HWND is owned by the calling UI thread and must outlive this presenter,
    /// including any temporary move to a backend configuration job.
    #[cfg(windows)]
    unsafe fn surface(
        hwnd: *mut std::ffi::c_void,
        instance: &wgpu::Instance,
    ) -> Result<wgpu::Surface<'static>, String> {
        use std::num::NonZeroIsize;
        let mut window = wgpu::rwh::Win32WindowHandle::new(
            NonZeroIsize::new(hwnd as isize).ok_or("null GPU window")?,
        );
        // Vulkan's Win32 surface requires the actual module instance as well
        // as HWND; omitting it silently leaves that backend with no surface.
        window.hinstance = NonZeroIsize::new(unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::GetWindowLongPtrW(
                hwnd,
                windows_sys::Win32::UI::WindowsAndMessaging::GWLP_HINSTANCE,
            )
        });
        if window.hinstance.is_none() {
            return Err("GPU window has no native module instance".into());
        }
        // SAFETY: caller keeps HWND alive until this surface is destroyed.
        let surface = unsafe {
            instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                raw_display_handle: Some(wgpu::rwh::WindowsDisplayHandle::new().into()),
                raw_window_handle: window.into(),
            })
        }
        .map_err(|error| error.to_string())?;
        Ok(surface)
    }

    #[cfg(target_os = "macos")]
    unsafe fn surface(
        view: *mut std::ffi::c_void,
        instance: &wgpu::Instance,
    ) -> Result<wgpu::Surface<'static>, String> {
        let view = std::ptr::NonNull::new(view).ok_or("null GPU view")?;
        // SAFETY: the UI owner keeps NSView alive until all surface jobs finish.
        unsafe {
            instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                raw_display_handle: Some(wgpu::rwh::AppKitDisplayHandle::new().into()),
                raw_window_handle: wgpu::rwh::AppKitWindowHandle::new(view).into(),
            })
        }
        .map_err(|error| error.to_string())
    }

    pub(crate) async unsafe fn request(
        hwnd: *mut std::ffi::c_void,
        width: u32,
        height: u32,
        fallback: bool,
    ) -> Result<(adv_device::gpu::Gpu, Self), String> {
        let descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
        #[cfg(target_os = "macos")]
        let descriptor = wgpu::InstanceDescriptor {
            backends: wgpu::Backends::METAL,
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
        Ok((gpu, presenter))
    }

    #[cfg(all(test, windows))]
    pub(crate) unsafe fn new(
        hwnd: *mut std::ffi::c_void,
        context: NativeContext,
        width: u32,
        height: u32,
    ) -> Result<Self, String> {
        let surface = unsafe { Self::surface(hwnd, &context.instance)? };
        Self::with_surface(surface, context, width, height)
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
        configuration.present_mode = if caps.present_modes.contains(&wgpu::PresentMode::Mailbox) {
            wgpu::PresentMode::Mailbox
        } else if caps.present_modes.contains(&wgpu::PresentMode::Immediate) {
            wgpu::PresentMode::Immediate
        } else {
            return Err("native surface has no nonblocking presentation mode".into());
        };
        configuration.desired_maximum_frame_latency = 2;
        let compositor = Compositor::new(context.clone(), configuration.format);
        Ok(Self {
            surface,
            context,
            configuration,
            compositor,
            upscaler: crate::Upscaler::Off,
            scaling: crate::Scaling::Fit,
            upscaling: None,
            max_acquire_ns: Cell::new(0),
            max_configure_ns: 0,
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
            .configure(&self.context.device, &self.configuration);
        let error = pollster::block_on(errors.pop());
        self.max_configure_ns = self.max_configure_ns.max(nanoseconds(started.elapsed()));
        if let Some(error) = error {
            return Err(format!("native surface configuration failed: {error}"));
        }
        Ok(())
    }

    fn draw_view(
        &mut self,
        source: &wgpu::TextureView,
        size: (u32, u32),
        rectangle: [f32; 4],
        canvas: (u32, u32),
        changed: bool,
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
            if refresh {
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
                upscaling.run(&self.context, submission);
            }
        }
        if (width, height) != (self.configuration.width, self.configuration.height) {
            return Ok(DrawResult::Reconfigure);
        }
        let started = Instant::now();
        let output = self.surface.get_current_texture();
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

    fn compose_view(
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
    software: Option<(wgpu::Texture, wgpu::TextureView)>,
    snapshot: Snapshot,
    candidate: Snapshot,
    serial: u64,
    width: u32,
    height: u32,
    video: Arc<[(u64, VideoLayer)]>,
    overlays: Vec<VideoOverlay>,
    cursor_regions: Arc<[VideoCursorRegion]>,
    stats: crate::performance::NativePresentation,
}
pub(crate) struct Rendered {
    pub(crate) result: DrawResult,
    pub(crate) serial: u64,
    pub(crate) canvas: (u32, u32),
    pub(crate) cursor_regions: Arc<[VideoCursorRegion]>,
}
impl Session {
    pub(crate) fn new(presenter: WindowPresenter, frames: FrameConsumer) -> Self {
        Self {
            presenter,
            frames,
            native: None,
            software: None,
            snapshot: Snapshot::default(),
            candidate: Snapshot::default(),
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
        dimensions
            != (
                self.presenter.configuration.width,
                self.presenter.configuration.height,
            )
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
    ) -> Result<Rendered, String> {
        let context = self.presenter.context.clone();
        let submission = context.submission().map_err(|error| error.to_string())?;
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
                self.native = Some(frame.clone());
                if update_video {
                    self.update_video(&context, &submission)?;
                }
                changed = true;
            } else {
                self.cursor_regions = Arc::from(self.frames.current().video_cursor_regions()?);
                self.candidate.width = 0;
                self.candidate.height = 0;
                self.candidate.bgra.clear();
                self.frames.current().snapshot(&mut self.candidate)?;
                if self.candidate.width == 0 || self.candidate.height == 0 {
                    return Ok(Rendered {
                        result: DrawResult::Skipped,
                        serial: self.serial,
                        canvas: (self.width, self.height),
                        cursor_regions: self.cursor_regions.clone(),
                    });
                }
                changed = self.native.is_some()
                    || self.snapshot.width != self.candidate.width
                    || self.snapshot.height != self.candidate.height
                    || self.snapshot.bgra != self.candidate.bgra;
                self.width = self.candidate.width;
                self.height = self.candidate.height;
                self.serial = self.candidate.frames;
                if changed {
                    if self.software.is_none()
                        || (self.snapshot.width, self.snapshot.height) != (self.width, self.height)
                    {
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
                                | wgpu::TextureUsages::COPY_DST,
                            view_formats: &[],
                        });
                        let view = texture.create_view(&Default::default());
                        self.software = Some((texture, view));
                    }
                    submission.write_texture(
                        self.software.as_ref().unwrap().0.as_image_copy(),
                        &self.candidate.bgra,
                        wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(self.width * 4),
                            rows_per_image: Some(self.height),
                        },
                        wgpu::Extent3d {
                            width: self.width,
                            height: self.height,
                            depth_or_array_layers: 1,
                        },
                    );
                    self.stats.uploads += 1;
                    std::mem::swap(&mut self.snapshot, &mut self.candidate);
                }
                self.native = None;
                self.overlays.clear();
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
                result: DrawResult::Skipped,
                serial: self.serial,
                canvas,
                cursor_regions: self.cursor_regions.clone(),
            });
        }
        let rectangle = if self.presenter.scaling == crate::Scaling::Fit {
            crate::window::native_viewport(width, height, self.width, self.height)
        } else {
            self.presenter
                .scaling
                .rectangle((f64::from(width), f64::from(height)), canvas)
                .map(|v| v as f32)
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
        let result = self.presenter.draw_view(
            view,
            (width, height),
            rectangle,
            canvas,
            changed,
            &self.overlays,
            &submission,
        )?;
        self.stats.presented += u64::from(matches!(result, DrawResult::Presented));
        Ok(Rendered {
            result,
            serial: self.serial,
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
        }
    }
    pub(crate) fn set_scaling(&mut self, value: crate::Scaling) {
        self.scaling = value;
    }
    pub(crate) fn set_upscaler(&mut self, value: crate::Upscaler) {
        self.upscaler = value;
    }
    pub(crate) fn has_update(&self) -> bool {
        self.observer.has_update() || self.reconfigure
    }
    pub(crate) fn in_flight(&self) -> bool {
        self.job.is_some()
    }
    pub(crate) fn poll(&mut self) -> Result<Option<Completed>, String> {
        if let Some(job) = &mut self.job {
            if let Some(done) = job.completion.pop() {
                self.session = Some(done.session);
                self.job = None;
                let rendered = done.result?;
                self.reconfigure = matches!(rendered.result, DrawResult::Reconfigure);
                return Ok(Some(Completed {
                    rendered,
                    elapsed: done.elapsed,
                    at: done.at,
                    stats: self.session.as_ref().unwrap().stats,
                }));
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
        if self.job.is_some() || dimensions.0 == 0 || dimensions.1 == 0 {
            return Ok(false);
        }
        let session = self
            .session
            .as_ref()
            .expect("idle presenter owns its session");
        let configure = self.reconfigure || session.needs_configuration(dimensions);
        // Pause is nonblocking. The worker receives its token and waits for the
        // last short Queue lease before configure waits for GPU idle.
        let pause = configure
            .then(|| session.context().pause_submissions())
            .transpose()
            .map_err(|error| error.to_string())?;
        let mut session = self.session.take().unwrap();
        session.presenter.upscaler = self.upscaler;
        session.presenter.scaling = self.scaling;
        let (mut output, completion) = spsc::channel(1);
        let scheduled = Instant::now();
        let task = background::Task::new(move || {
            let started = Instant::now();
            session.stats.max_queue_ns = session
                .stats
                .max_queue_ns
                .max(nanoseconds(started.saturating_duration_since(scheduled)));
            let result = (|| {
                if let Some(pause) = pause {
                    pause.wait_drained().map_err(|error| error.to_string())?;
                    session
                        .presenter
                        .configure_exclusive(dimensions.0, dimensions.1)?;
                    session.stats.configurations += 1;
                    drop(pause);
                }
                session.render(dimensions.0, dimensions.1, repaint || configure)
            })();
            session.stats.max_job_ns = session.stats.max_job_ns.max(nanoseconds(started.elapsed()));
            session.stats.max_acquire_ns = session.presenter.max_acquire_ns.get();
            session.stats.max_configure_ns = session.presenter.max_configure_ns;
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

#[cfg(test)]
mod tests {
    use super::*;
    use adv_device::gpu::Gpu;
    use adv_device::surface::Rect;
    use std::time::Duration;

    // spec: HOST-VIEW-1
    #[test]
    fn sharp_bilinear_preserves_integer_pixels_and_blends_fractional_boundaries() {
        let mut gpu = pollster::block_on(Gpu::request(!cfg!(target_os = "macos"))).unwrap();
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
        let mut gpu = pollster::block_on(Gpu::request(!cfg!(target_os = "macos"))).unwrap();
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

    // spec: HOST-VIEW-7
    #[test]
    fn upscalers_process_odd_sized_rgb_frames_without_changing_the_source() {
        use crate::{Upscaler, upscaling::Upscaling};
        let mut gpu = pollster::block_on(Gpu::request(!cfg!(target_os = "macos"))).unwrap();
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
                upscaling.run(&context, &submission);
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
            assert!(result.chunks_exact(4).all(|p| p[3] == 255));
            assert!(result.chunks_exact(4).any(|p| p[0] > 100));
            assert!(result.chunks_exact(4).any(|p| p[0] < 80));
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

    #[cfg(windows)]
    fn completed(ui: &mut UiPresenter, wake: &WakeEvent) -> Completed {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(done) = ui.poll().unwrap() {
                return done;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(
                !remaining.is_zero(),
                "presentation completion did not return"
            );
            assert!(wake.wait(Some(remaining)).unwrap());
        }
    }

    #[cfg(windows)]
    #[test]
    fn owned_window_jobs_coalesce_before_conversion_retry_full_and_drain_resize_before_destroy() {
        for fallback in [false, true]
            .into_iter()
            .filter(|fallback| !cfg!(target_os = "macos") || !fallback)
        {
            owned_window_regression(fallback);
        }
    }

    #[cfg(windows)]
    fn owned_window_regression(fallback: bool) {
        use adv_device::surface::{PixelFormat, SurfaceMemory, Surfaces, frame_mailbox};
        use std::borrow::Cow;
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
        let mut gpu = pollster::block_on(Gpu::request(fallback)).unwrap();
        let mut window =
            crate::window::Window::new("GPU ownership regression", 32, 24, true).unwrap();
        let context = gpu.context().clone();
        let execution = WakeEvent::new().unwrap();
        let wake = WakeEvent::new().unwrap();
        assert!(context.set_execution_wake(execution.clone()).is_ok());
        let presenter = window.prepare_native(context.clone()).unwrap();
        let (producer, consumer) = frame_mailbox();
        let mut surfaces = Surfaces::new(8, 6, 0x1000, 0x10000, Snapshot::default()).unwrap();
        surfaces.set_frame_mailbox(producer, wake.clone());
        let target = surfaces.create_native(&gpu, 8, 6, false).unwrap();
        gpu.clear(
            surfaces.native_target(target).unwrap(),
            Some([0.75, 0.25, 0.125, 1.0]),
            None,
        )
        .unwrap();
        let mut builder = background::Builder::new(execution.clone()).unwrap();
        let sender = builder
            .sender_with_capacity_wake(1, 1, wake.clone())
            .unwrap();
        let mut service = builder.start().unwrap();
        let mut ui = UiPresenter::new(presenter, consumer, sender, wake.clone());

        let started = WakeEvent::new().unwrap();
        let release = WakeEvent::new().unwrap();
        let job_started = started.clone();
        let job_release = release.clone();
        ui.sender
            .try_submit(
                background::Priority::Urgent,
                background::Task::new(move || {
                    job_started.notify();
                    assert!(job_release.wait(Some(Duration::from_secs(10))).unwrap());
                })
                .with_wake(wake.clone()),
            )
            .unwrap();
        assert!(started.wait(Some(Duration::from_secs(10))).unwrap());
        let second_started = WakeEvent::new().unwrap();
        let second_release = WakeEvent::new().unwrap();
        let job_started = second_started.clone();
        let job_release = second_release.clone();
        ui.sender
            .try_submit(
                background::Priority::Urgent,
                background::Task::new(move || {
                    job_started.notify();
                    assert!(job_release.wait(Some(Duration::from_secs(10))).unwrap());
                })
                .with_wake(wake.clone()),
            )
            .unwrap();
        for _ in 0..100 {
            surfaces.present_native(target, &mut gpu).unwrap();
        }
        assert!(ui.schedule((32, 24), false).unwrap());
        assert!(
            ui.job.as_ref().unwrap().task.is_some(),
            "full lane returns ownership for retry"
        );
        assert!(
            ui.observer.has_update(),
            "rejection cannot consume or clear the latest frame"
        );
        release.notify();
        assert!(second_started.wait(Some(Duration::from_secs(10))).unwrap());
        // Capacity becomes available before the second blocking job finishes.
        assert!(wake.wait(Some(Duration::from_secs(10))).unwrap());
        assert!(ui.poll().unwrap().is_none());
        assert!(ui.job.as_ref().unwrap().ticket.is_some());
        for _ in 0..50 {
            surfaces.present_native(target, &mut gpu).unwrap();
        }
        second_release.notify();
        let first = completed(&mut ui, &wake);
        assert_eq!(first.rendered.serial, 150);
        assert_eq!(first.rendered.canvas, (8, 6));
        assert!(matches!(
            first.rendered.result,
            DrawResult::Presented | DrawResult::Skipped
        ));
        assert_eq!(ui.session.as_ref().unwrap().stats.consumed, 1);
        assert_eq!(ui.session.as_ref().unwrap().stats.uploads, 0);
        assert_eq!(gpu.stats().readbacks, 0);
        assert!(
            !execution.wait(Some(Duration::ZERO)).unwrap(),
            "UI jobs never interrupt the guest"
        );

        // CPU/GDI frames use the exact same final compositor. A burst converts
        // and uploads once, after the background consumer selects its last frame.
        let mut memory = Memory(vec![0; 0x11000]);
        let software = surfaces
            .create(8, 6, false, PixelFormat::Bgra32, &mut memory)
            .unwrap();
        for value in 1..=50 {
            surfaces
                .fill_raw(
                    software,
                    Rect {
                        x: 0,
                        y: 0,
                        w: 8,
                        h: 6,
                    },
                    value,
                    &mut memory,
                )
                .unwrap();
            surfaces.present(software, &memory).unwrap();
        }
        assert!(window.native_canvas((8, 6)).unwrap());
        assert!(
            ui.schedule(window.native_client_size().unwrap(), false)
                .unwrap()
        );
        let second = completed(&mut ui, &wake);
        assert_eq!(second.rendered.serial, 200);
        assert_eq!(ui.session.as_ref().unwrap().stats.consumed, 2);
        assert_eq!(ui.session.as_ref().unwrap().stats.uploads, 1);
        assert_eq!(
            ui.session.as_ref().unwrap().snapshot.bgra,
            [50, 0, 0, 0].repeat(48)
        );
        assert_eq!(ui.session.as_ref().unwrap().stats.configurations, 2);
        // Configuration pause completion intentionally resumes guest Queue use.
        assert!(execution.wait(Some(Duration::ZERO)).unwrap());

        // A required resize is accepted, cannot be cancelled, and is completed
        // on shutdown while HWND and its original UI owner are still alive.
        assert!(window.native_canvas((40, 30)).unwrap());
        assert!(
            ui.schedule(window.native_client_size().unwrap(), true)
                .unwrap()
        );
        assert!(!ui.job.as_ref().unwrap().ticket.as_ref().unwrap().cancel());
        service.shutdown();
        let last = ui.poll().unwrap().unwrap();
        assert!(matches!(
            last.rendered.result,
            DrawResult::Presented | DrawResult::Skipped
        ));
        assert_eq!(ui.session.as_ref().unwrap().stats.configurations, 3);
        assert_eq!(gpu.stats().readbacks, 0);
        eprintln!(
            "native HWND {:?}: {:?}; renderer readbacks={}",
            context.adapter.get_info(),
            ui.session.as_ref().unwrap().stats,
            gpu.stats().readbacks
        );
        drop(ui); // Surface and any returned native image die before HWND.
        drop(window);
    }

    #[cfg(windows)]
    #[test]
    fn startup_selects_a_surface_compatible_adapter_and_rejects_a_destroyed_device() {
        let mut window =
            crate::window::Window::new("GPU startup regression", 32, 24, true).unwrap();
        let (gpu, mut presenter) = window.create_native(false).unwrap();
        assert!(
            gpu.context()
                .adapter
                .is_surface_supported(&presenter.surface)
        );
        gpu.context().device.destroy();
        let _ = gpu.poll();
        assert!(presenter.configure_exclusive(64, 48).is_err());
        assert!(gpu.context().submission().is_err());
        drop(presenter);
        drop(window);
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

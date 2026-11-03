//! NativeActivity probe. All lifecycle and Choreographer callbacks run on the
//! activity's looper; only SurfaceControl's numeric completion inbox is shared.

#[cfg(target_os = "android")]
mod android {
    use adv_display::{
        Rect,
        compositor::{android::SurfaceCompositor, *},
        plan::*,
    };
    use adv_gpu::{
        Device, Format, FormatCaps, PresentPool, Queue,
        encode::{ColorAttachment, Load, RenderPassDesc, Store},
        present::{PoolDesc, YuvColor},
        queue::DeviceOptions,
    };
    use std::{
        cell::RefCell,
        ffi::{CString, c_char, c_void},
        ptr::NonNull,
        rc::Rc,
        time::{Duration, Instant},
    };

    // The prefix of ANativeActivity used here, from android/native_activity.h.
    // Java, paths and the asset manager are opaque and never accessed.
    #[repr(C)]
    pub struct Activity {
        callbacks: *mut Callbacks,
        vm: *mut c_void,
        env: *mut c_void,
        class: *mut c_void,
        internal_path: *const c_char,
        external_path: *const c_char,
        sdk: i32,
        instance: *mut c_void,
    }
    #[repr(C)]
    struct Callbacks {
        start_through_stop: [usize; 5],
        destroy: Option<unsafe extern "C" fn(*mut Activity)>,
        focus: usize,
        created: Option<unsafe extern "C" fn(*mut Activity, *mut ndk_sys::ANativeWindow)>,
        resized_and_redraw: [usize; 2],
        destroyed: Option<unsafe extern "C" fn(*mut Activity, *mut ndk_sys::ANativeWindow)>,
    }
    #[link(name = "android")]
    unsafe extern "C" {
        fn ANativeWindow_getWidth(window: *mut ndk_sys::ANativeWindow) -> i32;
        fn ANativeWindow_getHeight(window: *mut ndk_sys::ANativeWindow) -> i32;
        fn ANativeActivity_finish(activity: *mut Activity);
        fn AChoreographer_getInstance() -> *mut c_void;
        fn AChoreographer_postFrameCallback64(
            choreographer: *mut c_void,
            callback: unsafe extern "C" fn(i64, *mut c_void),
            data: *mut c_void,
        );
    }
    #[link(name = "log")]
    unsafe extern "C" {
        fn __android_log_write(priority: i32, tag: *const c_char, text: *const c_char) -> i32;
    }
    unsafe extern "C" {
        fn __system_property_get(name: *const c_char, value: *mut c_char) -> i32;
    }
    fn log(text: String) {
        unsafe {
            __android_log_write(
                4,
                c"adv-display-probe".as_ptr(),
                CString::new(text).unwrap().as_ptr(),
            );
        }
    }
    fn clock(id: libc::clockid_t) -> i64 {
        let mut t = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        assert_eq!(unsafe { libc::clock_gettime(id, &mut t) }, 0);
        t.tv_sec * 1_000_000_000 + t.tv_nsec
    }
    fn rotation() -> Rotation {
        let mut value = [0; 92]; // PROP_VALUE_MAX
        let len = unsafe {
            __system_property_get(c"debug.adv.probe.rotation".as_ptr(), value.as_mut_ptr())
        } as usize;
        let bytes = unsafe { std::slice::from_raw_parts(value.as_ptr().cast::<u8>(), len) };
        match bytes {
            b"" | b"0" => Rotation::None,
            b"90" => Rotation::Quarter,
            b"180" => Rotation::Half,
            b"270" => Rotation::ThreeQuarters,
            _ => panic!("probe rotation must be 0, 90, 180 or 270"),
        }
    }
    fn yuv_mode() -> bool {
        let mut value = [0; 92];
        let len =
            unsafe { __system_property_get(c"debug.adv.probe.yuv".as_ptr(), value.as_mut_ptr()) };
        len == 1 && value[0] == b'1' as c_char
    }
    fn caps(device: &Device) -> CompositorCaps {
        CompositorCaps {
            scales: true,
            nearest: false,
            rotates: true,
            yuv_layers: true,
            solid_color_layers: true,
            damage_hints: true,
            presentable_compressed: device
                .caps()
                .format(Format::Rgba8Unorm)
                .contains(FormatCaps::PRESENTABLE_COMPRESSED),
            tile: device.caps().limits.tile,
        }
    }
    fn pool(device: &Device, bounds: Rect) -> PresentPool {
        device
            .create_present_pool(&PoolDesc {
                color: None,
                width: bounds.w,
                height: bounds.h,
                format: Format::Rgba8Unorm,
                count: 3,
                label: "display probe",
            })
            .expect("present pool")
    }
    #[derive(Default)]
    struct Metrics {
        cpu: Vec<i64>,
        latch: Vec<i64>,
        shown: Vec<i64>,
        releases: Vec<i64>,
        ages: Vec<i64>,
        unlatched: usize,
    }
    impl Metrics {
        fn summary(&mut self) {
            log(format!("summary unlatched={}", self.unlatched));
            for (name, values) in [
                ("present_cpu_ns", &mut self.cpu),
                ("latch_minus_desired_ns", &mut self.latch),
                ("shown_minus_latch_ns", &mut self.shown),
                ("releases", &mut self.releases),
                ("age", &mut self.ages),
            ] {
                values.sort_unstable();
                if !values.is_empty() {
                    log(format!(
                        "summary {name} n={} min={} median={} p95={}",
                        values.len(),
                        values[0],
                        values[values.len() / 2],
                        values[(values.len() * 95).div_ceil(100) - 1]
                    ));
                }
            }
        }
    }
    struct Gpu {
        device: Device,
        queue: Queue,
        pool: PresentPool,
        yuv: Option<PresentPool>,
    }
    #[derive(Default)]
    struct Probe {
        activity: *mut Activity,
        yuv_mode: bool,
        attach_base: bool,
        compositor: Option<SurfaceCompositor>,
        gpu: Option<Gpu>,
        bounds: Rect,
        layers: Option<Layers>,
        frame: usize,
        pending: bool,
        destroyed: bool,
        desired: Vec<Instant>,
        latched: Vec<Option<Instant>>,
        last_bar: Option<Rect>,
        buffers: Vec<(u64, Rect, u32)>,
        metrics: Metrics,
        presented: Vec<Presented>,
        released: Vec<ReleasedImage>,
    }
    impl Probe {
        fn drain(&mut self) {
            self.compositor
                .as_mut()
                .unwrap()
                .drain(&mut self.presented, &mut self.released);
            for p in self.presented.drain(..).filter(|p| p.serial != 0) {
                let i = p.serial as usize - 1;
                let Some(latch) = p.latched else {
                    self.metrics.unlatched += 1;
                    continue;
                };
                if self.latched[i].is_none() {
                    let ns = delta(latch, self.desired[i]);
                    self.metrics.latch.push(ns);
                    self.latched[i] = Some(latch);
                    log(format!("serial={} latch_minus_desired_ns={ns}", p.serial));
                }
                if let Some(shown) = p.shown {
                    let ns = delta(shown, latch);
                    self.metrics.shown.push(ns);
                    log(format!("serial={} shown_minus_latch_ns={ns}", p.serial));
                }
            }
            let releases = self.released.len();
            for r in self.released.drain(..) {
                r.image.released(r.fence);
            }
            self.metrics.releases.push(releases as i64);
            log(format!("frame={} releases={releases}", self.frame));
        }
        fn present(
            &mut self,
            encoder: adv_gpu::Encoder,
            mut image: adv_gpu::PresentImage,
            serial: u64,
            desired: Instant,
        ) {
            let id = image.image().id();
            let age = image.age();
            let token = self
                .gpu
                .as_mut()
                .unwrap()
                .queue
                .submit_present(encoder, &mut [&mut image])
                .expect("submit present");
            for (_, _, age) in &mut self.buffers {
                *age += 1;
            }
            let content = (id, self.last_bar.unwrap(), 1);
            if let Some(buffer) = self.buffers.iter_mut().find(|(buffer, _, _)| *buffer == id) {
                *buffer = content;
            } else {
                self.buffers.push(content);
            }
            if let Some(age) = age {
                self.metrics.ages.push(i64::from(age));
            }
            let ages: arrayvec::ArrayVec<_, 3> = self
                .buffers
                .iter()
                .map(|(id, _, age)| (*id, *age))
                .collect();
            log(format!(
                "serial={serial} acquired_age={age:?} pool_ages={ages:?}"
            ));
            let start = clock(libc::CLOCK_THREAD_CPUTIME_ID);
            self.compositor
                .as_mut()
                .unwrap()
                .present(
                    self.layers.as_ref().unwrap(),
                    &Tiles::default(),
                    vec![AcquiredImage {
                        layer: LayerId::Base,
                        image,
                        acquire: token,
                    }],
                    serial,
                    desired,
                )
                .unwrap_or_else(|_| panic!("present"));
            let cpu = clock(libc::CLOCK_THREAD_CPUTIME_ID) - start;
            self.metrics.cpu.push(cpu);
            log(format!("serial={serial} present_cpu_ns={cpu}"));
        }
        fn frame_yuv(&mut self, frame_time: i64) {
            let gpu = self.gpu.as_mut().unwrap();
            let Some(mut image) = gpu.yuv.as_mut().unwrap().acquire_cpu() else {
                return;
            };
            let phase = self.frame;
            image.write(|planes| {
                for y in 0..360 {
                    for x in 0..640 {
                        planes[0].data[y * planes[0].stride + x] =
                            (16 + ((x + y + phase * 3) % 220)) as u8;
                    }
                }
                for y in 0..180 {
                    for x in 0..320 {
                        let at = y * planes[1].stride + x * 2;
                        planes[1].data[at] = (64 + (x + phase) % 128) as u8;
                        planes[1].data[at + 1] = (64 + (y + phase) % 128) as u8;
                    }
                }
            });
            assert!(image.acquire_fence().is_none());
            let mut images = vec![AcquiredImage {
                layer: LayerId::Video(1),
                image,
                acquire: adv_gpu::Token(0),
            }];
            if self.attach_base {
                self.attach_base = false;
                let mut base = gpu.pool.acquire(&mut gpu.queue).unwrap().unwrap();
                let mut encoder = gpu.queue.encoder("YUV background");
                drop(
                    encoder.render_pass(&RenderPassDesc {
                        colors: [ColorAttachment {
                            view: base.image().view(),
                            load: Load::Clear([0.0, 0.0, 0.0, 1.0]),
                            store: Store::Store,
                            resolve: None,
                        }]
                        .into_iter()
                        .collect(),
                        depth_stencil: None,
                        area: self.bounds.into(),
                        timestamps: None,
                        label: "YUV background",
                    }),
                );
                let acquire = gpu.queue.submit_present(encoder, &mut [&mut base]).unwrap();
                images.push(AcquiredImage {
                    layer: LayerId::Base,
                    image: base,
                    acquire,
                });
            }
            let now = Instant::now();
            let offset = frame_time + 16_666_667 - clock(libc::CLOCK_MONOTONIC);
            let desired = if offset >= 0 {
                now + Duration::from_nanos(offset as u64)
            } else {
                now - Duration::from_nanos((-offset) as u64)
            };
            self.desired.push(desired);
            self.latched.push(None);
            self.frame += 1;
            self.compositor
                .as_mut()
                .unwrap()
                .present(
                    self.layers.as_ref().unwrap(),
                    &Tiles::default(),
                    images,
                    self.frame as u64,
                    desired,
                )
                .unwrap_or_else(|_| panic!("YUV present"));
        }
        fn frame(&mut self, frame_time: i64) {
            self.drain();
            let gpu = self.gpu.as_mut().unwrap();
            gpu.queue.poll().expect("poll GPU");
            if self.frame == 600 {
                self.metrics.summary();
                unsafe {
                    ANativeActivity_finish(self.activity);
                }
                return;
            }
            if self.yuv_mode {
                self.frame_yuv(frame_time);
                return;
            }
            let gpu = self.gpu.as_mut().unwrap();
            let Some(image) = gpu.pool.acquire(&mut gpu.queue).expect("acquire buffer") else {
                return;
            };
            let id = image.image().id();
            let old = self
                .buffers
                .iter()
                .find(|(buffer, _, _)| *buffer == id)
                .map(|(_, bar, _)| *bar);
            let bar = Rect {
                x: (self.frame as u32 * 8) % (self.bounds.w - self.bounds.w.min(64) + 1),
                y: 0,
                w: self.bounds.w.min(64),
                h: self.bounds.h,
            };
            let mut encoder = gpu.queue.encoder("moving bar");
            for (area, color) in [
                (old.unwrap_or(self.bounds), [0.02, 0.02, 0.02, 1.0]),
                (bar, [1.0, 0.4, 0.0, 1.0]),
            ] {
                drop(
                    encoder.render_pass(&RenderPassDesc {
                        colors: [ColorAttachment {
                            view: image.image().view(),
                            load: Load::Clear(color),
                            store: Store::Store,
                            resolve: None,
                        }]
                        .into_iter()
                        .collect(),
                        depth_stencil: None,
                        area: area.into(),
                        timestamps: None,
                        label: "bar damage",
                    }),
                );
            }
            let damage = self
                .last_bar
                .map_or(Damage::Whole, |previous| Damage::rects([previous, bar]));
            self.layers.as_mut().unwrap().base.damage = damage;
            let now = Instant::now();
            let offset = frame_time + 16_666_667 - clock(libc::CLOCK_MONOTONIC);
            let desired = if offset >= 0 {
                now + Duration::from_nanos(offset as u64)
            } else {
                now - Duration::from_nanos((-offset) as u64)
            };
            self.desired.push(desired);
            self.latched.push(None);
            self.last_bar = Some(bar);
            self.frame += 1;
            self.present(encoder, image, self.frame as u64, desired);
        }
    }
    fn delta(a: Instant, b: Instant) -> i64 {
        if a >= b {
            a.duration_since(b).as_nanos() as i64
        } else {
            -(b.duration_since(a).as_nanos() as i64)
        }
    }
    unsafe fn state(activity: *mut Activity) -> Rc<RefCell<Probe>> {
        let pointer = unsafe { (*activity).instance.cast::<RefCell<Probe>>() };
        unsafe {
            Rc::increment_strong_count(pointer);
            Rc::from_raw(pointer)
        }
    }
    fn schedule(state: &Rc<RefCell<Probe>>) {
        state.borrow_mut().pending = true;
        unsafe {
            AChoreographer_postFrameCallback64(
                AChoreographer_getInstance(),
                frame,
                Rc::into_raw(state.clone()).cast_mut().cast(),
            );
        }
    }
    unsafe extern "C" fn frame(time: i64, data: *mut c_void) {
        let state = unsafe { Rc::from_raw(data.cast::<RefCell<Probe>>()) };
        let mut probe = state.borrow_mut();
        probe.pending = false;
        if probe.destroyed || probe.compositor.is_none() {
            return;
        }
        let done = probe.frame == 600;
        probe.frame(time);
        drop(probe);
        if !done {
            schedule(&state);
        }
    }
    unsafe extern "C" fn created(activity: *mut Activity, window: *mut ndk_sys::ANativeWindow) {
        let state = unsafe { state(activity) };
        let mut probe = state.borrow_mut();
        let window = NonNull::new(window).expect("native window");
        let (w, h) = unsafe {
            (
                ANativeWindow_getWidth(window.as_ptr()),
                ANativeWindow_getHeight(window.as_ptr()),
            )
        };
        assert!(w > 0 && h > 0);
        let bounds = Rect {
            x: 0,
            y: 0,
            w: w as u32,
            h: h as u32,
        };
        let reattach = probe.frame > 0;
        if probe.gpu.is_none() {
            let (device, queue) = Device::open(&DeviceOptions {
                label: "display probe",
                ..Default::default()
            })
            .expect("open GPU");
            // A quarter turn draws landscape content for a portrait window.
            probe.bounds = match rotation() {
                Rotation::Quarter | Rotation::ThreeQuarters => Rect {
                    w: bounds.h,
                    h: bounds.w,
                    ..bounds
                },
                _ => bounds,
            };
            let pool = pool(&device, probe.bounds);
            let yuv = if probe.yuv_mode {
                let Some(format) = [Format::Nv12, Format::Nv21].into_iter().find(|&format| {
                    device
                        .caps()
                        .format(format)
                        .contains(FormatCaps::PRESENTABLE)
                }) else {
                    log("YUV probe unavailable: no presentable YUV format".into());
                    unsafe {
                        ANativeActivity_finish(probe.activity);
                    }
                    return;
                };
                Some(
                    device
                        .create_present_pool(&PoolDesc {
                            width: 640,
                            height: 360,
                            format,
                            count: 4,
                            color: Some(YuvColor::Bt601Limited),
                            label: "YUV gradient",
                        })
                        .expect("create presentable YUV probe pool"),
                )
            } else {
                None
            };
            probe.gpu = Some(Gpu {
                device,
                queue,
                pool,
                yuv,
            });
        }
        probe.layers = Some(Layers {
            base: Layer {
                source: Source::Base,
                crop: probe.bounds,
                dst: bounds,
                rotation: rotation(),
                nearest: false,
                damage: Damage::Whole,
            },
            video: Default::default(),
            host: Default::default(),
            background: [0.0; 3],
        });
        probe.attach_base = true;
        if probe.yuv_mode {
            probe.layers.as_mut().unwrap().video.push((
                1,
                Layer {
                    source: Source::Video(1),
                    crop: Rect {
                        x: 0,
                        y: 0,
                        w: 640,
                        h: 360,
                    },
                    dst: bounds,
                    rotation: rotation(),
                    nearest: false,
                    damage: Damage::Whole,
                },
            ));
        }
        probe.compositor = Some(
            unsafe { SurfaceCompositor::new(window, caps(&probe.gpu.as_ref().unwrap().device)) }
                .unwrap_or_else(|_| panic!("SurfaceControl")),
        );
        if reattach && !probe.yuv_mode {
            let gpu = probe.gpu.as_mut().unwrap();
            let image = gpu
                .pool
                .acquire(&mut gpu.queue)
                .expect("last buffer")
                .expect("last buffer released");
            let encoder = gpu.queue.encoder("reattach");
            probe.layers.as_mut().unwrap().base.damage = Damage::None;
            probe.present(encoder, image, 0, Instant::now());
        }
        let pending = probe.pending;
        drop(probe);
        if !pending {
            schedule(&state);
        }
    }
    unsafe extern "C" fn destroyed(activity: *mut Activity, _window: *mut ndk_sys::ANativeWindow) {
        unsafe { state(activity) }.borrow_mut().compositor = None;
    }
    unsafe extern "C" fn destroy(activity: *mut Activity) {
        let state = unsafe { Rc::from_raw((*activity).instance.cast::<RefCell<Probe>>()) };
        let mut probe = state.borrow_mut();
        probe.destroyed = true;
        probe.compositor = None;
    }
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn ANativeActivity_onCreate(
        activity: *mut Activity,
        _saved: *mut c_void,
        _size: usize,
    ) {
        let probe = Probe {
            activity,
            yuv_mode: yuv_mode(),
            desired: Vec::with_capacity(600),
            latched: Vec::with_capacity(600),
            buffers: Vec::with_capacity(3),
            ..Default::default()
        };
        unsafe {
            (*activity).instance = Rc::into_raw(Rc::new(RefCell::new(probe))).cast_mut().cast();
            let callbacks = &mut *(*activity).callbacks;
            callbacks.created = Some(created);
            callbacks.destroyed = Some(destroyed);
            callbacks.destroy = Some(destroy);
        }
    }
}

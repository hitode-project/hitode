//! Android API 29 SurfaceControl presentation. Callers supply caps with scaling,
//! rotation, YUV layers, solid colors and damage hints enabled, nearest
//! filtering disabled. Compression and tile size come from the GPU device.

use crate::{
    Rect,
    plan::{Layer, Rotation},
};

fn transform(rotation: Rotation) -> i32 {
    // ANATIVEWINDOW_TRANSFORM_ROTATE_90 / 180 / 270.
    match rotation {
        Rotation::None => 0,
        Rotation::Quarter => 4,
        Rotation::Half => 3,
        Rotation::ThreeQuarters => 7,
    }
}

#[derive(Clone, Copy, PartialEq)]
struct Geometry(Rect, Rect, Rotation, Option<[f32; 4]>);
impl Geometry {
    fn update(last: &mut Option<Self>, layer: &Layer) -> bool {
        let next = Self(layer.crop, layer.dst, layer.rotation, layer.source_uv);
        let changed = *last != Some(next);
        *last = Some(next);
        changed
    }
}

// The callback belongs to the replacing transaction, not to the old image.
fn release_index(
    history: impl Iterator<Item = (usize, u64)>,
    surface: usize,
    serial: u64,
) -> Option<usize> {
    history
        .enumerate()
        .filter(|(_, (s, n))| *s == surface && *n < serial)
        .max_by_key(|(_, (_, n))| *n)
        .map(|(i, _)| i)
}

#[cfg(target_os = "android")]
pub use native::SurfaceCompositor;

#[cfg(target_os = "android")]
mod native {
    use super::*;
    use crate::compositor::*;
    use crate::plan::HostLayer;
    use adv_gpu::present::Export;
    use std::{
        collections::VecDeque,
        ffi::{c_char, c_void},
        os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd},
        ptr::{self, NonNull},
        sync::{Arc, Condvar, Mutex, OnceLock},
        time::Duration,
    };

    type Handle = *mut c_void;
    #[repr(C)]
    struct ARect {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }
    impl From<Rect> for ARect {
        fn from(r: Rect) -> Self {
            Self {
                left: r.x as i32,
                top: r.y as i32,
                right: (r.x + r.w) as i32,
                bottom: (r.y + r.h) as i32,
            }
        }
    }
    #[repr(C)]
    #[derive(Default)]
    struct SyncFileInfo {
        name: [u8; 32],
        status: i32,
        flags: u32,
        count: u32,
        pad: u32,
        fences: u64,
    }
    #[repr(C)]
    #[derive(Default)]
    struct SyncFenceInfo {
        object: [u8; 32],
        driver: [u8; 32],
        status: i32,
        flags: u32,
        timestamp: u64,
    }

    #[rustfmt::skip]
    #[link(name = "android")]
    unsafe extern "C" {
        fn ANativeWindow_getWidth(window: *mut ndk_sys::ANativeWindow) -> i32;
        fn ANativeWindow_getHeight(window: *mut ndk_sys::ANativeWindow) -> i32;
        fn ASurfaceControl_createFromWindow(window: *mut ndk_sys::ANativeWindow, name: *const c_char) -> Handle;
        fn ASurfaceControl_create(parent: Handle, name: *const c_char) -> Handle;
        fn ASurfaceControl_release(surface: Handle);
        fn ASurfaceTransaction_create() -> Handle;
        fn ASurfaceTransaction_delete(tx: Handle);
        fn ASurfaceTransaction_apply(tx: Handle);
        fn ASurfaceTransaction_reparent(tx: Handle, surface: Handle, parent: Handle);
        fn ASurfaceTransaction_setVisibility(tx: Handle, surface: Handle, visibility: i8);
        fn ASurfaceTransaction_setZOrder(tx: Handle, surface: Handle, z: i32);
        fn ASurfaceTransaction_setBuffer(tx: Handle, surface: Handle, buffer: Handle, fence: i32);
        fn ASurfaceTransaction_setBufferDataSpace(tx: Handle, surface: Handle, dataspace: i32);
        fn ASurfaceTransaction_setBufferTransparency(tx: Handle, surface: Handle, transparency: i8);
        fn ASurfaceTransaction_setColor(tx: Handle, surface: Handle, r: f32, g: f32, b: f32, alpha: f32, dataspace: i32);
        fn ASurfaceTransaction_setGeometry(tx: Handle, surface: Handle, crop: *const ARect, dst: *const ARect, transform: i32);
        fn ASurfaceTransaction_setDamageRegion(tx: Handle, surface: Handle, rects: *const ARect, count: u32);
        fn ASurfaceTransaction_setDesiredPresentTime(tx: Handle, time: i64);
        fn ASurfaceTransaction_setOnComplete(tx: Handle, context: Handle, callback: unsafe extern "C" fn(Handle, Handle));
        fn ASurfaceTransactionStats_getLatchTime(stats: Handle) -> i64;
        fn ASurfaceTransactionStats_getPresentFenceFd(stats: Handle) -> i32;
        fn ASurfaceTransactionStats_getPreviousReleaseFenceFd(stats: Handle, surface: Handle) -> i32;
    }
    fn owned(fd: i32) -> Option<OwnedFd> {
        assert!(fd >= -1, "invalid compositor fence");
        (fd != -1).then(|| unsafe { OwnedFd::from_raw_fd(fd) })
    }
    /// `None` while the fence is pending; then its signal time, or `None` inside
    /// when it failed (the frame never reached the panel).
    fn signal_time(fd: &OwnedFd, fences: &mut Vec<SyncFenceInfo>) -> Option<Option<i64>> {
        // android/sync.h's sync_file_info query, with reusable storage instead
        // of libsync's per-call allocation. Both ioctls are nonblocking.
        unsafe {
            let request = libc::_IOWR::<SyncFileInfo>(u32::from(b'>'), 4);
            let mut info = SyncFileInfo::default();
            assert_eq!(
                libc::ioctl(fd.as_raw_fd(), request, &mut info),
                0,
                "sync file info"
            );
            match info.status {
                0 => return None,
                s if s < 0 => return Some(None),
                _ => {}
            }
            assert!(info.count > 0, "empty present fence");
            fences.resize_with(info.count as usize, SyncFenceInfo::default);
            info.fences = fences.as_mut_ptr() as u64;
            assert_eq!(
                libc::ioctl(fd.as_raw_fd(), request, &mut info),
                0,
                "sync fence info"
            );
            Some(Some(
                fences.iter().map(|f| f.timestamp).max().unwrap() as i64
            ))
        }
    }

    struct Clock {
        instant: Instant,
        ns: i64,
    }
    impl Clock {
        fn new() -> Self {
            let instant = Instant::now();
            let mut time = libc::timespec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut time) };
            Self {
                instant,
                ns: time.tv_sec * 1_000_000_000 + time.tv_nsec,
            }
        }
        fn ns(&self, at: Instant) -> i64 {
            self.ns
                + if at >= self.instant {
                    at.duration_since(self.instant).as_nanos() as i64
                } else {
                    -(self.instant.duration_since(at).as_nanos() as i64)
                }
        }
        fn instant(&self, ns: i64) -> Instant {
            if ns >= self.ns {
                self.instant + Duration::from_nanos((ns - self.ns) as u64)
            } else {
                self.instant - Duration::from_nanos((self.ns - ns) as u64)
            }
        }
    }

    fn set_rate(surface: Handle, rate: u32) {
        type SetRate = unsafe extern "C" fn(Handle, Handle, f32, i8);
        static SET: OnceLock<Option<SetRate>> = OnceLock::new();
        let setter = SET.get_or_init(|| {
            let symbol = unsafe {
                libc::dlsym(
                    libc::RTLD_DEFAULT,
                    c"ASurfaceTransaction_setFrameRate".as_ptr(),
                )
            };
            if symbol.is_null() {
                None
            } else {
                Some(unsafe { std::mem::transmute::<*mut c_void, SetRate>(symbol) })
            }
        });
        if let Some(set) = setter {
            unsafe {
                let tx = ASurfaceTransaction_create();
                set(tx, surface, rate as f32, 0);
                ASurfaceTransaction_apply(tx);
                ASurfaceTransaction_delete(tx);
            }
        }
    }

    struct Surface {
        id: Option<LayerId>,
        handle: NonNull<c_void>,
        geometry: Option<Geometry>,
        visible: bool,
        z: Option<i32>,
        dataspace: i32,
        color: Option<[f32; 4]>,
    }
    /// `ADataSpace` of a buffer: sRGB (the platform default) for RGB, the
    /// pool's matrix and range with the SMPTE 170M transfer for YUV.
    fn dataspace(color: Option<adv_gpu::present::YuvColor>) -> i32 {
        use adv_gpu::present::YuvColor as C;
        const BT709: i32 = 1 << 16;
        const BT601_625: i32 = 2 << 16;
        const SRGB: i32 = 2 << 22;
        const SMPTE_170M: i32 = 3 << 22;
        const FULL: i32 = 1 << 27;
        const LIMITED: i32 = 2 << 27;
        match color {
            None => BT709 | SRGB | FULL,
            Some(C::Bt601Limited) => BT601_625 | SMPTE_170M | LIMITED,
            Some(C::Bt601Full) => BT601_625 | SMPTE_170M | FULL,
            Some(C::Bt709Limited) => BT709 | SMPTE_170M | LIMITED,
            Some(C::Bt709Full) => BT709 | SMPTE_170M | FULL,
        }
    }
    #[derive(Default)]
    struct Completion {
        inbox: Option<Arc<Inbox>>,
        order: u64,
        serial: u64,
        latch: i64,
        present: Option<OwnedFd>,
        previous: Vec<(usize, Option<OwnedFd>)>,
        reported: bool,
    }
    #[derive(Default)]
    struct Inbox {
        ready: Mutex<VecDeque<Box<Completion>>>,
        wake: Condvar,
        owner: OnceLock<Signals>,
    }

    struct Signals {
        poll: OwnedFd,
        stop: OwnedFd,
        wake: Wake,
    }
    impl Signals {
        fn new(wake: Wake) -> (Self, std::thread::JoinHandle<()>) {
            let poll = owned(unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) }).expect("epoll");
            let stop = owned(unsafe { libc::eventfd(0, libc::EFD_CLOEXEC) }).expect("eventfd");
            let mut event = libc::epoll_event {
                events: libc::EPOLLIN as u32,
                u64: 0,
            };
            assert_eq!(
                unsafe {
                    libc::epoll_ctl(
                        poll.as_raw_fd(),
                        libc::EPOLL_CTL_ADD,
                        stop.as_raw_fd(),
                        &mut event,
                    )
                },
                0
            );
            let fd = poll.try_clone().expect("epoll duplicate");
            let notify = wake.clone();
            let thread = std::thread::Builder::new()
                .name("display fences".into())
                .spawn(move || {
                    let mut event = libc::epoll_event { events: 0, u64: 0 };
                    loop {
                        let n = unsafe { libc::epoll_wait(fd.as_raw_fd(), &mut event, 1, -1) };
                        if n < 0
                            && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR)
                        {
                            continue;
                        }
                        assert_eq!(n, 1, "present fence wait");
                        if event.u64 == 0 {
                            break;
                        }
                        notify();
                    }
                })
                .expect("present fence thread");
            (Self { poll, stop, wake }, thread)
        }
        fn watch(&self, fd: &OwnedFd) {
            let mut event = libc::epoll_event {
                events: (libc::EPOLLIN | libc::EPOLLONESHOT) as u32,
                u64: 1,
            };
            assert_eq!(
                unsafe {
                    libc::epoll_ctl(
                        self.poll.as_raw_fd(),
                        libc::EPOLL_CTL_ADD,
                        fd.as_raw_fd(),
                        &mut event,
                    )
                },
                0,
                "present fence registration"
            );
        }
        fn stop(&self) {
            let value = 1u64;
            assert_eq!(
                unsafe { libc::write(self.stop.as_raw_fd(), (&value as *const u64).cast(), 8) },
                8
            );
        }
    }
    // This callback owns only numeric surface identities and fds. Its box and
    // previous-fence storage return to the owner thread for reuse.
    unsafe extern "C" fn complete(context: Handle, stats: Handle) {
        let mut event = unsafe { Box::from_raw(context.cast::<Completion>()) };
        event.latch = unsafe { ASurfaceTransactionStats_getLatchTime(stats) };
        event.present = owned(unsafe { ASurfaceTransactionStats_getPresentFenceFd(stats) });
        for (surface, fence) in &mut event.previous {
            *fence = owned(unsafe {
                ASurfaceTransactionStats_getPreviousReleaseFenceFd(stats, *surface as Handle)
            });
        }
        let inbox = event.inbox.take().unwrap();
        if let Some(signals) = inbox.owner.get()
            && let Some(fd) = &event.present
        {
            signals.watch(fd);
        }
        inbox.ready.lock().unwrap().push_back(event);
        inbox.wake.notify_one();
        if let Some(signals) = inbox.owner.get() {
            (signals.wake)();
        }
    }

    /// SurfaceFlinger layers backed by Android hardware buffers.
    // SAFETY: SurfaceControl handles and the retained window have no thread affinity.
    unsafe impl Send for SurfaceCompositor {}
    pub struct SurfaceCompositor {
        window: NonNull<ndk_sys::ANativeWindow>,
        parent: NonNull<c_void>,
        bounds: Rect,
        caps: CompositorCaps,
        clock: Clock,
        surfaces: Vec<Surface>,
        /// The surface, serial and layer of each attached buffer.
        held: Vec<(usize, u64, LayerId)>,
        inbox: Arc<Inbox>,
        free: VecDeque<Box<Completion>>,
        pending: VecDeque<Box<Completion>>,
        submitted: u64,
        drained: u64,
        rects: Vec<ARect>,
        fences: Vec<SyncFenceInfo>,
        signal_thread: Option<std::thread::JoinHandle<()>>,
    }
    impl SurfaceCompositor {
        /// # Safety
        /// `window` must be valid on entry; the compositor acquires its own reference. Caps must describe
        /// Android as documented by this module.
        pub unsafe fn new(
            window: NonNull<ndk_sys::ANativeWindow>,
            mut caps: CompositorCaps,
        ) -> Result<Self, Error> {
            caps.yuv_layers = true;
            let clock = Clock::new();
            let (w, h) = unsafe {
                (
                    ANativeWindow_getWidth(window.as_ptr()),
                    ANativeWindow_getHeight(window.as_ptr()),
                )
            };
            if w <= 0 || h <= 0 {
                return Err(Error::SurfaceLost);
            }
            let parent = NonNull::new(unsafe {
                ASurfaceControl_createFromWindow(window.as_ptr(), c"adv-display".as_ptr())
            })
            .ok_or(Error::SurfaceLost)?;
            unsafe {
                ndk_sys::ANativeWindow_acquire(window.as_ptr());
            }
            Ok(Self {
                window,
                parent,
                bounds: Rect {
                    x: 0,
                    y: 0,
                    w: w as u32,
                    h: h as u32,
                },
                caps,
                clock,
                surfaces: Vec::new(),
                held: Vec::new(),
                inbox: Arc::default(),
                free: VecDeque::new(),
                pending: VecDeque::new(),
                submitted: 0,
                drained: 0,
                rects: Vec::new(),
                fences: Vec::new(),
                signal_thread: None,
            })
        }
        fn surface(&mut self, id: Option<LayerId>) -> Result<(), Error> {
            if !self.surfaces.iter().any(|s| s.id == id) {
                let name = match id {
                    None => c"background",
                    Some(LayerId::Base) => c"base",
                    Some(LayerId::Video(_)) => c"video",
                    Some(LayerId::VideoBackground(_)) => c"video background",
                    Some(LayerId::Host(_)) => c"host",
                };
                let handle = NonNull::new(unsafe {
                    ASurfaceControl_create(self.parent.as_ptr(), name.as_ptr())
                })
                .ok_or(Error::SurfaceLost)?;
                self.surfaces.push(Surface {
                    id,
                    handle,
                    geometry: None,
                    visible: true,
                    z: None,
                    dataspace: dataspace(None),
                    color: None,
                });
                if id == Some(LayerId::Base) {
                    // Presentation never exceeds 60 Hz (HOST-SES-8); an idle
                    // layer posts nothing and its request lapses (HOST-SES-11).
                    set_rate(handle.as_ptr(), 60);
                }
            }
            Ok(())
        }
        fn completion(&mut self, serial: u64) -> Box<Completion> {
            self.submitted += 1;
            let mut event = self.free.pop_front().unwrap_or_default();
            event.inbox = Some(self.inbox.clone());
            event.order = self.submitted;
            event.serial = serial;
            event.reported = false;
            event
        }
        fn apply(tx: Handle, event: Box<Completion>) {
            unsafe {
                ASurfaceTransaction_setOnComplete(tx, Box::into_raw(event).cast(), complete);
                ASurfaceTransaction_apply(tx);
                ASurfaceTransaction_delete(tx);
            }
        }
    }
    impl Compositor for SurfaceCompositor {
        fn set_waker(&mut self, wake: Wake) {
            let (signals, thread) = Signals::new(wake);
            assert!(self.inbox.owner.set(signals).is_ok());
            self.signal_thread = Some(thread);
        }
        fn detach(&mut self) -> Option<Fence> {
            self.held.clear();
            if self.surfaces.is_empty() {
                return None;
            }
            let tx = unsafe { ASurfaceTransaction_create() };
            for surface in &self.surfaces {
                unsafe {
                    ASurfaceTransaction_reparent(tx, surface.handle.as_ptr(), ptr::null_mut());
                }
            }
            let event = self.completion(u64::MAX);
            let order = event.order;
            Self::apply(tx, event);
            let ready = self.inbox.ready.lock().unwrap();
            let mut ready = self
                .inbox
                .wake
                .wait_while(ready, |r| !r.iter().any(|e| e.order == order))
                .unwrap();
            let i = ready.iter().position(|e| e.order == order).unwrap();
            let mut event = ready.remove(i).unwrap();
            drop(ready);
            for surface in self.surfaces.drain(..) {
                unsafe {
                    ASurfaceControl_release(surface.handle.as_ptr());
                }
            }
            event.present.take().map(Fence::from_fd)
        }

        fn caps(&self) -> CompositorCaps {
            self.caps.clone()
        }
        fn present(
            &mut self,
            layers: &Layers,
            _damage: &Tiles,
            images: &mut [AcquiredImage],
            serial: u64,
            at: Instant,
        ) -> Result<(), Error> {
            let create = self
                .surface(None)
                .and_then(|()| self.surface(Some(LayerId::Base)))
                .and_then(|()| {
                    for (id, _, _) in &layers.solids {
                        self.surface(Some(LayerId::VideoBackground(*id)))?;
                    }
                    for (id, _) in &layers.video {
                        self.surface(Some(LayerId::Video(*id)))?;
                    }
                    for id in &layers.host {
                        self.surface(Some(LayerId::Host(*id)))?;
                    }
                    Ok(())
                });
            create?;
            let tx = unsafe { ASurfaceTransaction_create() };
            let mut event = self.completion(serial);
            let damage_layer = if layers.host.contains(&HostLayer::Damage) {
                let image = &images
                    .iter()
                    .find(|a| a.layer == LayerId::Host(HostLayer::Damage))
                    .unwrap()
                    .image;
                let size = image.image().desc().size;
                Some(crate::damage::layer((size.width, size.height)))
            } else {
                None
            };
            for surface in &mut self.surfaces {
                let handle = surface.handle.as_ptr();
                let (layer, visible, z) = match surface.id {
                    None => (None, true, 0),
                    Some(LayerId::Base) => (Some(&layers.base), true, 1),
                    Some(LayerId::Video(id)) => {
                        let position = layers.video.iter().position(|(v, _)| *v == id);
                        (
                            position.map(|i| &layers.video[i].1),
                            position.is_some(),
                            position.map_or(0, |i| 3 + 2 * i as i32),
                        )
                    }
                    Some(LayerId::VideoBackground(id)) => {
                        let solid = layers.solids.iter().find(|(v, _, _)| *v == id);
                        if let Some((_, dst, color)) = solid {
                            unsafe {
                                if surface.color != Some(*color) {
                                    ASurfaceTransaction_setColor(
                                        tx,
                                        handle,
                                        color[0],
                                        color[1],
                                        color[2],
                                        color[3],
                                        dataspace(None),
                                    );
                                    surface.color = Some(*color);
                                }
                                let geometry = Geometry(*dst, *dst, Rotation::None, None);
                                if surface.geometry != Some(geometry) {
                                    surface.geometry = Some(geometry);
                                    ASurfaceTransaction_setGeometry(
                                        tx,
                                        handle,
                                        &(*dst).into(),
                                        &(*dst).into(),
                                        0,
                                    );
                                }
                            }
                        }
                        (
                            None,
                            solid.is_some(),
                            layers
                                .video
                                .iter()
                                .position(|(v, _)| *v == id)
                                .map_or(0, |i| 2 + 2 * i as i32),
                        )
                    }
                    Some(LayerId::Host(HostLayer::Damage)) => {
                        // ASURFACE_TRANSACTION_TRANSPARENCY_TRANSLUCENT.
                        unsafe {
                            ASurfaceTransaction_setBufferTransparency(tx, handle, 1);
                        }
                        (
                            damage_layer.as_ref(),
                            damage_layer.is_some(),
                            3 + 2 * layers.video.len() as i32 + HostLayer::Damage as i32,
                        )
                    }
                    Some(LayerId::Host(id)) => (
                        None,
                        layers.host.contains(&id),
                        2 + layers.video.len() as i32 + id as i32,
                    ),
                };
                unsafe {
                    if visible != surface.visible {
                        ASurfaceTransaction_setVisibility(tx, handle, i8::from(visible));
                        surface.visible = visible;
                    }
                    if !visible {
                        if matches!(
                            surface.id,
                            Some(
                                LayerId::Video(_)
                                    | LayerId::VideoBackground(_)
                                    | LayerId::Host(HostLayer::Damage)
                            )
                        ) {
                            ASurfaceTransaction_reparent(tx, handle, ptr::null_mut());
                            event.previous.push((handle as usize, None));
                        }
                        continue;
                    }
                    if surface.z != Some(z) {
                        ASurfaceTransaction_setZOrder(tx, handle, z);
                        surface.z = Some(z);
                    }
                    if let Some(layer) = layer
                        && Geometry::update(&mut surface.geometry, layer)
                    {
                        ASurfaceTransaction_setGeometry(
                            tx,
                            handle,
                            &if let Some([x, y, w, h]) = layer.source_uv {
                                Rect {
                                    x: (x * layer.crop.w as f32).round() as u32,
                                    y: (y * layer.crop.h as f32).round() as u32,
                                    w: (w * layer.crop.w as f32).round() as u32,
                                    h: (h * layer.crop.h as f32).round() as u32,
                                }
                                .into()
                            } else {
                                layer.crop.into()
                            },
                            &layer.dst.into(),
                            transform(layer.rotation),
                        );
                    }
                    let [r, g, b] = layers.background;
                    let color = [r, g, b, 1.];
                    if surface.id.is_none() && surface.color != Some(color) {
                        let [r, g, b] = layers.background;
                        ASurfaceTransaction_setColor(tx, handle, r, g, b, 1.0, dataspace(None));
                        if surface.geometry.is_none() {
                            ASurfaceTransaction_setGeometry(
                                tx,
                                handle,
                                &self.bounds.into(),
                                &self.bounds.into(),
                                0,
                            );
                            surface.geometry =
                                Some(Geometry(self.bounds, self.bounds, Rotation::None, None));
                        }
                        surface.color = Some(color);
                    }
                }
            }
            for acquired in images.iter_mut() {
                let state = self
                    .surfaces
                    .iter_mut()
                    .find(|s| s.id == Some(acquired.layer))
                    .expect("image layer exists");
                let surface = state.handle.as_ptr();
                let Export::AndroidHardwareBuffer(buffer, _) = acquired.image.export() else {
                    panic!("Android presentable buffer");
                };
                let buffer = buffer.as_ptr();
                let fd = acquired
                    .image
                    .acquire_fence()
                    .map_or(-1, |f| f.into_fd().into_raw_fd());
                unsafe {
                    ASurfaceTransaction_setBuffer(tx, surface, buffer, fd);
                    let dataspace = dataspace(acquired.image.color());
                    if state.dataspace != dataspace {
                        ASurfaceTransaction_setBufferDataSpace(tx, surface, dataspace);
                        state.dataspace = dataspace;
                    }
                }
                let layer = match acquired.layer {
                    LayerId::Base => &layers.base,
                    LayerId::Video(id) => &layers.video.iter().find(|(v, _)| *v == id).unwrap().1,
                    LayerId::VideoBackground(_) => unreachable!("solid layer has no image"),
                    LayerId::Host(HostLayer::Damage) => damage_layer.as_ref().unwrap(),
                    LayerId::Host(_) => panic!("host content is not implemented"),
                };
                self.rects.clear();
                let size = acquired.image.image().desc().size;
                let damage = if layer.damage.size() == (size.width, size.height) {
                    layer.damage.rects()
                } else {
                    vec![adv_gpu::Rect {
                        x: 0,
                        y: 0,
                        w: size.width,
                        h: size.height,
                    }]
                };
                self.rects
                    .extend(damage.into_iter().map(|r| ARect::from(Rect::from(r))));
                unsafe {
                    ASurfaceTransaction_setDamageRegion(
                        tx,
                        surface,
                        self.rects.as_ptr(),
                        self.rects.len() as u32,
                    );
                }
                event.previous.push((surface as usize, None));
                self.held.push((surface as usize, serial, acquired.layer));
            }
            unsafe {
                ASurfaceTransaction_setDesiredPresentTime(tx, self.clock.ns(at));
            }
            Self::apply(tx, event);
            // Applied transactions retain their controls through completion.
            self.surfaces.retain(|s| {
                let keep = s.visible
                    || !matches!(
                        s.id,
                        Some(
                            LayerId::Video(_)
                                | LayerId::VideoBackground(_)
                                | LayerId::Host(HostLayer::Damage)
                        )
                    );
                if !keep {
                    unsafe {
                        ASurfaceControl_release(s.handle.as_ptr());
                    }
                }
                keep
            });
            Ok(())
        }
        fn drain(&mut self, presented: &mut Vec<Presented>, released: &mut Vec<Released>) {
            loop {
                let event = {
                    let mut ready = self.inbox.ready.lock().unwrap();
                    ready
                        .iter()
                        .position(|e| e.order == self.drained + 1)
                        .map(|i| ready.remove(i).unwrap())
                };
                let Some(mut event) = event else {
                    break;
                };
                self.drained = event.order;
                for (surface, fence) in event.previous.drain(..) {
                    if let Some(i) = release_index(
                        self.held.iter().map(|(s, n, _)| (*s, *n)),
                        surface,
                        event.serial,
                    ) {
                        let (_, serial, layer) = self.held.swap_remove(i);
                        released.push(Released {
                            serial,
                            layer,
                            fence: fence.map(Fence::from_fd),
                        });
                    }
                }
                self.pending.push_back(event);
            }
            // Keep unsignaled present fences and report their shown time on a
            // later drain. Latch reports and shown updates are oldest first.
            for event in &mut self.pending {
                let signaled = match &event.present {
                    Some(fd) => signal_time(fd, &mut self.fences),
                    None => Some(None),
                };
                let shown = signaled.flatten().map(|ns| self.clock.instant(ns));
                if !event.reported || shown.is_some() {
                    presented.push(Presented {
                        serial: event.serial,
                        // A transaction replaced before latching reports none.
                        latched: (event.latch > 0).then(|| self.clock.instant(event.latch)),
                        shown,
                    });
                    event.reported = true;
                }
                if signaled.is_some() {
                    event.present = None;
                }
            }
            let mut i = 0;
            while i < self.pending.len() {
                if self.pending[i].present.is_none() {
                    self.free.push_back(self.pending.remove(i).unwrap());
                } else {
                    i += 1;
                }
            }
        }
    }
    impl Drop for SurfaceCompositor {
        fn drop(&mut self) {
            self.detach();
            if let Some(thread) = self.signal_thread.take() {
                self.inbox.owner.get().unwrap().stop();
                thread.join().unwrap();
            }
            unsafe {
                ASurfaceControl_release(self.parent.as_ptr());
                ndk_sys::ANativeWindow_release(self.window.as_ptr());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::{Region, Source};

    #[test]
    fn transforms() {
        assert_eq!(
            [
                Rotation::None,
                Rotation::Quarter,
                Rotation::Half,
                Rotation::ThreeQuarters
            ]
            .map(transform),
            [0, 4, 3, 7]
        );
    }
    #[test]
    fn geometry_diff() {
        let mut last = None;
        let rect = Rect {
            x: 0,
            y: 0,
            w: 64,
            h: 32,
        };
        let mut layer = Layer {
            source_uv: None,
            source: Source::Base,
            crop: rect,
            dst: rect,
            rotation: Rotation::None,
            nearest: false,
            damage: Region::whole((32, 32)),
        };
        assert!(Geometry::update(&mut last, &layer));
        assert!(!Geometry::update(&mut last, &layer));
        layer.damage = Region::empty((32, 32));
        layer.nearest = true;
        assert!(!Geometry::update(&mut last, &layer));
        layer.crop.x = 1;
        assert!(Geometry::update(&mut last, &layer));
        layer.dst.w = 128;
        assert!(Geometry::update(&mut last, &layer));
        layer.rotation = Rotation::Quarter;
        assert!(Geometry::update(&mut last, &layer));
    }
    #[cfg(unix)]
    #[test]
    fn previous_release_matches_surface_before_serial() {
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
        let mut fds = [-1; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        let read = unsafe { OwnedFd::from_raw_fd(fds[0]) };
        let write = unsafe { OwnedFd::from_raw_fd(fds[1]) };
        let mut history = vec![(10, 1), (20, 2), (10, 4), (10, 7)];
        let mut images = vec![101, 202, 104, 107];
        assert_eq!(release_index(history.iter().copied(), 10, 1), None);
        assert_eq!(release_index(history.iter().copied(), 30, 9), None);
        for (surface, serial, fence, expected) in [(10, 4, read, 101), (20, 5, write, 202)] {
            let fd = fence.as_raw_fd();
            let i = release_index(history.iter().copied(), surface, serial).unwrap();
            history.swap_remove(i);
            let released = (images.swap_remove(i), fence);
            assert_eq!(released.0, expected);
            assert_eq!(released.1.as_raw_fd(), fd);
            assert_ne!(unsafe { libc::fcntl(fd, libc::F_GETFD) }, -1);
        }
        assert_eq!(
            images[release_index(history.iter().copied(), 10, 7).unwrap()],
            104
        );
    }
}

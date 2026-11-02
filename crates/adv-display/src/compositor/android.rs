//! Android API 29 SurfaceControl presentation. Callers supply caps with scaling,
//! rotation, solid colors and damage hints enabled, nearest filtering and YUV
//! layers disabled. Compression and tile size come from the GPU device.

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

#[derive(Clone, Copy, PartialEq, Eq)]
struct Geometry(Rect, Rect, Rotation);
impl Geometry {
    fn update(last: &mut Option<Self>, layer: &Layer) -> bool {
        let next = Self(layer.crop, layer.dst, layer.rotation);
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
    use crate::{compositor::*, plan::Damage};
    use adv_gpu::present::Export;
    use std::{
        collections::VecDeque,
        ffi::{c_char, c_void},
        os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd},
        ptr::{self, NonNull},
        sync::{Arc, Condvar, Mutex},
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

    struct Surface {
        id: Option<LayerId>,
        handle: NonNull<c_void>,
        geometry: Option<Geometry>,
        visible: bool,
        z: Option<i32>,
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
        inbox.ready.lock().unwrap().push_back(event);
        inbox.wake.notify_one();
    }

    /// SurfaceFlinger layers backed by Android hardware buffers.
    pub struct SurfaceCompositor {
        parent: NonNull<c_void>,
        bounds: Rect,
        caps: CompositorCaps,
        clock: Clock,
        surfaces: Vec<Surface>,
        color: Option<[f32; 3]>,
        held: Vec<(usize, u64, AcquiredImage)>,
        inbox: Arc<Inbox>,
        free: VecDeque<Box<Completion>>,
        pending: VecDeque<Box<Completion>>,
        submitted: u64,
        drained: u64,
        rects: Vec<ARect>,
        fences: Vec<SyncFenceInfo>,
    }
    impl SurfaceCompositor {
        /// # Safety
        /// `window` stays valid until the compositor drops. Caps must describe
        /// Android as documented by this module.
        pub unsafe fn new(
            window: NonNull<ndk_sys::ANativeWindow>,
            caps: CompositorCaps,
        ) -> Result<Self, Error> {
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
            Ok(Self {
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
                color: None,
                held: Vec::new(),
                inbox: Arc::default(),
                free: VecDeque::new(),
                pending: VecDeque::new(),
                submitted: 0,
                drained: 0,
                rects: Vec::new(),
                fences: Vec::new(),
            })
        }
        fn surface(&mut self, id: Option<LayerId>) -> Result<(), Error> {
            if !self.surfaces.iter().any(|s| s.id == id) {
                let name = match id {
                    None => c"background",
                    Some(LayerId::Base) => c"base",
                    Some(LayerId::Video(_)) => c"video",
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
                });
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
        fn caps(&self) -> CompositorCaps {
            self.caps.clone()
        }
        fn present(
            &mut self,
            layers: &Layers,
            _damage: &Tiles,
            images: Vec<AcquiredImage>,
            serial: u64,
            at: Instant,
        ) -> Result<(), PresentError> {
            let create = self
                .surface(None)
                .and_then(|()| self.surface(Some(LayerId::Base)))
                .and_then(|()| {
                    for (id, _) in &layers.video {
                        self.surface(Some(LayerId::Video(*id)))?;
                    }
                    for id in &layers.host {
                        self.surface(Some(LayerId::Host(*id)))?;
                    }
                    Ok(())
                });
            if let Err(error) = create {
                return Err(PresentError { error, images });
            }
            let tx = unsafe { ASurfaceTransaction_create() };
            let mut event = self.completion(serial);
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
                            position.map_or(0, |i| 2 + i as i32),
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
                        if matches!(surface.id, Some(LayerId::Video(_))) {
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
                            &layer.crop.into(),
                            &layer.dst.into(),
                            transform(layer.rotation),
                        );
                    }
                    if surface.id.is_none() && self.color != Some(layers.background) {
                        let [r, g, b] = layers.background;
                        ASurfaceTransaction_setColor(tx, handle, r, g, b, 1.0, 142671872); // ADATASPACE_SRGB
                        if surface.geometry.is_none() {
                            ASurfaceTransaction_setGeometry(
                                tx,
                                handle,
                                &self.bounds.into(),
                                &self.bounds.into(),
                                0,
                            );
                            surface.geometry =
                                Some(Geometry(self.bounds, self.bounds, Rotation::None));
                        }
                        self.color = Some(layers.background);
                    }
                }
            }
            for mut acquired in images {
                let surface = self
                    .surfaces
                    .iter()
                    .find(|s| s.id == Some(acquired.layer))
                    .expect("image layer exists")
                    .handle
                    .as_ptr();
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
                }
                let layer = match acquired.layer {
                    LayerId::Base => &layers.base,
                    LayerId::Video(id) => &layers.video.iter().find(|(v, _)| *v == id).unwrap().1,
                    LayerId::Host(_) => panic!("host content is not implemented"),
                };
                self.rects.clear();
                match &layer.damage {
                    Damage::Whole => self.rects.push(
                        Rect {
                            x: 0,
                            y: 0,
                            w: acquired.image.image().desc().size.width,
                            h: acquired.image.image().desc().size.height,
                        }
                        .into(),
                    ),
                    Damage::Rects(rects) => {
                        self.rects.extend(rects.iter().copied().map(ARect::from))
                    }
                    Damage::None => {}
                }
                unsafe {
                    ASurfaceTransaction_setDamageRegion(
                        tx,
                        surface,
                        self.rects.as_ptr(),
                        self.rects.len() as u32,
                    );
                }
                event.previous.push((surface as usize, None));
                self.held.push((surface as usize, serial, acquired));
            }
            unsafe {
                ASurfaceTransaction_setDesiredPresentTime(tx, self.clock.ns(at));
            }
            Self::apply(tx, event);
            // Applied transactions retain their controls through completion.
            self.surfaces.retain(|s| {
                let keep = s.visible || !matches!(s.id, Some(LayerId::Video(_)));
                if !keep {
                    unsafe {
                        ASurfaceControl_release(s.handle.as_ptr());
                    }
                }
                keep
            });
            Ok(())
        }
        fn drain(&mut self, presented: &mut Vec<Presented>, released: &mut Vec<ReleasedImage>) {
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
                        let (_, serial, acquired) = self.held.swap_remove(i);
                        released.push(ReleasedImage {
                            serial,
                            layer: acquired.layer,
                            image: acquired.image,
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
                        latched: Some(self.clock.instant(event.latch)),
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
            // AND-APP-10: pools outlive windows. Only the detach present makes
            // every image still held here safe for the pool to write again.
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
            let (mut ready, _) = self
                .inbox
                .wake
                .wait_timeout_while(ready, Duration::from_secs(1), |ready| {
                    !ready.iter().any(|e| e.order == order)
                })
                .unwrap();
            if let Some(i) = ready.iter().position(|e| e.order == order) {
                let event = ready.remove(i).unwrap();
                for (_, _, acquired) in self.held.drain(..) {
                    acquired.image.released(
                        event.present.as_ref().map(|fd| {
                            Fence::from_fd(fd.try_clone().expect("duplicate detach fence"))
                        }),
                    );
                }
            } else {
                // No completion means no safe release fence. Keep the pool's
                // outstanding leases rather than return buffers still read.
                for (_, _, acquired) in self.held.drain(..) {
                    std::mem::forget(acquired);
                }
            }
            for surface in &self.surfaces {
                unsafe {
                    ASurfaceControl_release(surface.handle.as_ptr());
                }
            }
            unsafe {
                ASurfaceControl_release(self.parent.as_ptr());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::{Damage, Source};

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
            source: Source::Base,
            crop: rect,
            dst: rect,
            rotation: Rotation::None,
            nearest: false,
            damage: Damage::Whole,
        };
        assert!(Geometry::update(&mut last, &layer));
        assert!(!Geometry::update(&mut last, &layer));
        layer.damage = Damage::None;
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

use adv_device::surface::{PixelFormat, Snapshot, SurfaceMemory, Surfaces};
use adv_dx::ddraw::{DirectDraw, IID_DDRAW7, api as dd};
use std::borrow::Cow;

struct Memory(Vec<u8>);
impl SurfaceMemory for Memory {
    fn read(&self, addr: u32, len: usize) -> Result<Cow<'_, [u8]>, String> {
        self.0
            .get(addr as usize..addr as usize + len)
            .map(Cow::Borrowed)
            .ok_or_else(|| "read bounds".into())
    }
    fn write(&mut self, addr: u32, data: &[u8]) -> Result<(), String> {
        self.writable(addr, data.len())?;
        self.0[addr as usize..addr as usize + data.len()].copy_from_slice(data);
        Ok(())
    }
    fn writable(&self, addr: u32, len: usize) -> Result<(), String> {
        self.read(addr, len).map(|_| ())
    }
    fn copy(&mut self, src: u32, dst: u32, len: usize) -> Result<(), String> {
        self.read(src, len)?;
        self.writable(dst, len)?;
        self.0
            .copy_within(src as usize..src as usize + len, dst as usize);
        Ok(())
    }
}
struct Fixture {
    objects: adv_machine::Objects,
    memory: Memory,
    surfaces: Surfaces<Snapshot>,
    #[cfg(feature = "gpu")]
    capture: adv_device::gpu::test_support::SurfaceCapture,
    ddraw: DirectDraw,
    device: u32,
}

#[test]
// spec: GFX-DDRAW-1
fn write_only_surface_locks_pair_with_unlock_and_preserve_readonly_versioning() {
    let mut f = Fixture::new();
    let (surface, pixels, _) = f.surface(8, 8, PixelFormat::Bgra32, false);
    let sid = (0..32)
        .find(|&id| f.surfaces.get(id).is_some_and(|s| s.pixels == pixels))
        .unwrap();
    let before = f.surfaces[sid].version;
    f.memory.put_u32(64, 124).unwrap();
    assert_eq!(f.dd(dd::LOCK, &[surface, 0, 64, 0x821, 0]), 0);
    f.memory.put_u32(pixels, 0xff123456).unwrap();
    assert_eq!(f.dd(dd::LOCK, &[surface, 0, 64, 0x821, 0]), 0x887601ae);
    assert_eq!(f.dd(dd::UNLOCK, &[surface, 0]), 0);
    assert_eq!(f.surfaces[sid].version, before + 1);
    assert_eq!(f.dd(dd::UNLOCK, &[surface, 0]), 0x88760248);
    assert_eq!(f.dd(dd::LOCK, &[surface, 0, 64, 0x811, 0]), 0);
    assert_eq!(f.memory.u32(pixels).unwrap(), 0xff123456);
    assert_eq!(f.dd(dd::UNLOCK, &[surface, 0]), 0);
    assert_eq!(f.surfaces[sid].version, before + 1);
    assert_eq!(f.dd(dd::LOCK, &[surface, 0, 64, 0x30, 0]), 0);
    assert_eq!(f.dd(dd::UNLOCK, &[surface, 0]), 0);
    assert_eq!(f.dd(dd::COOPERATIVE, &[f.device, 0, 8]), 0);
    assert_eq!(f.dd(dd::COOPERATIVE, &[f.device, 123, 0x80]), 0);
    assert_eq!(f.dd(dd::COOPERATIVE, &[f.device, 0, 0x80]), 0x80070057);
    assert_eq!(f.dd(dd::RELEASE, &[surface]), 0);
    assert_eq!(f.dd(dd::RELEASE, &[f.device]), 0);
}
impl Fixture {
    fn new() -> Self {
        let mut f = Self::empty(adv_machine::Objects::default());
        f.memory.write(16, &IID_DDRAW7).unwrap();
        assert_eq!(f.dd(dd::DD_CREATE, &[0, 32, 16, 0]), 0);
        f.device = f.memory.u32(32).unwrap();
        // Windows creates surfaces only after a cooperative level is set.
        assert_eq!(f.dd(dd::COOPERATIVE, &[f.device, 0, 8]), 0);
        f
    }
    fn empty(objects: adv_machine::Objects) -> Self {
        #[cfg(feature = "gpu")]
        let capture = adv_device::gpu::test_support::SurfaceCapture::new().unwrap();
        Self {
            #[cfg(feature = "gpu")]
            capture,
            objects,
            // Past the surface arena's end, 0x10000.. is client memory.
            memory: Memory(vec![0; 0x20000]),
            surfaces: Surfaces::new(5, 4, 4096, 61440, Snapshot::default()).unwrap(),
            ddraw: DirectDraw::new(
                adv_dx::ddraw::front::BINDINGS
                    .iter()
                    .map(|&(id, _)| (id, id * 16))
                    .chain([(dd::UNIMPLEMENTED_COM, dd::UNIMPLEMENTED_COM * 16)])
                    .collect(),
            ),
            device: 0,
        }
    }
    fn dd(&mut self, id: u32, args: &[u32]) -> u32 {
        let result = self
            .ddraw
            .call(
                id,
                args,
                &mut self.surfaces,
                &mut self.objects,
                &mut self.memory,
            )
            .unwrap();
        #[cfg(feature = "gpu")]
        self.capture.update(&mut self.surfaces);
        result
    }
    fn surface(&mut self, w: u32, h: u32, format: PixelFormat, primary: bool) -> (u32, u32, u32) {
        let mut desc = [0u8; 124];
        let (r, g, b) = format.masks();
        for (offset, value) in [
            (0, 124),
            (4, if primary { 1 } else { 0x1007 }),
            (8, h),
            (12, w),
            (76, 0x40),
            (84, format.bytes() * 8),
            (88, r),
            (92, g),
            (96, b),
            (104, if primary { 0x200 } else { 0x40 }),
        ] {
            desc[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        self.memory.write(64, &desc).unwrap();
        assert_eq!(self.dd(dd::CREATE_SURFACE, &[self.device, 64, 32, 0]), 0);
        let object = self.memory.u32(32).unwrap();
        self.memory.put_u32(64, 124).unwrap();
        assert_eq!(self.dd(dd::LOCK, &[object, 0, 64, 0, 0]), 0);
        let pixels = self.memory.u32(64 + 36).unwrap();
        let pitch = self.memory.u32(64 + 16).unwrap();
        assert_eq!(self.dd(dd::UNLOCK, &[object, 0]), 0);
        (object, pixels, pitch)
    }
    fn rect(&mut self, addr: u32, values: [i32; 4]) {
        for (i, value) in values.into_iter().enumerate() {
            self.memory
                .write(addr + i as u32 * 4, &value.to_le_bytes())
                .unwrap();
        }
    }
    fn query(&mut self, object: u32, iid: [u8; 16]) -> u32 {
        self.memory.write(16, &iid).unwrap();
        assert_eq!(self.dd(dd::QUERY, &[object, 16, 32]), 0);
        self.memory.u32(32).unwrap()
    }
}

#[test]
fn failed_com_publication_reclaims_all_unpublished_interface_allocations() {
    for limits in [
        adv_machine::ObjectLimits {
            objects: 0,
            ..Default::default()
        },
        adv_machine::ObjectLimits {
            references: 2,
            ..Default::default()
        },
    ] {
        let mut f = Fixture::empty(adv_machine::Objects::with_limits(limits));
        f.memory.write(16, &IID_DDRAW7).unwrap();
        // Enough retries to exhaust the guest arena if a partial old/new
        // interface publication leaked even one allocation per failure.
        for _ in 0..4096 {
            f.memory.put_u32(32, 0xcccccccc).unwrap();
            assert_eq!(f.dd(dd::DD_CREATE, &[0, 32, 16, 0]), 0x8007000e);
            assert_eq!(f.memory.u32(32).unwrap(), 0);
            assert_eq!(f.objects.object_count(), 0);
        }
    }
}

// spec: PERS-SCOPE-13
#[test]
fn directdraw_versions_share_canonical_unknown_and_count_apart() {
    let mut f = Fixture::new();
    let device = f.device;
    let old_iid = [
        0x80, 0xdb, 0x14, 0x6c, 0x33, 0xa7, 0xce, 0x11, 0xa5, 0x21, 0, 0x20, 0xaf, 0x0b, 0xe5, 0x60,
    ];
    let old = f.query(device, old_iid);
    assert_ne!(old, device);
    let from_old = f.query(old, adv_os::com::IUNKNOWN);
    let from_new = f.query(device, adv_os::com::IUNKNOWN);
    assert_eq!((from_old, from_new), (device, device));
    assert_eq!(f.objects.object_count(), 1);
    assert!(f.objects.close(old).is_none());
    // Each version keeps its own count; the object lives while any does.
    assert_eq!(f.dd(dd::RELEASE, &[from_old]), 2);
    assert_eq!(f.dd(dd::RELEASE, &[from_new]), 1);
    assert_eq!(f.dd(dd::RELEASE, &[device]), 0);
    assert_eq!(f.objects.object_count(), 1);
    assert_eq!(f.dd(dd::RELEASE, &[old]), 0);
    assert_eq!(f.objects.object_count(), 0);
    assert_eq!(f.dd(dd::ADDREF, &[device]), 0x80070057);
}

// spec: GFX-DDRAW-15
#[test]
fn surfaces_need_a_cooperative_level_and_report_their_completed_descriptor() {
    let mut f = Fixture::new();
    let owners = f.objects.object_count();
    f.memory.write(16, &IID_DDRAW7).unwrap();
    assert_eq!(f.dd(dd::DD_CREATE, &[0, 32, 16, 0]), 0);
    let device = f.memory.u32(32).unwrap();
    // An offscreen surface without DDSD_CAPS, which Windows completes.
    let mut desc = [0u8; 124];
    for (at, value) in [(0, 124_u32), (4, 6), (8, 2), (12, 3), (104, 0x40)] {
        desc[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }
    f.memory.write(64, &desc).unwrap();
    let create = |f: &mut Fixture| f.dd(dd::CREATE_SURFACE, &[device, 64, 32, 0]);
    assert_eq!(create(&mut f), 0x887600d4); // DDERR_NOCOOPERATIVELEVELSET
    // DDSCL_NORMAL | DDSCL_MULTITHREADED.
    assert_eq!(f.dd(dd::COOPERATIVE, &[device, 0, 0x408]), 0);
    // DirectDraw7 takes DDSURFACEDESC2 only.
    f.memory.put_u32(64, 108).unwrap();
    assert_eq!(create(&mut f), 0x80070057);
    f.memory.put_u32(64, 124).unwrap();
    assert_eq!(create(&mut f), 0);
    let surface = f.memory.u32(32).unwrap();
    f.memory.write(128, &[0; 124]).unwrap();
    f.memory.put_u32(128, 124).unwrap();
    assert_eq!(f.dd(dd::GET_DESC, &[surface, 128]), 0);
    // DDSD_CAPS | HEIGHT | WIDTH | PITCH | PIXELFORMAT; local video memory.
    assert_eq!(f.memory.u32(128 + 4).unwrap(), 0x100f);
    assert_eq!(f.memory.u32(128 + 104).unwrap(), 0x1000_4040);
    // The surface keeps its DirectDraw7 creator alive.
    assert_eq!(f.dd(dd::RELEASE, &[device]), 1);
    assert_eq!(f.dd(dd::RELEASE, &[surface]), 0);
    assert_eq!(f.objects.object_count(), owners);
}

#[test]
fn attached_surface_has_stable_identity_and_parent_retains_it_until_release() {
    let mut f = Fixture::new();
    // Flipping primaries need exclusive mode.
    assert_eq!(f.dd(dd::COOPERATIVE, &[f.device, 7, 0x11]), 0);
    let mut desc = [0u8; 124];
    for (at, value) in [(0, 124_u32), (4, 0x21), (20, 1), (104, 0x218)] {
        desc[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }
    f.memory.write(64, &desc).unwrap();
    assert_eq!(f.dd(dd::CREATE_SURFACE, &[f.device, 64, 32, 0]), 0);
    let parent = f.memory.u32(32).unwrap();
    f.memory.put_u32(256, 4).unwrap();
    assert_eq!(f.dd(dd::GET_ATTACHED, &[parent, 256, 32]), 0);
    let first = f.memory.u32(32).unwrap();
    assert_eq!(f.dd(dd::GET_ATTACHED, &[parent, 256, 32]), 0);
    let second = f.memory.u32(32).unwrap();
    assert_eq!(first, second);
    assert_eq!(f.dd(dd::RELEASE, &[first]), 2);
    assert_eq!(f.dd(dd::RELEASE, &[second]), 1);
    assert_eq!(f.objects.object_count(), 3);
    assert_eq!(f.dd(dd::RELEASE, &[parent]), 0);
    assert_eq!(f.objects.object_count(), 1);
    assert_eq!(f.dd(dd::ADDREF, &[first]), 0x80070057);
}

impl Fixture {
    /// Create a surface from (offset, value) descriptor words.
    fn create(&mut self, words: &[(usize, u32)]) -> u32 {
        let mut desc = [0u8; 124];
        desc[..4].copy_from_slice(&124u32.to_le_bytes());
        for &(at, value) in words {
            desc[at..at + 4].copy_from_slice(&value.to_le_bytes());
        }
        self.memory.write(64, &desc).unwrap();
        assert_eq!(self.dd(dd::CREATE_SURFACE, &[self.device, 64, 32, 0]), 0);
        self.memory.u32(32).unwrap()
    }
    /// The surface attached to `surface` with capabilities `caps`.
    fn attached(&mut self, surface: u32, caps: u32) -> u32 {
        self.memory.write(256, &[0; 16]).unwrap();
        self.memory.put_u32(256, caps).unwrap();
        assert_eq!(self.dd(dd::GET_ATTACHED, &[surface, 256, 32]), 0);
        self.memory.u32(32).unwrap()
    }
    /// Lock `surface` and return its descriptor's flags, width and address.
    fn locked(&mut self, surface: u32) -> (u32, u32, u32) {
        self.memory.put_u32(512, 124).unwrap();
        assert_eq!(self.dd(dd::LOCK, &[surface, 0, 512, 0, 0]), 0);
        assert_eq!(self.dd(dd::UNLOCK, &[surface, 0]), 0);
        let word = |f: &Self, at: u32| f.memory.u32(512 + at).unwrap();
        (word(self, 4), word(self, 12), word(self, 36))
    }
}

// spec: GFX-DDRAW-16
#[test]
fn flip_chains_rotate_memory_and_mipmap_levels_halve() {
    let mut f = Fixture::new();
    assert_eq!(f.dd(dd::COOPERATIVE, &[f.device, 7, 0x11]), 0);
    // A primary with two back buffers forms a ring back to the front.
    let front = f.create(&[(4, 0x21), (20, 2), (104, 0x218)]);
    let first = f.attached(front, 4);
    let second = f.attached(first, 0x10);
    assert_eq!(f.attached(second, 0x10), front);
    assert_eq!(f.dd(dd::RELEASE, &[front]), 1);
    let memory = [front, first, second].map(|s| f.locked(s).2);
    assert_eq!(f.dd(dd::FLIP, &[front, 0, 1]), 0);
    let flipped = [front, first, second].map(|s| f.locked(s).2);
    assert_eq!(flipped, [memory[1], memory[2], memory[0]]);
    assert_eq!(f.dd(dd::FLIP, &[front, second, 1]), 0);
    assert_eq!(f.locked(front).2, memory[0]);
    assert_eq!(f.locked(second).2, memory[1]);
    assert_eq!(f.dd(dd::FLIP, &[front, front, 1]), 0x8876_0246);
    assert_eq!(f.dd(dd::FLIP, &[first, 0, 1]), 0x8876_0246);
    for surface in [first, second] {
        assert_eq!(f.dd(dd::RELEASE, &[surface]), 1);
    }
    assert_eq!(f.dd(dd::RELEASE, &[front]), 0);
    assert_eq!(f.objects.object_count(), 1);

    // Three mipmap levels, each half the last and owned by the top level.
    let top = f.create(&[(4, 0x2_0007), (8, 8), (12, 8), (104, 0x40_1808), (24, 3)]);
    let level = f.attached(top, 0x40_0000);
    assert_eq!(f.locked(level).1, 4);
    let last = f.attached(level, 0x40_0000);
    assert_eq!(f.locked(last).1, 2);
    f.memory.write(256, &[0; 16]).unwrap();
    f.memory.put_u32(256, 0x40_0000).unwrap();
    assert_eq!(f.dd(dd::GET_ATTACHED, &[last, 256, 32]), 0x8876_00ff);
    assert_eq!(f.memory.u32(32).unwrap(), 0);
    for surface in [level, last] {
        assert_eq!(f.dd(dd::RELEASE, &[surface]), 1);
    }
    assert_eq!(f.dd(dd::RELEASE, &[top]), 0);
    assert_eq!(f.objects.object_count(), 1);
}

// spec: GFX-DDRAW-16
#[test]
fn attachments_and_client_memory_follow_windows_rules() {
    let mut f = Fixture::new();
    let (plain, _, _) = f.surface(4, 4, PixelFormat::Bgra32, false);
    let (other, _, _) = f.surface(4, 4, PixelFormat::Bgra32, false);
    // DirectDraw7 attaches only Z buffers of the parent's size.
    assert_eq!(f.dd(dd::ADD_ATTACHED, &[plain, other]), 0x8876_000a);
    let small = f.create(&[(4, 7), (8, 2), (12, 2), (104, 0x2_0000)]);
    assert_eq!(f.dd(dd::ADD_ATTACHED, &[plain, small]), 0x8876_000a);
    let z = f.create(&[(4, 7), (8, 4), (12, 4), (104, 0x2_0000)]);
    assert_eq!(f.dd(dd::ADD_ATTACHED, &[plain, z]), 0);
    assert_eq!(f.dd(dd::ADD_ATTACHED, &[plain, z]), 0x8876_019a);
    assert_eq!(f.attached(plain, 0x2_0000), z);
    assert_eq!(f.dd(dd::RELEASE, &[z]), 2);
    assert_eq!(f.dd(dd::DELETE_ATTACHED, &[plain, 0, 0]), 0x8876_0014);
    assert_eq!(f.dd(dd::DELETE_ATTACHED, &[plain, 0, small]), 0x8876_0014);
    assert_eq!(f.dd(dd::RELEASE, &[z]), 1);
    assert_eq!(f.dd(dd::DELETE_ATTACHED, &[plain, 0, z]), 0);
    assert_eq!(f.dd(dd::ADDREF, &[z]), 0x8007_0057);

    // A client-memory surface locks at that memory but never reports it.
    let client = f.create(&[
        (4, 0x180f),
        (8, 2),
        (12, 2),
        (16, 8),
        (36, 0x1_0000),
        (76, 0x40),
        (84, 32),
        (88, 0xff_0000),
        (92, 0xff00),
        (96, 0xff),
        (104, 0x840),
    ]);
    let (flags, _, address) = f.locked(client);
    assert_eq!((flags & 0x800, address), (0, 0x1_0000));
    // SetSurfaceDesc moves it to other client memory and layout.
    f.memory.write(64, &[0; 124]).unwrap();
    for (at, value) in [(0, 124), (4, 0x80c), (12, 3), (16, 16), (36, 0x1_8000)] {
        f.memory.put_u32(64 + at, value).unwrap();
    }
    assert_eq!(f.dd(dd::SET_SURFACE_DESC, &[client, 64, 1]), 0x8007_0057);
    assert_eq!(f.dd(dd::SET_SURFACE_DESC, &[client, 64, 0]), 0);
    assert_eq!(f.locked(client), (0x100f, 3, 0x1_8000));
    // Video-memory surfaces keep their storage.
    assert_eq!(f.dd(dd::SET_SURFACE_DESC, &[plain, 64, 0]), 0x8876_0250);
    for surface in [client, small, other, plain] {
        assert_eq!(f.dd(dd::RELEASE, &[surface]), 0);
    }
    assert_eq!(f.objects.object_count(), 1);
}

#[test]
fn surface_versions_and_clipper_replacement_release_their_retained_objects() {
    let mut f = Fixture::new();
    let (surface, _, _) = f.surface(2, 2, PixelFormat::Bgra32, false);
    let old_iid = [
        0x81, 0xdb, 0x14, 0x6c, 0x33, 0xa7, 0xce, 0x11, 0xa5, 0x21, 0, 0x20, 0xaf, 0x0b, 0xe5, 0x60,
    ];
    let old = f.query(surface, old_iid);
    let unknown = f.query(old, adv_os::com::IUNKNOWN);
    assert_eq!(unknown, surface);
    assert_eq!(f.dd(dd::RELEASE, &[unknown]), 1);
    assert_eq!(f.dd(dd::RELEASE, &[old]), 0);
    assert_eq!(f.dd(dd::CREATE_CLIPPER, &[f.device, 0, 32, 0]), 0);
    let clipper = f.memory.u32(32).unwrap();
    assert_eq!(f.dd(dd::SET_CLIPPER, &[surface, clipper]), 0);
    assert_eq!(f.dd(dd::RELEASE, &[clipper]), 1);
    assert_eq!(f.dd(dd::SET_CLIPPER, &[surface, clipper]), 0);
    assert_eq!(f.dd(dd::SET_CLIPPER, &[surface, 0]), 0);
    assert_eq!(f.dd(dd::ADDREF, &[clipper]), 0x80070057);
    assert_eq!(f.objects.object_count(), 2);
    assert_eq!(f.dd(dd::RELEASE, &[surface]), 0);
    assert_eq!(f.objects.object_count(), 1);
}

#[test]
fn directdraw_scaled_blt_remaps_the_clip_and_requires_a_source_color_key() {
    let mut f = Fixture::new();
    let (source, pixels, pitch) = f.surface(5, 4, PixelFormat::Bgra32, false);
    let (primary, _, _) = f.surface(5, 4, PixelFormat::Bgra32, true);
    for y in 0..4 {
        for x in 0..5 {
            f.memory
                .write(
                    pixels + y * pitch + x * 4,
                    &[1 + x as u8 + y as u8 * 5, 0, 0, 0],
                )
                .unwrap();
        }
    }
    assert_eq!(f.dd(dd::CREATE_CLIPPER, &[f.device, 0, 32, 0]), 0);
    let clipper = f.memory.u32(32).unwrap();
    assert_eq!(f.dd(dd::SET_CLIPPER, &[primary, clipper]), 0);
    f.ddraw.configure_clipper(clipper, (10, 20));
    f.rect(192, [0, 0, 4, 3]);
    f.rect(208, [7, 19, 13, 24]);
    assert_eq!(
        f.dd(dd::BLT, &[primary, 208, source, 192, 0x01000000, 0]),
        0
    );
    let values: Vec<_> = f
        .surfaces
        .presenter
        .bgra
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| p[0])
        .collect();
    assert_eq!(
        values,
        [
            8, 8, 9, 0, 0, 8, 8, 9, 0, 0, 8, 8, 9, 0, 0, 13, 13, 14, 0, 0
        ]
    );
    let before = f.surfaces.presenter.bgra.clone();
    // Source-keyed blits fail without a source key and leave pixels alone.
    assert_eq!(
        f.dd(dd::BLT, &[primary, 208, source, 192, 0x8000, 0]),
        0x80070057
    );
    assert_eq!(f.surfaces.presenter.bgra, before);
    assert_eq!(
        f.dd(dd::BLT_FAST, &[primary, 0, 0, source, 0, 1]),
        0x80070057
    );
    assert_eq!(f.surfaces.presenter.bgra, before);
    f.rect(208, [4, 14, 8, 17]); // Complete clip: no extra frame boundary.
    let frames = f.surfaces.stats.presentations;
    assert_eq!(f.dd(dd::BLT, &[primary, 208, source, 192, 0, 0]), 0);
    assert_eq!(f.surfaces.stats.presentations, frames);
}

#[test]
fn directdraw_copy_accepts_unused_effects_and_validates_them_before_pixels() {
    let mut f = Fixture::new();
    let (source, pixels, pitch) = f.surface(5, 4, PixelFormat::Bgra32, false);
    let (primary, _, _) = f.surface(5, 4, PixelFormat::Bgra32, true);
    for y in 0..4 {
        for x in 0..5 {
            f.memory
                .write(pixels + y * pitch + x * 4, &[x as u8, y as u8, 42, 0])
                .unwrap();
        }
    }
    let mut effects = [0xcc; 100];
    effects[..4].copy_from_slice(&100u32.to_le_bytes());
    f.memory.write(256, &effects).unwrap();
    for flags in [0, 0x01000000] {
        assert_eq!(f.dd(dd::BLT, &[primary, 0, source, 0, flags, 256]), 0);
        for (index, pixel) in f
            .surfaces
            .presenter
            .bgra
            .as_chunks::<4>()
            .0
            .iter()
            .enumerate()
        {
            assert_eq!(pixel, &[index as u8 % 5, index as u8 / 5, 42, 0]);
        }
    }
    let before = f.surfaces.presenter.bgra.clone();
    let frames = f.surfaces.stats.presentations;
    for size in [0u32, 4, 128, u32::MAX] {
        f.memory.put_u32(256, size).unwrap();
        assert_eq!(
            f.dd(dd::BLT, &[primary, 0, source, 0, 0x01000000, 256]),
            0x80070057
        );
    }
    for pointer in [65500, 0xdeadbeef, u32::MAX - 20] {
        assert_eq!(
            f.dd(dd::BLT, &[primary, 0, source, 0, 0x01000000, pointer]),
            0x80070057
        );
    }
    f.memory.put_u32(256, 100).unwrap();
    assert_eq!(f.dd(dd::LOCK, &[source, 0, 64, 0, 0]), 0);
    assert_eq!(
        f.dd(dd::BLT, &[primary, 0, source, 0, 0x01000000, 256]),
        0x887601ae
    );
    assert_eq!(f.surfaces.stats.presentations, frames);
    assert_eq!(f.surfaces.presenter.bgra, before);
    assert_eq!(f.dd(dd::UNLOCK, &[source, 0]), 0);
}

#[test]
fn directdraw_overlapping_scaling_and_conversion_preserve_original_pixels() {
    let mut f = Fixture::new();
    let (source, pixels, pitch) = f.surface(5, 4, PixelFormat::Rgb565, false);
    let raw = [0xf800u16, 0x7e0, 0x1f, 0xffff, 0];
    for y in 0..4 {
        for x in 0..5 {
            f.memory
                .write(pixels + y * pitch + x * 2, &raw[x as usize].to_le_bytes())
                .unwrap();
        }
        f.memory
            .write(pixels + y * pitch + 10, &[0xaa, 0xbb])
            .unwrap();
    }
    f.rect(192, [0, 0, 3, 2]);
    f.rect(208, [1, 1, 5, 4]);
    assert_eq!(f.dd(dd::BLT, &[source, 208, source, 192, 0, 0]), 0);
    for y in 1..4 {
        assert_eq!(
            f.memory.read(pixels + y * pitch, 12).unwrap().as_ref(),
            &[0, 0xf8, 0, 0xf8, 0, 0xf8, 0xe0, 7, 0x1f, 0, 0xaa, 0xbb]
        );
    }
    let (primary, _, _) = f.surface(5, 4, PixelFormat::Bgra32, true);
    assert_eq!(f.dd(dd::BLT, &[primary, 0, source, 0, 0, 0]), 0);
    let first = [
        0, 0, 255, 0, 0, 255, 0, 0, 255, 0, 0, 0, 255, 255, 255, 0, 0, 0, 0, 0,
    ];
    let changed = [
        0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 255, 0, 0, 255, 0, 0, 0,
    ];
    assert_eq!(&f.surfaces.presenter.bgra[..20], &first);
    for y in 1..4 {
        assert_eq!(&f.surfaces.presenter.bgra[y * 20..(y + 1) * 20], &changed);
    }
}

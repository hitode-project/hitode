use adv_device::surface::{PixelFormat, Snapshot, SurfaceMemory, Surfaces};
use adv_dx::ddraw::{DirectDraw, IID_DDRAW7, api as dd};
use adv_os::gdi::{DeviceContexts, WINDOW_DC};
use adv_win32::gdi32::raster::{Raster, api as gdi};
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
    contexts: DeviceContexts,
    ddraw: DirectDraw,
    raster: Raster,
    device: u32,
}
impl Fixture {
    fn new() -> Self {
        let mut f = Self::empty(adv_machine::Objects::default());
        f.memory.write(16, &IID_DDRAW7).unwrap();
        assert_eq!(f.dd(dd::DD_CREATE, &[0, 32, 16, 0]), 0);
        f.device = f.memory.u32(32).unwrap();
        f
    }
    fn empty(objects: adv_machine::Objects) -> Self {
        Self {
            objects,
            memory: Memory(vec![0; 65536]),
            surfaces: Surfaces::new(5, 4, 4096, 61440, Snapshot::default()).unwrap(),
            contexts: DeviceContexts::default(),
            ddraw: DirectDraw::new(
                adv_dx::ddraw::front::BINDINGS
                    .iter()
                    .map(|&(id, _)| (id, id * 16))
                    .chain([(dd::UNIMPLEMENTED_COM, dd::UNIMPLEMENTED_COM * 16)])
                    .collect(),
            ),
            raster: Raster::default(),
            device: 0,
        }
    }
    fn dd(&mut self, id: u32, args: &[u32]) -> u32 {
        self.ddraw
            .call(
                id,
                args,
                &mut self.surfaces,
                &mut self.contexts,
                &mut self.objects,
                &mut self.memory,
            )
            .unwrap()
    }
    fn gdi(&mut self, id: u32, args: &[u32]) -> u32 {
        match id {
            gdi::GET_DC => {
                self.surfaces.primary(&mut self.memory).unwrap();
                WINDOW_DC
            }
            gdi::CREATE_DC => self
                .raster
                .create_dc(
                    &mut self.surfaces,
                    &mut self.contexts,
                    &mut self.objects,
                    &mut self.memory,
                    args[0],
                )
                .unwrap(),
            gdi::CREATE_DIB => self
                .raster
                .create_dib(
                    &mut self.surfaces,
                    &mut self.contexts,
                    &mut self.objects,
                    &mut self.memory,
                    args[0],
                    args[1],
                    args[2],
                    args[3],
                    args[4],
                    args[5],
                )
                .unwrap(),
            gdi::SELECT => self
                .raster
                .select_bitmap(
                    &mut self.surfaces,
                    &mut self.contexts,
                    &mut self.objects,
                    &mut self.memory,
                    args[0],
                    args[1],
                )
                .unwrap(),
            gdi::DELETE_DC => self
                .raster
                .delete_dc(
                    &mut self.surfaces,
                    &mut self.contexts,
                    &mut self.objects,
                    &mut self.memory,
                    args[0],
                )
                .unwrap(),
            gdi::SET_STRETCH_MODE => self
                .raster
                .set_stretch_mode(
                    &mut self.surfaces,
                    &mut self.contexts,
                    &mut self.objects,
                    &mut self.memory,
                    args[0],
                    args[1],
                )
                .unwrap(),
            gdi::DELETE_OBJECT => self
                .raster
                .delete_bitmap(
                    &mut self.surfaces,
                    &mut self.contexts,
                    &mut self.objects,
                    &mut self.memory,
                    args[0],
                )
                .unwrap(),
            gdi::BITBLT => self
                .raster
                .bit_blt(
                    &mut self.surfaces,
                    &mut self.contexts,
                    &mut self.objects,
                    &mut self.memory,
                    args[0],
                    args[1],
                    args[2],
                    args[3],
                    args[4],
                    args[5],
                    args[6],
                    args[7],
                    args[8],
                )
                .unwrap(),
            gdi::STRETCHBLT => self
                .raster
                .stretch_blt(
                    &mut self.surfaces,
                    &mut self.contexts,
                    &mut self.objects,
                    &mut self.memory,
                    args[0],
                    args[1],
                    args[2],
                    args[3],
                    args[4],
                    args[5],
                    args[6],
                    args[7],
                    args[8],
                    args[9],
                    args[10],
                )
                .unwrap(),
            _ => panic!("unexpected raster fixture operation {id}"),
        }
    }
    fn surface(&mut self, w: u32, h: u32, format: PixelFormat, primary: bool) -> (u32, u32, u32) {
        let mut desc = [0u8; 108];
        let (r, g, b) = format.masks();
        for (offset, value) in [
            (0, 108),
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
    fn dc(&mut self, object: u32) -> u32 {
        assert_eq!(self.dd(dd::SURFACE_DC, &[object, 32]), 0);
        self.memory.u32(32).unwrap()
    }
    fn query(&mut self, object: u32, iid: [u8; 16]) -> u32 {
        self.memory.write(16, &iid).unwrap();
        assert_eq!(self.dd(dd::QUERY, &[object, 16, 32]), 0);
        self.memory.u32(32).unwrap()
    }
}

#[test]
fn indexed_dib_color_table_is_copied_and_blits_to_the_shared_primary() {
    for height in [-2i32, 2] {
        let mut f = Fixture::new();
        let dc = f.gdi(gdi::CREATE_DC, &[0]);
        let mut info = [0u8; 56];
        info[..4].copy_from_slice(&40u32.to_le_bytes());
        info[4..8].copy_from_slice(&3i32.to_le_bytes());
        info[8..12].copy_from_slice(&height.to_le_bytes());
        info[12..14].copy_from_slice(&1u16.to_le_bytes());
        info[14..16].copy_from_slice(&8u16.to_le_bytes());
        info[32..36].copy_from_slice(&4u32.to_le_bytes());
        info[40..56].copy_from_slice(&[0, 0, 0, 0, 0, 0, 255, 7, 0, 255, 0, 8, 255, 0, 0, 9]);
        f.memory.write(256, &info).unwrap();
        let bitmap = f.gdi(gdi::CREATE_DIB, &[dc, 256, 0, 32, 0, 0]);
        assert_ne!(bitmap, 0);
        let bits = f.memory.u32(32).unwrap();
        assert_eq!(f.surfaces.get(bitmap).unwrap().pitch, 4);
        let bytes = if height > 0 {
            [3, 2, 1, 0, 1, 2, 3, 0]
        } else {
            [1, 2, 3, 0, 3, 2, 1, 0]
        };
        f.memory.write(bits, &bytes).unwrap();
        // Color ownership is with the bitmap, not the caller's temporary header.
        f.memory.write(296, &[0; 16]).unwrap();
        assert_ne!(f.gdi(gdi::SELECT, &[dc, bitmap]), 0);
        assert_eq!(f.gdi(gdi::GET_DC, &[0]), WINDOW_DC);
        assert_eq!(
            f.gdi(gdi::BITBLT, &[WINDOW_DC, 0, 0, 3, 2, dc, 0, 0, 0xcc0020]),
            1
        );
        let pixels = &f.surfaces.presenter.bgra;
        assert_eq!(&pixels[..12], &[0, 0, 255, 0, 0, 255, 0, 0, 255, 0, 0, 0]);
        assert_eq!(&pixels[20..32], &[255, 0, 0, 0, 0, 255, 0, 0, 0, 0, 255, 0]);
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

#[test]
fn directdraw_versions_share_canonical_unknown_and_one_reference_lifetime() {
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
    assert_eq!(f.dd(dd::RELEASE, &[from_old]), 3);
    assert_eq!(f.dd(dd::RELEASE, &[from_new]), 2);
    assert_eq!(f.dd(dd::RELEASE, &[device]), 1);
    assert_eq!(f.dd(dd::RELEASE, &[old]), 0);
    assert_eq!(f.objects.object_count(), 0);
    assert_eq!(f.dd(dd::ADDREF, &[device]), 0x80070057);
}

#[test]
fn attached_surface_has_stable_identity_and_parent_retains_it_until_release() {
    let mut f = Fixture::new();
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
    assert_eq!(f.dd(dd::RELEASE, &[unknown]), 2);
    assert_eq!(f.dd(dd::RELEASE, &[old]), 1);
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
fn directdraw_scaled_blt_keeps_clipper_phase_and_rejects_color_key_flags() {
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
            3, 3, 4, 0, 0, 8, 8, 9, 0, 0, 8, 8, 9, 0, 0, 13, 13, 14, 0, 0
        ]
    );
    let before = f.surfaces.presenter.bgra.clone();
    assert_eq!(
        f.dd(dd::BLT, &[primary, 208, source, 192, 0x8000, 0]),
        0x80004001
    );
    assert_eq!(f.surfaces.presenter.bgra, before);
    assert_eq!(
        f.dd(dd::BLT_FAST, &[primary, 0, 0, source, 0, 1]),
        0x80004001
    );
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

#[test]
fn gdi_stretch_modes_mirrors_and_clipping_use_the_shared_pixel_store() {
    let mut f = Fixture::new();
    let (source, pixels, _) = f.surface(4, 1, PixelFormat::Bgra32, false);
    f.memory
        .write(pixels, &[1, 0, 0, 0, 2, 0, 0, 0, 4, 0, 0, 0, 8, 0, 0, 0])
        .unwrap();
    f.gdi(gdi::GET_DC, &[0]);
    let dc = f.dc(source);
    assert_eq!(f.gdi(gdi::SET_STRETCH_MODE, &[WINDOW_DC, 3]), 1);
    assert_eq!(
        f.gdi(
            gdi::STRETCHBLT,
            &[
                WINDOW_DC,
                (-3i32) as u32,
                0,
                6,
                1,
                dc,
                0,
                0,
                4,
                1,
                0x00cc0020
            ]
        ),
        1
    );
    assert_eq!(
        &f.surfaces.presenter.bgra[..12],
        &[4, 0, 0, 0, 4, 0, 0, 0, 8, 0, 0, 0]
    );
    assert_eq!(
        f.gdi(
            gdi::STRETCHBLT,
            &[
                WINDOW_DC,
                4,
                0,
                (-4i32) as u32,
                1,
                dc,
                0,
                0,
                4,
                1,
                0x00cc0020
            ]
        ),
        1
    );
    assert_eq!(
        &f.surfaces.presenter.bgra[..16],
        &[8, 0, 0, 0, 4, 0, 0, 0, 2, 0, 0, 0, 1, 0, 0, 0]
    );
    assert_eq!(f.gdi(gdi::SET_STRETCH_MODE, &[WINDOW_DC, 1]), 3);
    assert_eq!(
        f.gdi(
            gdi::STRETCHBLT,
            &[WINDOW_DC, 0, 0, 2, 1, dc, 0, 0, 4, 1, 0x00cc0020]
        ),
        1
    );
    assert_eq!(&f.surfaces.presenter.bgra[..8], &[0; 8]);
    assert_eq!(f.gdi(gdi::SET_STRETCH_MODE, &[WINDOW_DC, 2]), 1);
    assert_eq!(
        f.gdi(
            gdi::STRETCHBLT,
            &[WINDOW_DC, 0, 0, 2, 1, dc, 0, 0, 4, 1, 0x00cc0020]
        ),
        1
    );
    assert_eq!(&f.surfaces.presenter.bgra[..8], &[3, 0, 0, 0, 12, 0, 0, 0]);
    assert_eq!(f.gdi(gdi::SET_STRETCH_MODE, &[WINDOW_DC, 4]), 0);
    assert_eq!(f.raster.last_error(), 87);
    let before = f.surfaces.presenter.bgra.clone();
    assert_eq!(
        f.gdi(
            gdi::STRETCHBLT,
            &[WINDOW_DC, 0, 0, 2, 1, dc, 3, 0, 4, 1, 0x00cc0020]
        ),
        0
    );
    assert_eq!(f.surfaces.presenter.bgra, before);
}

#[test]
fn failed_directdraw_dc_output_never_publishes_a_gdi_alias() {
    struct RejectOutput<'a> {
        memory: &'a mut Memory,
        reject_preflight: bool,
    }
    impl SurfaceMemory for RejectOutput<'_> {
        fn read(&self, addr: u32, len: usize) -> Result<Cow<'_, [u8]>, String> {
            self.memory.read(addr, len)
        }
        fn write(&mut self, addr: u32, bytes: &[u8]) -> Result<(), String> {
            if addr == 32 {
                Err("injected DC publication failure".into())
            } else {
                self.memory.write(addr, bytes)
            }
        }
        fn writable(&self, addr: u32, len: usize) -> Result<(), String> {
            if self.reject_preflight && addr == 32 {
                Err("injected inaccessible output".into())
            } else {
                self.memory.writable(addr, len)
            }
        }
        fn copy(&mut self, src: u32, dst: u32, len: usize) -> Result<(), String> {
            self.memory.copy(src, dst, len)
        }
    }
    let mut f = Fixture::new();
    let (surface, _, _) = f.surface(4, 2, PixelFormat::Bgra32, false);
    let before = f.objects.object_count();
    for reject_preflight in [true, false] {
        for _ in 0..8 {
            f.memory.put_u32(32, 0xcccccccc).unwrap();
            assert!(
                f.ddraw
                    .call(
                        dd::SURFACE_DC,
                        &[surface, 32],
                        &mut f.surfaces,
                        &mut f.contexts,
                        &mut f.objects,
                        &mut RejectOutput {
                            memory: &mut f.memory,
                            reject_preflight
                        }
                    )
                    .is_err()
            );
            assert_eq!(f.memory.u32(32).unwrap(), 0xcccccccc);
            assert_eq!(f.objects.object_count(), before);
            assert_eq!(f.objects.all_hosts::<adv_os::gdi::GdiObj>().count(), 0);
        }
    }
    let dc = f.dc(surface);
    assert_eq!(f.objects.all_hosts::<adv_os::gdi::GdiObj>().count(), 1);
    assert_eq!(f.dd(dd::SURFACE_RELEASE_DC, &[surface, dc]), 0);
    assert_eq!(f.objects.object_count(), before);
    assert_eq!(f.objects.all_hosts::<adv_os::gdi::GdiObj>().count(), 0);
}

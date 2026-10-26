//! Authored DIBs and memory DCs only; no window, renderer thread or game data.
use adv_device::surface::{
    BlitOptions, BlitRect, PixelFormat, Snapshot, Surface, SurfaceMemory, Surfaces,
};
use adv_os::gdi::{DeviceContexts, WINDOW_DC};
use adv_win32::gdi32::raster::Raster;
use std::{borrow::Cow, ops::Range};

const INFO: u32 = 64;
const BITS: u32 = 1536;
const COPY: u32 = 0x00cc0020;
type Coordinates = [i32; 8];

struct Memory {
    bytes: Vec<u8>,
    unreadable: Option<Range<u32>>,
}
impl SurfaceMemory for Memory {
    fn read(&self, address: u32, length: usize) -> Result<Cow<'_, [u8]>, String> {
        if self.unreadable.as_ref().is_some_and(|range| {
            address < range.end && u64::from(address) + length as u64 > u64::from(range.start)
        }) {
            return Err("protected source".into());
        }
        self.bytes
            .get(address as usize..address as usize + length)
            .map(Cow::Borrowed)
            .ok_or("read bounds".into())
    }
    fn write(&mut self, address: u32, data: &[u8]) -> Result<(), String> {
        self.writable(address, data.len())?;
        self.bytes[address as usize..address as usize + data.len()].copy_from_slice(data);
        Ok(())
    }
    fn writable(&self, address: u32, length: usize) -> Result<(), String> {
        self.bytes
            .get(address as usize..address as usize + length)
            .ok_or("write bounds")?;
        Ok(())
    }
    fn copy(&mut self, source: u32, destination: u32, length: usize) -> Result<(), String> {
        self.read(source, length)?;
        self.writable(destination, length)?;
        self.bytes.copy_within(
            source as usize..source as usize + length,
            destination as usize,
        );
        Ok(())
    }
}

#[repr(C)]
struct Info {
    header: [u32; 10],
    colors: [[u8; 4]; 256],
}
fn input(format: PixelFormat, bottom_up: bool) -> (Info, Vec<u8>) {
    let mut info = Info {
        header: [0; 10],
        colors: [[0; 4]; 256],
    };
    info.header[0] = 40;
    info.header[1] = 4;
    info.header[2] = if bottom_up { 3 } else { (-3i32) as u32 };
    info.header[3] = 1 | format.bits() << 16;
    if format == PixelFormat::Rgb565 {
        info.header[4] = 3;
        info.colors[0] = 0xf800u32.to_le_bytes();
        info.colors[1] = 0x7e0u32.to_le_bytes();
        info.colors[2] = 0x1fu32.to_le_bytes();
    }
    if format.indexed() {
        info.header[8] = format.colors() as u32;
    }
    let pitch = (format.row_bytes(4) + 3) & !3;
    let mut bits = vec![0xa5; (pitch * 3) as usize];
    for y in 0..3 {
        for x in 0..4 {
            let color = if matches!(format, PixelFormat::Rgb555 | PixelFormat::Rgb565) {
                [
                    if y == 2 { 255 } else { 0 },
                    if y == 1 { 255 } else { 0 },
                    if y == 0 || x % 2 != 0 { 255 } else { 0 },
                    0,
                ]
            } else {
                [
                    1 + y as u8 * 16 + x as u8,
                    4 + x as u8 * 17,
                    3 + y as u8 * 53,
                    0,
                ]
            };
            let index = (y * 4 + x) as usize % format.colors().max(1);
            info.colors[index] = if format.indexed() {
                [color[0], color[1], color[2], 0x99]
            } else {
                info.colors[index]
            };
            let packed = if format.indexed() {
                [index as u8, 0, 0, 0]
            } else {
                format.encode(color)
            };
            let physical = if bottom_up { 2 - y } else { y };
            let at = (physical * pitch + format.byte_offset(x)) as usize;
            if format.packed() {
                let shift = 8 - format.bits() - (x * format.bits()) % 8;
                let mask = ((1u16 << format.bits()) - 1) as u8;
                bits[at] = (bits[at] & !(mask << shift)) | (packed[0] << shift);
            } else {
                bits[at..at + format.bytes() as usize]
                    .copy_from_slice(&packed[..format.bytes() as usize]);
            }
        }
    }
    (info, bits)
}
fn info_bytes(info: &Info) -> Vec<u8> {
    info.header
        .iter()
        .flat_map(|word| word.to_le_bytes())
        .chain(info.colors.iter().flatten().copied())
        .collect()
}

struct Fixture {
    raster: Raster,
    surfaces: Surfaces<Snapshot>,
    dcs: DeviceContexts,
    objects: adv_machine::Objects,
    memory: Memory,
    destination: u32,
}
impl Fixture {
    fn new() -> Self {
        let mut memory = Memory {
            bytes: vec![0; 65536],
            unreadable: None,
        };
        let mut surfaces = Surfaces::new(8, 6, 4096, 61440, Snapshot::default()).unwrap();
        let destination = surfaces.primary(&mut memory).unwrap();
        let mut dcs = DeviceContexts::default();
        let mut objects = adv_machine::Objects::default();
        dcs.set_stretch_mode(&mut objects, WINDOW_DC, 3).unwrap();
        let mut f = Self {
            raster: Raster::default(),
            surfaces,
            dcs,
            objects,
            memory,
            destination,
        };
        f.reset();
        f
    }
    fn reset(&mut self) {
        let surface = self.surfaces.get(self.destination).unwrap();
        for y in 0..6 {
            for x in 0..8 {
                self.memory
                    .write(surface.row(x, y), &0x00555555u32.to_le_bytes())
                    .unwrap();
            }
        }
    }
    fn pixels(&self) -> Vec<u32> {
        let surface = self.surfaces.get(self.destination).unwrap();
        (0..6)
            .flat_map(|y| (0..8).map(move |x| (x, y)))
            .map(|(x, y)| self.memory.u32(surface.row(x, y)).unwrap())
            .collect()
    }
    fn run(&mut self, c: Coordinates, info: &Info, bits: &[u8]) -> Result<u32, String> {
        self.memory.write(INFO, &info_bytes(info)).unwrap();
        self.memory.write(BITS, bits).unwrap();
        self.raster.stretch_dib(
            &mut self.surfaces,
            &mut self.dcs,
            &mut self.objects,
            &mut self.memory,
            WINDOW_DC,
            c[0],
            c[1],
            c[2],
            c[3],
            c[4],
            c[5],
            c[6],
            c[7],
            BITS,
            INFO,
            0,
            COPY,
            None,
        )
    }
}

#[test]
fn typed_dib_binding_uses_generated_sdk_signature() {
    let mut registry = adv_abi::Registry::default();
    adv_win32::gdi32::raster::register(&mut registry);
    assert_eq!(
        registry.level("gdi32.dll", "StretchDIBits"),
        Some(adv_abi::ImplementationLevel::Implement)
    );
    assert!(registry.dynamic_available("gdi32.dll", "StretchDIBits"));
    assert!(registry.signature_conflicts().is_empty());
}

#[test]
fn borrowed_pixels_are_not_owned_or_copied_to_a_scratch_surface() {
    let mut f = Fixture::new();
    let (info, bits) = input(PixelFormat::Indexed8, false);
    let previous_id = f.surfaces.new_id();
    let objects = f.objects.object_count();
    for _ in 0..3 {
        assert_eq!(f.run([0, 0, 4, 3, 0, 0, 4, 3], &info, &bits).unwrap(), 3);
    }
    assert_eq!(f.surfaces.new_id(), previous_id + 1);
    assert_eq!(f.objects.object_count(), objects);
    assert_eq!(
        &f.memory.bytes[BITS as usize..BITS as usize + bits.len()],
        bits
    );
    // Identical repeats publish nothing new.
    assert_eq!(f.surfaces.presenter.frames, 1);
    assert_eq!(f.surfaces.stats.empty_damage, 2);
}

#[test]
fn differently_laid_out_aliases_preserve_all_original_source_rows() {
    let mut f = Fixture::new();
    let target = f.surfaces.get(f.destination).unwrap().clone();
    for i in 0..48 {
        f.memory
            .write(target.pixels + i * 4, &(i + 1).to_le_bytes())
            .unwrap();
    }
    let source = Surface {
        pixels: target.pixels + 4,
        width: 2,
        height: 2,
        pitch: 16,
        bottom_up: true,
        locked: None,
        version: 0,
        format: PixelFormat::Bgra32,
        palette: None,
    };
    f.surfaces
        .blit_from(
            &source,
            source.rect(),
            f.destination,
            BlitRect {
                x: 0,
                y: 0,
                w: 2,
                h: 2,
            },
            BlitOptions::default(),
            &mut f.memory,
        )
        .unwrap();
    let pixels = f.pixels();
    assert_eq!(&pixels[..2], &[6, 7]);
    assert_eq!(&pixels[8..10], &[2, 3]);
}

#[test]
fn unavailable_final_source_row_cannot_partially_modify_the_destination() {
    let mut f = Fixture::new();
    let (info, bits) = input(PixelFormat::Bgr24, true);
    let before = f.pixels();
    f.memory.unreadable = Some(BITS + 24..BITS + 36);
    assert!(f.run([0, 0, 4, 3, 0, 0, 4, 3], &info, &bits).is_err());
    assert_eq!(f.pixels(), before);
    assert_eq!(f.surfaces.presenter.frames, 0);
}

#[test]
fn unsupported_rop_usage_and_outside_source_rectangles_fail_without_drawing() {
    let mut f = Fixture::new();
    let (info, bits) = input(PixelFormat::Bgr24, false);
    let before = f.pixels();
    assert_eq!(f.run([0, 0, 4, 3, -1, 0, 4, 3], &info, &bits).unwrap(), 0);
    assert_eq!(f.raster.last_error(), 87);
    assert_eq!(f.pixels(), before);
    for (usage, rop) in [(0, 0x008800c6), (2, COPY)] {
        assert_eq!(
            f.raster
                .stretch_dib(
                    &mut f.surfaces,
                    &mut f.dcs,
                    &mut f.objects,
                    &mut f.memory,
                    WINDOW_DC,
                    0,
                    0,
                    4,
                    3,
                    0,
                    0,
                    4,
                    3,
                    BITS,
                    INFO,
                    usage,
                    rop,
                    None,
                )
                .unwrap(),
            0
        );
        assert_eq!(f.pixels(), before);
    }
}

#[cfg(windows)]
mod native {
    use super::*;
    use std::ffi::c_void;
    #[link(name = "gdi32")]
    unsafe extern "system" {
        fn CreateCompatibleDC(dc: *mut c_void) -> *mut c_void;
        fn CreateDIBSection(
            dc: *mut c_void,
            info: *const Info,
            usage: u32,
            bits: *mut *mut c_void,
            section: *mut c_void,
            offset: u32,
        ) -> *mut c_void;
        fn SelectObject(dc: *mut c_void, object: *mut c_void) -> *mut c_void;
        fn DeleteDC(dc: *mut c_void) -> i32;
        fn DeleteObject(object: *mut c_void) -> i32;
        fn SetStretchBltMode(dc: *mut c_void, mode: i32) -> i32;
        fn StretchDIBits(
            dc: *mut c_void,
            x: i32,
            y: i32,
            w: i32,
            h: i32,
            sx: i32,
            sy: i32,
            sw: i32,
            sh: i32,
            bits: *const c_void,
            info: *const Info,
            usage: u32,
            rop: u32,
        ) -> i32;
        fn GdiFlush() -> i32;
    }
    struct Destination {
        dc: *mut c_void,
        bitmap: *mut c_void,
        pixels: *mut u32,
        previous: *mut c_void,
    }
    impl Destination {
        fn new() -> Self {
            let mut info = Info {
                header: [0; 10],
                colors: [[0; 4]; 256],
            };
            info.header[0] = 40;
            info.header[1] = 8;
            info.header[2] = (-6i32) as u32;
            info.header[3] = 1 | 32 << 16;
            // All native pointers are owned by this memory DC until Drop, with
            // GdiFlush before reading pixels and no concurrent native access.
            unsafe {
                let dc = CreateCompatibleDC(std::ptr::null_mut());
                assert!(!dc.is_null());
                let mut pixels = std::ptr::null_mut();
                let bitmap = CreateDIBSection(dc, &info, 0, &mut pixels, std::ptr::null_mut(), 0);
                assert!(!bitmap.is_null());
                let previous = SelectObject(dc, bitmap);
                assert!(!previous.is_null());
                SetStretchBltMode(dc, 3);
                Self {
                    dc,
                    bitmap,
                    pixels: pixels.cast(),
                    previous,
                }
            }
        }
        fn run(&mut self, c: Coordinates, info: &Info, bits: &[u8]) -> (u32, Vec<u32>) {
            unsafe {
                GdiFlush();
                std::slice::from_raw_parts_mut(self.pixels, 48).fill(0x00555555);
                let returned = StretchDIBits(
                    self.dc,
                    c[0],
                    c[1],
                    c[2],
                    c[3],
                    c[4],
                    c[5],
                    c[6],
                    c[7],
                    bits.as_ptr().cast(),
                    info,
                    0,
                    COPY,
                );
                GdiFlush();
                (
                    returned as u32,
                    std::slice::from_raw_parts(self.pixels, 48).to_vec(),
                )
            }
        }
    }
    impl Drop for Destination {
        fn drop(&mut self) {
            unsafe {
                SelectObject(self.dc, self.previous);
                DeleteObject(self.bitmap);
                DeleteDC(self.dc);
            }
        }
    }

    #[test]
    fn empty_extent_queries_match_native_without_presentation() {
        let mut reference = Destination::new();
        let mut f = Fixture::new();
        let (info, bits) = input(PixelFormat::Bgr24, false);
        for c in [
            [0, 0, 0, 3, 0, 0, 4, 3],
            [0, 0, 4, 0, 0, 0, 4, 3],
            [0, 0, 4, 3, 0, 0, 0, 3],
            [0, 0, 4, 3, 0, 0, 4, 0],
        ] {
            let (returned, pixels) = reference.run(c, &info, &bits);
            f.reset();
            let actual = f.run(c, &info, &bits).unwrap();
            assert_eq!(actual, returned, "empty coordinates={c:?}");
            assert_eq!(f.pixels(), pixels);
            assert_eq!(f.surfaces.presenter.frames, 0);
        }
    }

    #[test]
    fn formats_origins_signed_axes_clipping_and_scaling_match_native_memory_dc() {
        let cases: &[Coordinates] = &[
            [0, 0, 4, 3, 0, 0, 4, 3],
            [0, 0, 4, 1, 0, 0, 4, 1],
            [0, 0, 4, 1, 0, 1, 4, 1],
            [0, 0, 4, 1, 0, 2, 4, 1],
            [-1, -1, 6, 4, 0, 0, 4, 3],
            [4, 0, -4, 3, 0, 0, 4, 3],
            [0, 3, 4, -3, 0, 0, 4, 3],
            [4, 3, -4, -3, 0, 0, 4, 3],
            [0, 0, 8, 6, 0, 0, 4, 3],
            [0, 0, 2, 2, 0, 0, 4, 3],
            [0, 0, 3, 3, 3, 0, -3, 3],
            [0, 0, 4, 2, 0, 3, 4, -2],
            [0, 0, 2, 1, 1, 0, 2, 1],
            [0, 0, 4, 2, 0, 0, 4, 1],
            [0, 0, 8, 1, 0, 0, 4, 1],
            // Held-out combined crop/scale and partial-height scale cases.
            [2, 1, 5, 3, 1, 1, 3, 2],
            [0, 0, 6, 4, 0, 0, 4, 2],
            [3, 0, -3, 3, 3, 0, -3, 3],
            [20, 20, 4, 3, 0, 0, 4, 3],
        ];
        let mut reference = Destination::new();
        let mut f = Fixture::new();
        for mode in [1, 2, 3] {
            unsafe {
                SetStretchBltMode(reference.dc, mode);
            }
            f.dcs
                .set_stretch_mode(&mut f.objects, WINDOW_DC, mode as u32)
                .unwrap();
            for format in [
                PixelFormat::Indexed1,
                PixelFormat::Indexed4,
                PixelFormat::Indexed8,
                PixelFormat::Rgb555,
                PixelFormat::Rgb565,
                PixelFormat::Bgr24,
                PixelFormat::Bgra32,
            ] {
                for bottom_up in [false, true] {
                    let (info, bits) = input(format, bottom_up);
                    for &c in cases {
                        let (returned, pixels) = reference.run(c, &info, &bits);
                        f.reset();
                        let actual = f.run(c, &info, &bits).unwrap();
                        assert_eq!(
                            actual, returned,
                            "return format={format:?} bottom={bottom_up} coordinates={c:?}"
                        );
                        assert_eq!(
                            f.pixels(),
                            pixels,
                            "pixels mode={mode} format={format:?} bottom={bottom_up} coordinates={c:?}"
                        );
                    }
                }
            }
        }
    }
}

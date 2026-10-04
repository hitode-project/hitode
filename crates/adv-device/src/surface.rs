//! Software surfaces in guest memory and their presentation. Graphics fronts (GDI,
//! DirectDraw) name surfaces by id and translate their calls into these operations; the
//! presenter or the frame mailbox receives every completed frame.
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::Arc;

/// Memory needed by software surfaces. PAL guests with shared storage borrow pixels;
/// other CPU backends can supply an owned buffer at this boundary.
pub trait SurfaceMemory {
    fn read(&self, addr: u32, len: usize) -> Result<Cow<'_, [u8]>, String>;
    fn write(&mut self, addr: u32, data: &[u8]) -> Result<(), String>;
    fn writable(&self, addr: u32, len: usize) -> Result<(), String>;
    /// Checked byte copy with memmove semantics when the ranges overlap.
    fn copy(&mut self, src: u32, dst: u32, len: usize) -> Result<(), String>;
    fn u32(&self, addr: u32) -> Result<u32, String> {
        Ok(u32::from_le_bytes(
            self.read(addr, 4)?.as_ref().try_into().unwrap(),
        ))
    }
    fn put_u32(&mut self, addr: u32, value: u32) -> Result<(), String> {
        self.write(addr, &value.to_le_bytes())
    }
}

pub struct FrameView<'a> {
    pub width: u32,
    pub height: u32,
    pub pitch: u32,
    pub bgra: &'a [u8],
    pub serial: u64,
}
pub trait Presenter {
    fn present(&mut self, frame: FrameView<'_>) -> Result<(), String>;
    /// Native GPU composition is a separate capability; no implicit CPU readback.
    #[cfg(feature = "gpu")]
    fn present_native(&mut self, _frame: &crate::gpu::NativeFrame) -> Result<(), String> {
        Err("presenter does not support native GPU frames".into())
    }
}

#[derive(Default)]
pub struct Snapshot {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
    pub frames: u64,
}

/// One process-local pixel/target store. Snapshot is neutral device data, so
/// all personality fronts can borrow this type without depending on a host shell.
pub type SurfaceStore = Surfaces<Snapshot>;

/// Assembly-supplied byte access. The store does not know the guest CPU type.
pub type MemoryRead<T> = for<'a> fn(&'a T, u32, usize) -> Result<Cow<'a, [u8]>, String>;
pub struct MemoryOperations<T: ?Sized> {
    pub read: MemoryRead<T>,
    pub write: fn(&mut T, u32, &[u8]) -> Result<(), String>,
    pub writable: fn(&T, u32, usize) -> Result<(), String>,
    pub copy: fn(&mut T, u32, u32, usize) -> Result<(), String>,
}
impl<T: ?Sized> Copy for MemoryOperations<T> {}
impl<T: ?Sized> Clone for MemoryOperations<T> {
    fn clone(&self) -> Self {
        *self
    }
}

/// Stack-only borrowed view: no state allocation or ownership transfer.
pub struct MemoryView<'a, T: ?Sized> {
    value: &'a mut T,
    operations: MemoryOperations<T>,
}
impl<'a, T: ?Sized> MemoryView<'a, T> {
    pub fn new(value: &'a mut T, operations: MemoryOperations<T>) -> Self {
        Self { value, operations }
    }
}
impl<T: ?Sized> SurfaceMemory for MemoryView<'_, T> {
    fn read(&self, addr: u32, len: usize) -> Result<Cow<'_, [u8]>, String> {
        (self.operations.read)(self.value, addr, len)
    }
    fn write(&mut self, addr: u32, data: &[u8]) -> Result<(), String> {
        (self.operations.write)(self.value, addr, data)
    }
    fn writable(&self, addr: u32, len: usize) -> Result<(), String> {
        (self.operations.writable)(self.value, addr, len)
    }
    fn copy(&mut self, src: u32, dst: u32, len: usize) -> Result<(), String> {
        (self.operations.copy)(self.value, src, dst, len)
    }
}
impl Presenter for Snapshot {
    fn present(&mut self, f: FrameView<'_>) -> Result<(), String> {
        self.width = f.width;
        self.height = f.height;
        self.frames = f.serial;
        self.bgra.resize((f.width * f.height * 4) as usize, 0);
        for y in 0..f.height as usize {
            let n = f.width as usize * 4;
            self.bgra[y * n..(y + 1) * n]
                .copy_from_slice(&f.bgra[y * f.pitch as usize..y * f.pitch as usize + n]);
        }
        Ok(())
    }
}

#[derive(Default, Debug)]
pub struct GraphicsStats {
    pub presentations: u64,
    pub host_captures: u64,
    pub coalesced_frames: u64,
    pub converted_pixels: u64,
    pub bytes_blitted: u64,
    pub locks: u64,
    pub unlocks: u64,
    /// Growth of the bounded text patch, rather than one allocation per string.
    pub text_scratch_growths: u64,
}

pub struct PendingFrame {
    width: u32,
    height: u32,
    pitch: u32,
    format: PixelFormat,
    palette: Option<Arc<[[u8; 4]; 256]>>,
    serial: u64,
    pixels: Vec<u8>,
    #[cfg(feature = "gpu")]
    native: Option<crate::gpu::NativeFrame>,
}
impl Default for PendingFrame {
    fn default() -> Self {
        Self {
            width: 0,
            height: 0,
            pitch: 0,
            format: PixelFormat::Bgra32,
            palette: None,
            serial: 0,
            pixels: Vec::new(),
            #[cfg(feature = "gpu")]
            native: None,
        }
    }
}
impl PendingFrame {
    fn store(&mut self, surface: &Surface, pixels: &[u8], serial: u64) {
        #[cfg(feature = "gpu")]
        {
            self.native = None;
        }
        self.width = surface.width;
        self.height = surface.height;
        self.pitch = surface.pitch;
        self.format = surface.format;
        self.palette.clone_from(&surface.palette);
        self.serial = serial;
        self.pixels.resize(pixels.len(), 0);
        if surface.bottom_up {
            let pitch = surface.pitch as usize;
            for y in 0..surface.height as usize {
                self.pixels[y * pitch..(y + 1) * pitch].copy_from_slice(
                    &pixels[(surface.height as usize - 1 - y) * pitch
                        ..(surface.height as usize - y) * pitch],
                );
            }
        } else {
            self.pixels.copy_from_slice(pixels);
        }
    }
    /// Conversion is performed only by the display consumer, or an explicit capture.
    pub fn snapshot(&self, out: &mut Snapshot) -> Result<(), String> {
        #[cfg(feature = "gpu")]
        if self.native.is_some() {
            return Err("native GPU snapshot requires an explicit asynchronous readback".into());
        }
        if self.width == 0 {
            return Ok(());
        }
        out.width = self.width;
        out.height = self.height;
        out.frames = self.serial;
        out.bgra
            .resize(self.width as usize * self.height as usize * 4, 0);
        for y in 0..self.height as usize {
            let src = &self.pixels[y * self.pitch as usize..];
            let dst = &mut out.bgra[y * self.width as usize * 4..(y + 1) * self.width as usize * 4];
            if self.format == PixelFormat::Bgra32 {
                dst.copy_from_slice(&src[..dst.len()]);
            } else {
                for (x, pixel) in dst.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                    pixel.copy_from_slice(&decode_pixel(
                        self.format,
                        self.palette.as_deref(),
                        &src[x * self.format.bytes() as usize..],
                    ));
                }
            }
        }
        Ok(())
    }
    #[cfg(feature = "gpu")]
    pub fn native(&self) -> Option<&crate::gpu::NativeFrame> {
        self.native.as_ref()
    }

    #[cfg(feature = "gpu")]
    fn store_native(&mut self, frame: crate::gpu::NativeFrame) {
        self.width = frame.image.width();
        self.height = frame.image.height();
        self.pitch = self.width * 4;
        self.format = PixelFormat::Bgra32;
        self.palette = None;
        self.serial = frame.serial;
        self.pixels.clear();
        self.native = Some(frame);
    }
}

pub type FrameProducer = adv_sync::mailbox::Producer<PendingFrame>;
pub type FrameConsumer = adv_sync::mailbox::Consumer<PendingFrame>;
pub fn frame_mailbox() -> (FrameProducer, FrameConsumer) {
    adv_sync::mailbox::channel(std::array::from_fn(|_| PendingFrame::default()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]

pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}
#[derive(Clone)]
pub struct Surface {
    /// Guest address of the first stored row.
    pub pixels: u32,
    pub width: u32,
    pub height: u32,
    pub pitch: u32,
    pub bottom_up: bool,
    /// The locked rectangle and whether the lock is read-only.
    pub locked: Option<(Rect, bool)>,
    /// Bumped by every write through the store or a writable lock.
    pub version: u64,
    pub format: PixelFormat,
    /// BGRA entries owned by this surface. Completed frames retain the old Arc
    /// when the live surface changes its table. Non-indexed formats have no table.
    pub palette: Option<Arc<[[u8; 4]; 256]>>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFormat {
    Bgra32,
    Bgr24,
    Rgb565,
    Rgb555,
    Indexed8,
}
impl PixelFormat {
    pub fn bytes(self) -> u32 {
        match self {
            Self::Bgra32 => 4,
            Self::Bgr24 => 3,
            Self::Indexed8 => 1,
            _ => 2,
        }
    }
    pub fn masks(self) -> (u32, u32, u32) {
        match self {
            Self::Bgra32 | Self::Bgr24 => (0xff0000, 0xff00, 0xff),
            Self::Rgb565 => (0xf800, 0x7e0, 0x1f),
            Self::Rgb555 => (0x7c00, 0x3e0, 0x1f),
            Self::Indexed8 => (0, 0, 0),
        }
    }
    pub fn decode(self, p: &[u8]) -> [u8; 4] {
        assert!(
            self != Self::Indexed8,
            "indexed decoding requires a surface palette"
        );
        if self == Self::Bgra32 {
            return p[..4].try_into().unwrap();
        }
        if self == Self::Bgr24 {
            return [p[0], p[1], p[2], 0];
        }
        let v = u32::from(u16::from_le_bytes(p[..2].try_into().unwrap()));
        let (r, g) = match self {
            Self::Rgb565 => ((v >> 11) & 31, ((v >> 5) & 63) * 255 / 63),
            Self::Rgb555 => ((v >> 10) & 31, ((v >> 5) & 31) * 255 / 31),
            Self::Bgra32 | Self::Bgr24 | Self::Indexed8 => unreachable!(),
        };
        [
            ((v & 31) * 255 / 31) as u8,
            g as u8,
            (r * 255 / 31) as u8,
            0,
        ]
    }
    /// Packed pixel, padded to four bytes. Only the first `bytes()` bytes are stored.
    pub fn encode(self, p: [u8; 4]) -> [u8; 4] {
        assert!(
            self != Self::Indexed8,
            "indexed encoding requires a surface palette"
        );
        if matches!(self, Self::Bgra32 | Self::Bgr24) {
            return p;
        }
        let r = u16::from(p[2]) * 31 / 255;
        let b = u16::from(p[0]) * 31 / 255;
        let value = match self {
            Self::Rgb565 => r << 11 | (u16::from(p[1]) * 63 / 255) << 5 | b,
            Self::Rgb555 => r << 10 | (u16::from(p[1]) * 31 / 255) << 5 | b,
            Self::Bgra32 | Self::Bgr24 | Self::Indexed8 => unreachable!(),
        };
        let [lo, hi] = value.to_le_bytes();
        [lo, hi, 0, 0]
    }
}

const BLACK_PALETTE: [[u8; 4]; 256] = [[0; 4]; 256];

fn decode_pixel(format: PixelFormat, palette: Option<&[[u8; 4]; 256]>, raw: &[u8]) -> [u8; 4] {
    if format == PixelFormat::Indexed8 {
        palette.unwrap_or(&BLACK_PALETTE)[raw[0] as usize]
    } else {
        format.decode(raw)
    }
}

fn encode_pixel(format: PixelFormat, palette: Option<&[[u8; 4]; 256]>, pixel: [u8; 4]) -> [u8; 4] {
    if format != PixelFormat::Indexed8 {
        return format.encode(pixel);
    }
    let mut closest = 0;
    let mut distance = u32::MAX;
    for (index, color) in palette.unwrap_or(&BLACK_PALETTE).iter().enumerate() {
        let current = (0..3)
            .map(|channel| {
                let difference = i32::from(color[channel]) - i32::from(pixel[channel]);
                (difference * difference) as u32
            })
            .sum();
        if current < distance {
            closest = index as u8;
            distance = current;
            if current == 0 {
                break;
            }
        }
    }
    [closest, 0, 0, 0]
}

/// Destination bounds before clipping. Signed origins preserve the sampling phase
/// when a front clips a scaled transfer against the surface edge.
#[derive(Clone, Copy, Debug)]
pub struct BlitRect {
    pub x: i64,
    pub y: i64,
    pub w: u32,
    pub h: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Sampling {
    #[default]
    Nearest,
    /// Select the source pixel containing the destination pixel's centre.
    PixelCentre,
    /// Combine decoded color channels when reducing, selecting pixel centres
    /// on an enlarged axis. Palette indices are never combined as colors.
    AndColour,
    OrColour,
    And,
    Or,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct BlitOptions {
    pub mirror_x: bool,
    pub mirror_y: bool,
    pub sampling: Sampling,
}

fn convert_bgra(s: &Surface, input: &[u8], output: &mut Vec<u8>) {
    let stride = s.width as usize * 4;
    output.resize(stride * s.height as usize, 0);
    for (y, dst) in output.chunks_exact_mut(stride).enumerate() {
        let row = if s.bottom_up {
            s.height as usize - 1 - y
        } else {
            y
        };
        let src = &input[row * s.pitch as usize..];
        if s.format == PixelFormat::Bgra32 {
            dst.copy_from_slice(&src[..stride]);
        } else {
            for (pixel, raw) in dst
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip(src.chunks_exact(s.format.bytes() as usize))
            {
                pixel.copy_from_slice(&s.decode_pixel(raw));
            }
        }
    }
}
impl Surface {
    pub fn decode_pixel(&self, raw: &[u8]) -> [u8; 4] {
        decode_pixel(self.format, self.palette.as_deref(), raw)
    }
    /// Packed pixel, with indexed colors chosen by nearest RGB distance.
    /// Equal distances choose the first palette entry; alpha is not compared.
    pub fn encode_pixel(&self, pixel: [u8; 4]) -> [u8; 4] {
        encode_pixel(self.format, self.palette.as_deref(), pixel)
    }
    pub fn rect(&self) -> Rect {
        Rect {
            x: 0,
            y: 0,
            w: self.width,
            h: self.height,
        }
    }
    pub fn row(&self, x: u32, y: u32) -> u32 {
        self.pixels
            + (if self.bottom_up {
                self.height - 1 - y
            } else {
                y
            }) * self.pitch
            + x * self.format.bytes()
    }
}

struct Arena {
    free: Vec<(u32, u32)>,
}
impl Arena {
    fn new(start: u32, len: u32) -> Result<Self, String> {
        start.checked_add(len).ok_or("arena overflow")?;
        if !start.is_multiple_of(16) || len == 0 {
            return Err("arena must be aligned and nonempty".into());
        }
        Ok(Self {
            free: vec![(start, len)],
        })
    }
    fn alloc(&mut self, n: u32, m: &mut dyn SurfaceMemory) -> Result<u32, String> {
        let n = n.checked_add(15).ok_or("allocation overflow")? & !15;
        let index = self
            .free
            .iter()
            .position(|(_, len)| *len >= n)
            .ok_or("surface arena exhausted")?;
        let (p, len) = self.free[index];
        m.writable(p, n as usize)?;
        self.free[index] = (p + n, len - n);
        if len == n {
            self.free.remove(index);
        }
        Ok(p)
    }
    fn release(&mut self, p: u32, n: u32) {
        self.free.push((p, (n + 15) & !15));
        self.free.sort_unstable();
        let mut merged: Vec<(u32, u32)> = Vec::new();
        for &(p, n) in &self.free {
            if let Some(last) = merged.last_mut()
                && last.0 + last.1 == p
            {
                last.1 += n;
                continue;
            }
            merged.push((p, n));
        }
        self.free = merged;
    }
}

pub struct Surfaces<P: Presenter> {
    pub presenter: P,
    pub stats: GraphicsStats,
    /// Display size: the primary surface's size.
    pub width: u32,
    pub height: u32,
    /// Format of the primary surface and of surfaces that do not name one.
    pub display_format: PixelFormat,
    arena: Arena,
    /// Next id; surfaces and device contexts share the numbering.
    next: u32,
    surfaces: BTreeMap<u32, Surface>,
    #[cfg(feature = "gpu")]
    native_surfaces: BTreeMap<u32, NativeSurface>,
    primary: Option<u32>,
    deferred_presentation: bool,
    pending_frame: Option<PendingFrame>,
    captured_serial: u64,
    conversion: Vec<u8>,
    blit_row: Vec<u8>,
    blit_sample: Vec<u8>,
    blit_source: Vec<u8>,
    text_patch: Vec<u8>,
    frame_mailbox: Option<(FrameProducer, adv_sync::WakeEvent)>,
}

#[cfg(feature = "gpu")]
struct NativeSurface {
    target: crate::gpu::Target,
    spares: Vec<crate::gpu::Target>,
}
#[cfg(feature = "gpu")]
const MAX_ROTATION_TARGETS: usize = 8;

impl<P: Presenter> Surfaces<P> {
    /// A store whose pixels and front objects live in the guest range `arena..arena + len`.
    pub fn new(
        width: u32,
        height: u32,
        arena: u32,
        len: u32,
        presenter: P,
    ) -> Result<Self, String> {
        size(width, height)?;
        Ok(Self {
            presenter,
            stats: GraphicsStats::default(),
            width,
            height,
            display_format: PixelFormat::Bgra32,
            arena: Arena::new(arena, len)?,
            next: 16,
            surfaces: BTreeMap::new(),
            #[cfg(feature = "gpu")]
            native_surfaces: BTreeMap::new(),
            primary: None,
            deferred_presentation: false,
            pending_frame: None,
            captured_serial: 0,
            conversion: Vec::new(),
            blit_row: Vec::new(),
            blit_sample: Vec::new(),
            blit_source: Vec::new(),
            text_patch: Vec::new(),
            frame_mailbox: None,
        })
    }

    /// A fresh id for a surface or a device context.
    pub fn new_id(&mut self) -> u32 {
        let id = self.next;
        self.next += 1;
        id
    }

    pub fn get(&self, id: u32) -> Option<&Surface> {
        self.surfaces.get(&id)
    }

    pub fn get_mut(&mut self, id: u32) -> Option<&mut Surface> {
        self.surfaces.get_mut(&id)
    }

    pub fn set_palette(
        &mut self,
        id: u32,
        start: u32,
        entries: &[[u8; 4]],
    ) -> Result<usize, String> {
        let surface = self
            .surfaces
            .get_mut(&id)
            .ok_or("unknown palette surface")?;
        if surface.format != PixelFormat::Indexed8 {
            return Err("surface is not indexed".into());
        }
        let start = start.min(256) as usize;
        let count = entries.len().min(256 - start);
        if count != 0 {
            let table = surface
                .palette
                .get_or_insert_with(|| Arc::new(BLACK_PALETTE));
            Arc::make_mut(table)[start..start + count].copy_from_slice(&entries[..count]);
            surface.version = surface.version.wrapping_add(1);
        }
        Ok(count)
    }

    pub fn palette_entries(&self, id: u32, start: u32, count: u32) -> Option<&[[u8; 4]]> {
        let surface = self.surfaces.get(&id)?;
        if surface.format != PixelFormat::Indexed8 {
            return None;
        }
        let start = start.min(256) as usize;
        let count = (count as usize).min(256 - start);
        Some(&surface.palette.as_deref().unwrap_or(&BLACK_PALETTE)[start..start + count])
    }

    pub fn contains(&self, id: u32) -> bool {
        self.surfaces.contains_key(&id)
    }

    /// The primary surface, if one exists yet.
    pub fn primary_id(&self) -> Option<u32> {
        self.primary
    }

    /// Delete surface `id` and free its pixels.
    pub fn delete(&mut self, id: u32) -> bool {
        let Some(s) = self.surfaces.remove(&id) else {
            return false;
        };
        #[cfg(feature = "gpu")]
        if self.native_surfaces.remove(&id).is_some() {
            return true;
        }
        self.arena.release(s.pixels, s.pitch * s.height);
        true
    }

    /// Guest memory from the arena for front objects (COM objects, vtables).
    pub fn alloc(&mut self, n: u32, m: &mut dyn SurfaceMemory) -> Result<u32, String> {
        self.arena.alloc(n, m)
    }

    pub fn release(&mut self, p: u32, n: u32) {
        self.arena.release(p, n);
    }

    pub fn set_deferred_presentation(&mut self, enabled: bool) -> Result<(), String> {
        if !enabled {
            self.capture_latest()?;
        }
        self.deferred_presentation = enabled;
        Ok(())
    }

    pub fn has_pending_frame(&self) -> bool {
        self.frame_mailbox
            .as_ref()
            .and_then(|(mailbox, _)| mailbox.latest())
            .or(self.pending_frame.as_ref())
            .as_ref()
            .is_some_and(|f| f.serial != self.captured_serial)
    }
    #[cfg(feature = "gpu")]
    pub fn latest_native_frame(&self) -> Option<&crate::gpu::NativeFrame> {
        self.frame_mailbox
            .as_ref()
            .and_then(|(mailbox, _)| mailbox.latest())
            .or(self.pending_frame.as_ref())
            .and_then(|frame| frame.native.as_ref())
    }
    /// Commit an explicit diagnostic readback, never used by normal native
    /// presentation. A newer frame invalidates this capture instead of mixing it.
    #[cfg(feature = "gpu")]
    pub fn capture_native_pixels(
        &mut self,
        serial: u64,
        width: u32,
        height: u32,
        bgra: &[u8],
    ) -> Result<bool, String> {
        let Some(frame) = self.latest_native_frame() else {
            return Ok(false);
        };
        if frame.serial != serial {
            return Ok(false);
        }
        if frame.image.width() != width
            || frame.image.height() != height
            || bgra.len() != width as usize * height as usize * 4
        {
            return Err("native capture dimensions or byte length mismatch".into());
        }
        self.presenter.present(FrameView {
            width,
            height,
            pitch: width * 4,
            bgra,
            serial,
        })?;
        self.captured_serial = serial;
        self.stats.host_captures += 1;
        Ok(true)
    }

    /// Materialize the newest complete frame once, including after the guest stops
    /// producing frames. Raw pixels are owned, so later guest writes or surface
    /// release cannot turn this into a partially drawn frame.
    pub fn capture_latest(&mut self) -> Result<bool, String> {
        let Some(frame) = self
            .frame_mailbox
            .as_ref()
            .and_then(|(mailbox, _)| mailbox.latest())
            .or(self.pending_frame.as_ref())
            .filter(|f| f.serial != self.captured_serial)
        else {
            return Ok(false);
        };
        #[cfg(feature = "gpu")]
        if let Some(native) = &frame.native {
            self.presenter.present_native(native)?;
            self.captured_serial = frame.serial;
            self.stats.host_captures += 1;
            return Ok(true);
        }
        let (pixels, pitch) = if frame.format == PixelFormat::Bgra32 {
            (frame.pixels.as_slice(), frame.pitch)
        } else {
            self.conversion
                .resize(frame.width as usize * frame.height as usize * 4, 0);
            for y in 0..frame.height as usize {
                for x in 0..frame.width as usize {
                    let at = y * frame.pitch as usize + x * frame.format.bytes() as usize;
                    self.conversion[(y * frame.width as usize + x) * 4
                        ..(y * frame.width as usize + x + 1) * 4]
                        .copy_from_slice(&decode_pixel(
                            frame.format,
                            frame.palette.as_deref(),
                            &frame.pixels[at..],
                        ));
                }
            }
            self.stats.converted_pixels += u64::from(frame.width) * u64::from(frame.height);
            (self.conversion.as_slice(), frame.width * 4)
        };
        self.presenter.present(FrameView {
            width: frame.width,
            height: frame.height,
            pitch,
            bgra: pixels,
            serial: frame.serial,
        })?;
        self.captured_serial = frame.serial;
        self.stats.host_captures += 1;
        Ok(true)
    }
    pub fn live_surfaces(&self) -> usize {
        self.surfaces.len()
    }
    pub fn set_frame_mailbox(&mut self, mailbox: FrameProducer, wake: adv_sync::WakeEvent) {
        self.frame_mailbox = Some((mailbox, wake));
        self.deferred_presentation = true;
    }
    /// Resize the display; an existing primary surface keeps its overlapping pixels.
    pub fn configure_window(
        &mut self,
        width: u32,
        height: u32,
        m: &mut dyn SurfaceMemory,
    ) -> Result<(), String> {
        size(width, height)?;
        self.width = width;
        self.height = height;
        let Some(primary) = self.primary else {
            return Ok(());
        };
        let old = self.surfaces[&primary].clone();
        if old.width == width && old.height == height {
            return Ok(());
        }
        if old.locked.is_some() {
            return Err("primary surface is locked during window resize".into());
        }
        let new = self.create(width, height, false, old.format, m)?;
        self.surfaces
            .get_mut(&new)
            .unwrap()
            .palette
            .clone_from(&old.palette);
        let rect = Rect {
            x: 0,
            y: 0,
            w: old.width.min(width),
            h: old.height.min(height),
        };
        self.copy(primary, rect, new, rect, m)?;
        let surface = self.surfaces.remove(&new).unwrap();
        self.surfaces.insert(primary, surface);
        self.arena.release(old.pixels, old.pitch * old.height);
        Ok(())
    }
    /// A zeroed surface; returns its id.
    pub fn create(
        &mut self,
        w: u32,
        h: u32,
        bottom_up: bool,
        format: PixelFormat,
        m: &mut dyn SurfaceMemory,
    ) -> Result<u32, String> {
        size(w, h)?;
        let pitch = (w * format.bytes() + 3) & !3;
        let bytes = pitch.checked_mul(h).ok_or("surface overflow")?;
        let p = self.arena.alloc(bytes, m)?;
        // Reused storage must not expose pixels from a previously deleted object.
        let zero = [0u8; 4096];
        let mut offset = 0;
        while offset < bytes {
            let n = (bytes - offset).min(4096);
            m.write(p + offset, &zero[..n as usize])?;
            offset += n;
        }
        let id = self.new_id();
        self.surfaces.insert(
            id,
            Surface {
                pixels: p,
                width: w,
                height: h,
                pitch,
                bottom_up,
                locked: None,
                version: 0,
                format,
                palette: (format == PixelFormat::Indexed8).then(|| Arc::new(BLACK_PALETTE)),
            },
        );
        Ok(id)
    }
    /// The primary (display) surface, created on first use in the display size and format.
    pub fn primary(&mut self, m: &mut dyn SurfaceMemory) -> Result<u32, String> {
        if let Some(id) = self.primary {
            return Ok(id);
        }
        let id = self.create(self.width, self.height, false, self.display_format, m)?;
        self.primary = Some(id);
        Ok(id)
    }
    /// Hand surface `id` to the frame mailbox, the pending frame or the presenter.
    pub fn present(&mut self, id: u32, m: &dyn SurfaceMemory) -> Result<(), String> {
        #[cfg(feature = "gpu")]
        if self.native_surfaces.contains_key(&id) {
            return Err("native surface requires GPU presentation".into());
        }
        let s = &self.surfaces[&id];
        self.stats.presentations += 1;
        let pixels = m.read(s.pixels, (s.pitch * s.height) as usize)?;
        if let Some((mailbox, wake)) = &mut self.frame_mailbox {
            let overwritten = mailbox.publish(|frame| {
                frame.store(s, pixels.as_ref(), self.stats.presentations);
            });
            self.stats.coalesced_frames += u64::from(overwritten);
            if !overwritten {
                wake.signal();
            }
            return Ok(());
        }
        if self.deferred_presentation {
            if self
                .pending_frame
                .as_ref()
                .is_some_and(|f| f.serial != self.captured_serial)
            {
                self.stats.coalesced_frames += 1;
            }
            self.pending_frame
                .get_or_insert_with(PendingFrame::default)
                .store(s, pixels.as_ref(), self.stats.presentations);
            return Ok(());
        }
        self.stats.host_captures += 1;
        self.captured_serial = self.stats.presentations;
        if s.format != PixelFormat::Bgra32 || s.bottom_up {
            convert_bgra(s, pixels.as_ref(), &mut self.conversion);
            if s.format != PixelFormat::Bgra32 {
                self.stats.converted_pixels += u64::from(s.width) * u64::from(s.height);
            }
            return self.presenter.present(FrameView {
                width: s.width,
                height: s.height,
                pitch: s.width * 4,
                bgra: &self.conversion,
                serial: self.stats.presentations,
            });
        }
        self.presenter.present(FrameView {
            width: s.width,
            height: s.height,
            pitch: s.pitch,
            bgra: &pixels,
            serial: self.stats.presentations,
        })
    }

    /// A GPU-backed entry in the same surface-id space. Its pixels are not mapped
    /// in guest memory until the API frontend explicitly requests a lock/readback.
    #[cfg(feature = "gpu")]
    pub fn create_native(
        &mut self,
        gpu: &crate::gpu::Gpu,
        width: u32,
        height: u32,
        depth: bool,
    ) -> Result<u32, String> {
        size(width, height)?;
        let target = gpu
            .target(width, height, depth)
            .map_err(|error| error.to_string())?;
        self.insert_native(target)
    }

    /// Adopt an already created device target into the one shared id space.
    #[cfg(feature = "gpu")]
    pub fn insert_native(&mut self, target: crate::gpu::Target) -> Result<u32, String> {
        let width = target.image.width();
        let height = target.image.height();
        size(width, height)?;
        let id = self.new_id();
        self.surfaces.insert(
            id,
            Surface {
                pixels: 0,
                width,
                height,
                pitch: width * 4,
                bottom_up: false,
                locked: None,
                version: 0,
                format: PixelFormat::Bgra32,
                palette: None,
            },
        );
        self.native_surfaces.insert(
            id,
            NativeSurface {
                target,
                spares: Vec::new(),
            },
        );
        Ok(id)
    }

    #[cfg(feature = "gpu")]
    pub fn native_target(&self, id: u32) -> Option<&crate::gpu::Target> {
        self.native_surfaces.get(&id).map(|surface| &surface.target)
    }

    #[cfg(feature = "gpu")]
    pub fn native_pool_size(&self, id: u32) -> Option<usize> {
        self.native_surfaces
            .get(&id)
            .map(|surface| surface.spares.len() + 1)
    }

    /// Reset/replace storage while preserving the logical surface id. All old
    /// frames retain their own images; their pixels cannot observe the new target.
    #[cfg(feature = "gpu")]
    pub fn replace_native(
        &mut self,
        id: u32,
        target: crate::gpu::Target,
    ) -> Result<crate::gpu::Target, String> {
        let native = self
            .native_surfaces
            .get_mut(&id)
            .ok_or("unknown native surface")?;
        let surface = self.surfaces.get_mut(&id).unwrap();
        if surface.locked.is_some() {
            return Err("native surface is locked during replacement".into());
        }
        surface.width = target.image.width();
        surface.height = target.image.height();
        surface.pitch = surface.width * 4;
        surface.version = surface.version.wrapping_add(1);
        native.spares.clear();
        Ok(std::mem::replace(&mut native.target, target))
    }

    /// Reuse the common presentation serial, coalescing and ownership mailbox.
    #[cfg(feature = "gpu")]
    pub fn present_native(&mut self, id: u32, gpu: &mut crate::gpu::Gpu) -> Result<(), String> {
        let image = &self
            .native_surfaces
            .get(&id)
            .ok_or("unknown native surface")?
            .target
            .image;
        let serial = self.stats.presentations + 1;
        let frame = gpu
            .present(image, serial)
            .map_err(|error| error.to_string())?;
        self.publish_native_frame(id, frame)?;
        Ok(())
    }

    /// A discard boundary transfers the old target into the immutable frame and
    /// rotates one available target. No texture copy or Queue submit is needed.
    /// The pool is bounded; retaining all frames returns Busy before publication.
    #[cfg(feature = "gpu")]
    pub fn present_native_rotating(
        &mut self,
        id: u32,
        gpu: &mut crate::gpu::Gpu,
    ) -> Result<u64, crate::gpu::Error> {
        use crate::gpu::Error;
        let context = gpu.context().clone();
        let submission = context.submission()?;
        let native = self
            .native_surfaces
            .get_mut(&id)
            .ok_or_else(|| Error::Invalid("unknown native surface".into()))?;
        let mut reusable = native
            .spares
            .iter()
            .position(|target| target.image.reusable());
        if reusable.is_none() && native.spares.len() + 1 >= MAX_ROTATION_TARGETS {
            // Register before the second capacity check, covering release
            // between the first observation and a future Machine wait.
            gpu.wait_for_frame_release();
            reusable = native
                .spares
                .iter()
                .position(|target| target.image.reusable());
            if reusable.is_none() {
                return Err(Error::Busy);
            }
        }
        // Prepare every fallible operation before changing the active target.
        let replacement = if reusable.is_none() {
            Some(
                gpu.target(
                    native.target.image.width(),
                    native.target.image.height(),
                    native.target.has_depth(),
                )?
                .with_alpha_mode(native.target.image.alpha_mode()),
            )
        } else {
            None
        };
        let serial = self
            .stats
            .presentations
            .checked_add(1)
            .ok_or_else(|| Error::Invalid("presentation serial exhausted".into()))?;
        let frame = gpu.completed_frame_under(native.target.image.clone(), serial, &submission)?;
        let replacement =
            replacement.unwrap_or_else(|| native.spares.swap_remove(reusable.unwrap()));
        if let Err(error) = self.publish_native_frame(id, frame) {
            if reusable.is_some() {
                self.native_surfaces
                    .get_mut(&id)
                    .unwrap()
                    .spares
                    .push(replacement);
            }
            return Err(Error::Invalid(error));
        }
        let native = self.native_surfaces.get_mut(&id).unwrap();
        let old = std::mem::replace(&mut native.target, replacement);
        native.spares.push(old);
        Ok(serial)
    }

    /// Commit a frame prepared by a device command producer through the same
    /// serial, ownership mailbox and capture path used by GDI/DirectDraw.
    #[cfg(feature = "gpu")]
    pub fn publish_native_frame(
        &mut self,
        id: u32,
        mut frame: crate::gpu::NativeFrame,
    ) -> Result<u64, String> {
        let native = self
            .native_surfaces
            .get(&id)
            .ok_or("unknown native surface")?;
        if !native.target.image.same_context(&frame.image)
            || (native.target.image.width(), native.target.image.height())
                != (frame.image.width(), frame.image.height())
        {
            return Err("prepared frame does not match its native surface".into());
        }
        let serial = self
            .stats
            .presentations
            .checked_add(1)
            .ok_or("presentation serial exhausted")?;
        frame.serial = serial;
        if self.frame_mailbox.is_none() && !self.deferred_presentation {
            self.presenter.present_native(&frame)?;
            self.captured_serial = serial;
            self.stats.host_captures += 1;
        }
        self.stats.presentations = serial;
        if let Some((mailbox, wake)) = &mut self.frame_mailbox {
            let overwritten = mailbox.publish(|slot| slot.store_native(frame));
            self.stats.coalesced_frames += u64::from(overwritten);
            if !overwritten {
                wake.signal();
            }
        } else if self.deferred_presentation {
            if self
                .pending_frame
                .as_ref()
                .is_some_and(|old| old.serial != self.captured_serial)
            {
                self.stats.coalesced_frames += 1;
            }
            self.pending_frame
                .get_or_insert_with(PendingFrame::default)
                .store_native(frame);
        }
        Ok(serial)
    }
    /// Draw a counted text run through the cached backend. Scratch storage is
    /// bounded and reused; no surface payload or guest object is created.
    pub fn draw_text(
        &mut self,
        id: u32,
        memory: &mut dyn SurfaceMemory,
        text: &mut crate::text::Text,
        font: &crate::text::FontRequest,
        paint: crate::text::Paint<'_>,
    ) -> Result<bool, String> {
        use crate::text::{Horizontal, Pixels, Vertical};
        #[cfg(feature = "gpu")]
        if self.native_surfaces.contains_key(&id) {
            return Err("native text target requires an explicit GPU text command".into());
        }
        let surface = self
            .surfaces
            .get(&id)
            .ok_or("unknown text surface")?
            .clone();
        if surface.format == PixelFormat::Indexed8 {
            return Err("indexed text requires explicit palette rasterization".into());
        }
        if surface.locked.is_some() {
            return Err("text surface is locked".into());
        }
        if font.baseline_angle != 0 || font.glyph_angle != 0 || font.face.starts_with('@') {
            return Err("rotated or vertical text is not supported".into());
        }
        let size = text.extent(font, paint.input).map_err(|e| e.to_string())?;
        let metrics = text.metrics(font).map_err(|e| e.to_string())?;
        let x = i64::from(paint.origin.x)
            - match paint.horizontal {
                Horizontal::Left => 0,
                Horizontal::Center => i64::from(size.width) / 2,
                Horizontal::Right => i64::from(size.width),
            };
        let y = i64::from(paint.origin.y)
            - match paint.vertical {
                Vertical::Top => 0,
                Vertical::Baseline => i64::from(metrics.ascent),
                Vertical::Bottom => i64::from(metrics.height),
            };
        // Conservative ink bounds include italics, underline and fallback glyphs.
        let mx = i64::from(metrics.maximum_width).abs().max(1) * 2;
        let my = i64::from(metrics.height).abs().max(1) * 2;
        let (mut left, mut top, mut right, mut bottom) = (
            x - mx,
            y - my,
            x + i64::from(size.width) + mx,
            y + i64::from(size.height) + my,
        );
        if let Some(rect) = paint.fill {
            left = left.min(i64::from(rect.left));
            top = top.min(i64::from(rect.top));
            right = right.max(i64::from(rect.right));
            bottom = bottom.max(i64::from(rect.bottom));
        }
        if let Some(rect) = paint.clip {
            left = left.max(i64::from(rect.left));
            top = top.max(i64::from(rect.top));
            right = right.min(i64::from(rect.right));
            bottom = bottom.min(i64::from(rect.bottom));
        }
        left = left.max(0);
        top = top.max(0);
        right = right.min(i64::from(surface.width));
        bottom = bottom.min(i64::from(surface.height));
        if left >= right || top >= bottom {
            return Ok(false);
        }
        let bounds = Rect {
            x: left as u32,
            y: top as u32,
            w: (right - left) as u32,
            h: (bottom - top) as u32,
        };
        let row = usize::try_from(bounds.w).unwrap() * surface.format.bytes() as usize;
        let pitch = (row + 3) & !3;
        let len = pitch
            .checked_mul(bounds.h as usize)
            .filter(|&n| n <= 32 * 1024 * 1024)
            .ok_or("text patch exceeds allocation limit")?;
        if len > self.text_patch.capacity() {
            self.text_patch.reserve_exact(len - self.text_patch.len());
            self.stats.text_scratch_growths += 1;
        }
        if len > self.text_patch.len() {
            self.text_patch.resize(len, 0);
        }
        let bytes = &mut self.text_patch[..len];
        for y in 0..bounds.h {
            let addr = surface.row(bounds.x, bounds.y + y);
            memory.writable(addr, row)?;
            bytes[y as usize * pitch..y as usize * pitch + row]
                .copy_from_slice(memory.read(addr, row)?.as_ref());
        }
        text.paint(
            font,
            paint,
            Pixels {
                bounds,
                pitch: pitch as u32,
                format: surface.format,
                bytes,
            },
        )
        .map_err(|e| e.to_string())?;
        let changed = (0..bounds.h).try_fold(false, |changed, y| {
            let original = memory.read(surface.row(bounds.x, bounds.y + y), row)?;
            Ok::<bool, String>(
                changed
                    || original.as_ref() != &bytes[y as usize * pitch..y as usize * pitch + row],
            )
        })?;
        if changed {
            for y in 0..bounds.h {
                memory.write(
                    surface.row(bounds.x, bounds.y + y),
                    &bytes[y as usize * pitch..y as usize * pitch + row],
                )?;
            }
            self.surfaces.get_mut(&id).unwrap().version += 1;
        }
        Ok(changed)
    }
    /// Fill a rectangle with a packed pixel in the destination's own format.
    pub fn fill_raw(
        &mut self,
        dst: u32,
        rect: Rect,
        value: u32,
        m: &mut dyn SurfaceMemory,
    ) -> Result<(), String> {
        #[cfg(feature = "gpu")]
        if self.native_surfaces.contains_key(&dst) {
            return Err("native fill requires a GPU drawing command".into());
        }
        let d = self
            .surfaces
            .get(&dst)
            .ok_or("unknown destination surface")?
            .clone();
        if rect.x.checked_add(rect.w).is_none_or(|x| x > d.width)
            || rect.y.checked_add(rect.h).is_none_or(|y| y > d.height)
        {
            return Err("destination rectangle outside surface".into());
        }
        if rect.w == 0 || rect.h == 0 {
            return Ok(());
        }
        let bytes = d.format.bytes() as usize;
        let color = value.to_le_bytes();
        self.blit_row.resize(rect.w as usize * bytes, 0);
        for pixel in self.blit_row.chunks_exact_mut(bytes) {
            pixel.copy_from_slice(&color[..bytes]);
        }
        for y in rect.y..rect.y + rect.h {
            m.write(d.row(rect.x, y), &self.blit_row)?;
        }
        self.surfaces.get_mut(&dst).unwrap().version += 1;
        Ok(())
    }

    /// Copy or scale a rectangle, converting when the formats differ.
    pub fn copy(
        &mut self,
        src: u32,
        sr: Rect,
        dst: u32,
        dr: Rect,
        m: &mut dyn SurfaceMemory,
    ) -> Result<(), String> {
        self.blit(
            src,
            sr,
            dst,
            BlitRect {
                x: i64::from(dr.x),
                y: i64::from(dr.y),
                w: dr.w,
                h: dr.h,
            },
            BlitOptions::default(),
            m,
        )
    }

    /// Scale from an in-bounds source rectangle into a destination rectangle that
    /// may extend beyond the surface. Clipping does not change the scale or phase.
    pub fn blit(
        &mut self,
        src: u32,
        sr: Rect,
        dst: u32,
        dr: BlitRect,
        options: BlitOptions,
        m: &mut dyn SurfaceMemory,
    ) -> Result<(), String> {
        #[cfg(feature = "gpu")]
        if self.native_surfaces.contains_key(&src) || self.native_surfaces.contains_key(&dst) {
            return Err("native blit requires GPU copy or explicit asynchronous readback".into());
        }
        let s = self
            .surfaces
            .get(&src)
            .ok_or("unknown source surface")?
            .clone();
        self.blit_surface(&s, sr, dst, dr, options, m)
    }

    /// Borrow external pixels for this operation only. No id, storage or guest
    /// allocation is adopted. Palette and row order use the same blit kernel.
    pub fn blit_from(
        &mut self,
        source: &Surface,
        sr: Rect,
        dst: u32,
        dr: BlitRect,
        options: BlitOptions,
        m: &mut dyn SurfaceMemory,
    ) -> Result<(), String> {
        if source.width == 0 || source.height == 0 {
            return Err("empty external surface".into());
        }
        size(source.width, source.height)?;
        let row_bytes = source
            .width
            .checked_mul(source.format.bytes())
            .ok_or("source overflow")?;
        if source.pitch < row_bytes
            || source.pixels == 0
            || (source.format == PixelFormat::Indexed8 && source.palette.is_none())
            || (source.height - 1)
                .checked_mul(source.pitch)
                .and_then(|offset| offset.checked_add(row_bytes))
                .and_then(|length| source.pixels.checked_add(length - 1))
                .is_none()
        {
            return Err("invalid external surface layout".into());
        }
        if sr
            .x
            .checked_add(sr.w)
            .is_none_or(|right| right > source.width)
            || sr
                .y
                .checked_add(sr.h)
                .is_none_or(|bottom| bottom > source.height)
        {
            return Err("source rectangle outside surface".into());
        }
        #[cfg(feature = "gpu")]
        if self.native_surfaces.contains_key(&dst) {
            return Err("native blit requires GPU copy or explicit asynchronous readback".into());
        }
        let d = self
            .surfaces
            .get(&dst)
            .ok_or("unknown destination surface")?;
        let left = dr.x.max(0);
        let top = dr.y.max(0);
        let right =
            dr.x.checked_add(i64::from(dr.w))
                .ok_or("destination overflow")?
                .min(i64::from(d.width));
        let bottom =
            dr.y.checked_add(i64::from(dr.h))
                .ok_or("destination overflow")?
                .min(i64::from(d.height));
        if right <= left || bottom <= top || sr.w == 0 || sr.h == 0 {
            return Ok(());
        }
        // Validate every source/destination row before the first write. DIB
        // padding is not part of a pixel and need not be readable or writable.
        for y in sr.y..sr.y + sr.h {
            m.read(
                source.row(sr.x, y),
                sr.w as usize * source.format.bytes() as usize,
            )?;
        }
        for y in top..bottom {
            m.writable(
                d.row(left as u32, y as u32),
                (right - left) as usize * d.format.bytes() as usize,
            )?;
        }
        self.blit_surface(source, sr, dst, dr, options, m)
    }

    fn blit_surface(
        &mut self,
        s: &Surface,
        sr: Rect,
        dst: u32,
        dr: BlitRect,
        options: BlitOptions,
        m: &mut dyn SurfaceMemory,
    ) -> Result<(), String> {
        let d = self
            .surfaces
            .get(&dst)
            .ok_or("unknown destination surface")?
            .clone();
        if sr.x.checked_add(sr.w).is_none_or(|right| right > s.width)
            || sr
                .y
                .checked_add(sr.h)
                .is_none_or(|bottom| bottom > s.height)
        {
            return Err("source rectangle outside surface".into());
        }
        let right =
            dr.x.checked_add(i64::from(dr.w))
                .ok_or("destination overflow")?
                .min(i64::from(d.width));
        let bottom =
            dr.y.checked_add(i64::from(dr.h))
                .ok_or("destination overflow")?
                .min(i64::from(d.height));
        let left = dr.x.max(0);
        let top = dr.y.max(0);
        if sr.w == 0 || sr.h == 0 || dr.w == 0 || dr.h == 0 || right <= left || bottom <= top {
            return Ok(());
        }
        let clipped = Rect {
            x: left as u32,
            y: top as u32,
            w: (right - left) as u32,
            h: (bottom - top) as u32,
        };
        let ox = (left - dr.x) as u32;
        let oy = (top - dr.y) as u32;
        let sb = s.format.bytes() as usize;
        let db = d.format.bytes() as usize;
        let unscaled = sr.w == dr.w && sr.h == dr.h;
        let same_encoding =
            s.format == d.format && (s.format != PixelFormat::Indexed8 || s.palette == d.palette);
        let colour = matches!(options.sampling, Sampling::AndColour | Sampling::OrColour);
        let sample_bytes = if colour { 4 } else { sb };
        let same_samples = same_encoding && (!colour || s.format == PixelFormat::Bgra32);
        let same_layout = s.pixels == d.pixels
            && s.pitch == d.pitch
            && s.height == d.height
            && s.bottom_up == d.bottom_up
            && s.format == d.format;
        let aliases = u64::from(s.pixels)
            < u64::from(d.pixels) + u64::from(d.pitch) * u64::from(d.height)
            && u64::from(d.pixels) < u64::from(s.pixels) + u64::from(s.pitch) * u64::from(s.height);
        let remap: Option<[u8; 256]> = (s.format == PixelFormat::Indexed8
            && d.format == PixelFormat::Indexed8
            && !same_encoding
            && !colour)
            .then(|| {
                std::array::from_fn(|index| d.encode_pixel(s.decode_pixel(&[index as u8]))[0])
            });
        if unscaled
            && !options.mirror_x
            && !options.mirror_y
            && same_encoding
            && (!aliases || same_layout)
        {
            // Logical row order is independent of top-down/bottom-up storage.
            for row in 0..clipped.h {
                let y = if same_layout && clipped.y > sr.y + oy {
                    clipped.h - 1 - row
                } else {
                    row
                };
                m.copy(
                    s.row(sr.x + ox, sr.y + oy + y),
                    d.row(clipped.x, clipped.y + y),
                    clipped.w as usize * sb,
                )?;
            }
        } else {
            // A scaled/mirrored overlap can overwrite pixels needed by a later
            // sample in either direction. Preserve the complete original rectangle.
            let overlap = aliases
                && (!same_layout
                    || (clipped.x < sr.x + sr.w
                        && sr.x < clipped.x + clipped.w
                        && clipped.y < sr.y + sr.h
                        && sr.y < clipped.y + clipped.h));
            let input_stride = sr.w as usize * sb;
            if overlap {
                self.blit_source.resize(input_stride * sr.h as usize, 0);
                for y in 0..sr.h {
                    let row = m.read(s.row(sr.x, sr.y + y), input_stride)?;
                    self.blit_source[y as usize * input_stride..(y as usize + 1) * input_stride]
                        .copy_from_slice(row.as_ref());
                }
            }
            self.blit_sample
                .resize(clipped.w as usize * sample_bytes, 0);
            if !same_samples {
                self.blit_row.resize(clipped.w as usize * db, 0);
            }
            let mut previous = None;
            for y in 0..clipped.h {
                let rows = sample_range(oy + y, sr.h, dr.h, options.sampling);
                if previous != Some(rows) {
                    self.blit_sample.fill(
                        if matches!(options.sampling, Sampling::And | Sampling::AndColour) {
                            255
                        } else {
                            0
                        },
                    );
                    for sy in rows.0..rows.1 {
                        let sy = if options.mirror_y { sr.h - 1 - sy } else { sy };
                        let input = if overlap {
                            Cow::Borrowed(
                                &self.blit_source
                                    [sy as usize * input_stride..(sy as usize + 1) * input_stride],
                            )
                        } else {
                            m.read(s.row(sr.x, sr.y + sy), input_stride)?
                        };
                        if colour {
                            self.blit_row.resize(sr.w as usize * 4, 0);
                            for (packed, decoded) in input
                                .chunks_exact(sb)
                                .zip(self.blit_row.as_chunks_mut::<4>().0)
                            {
                                *decoded = s.decode_pixel(packed);
                            }
                            sample_row(
                                &self.blit_row,
                                &mut self.blit_sample,
                                4,
                                ox,
                                sr.w,
                                dr.w,
                                options,
                            );
                        } else {
                            sample_row(
                                input.as_ref(),
                                &mut self.blit_sample,
                                sb,
                                ox,
                                sr.w,
                                dr.w,
                                options,
                            );
                        }
                    }
                    if !same_samples {
                        self.blit_row.resize(clipped.w as usize * db, 0);
                        for (input, output) in self
                            .blit_sample
                            .chunks_exact(sample_bytes)
                            .zip(self.blit_row.chunks_exact_mut(db))
                        {
                            if let Some(remap) = &remap {
                                output[0] = remap[input[0] as usize];
                            } else {
                                let pixel = if colour {
                                    input.try_into().unwrap()
                                } else {
                                    s.decode_pixel(input)
                                };
                                output.copy_from_slice(&d.encode_pixel(pixel)[..db]);
                            }
                        }
                    }
                    previous = Some(rows);
                }
                let output = if same_samples {
                    &self.blit_sample
                } else {
                    &self.blit_row
                };
                m.write(d.row(clipped.x, clipped.y + y), output)?;
            }
        }
        self.surfaces.get_mut(&dst).unwrap().version += 1;
        self.stats.bytes_blitted +=
            u64::from(clipped.w) * u64::from(clipped.h) * u64::from(d.format.bytes());
        Ok(())
    }
}

fn sample_range(at: u32, src: u32, dst: u32, sampling: Sampling) -> (u32, u32) {
    let start = if src == dst {
        at
    } else if sampling == Sampling::PixelCentre
        || (src < dst && matches!(sampling, Sampling::AndColour | Sampling::OrColour))
    {
        ((u64::from(at) * u64::from(src) + u64::from(src / 2)) / u64::from(dst)) as u32
    } else {
        (u64::from(at) * u64::from(src) / u64::from(dst)) as u32
    };
    let end = if matches!(sampling, Sampling::Nearest | Sampling::PixelCentre) || src <= dst {
        start + 1
    } else {
        (u64::from(at + 1) * u64::from(src) / u64::from(dst)) as u32
    };
    (start, end)
}

fn sample_row(
    input: &[u8],
    output: &mut [u8],
    bytes: usize,
    offset: u32,
    src_width: u32,
    dst_width: u32,
    options: BlitOptions,
) {
    if src_width == dst_width && !options.mirror_x && options.sampling == Sampling::Nearest {
        let start = offset as usize * bytes;
        output.copy_from_slice(&input[start..start + output.len()]);
        return;
    }
    for (x, pixel) in output.chunks_exact_mut(bytes).enumerate() {
        let (start, end) = sample_range(offset + x as u32, src_width, dst_width, options.sampling);
        for sx in start..end {
            let sx = if options.mirror_x {
                src_width - 1 - sx
            } else {
                sx
            } as usize
                * bytes;
            let sample = &input[sx..sx + bytes];
            match options.sampling {
                Sampling::Nearest | Sampling::PixelCentre => pixel.copy_from_slice(sample),
                Sampling::And | Sampling::AndColour => {
                    pixel.iter_mut().zip(sample).for_each(|(p, s)| *p &= s)
                }
                Sampling::Or | Sampling::OrColour => {
                    pixel.iter_mut().zip(sample).for_each(|(p, s)| *p |= s)
                }
            }
        }
    }
}

impl<P: Presenter> std::ops::Index<u32> for Surfaces<P> {
    type Output = Surface;

    fn index(&self, id: u32) -> &Surface {
        &self.surfaces[&id]
    }
}

/// Check surface dimensions; returns the byte size at 32 bits per pixel.
pub fn size(w: u32, h: u32) -> Result<u32, String> {
    if w == 0 || h == 0 || w > 16384 || h > 16384 {
        return Err("invalid surface dimensions".into());
    }
    w.checked_mul(h)
        .and_then(|n| n.checked_mul(4))
        .ok_or("surface overflow".into())
}

//! Software surfaces in guest memory and their presentation. Graphics fronts (GDI,
//! DirectDraw) name surfaces by id and translate their calls into these operations; the
//! presenter or the frame mailbox receives every completed frame.
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::Arc;

mod packed;

/// Memory needed by software surfaces. PAL guests with shared storage borrow pixels;
/// other CPU backends can supply an owned buffer at this boundary.
pub trait SurfaceMemory {
    fn read(&self, addr: u32, len: usize) -> Result<Cow<'_, [u8]>, String>;
    fn write(&mut self, addr: u32, data: &[u8]) -> Result<(), String>;
    fn writable(&self, addr: u32, len: usize) -> Result<(), String>;
    /// Checked byte copy with memmove semantics when the ranges overlap.
    fn copy(&mut self, src: u32, dst: u32, len: usize) -> Result<(), String>;
    /// SRCCOPY row comparison; clipping adapters compare only writable spans.
    fn copy_changed(&mut self, src: u32, dst: u32, len: usize) -> Result<bool, String> {
        self.writable(dst, len)?;
        if self.read(src, len)?.as_ref() == self.read(dst, len)?.as_ref() {
            return Ok(false);
        }
        self.copy(src, dst, len)?;
        Ok(true)
    }
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
    /// Regions converted during the latest snapshot, for incremental upload.
    pub damage: Damage,
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
    pub copied_bytes: u64,
    pub software_started: Option<std::time::Instant>,
    pub converted_bytes: u64,
    pub published_frames: u64,
    /// Software flushes: writer wait, message retrieval, timeout, immediate visibility.
    pub software_flushes: [u64; 4],
    pub empty_damage: u64,
    pub host_captures: u64,
    pub coalesced_frames: u64,
    pub converted_pixels: u64,
    pub bytes_blitted: u64,
    pub locks: u64,
    pub unlocks: u64,
    /// Growth of the bounded text patch, rather than one allocation per string.
    pub text_scratch_growths: u64,
    /// Damaged and total pixels summed over software publications.
    pub damaged_pixels: u64,
    pub frame_pixels: u64,
}

#[derive(Clone)]
pub struct PendingFrame {
    width: u32,
    height: u32,
    pitch: u32,
    format: PixelFormat,
    palette: Option<Arc<[[u8; 4]; 256]>>,
    serial: u64,
    pixels: Arc<Vec<u8>>,
    damage: Damage,
    bottom_up: bool,
    history: DamageHistory,
    video: Arc<[(u64, VideoLayer)]>,
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
            pixels: Arc::new(Vec::new()),
            damage: Damage::default(),
            bottom_up: false,
            history: DamageHistory::default(),
            video: Arc::from([]),
            #[cfg(feature = "gpu")]
            native: None,
        }
    }
}
impl PendingFrame {
    fn store(
        &mut self,
        surface: &Surface,
        memory: &dyn SurfaceMemory,
        serial: u64,
        history: &DamageHistory,
        damage: &Damage,
    ) -> Result<u64, String> {
        let full = surface.rect();
        let reset = self.width != surface.width
            || self.height != surface.height
            || self.pitch != surface.pitch
            || self.format != surface.format
            || Arc::get_mut(&mut self.pixels).is_none();
        let regions = if reset {
            Damage::from_rect(full)
        } else {
            damage_since(history, self.serial, serial, full)
        };
        #[cfg(feature = "gpu")]
        {
            self.native = None;
        }
        self.width = surface.width;
        self.height = surface.height;
        self.pitch = surface.pitch;
        self.format = surface.format;
        self.palette.clone_from(&surface.palette);
        self.bottom_up = surface.bottom_up;
        self.serial = 0; // A failed write invalidates this private slot's baseline.
        self.history = history.clone();
        self.damage = *damage;
        if Arc::get_mut(&mut self.pixels).is_none() {
            self.pixels = Arc::new(Vec::new());
        }
        let output = Arc::get_mut(&mut self.pixels).unwrap();
        output.resize((surface.pitch * surface.height) as usize, 0);
        let mut copied = 0;
        for r in regions.iter().copied() {
            let start = surface.format.byte_offset(r.x) as usize;
            let n = surface.span(r.x, r.w) as usize;
            for y in r.y..r.y + r.h {
                let src = memory.read(surface.row(0, y) + start as u32, n)?;
                let dst = y as usize * surface.pitch as usize + start;
                output[dst..dst + n].copy_from_slice(src.as_ref());
                copied += n as u64;
            }
        }
        if verify_software() {
            for y in 0..surface.height {
                let n = surface.format.row_bytes(surface.width) as usize;
                let src = memory.read(surface.row(0, y), n)?;
                let at = y as usize * surface.pitch as usize;
                if output[at..at + n] != *src {
                    return Err("incremental software slot differs from full storage".into());
                }
            }
        }
        self.serial = serial;
        Ok(copied)
    }
    /// Convert only changes since this consumer's last snapshot. History gaps
    /// force a full conversion; each consumer independently owns its baseline.
    pub fn snapshot(&self, out: &mut Snapshot) -> Result<(), String> {
        #[cfg(feature = "gpu")]
        if self.native.is_some() {
            return Err("native GPU snapshot requires an explicit asynchronous readback".into());
        }
        if self.width == 0 {
            return Ok(());
        }
        let full = Rect {
            x: 0,
            y: 0,
            w: self.width,
            h: self.height,
        };
        let damage = if out.width != self.width
            || out.height != self.height
            || out.bgra.len() != self.width as usize * self.height as usize * 4
            || !self.video.is_empty()
        {
            Damage::from_rect(full)
        } else {
            damage_since(&self.history, out.frames, self.serial, full)
        };
        out.width = self.width;
        out.height = self.height;
        out.bgra
            .resize(self.width as usize * self.height as usize * 4, 0);
        self.convert(out, &damage);
        self.compose_video(out)?;
        out.frames = self.serial;
        out.damage = damage;
        if verify_software() {
            let mut reference = Snapshot {
                width: self.width,
                height: self.height,
                bgra: vec![0; out.bgra.len()],
                ..Default::default()
            };
            self.convert(&mut reference, &[full]);
            self.compose_video(&mut reference)?;
            if reference.bgra != out.bgra {
                return Err("incremental software snapshot differs from full conversion".into());
            }
        }
        Ok(())
    }
    fn convert(&self, out: &mut Snapshot, damage: &[Rect]) {
        for r in damage {
            for y in r.y..r.y + r.h {
                let src = &self.pixels[y as usize * self.pitch as usize..];
                let start = (y as usize * self.width as usize + r.x as usize) * 4;
                let dst = &mut out.bgra[start..start + r.w as usize * 4];
                if self.format == PixelFormat::Bgra32 {
                    dst.copy_from_slice(&src[r.x as usize * 4..(r.x + r.w) as usize * 4]);
                } else {
                    for (i, pixel) in dst.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                        let x = r.x + i as u32;
                        let raw = if self.format.packed() {
                            &[self.format.index(src, x)][..]
                        } else {
                            &src[self.format.byte_offset(x) as usize..]
                        };
                        pixel.copy_from_slice(&decode_pixel(
                            self.format,
                            self.palette.as_deref(),
                            raw,
                        ));
                    }
                }
            }
        }
    }
    pub fn damage(&self) -> &[Rect] {
        &self.damage
    }
    pub fn extent(&self) -> (u32, u32) {
        (self.width, self.height)
    }
    pub fn serial(&self) -> u64 {
        self.serial
    }
    pub fn video_layers(&self) -> &[(u64, VideoLayer)] {
        &self.video
    }
    pub fn shared_video_layers(&self) -> Arc<[(u64, VideoLayer)]> {
        self.video.clone()
    }
    pub fn hide_cursor(&self) -> bool {
        self.video_cursor_regions()
            .is_ok_and(|regions| regions.iter().any(|region| region.hide_cursor))
    }
    pub fn video_cursor_regions(&self) -> Result<Vec<VideoCursorRegion>, String> {
        let mut regions = Vec::new();
        for (_, layer) in self.video.iter() {
            if let Some(region) = layer
                .layout(self.extent())?
                .cursor_region(layer.hide_cursor)
            {
                regions.push(region);
            }
        }
        Ok(regions)
    }
    fn compose_video(&self, out: &mut Snapshot) -> Result<(), String> {
        for (_, layer) in self.video.iter() {
            layer.compose(out)?;
        }
        Ok(())
    }
    /// Explicit native readback only: preserve the same immutable layer metadata
    /// as the GPU presentation, even when a newer video sample arrives.
    #[cfg(feature = "gpu")]
    pub fn snapshot_native(&self, bgra: &[u8], out: &mut Snapshot) -> Result<(), String> {
        if self.native.is_none() || bgra.len() != self.width as usize * self.height as usize * 4 {
            return Err("native capture dimensions or byte length mismatch".into());
        }
        out.width = self.width;
        out.height = self.height;
        out.frames = self.serial;
        out.bgra.clear();
        out.bgra.extend_from_slice(bgra);
        self.compose_video(out)
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
        if !self.pixels.is_empty() {
            self.pixels = Arc::new(Vec::new());
        }
        self.native = Some(frame);
    }
}

pub type FrameProducer = adv_sync::mailbox::Producer<PendingFrame>;
pub type FrameConsumer = adv_sync::mailbox::Consumer<PendingFrame>;
pub fn frame_mailbox() -> (FrameProducer, FrameConsumer) {
    adv_sync::mailbox::channel(std::array::from_fn(|_| PendingFrame::default()))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]

pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}
impl Rect {
    pub fn empty(self) -> bool {
        self.w == 0 || self.h == 0
    }
    pub fn union(self, other: Self) -> Self {
        if self.empty() {
            return other;
        }
        if other.empty() {
            return self;
        }
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        Self {
            x,
            y,
            w: (self.x + self.w).max(other.x + other.w) - x,
            h: (self.y + self.h).max(other.y + other.h) - y,
        }
    }
    pub fn clipped(self, bounds: Self) -> Self {
        let x = self.x.max(bounds.x);
        let y = self.y.max(bounds.y);
        Self {
            x,
            y,
            w: self
                .x
                .saturating_add(self.w)
                .min(bounds.x + bounds.w)
                .saturating_sub(x),
            h: self
                .y
                .saturating_add(self.h)
                .min(bounds.y + bounds.h)
                .saturating_sub(y),
        }
    }
}
/// The damage of the last eight native publications, each relative to the
/// publication before it. A consumer that last saw publication `n` redraws the
/// union since `n`, so frames replaced in the mailbox still count.
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameDamage {
    publication: u64,
    /// (publication, surface, damage); `None` damage means the whole frame.
    recent: [(u64, u32, Option<Rect>); 8],
}
impl FrameDamage {
    pub fn publication(&self) -> u64 {
        self.publication
    }
    fn push(&mut self, surface: u32, damage: Option<Rect>) {
        self.publication += 1;
        self.recent[(self.publication % 8) as usize] = (self.publication, surface, damage);
    }
    /// The area changed since publication `last`, or `None` for the whole frame:
    /// when `last` is unknown or too old, or another surface published since it.
    pub fn since(&self, last: Option<u64>) -> Option<Rect> {
        let last = last?;
        if last >= self.publication || self.publication - last >= 8 {
            return None;
        }
        let surface = self.recent[(self.publication % 8) as usize].1;
        let mut union = Rect::default();
        for publication in last..=self.publication {
            let (n, s, damage) = self.recent[(publication % 8) as usize];
            if n != publication || s != surface {
                return None;
            }
            if publication > last {
                union = union.union(damage?);
            }
        }
        Some(union)
    }
}

/// Bounded software damage, stored without per-publication allocation.
#[derive(Clone, Copy, Debug, Default)]
pub struct Damage {
    rects: [Rect; 4],
    len: usize,
}
impl Damage {
    fn from_rect(rect: Rect) -> Self {
        Self {
            rects: [rect, Rect::default(), Rect::default(), Rect::default()],
            len: usize::from(!rect.empty()),
        }
    }
    fn add(&mut self, mut r: Rect) {
        let mut i = 0;
        while i < self.len {
            let old = self.rects[i];
            if r.x <= old.x + old.w
                && old.x <= r.x + r.w
                && r.y <= old.y + old.h
                && old.y <= r.y + r.h
            {
                r = r.union(old);
                self.rects.copy_within(i + 1..self.len, i);
                self.len -= 1;
                i = 0;
            } else {
                i += 1;
            }
        }
        if self.len == 4 {
            *self = Self::from_rect(self.iter().copied().fold(r, Rect::union));
        } else {
            self.rects[self.len] = r;
            self.len += 1;
        }
    }
}
impl std::ops::Deref for Damage {
    type Target = [Rect];
    fn deref(&self) -> &[Rect] {
        &self.rects[..self.len]
    }
}
#[derive(Clone, Default)]
struct DamageHistory {
    entries: [(u64, Damage); 16],
    first: usize,
    len: usize,
}
impl DamageHistory {
    fn clear(&mut self) {
        self.first = 0;
        self.len = 0;
    }
    fn push(&mut self, entry: (u64, Damage)) {
        self.entries[(self.first + self.len) % 16] = entry;
        if self.len < 16 {
            self.len += 1;
        } else {
            self.first = (self.first + 1) % 16;
        }
    }
    fn iter(&self) -> impl Iterator<Item = &(u64, Damage)> {
        (0..self.len).map(|i| &self.entries[(self.first + i) % 16])
    }
}
fn verify_software() -> bool {
    static VERIFY: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *VERIFY.get_or_init(|| std::env::var_os("ADV_VERIFY_SOFTWARE_FRAMES").is_some())
}
/// Content-diff tiles (DEV-FRAME-10).
const TILE_W: u32 = 64;
const TILE_H: u32 = 16;

/// Shrinks declared damage to the tiles whose pixels differ from `previous`,
/// the complete last publication in the same layout. Downstream copies,
/// uploads, composition and upscaling then follow what actually changed.
/// A rectangle that mostly changed keeps its declared bounds.
fn refine_damage(
    surface: &Surface,
    memory: &dyn SurfaceMemory,
    previous: &[u8],
    declared: &Damage,
) -> Result<Damage, String> {
    let format = surface.format;
    let mut out = Damage::default();
    for r in declared.iter().copied() {
        if r.w < TILE_W && r.h < TILE_H {
            out.add(r);
            continue;
        }
        let total = u64::from(r.w) * u64::from(r.h);
        let mut changed = 0u64;
        let mut bands = Damage::default();
        let mut y = r.y;
        while y < r.y + r.h && changed * 4 <= total * 3 {
            let h = TILE_H.min(r.y + r.h - y);
            let (mut lo, mut hi) = (u32::MAX, 0);
            for row in y..y + h {
                let start = format.byte_offset(r.x) as usize;
                let n = surface.span(r.x, r.w) as usize;
                let src = memory.read(surface.row(0, row) + start as u32, n)?;
                let at = row as usize * surface.pitch as usize + start;
                let old = &previous[at..at + n];
                if *src == *old {
                    continue;
                }
                let mut x = r.x;
                while x < r.x + r.w {
                    let w = TILE_W.min(r.x + r.w - x);
                    if x < lo || x + w > hi {
                        let a = (format.byte_offset(x) - format.byte_offset(r.x)) as usize;
                        let b = a + surface.span(x, w) as usize;
                        if src[a..b] != old[a..b] {
                            (lo, hi) = (lo.min(x), hi.max(x + w));
                        }
                    }
                    x += w;
                }
            }
            if lo < hi {
                changed += u64::from(hi - lo) * u64::from(h);
                bands.add(Rect {
                    x: lo,
                    y,
                    w: hi - lo,
                    h,
                });
            }
            y += h;
        }
        if changed * 4 > total * 3 {
            out.add(r);
        } else {
            for band in bands.iter().copied() {
                out.add(band);
            }
        }
    }
    Ok(out)
}

fn damage_since(history: &DamageHistory, since: u64, serial: u64, full: Rect) -> Damage {
    if since == serial && since != 0 {
        return Damage::default();
    }
    if since == 0 || since > serial || history.iter().next().is_none_or(|(n, _)| since < n - 1) {
        return Damage::from_rect(full);
    }
    let mut result = Damage::default();
    for (_, damage) in history.iter().filter(|(n, _)| *n > since) {
        for &r in damage.iter() {
            result.add(r);
        }
    }
    result
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
    Indexed4,
    Indexed1,
}
impl PixelFormat {
    /// Storage unit; packed indexed formats use a byte containing several pixels.
    pub fn bytes(self) -> u32 {
        match self {
            Self::Bgra32 => 4,
            Self::Bgr24 => 3,
            Self::Indexed8 | Self::Indexed4 | Self::Indexed1 => 1,
            _ => 2,
        }
    }
    pub fn masks(self) -> (u32, u32, u32) {
        match self {
            Self::Bgra32 | Self::Bgr24 => (0xff0000, 0xff00, 0xff),
            Self::Rgb565 => (0xf800, 0x7e0, 0x1f),
            Self::Rgb555 => (0x7c00, 0x3e0, 0x1f),
            Self::Indexed8 | Self::Indexed4 | Self::Indexed1 => (0, 0, 0),
        }
    }
    pub fn decode(self, p: &[u8]) -> [u8; 4] {
        assert!(
            !self.indexed(),
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
            Self::Bgra32 | Self::Bgr24 | Self::Indexed8 | Self::Indexed4 | Self::Indexed1 => {
                unreachable!()
            }
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
            !self.indexed(),
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
            Self::Bgra32 | Self::Bgr24 | Self::Indexed8 | Self::Indexed4 | Self::Indexed1 => {
                unreachable!()
            }
        };
        let [lo, hi] = value.to_le_bytes();
        [lo, hi, 0, 0]
    }
}

const BLACK_PALETTE: [[u8; 4]; 256] = [[0; 4]; 256];

fn decode_pixel(format: PixelFormat, palette: Option<&[[u8; 4]; 256]>, raw: &[u8]) -> [u8; 4] {
    if format.indexed() {
        palette.unwrap_or(&BLACK_PALETTE)[raw[0] as usize]
    } else {
        format.decode(raw)
    }
}

fn encode_pixel(format: PixelFormat, palette: Option<&[[u8; 4]; 256]>, pixel: [u8; 4]) -> [u8; 4] {
    if !format.indexed() {
        return format.encode(pixel);
    }
    let mut closest = 0;
    let mut distance = u32::MAX;
    for (index, color) in palette.unwrap_or(&BLACK_PALETTE)[..format.colors()]
        .iter()
        .enumerate()
    {
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

/// A decoder-owned, immutable top-down BGRA image. Presentation does not own a
/// decoder or clock, and retains at most the newest image for each layer.
#[derive(Clone, Debug)]
pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    pub pitch: u32,
    pub bgra: Arc<[u8]>,
}

#[derive(Clone, Debug)]
pub struct VideoLayer {
    pub frame: VideoFrame,
    pub source: Rect,
    pub destination: BlitRect,
    /// Opaque video-window border, painted atomically before the decoded image.
    /// When supplied its rectangle covers the owner clip rectangle.
    pub background: Option<(BlitRect, [u8; 4])>,
    /// Owner client bounds, expressed in the presentation canvas.
    pub clip: Rect,
    /// Logical client extent for destination, background and clip coordinates.
    /// The completed base frame may have a different backbuffer resolution.
    pub canvas: (u32, u32),
    pub visible: bool,
    pub hide_cursor: bool,
}

/// Visible geometry in completed-frame pixels, shared by software capture and
/// the GPU presenter. UVs crop the immutable decoder image without repacking it.
#[derive(Clone, Copy, Debug)]
pub struct VideoRegion {
    pub destination: Rect,
    pub source_uv: [f32; 4],
}
#[derive(Clone, Copy, Debug)]
pub struct VideoLayout {
    pub video: Option<VideoRegion>,
    pub background: Option<(Rect, [u8; 4])>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VideoCursorRegion {
    pub rectangle: Rect,
    pub hide_cursor: bool,
}
impl VideoCursorRegion {
    /// Regions are ordered like paint commands; the uppermost window owns the
    /// cursor, including when that window explicitly leaves its cursor visible.
    pub fn hidden_at(regions: &[Self], x: i32, y: i32) -> bool {
        regions
            .iter()
            .rev()
            .find(|region| {
                let dx = i64::from(x) - i64::from(region.rectangle.x);
                let dy = i64::from(y) - i64::from(region.rectangle.y);
                dx >= 0
                    && dy >= 0
                    && dx < i64::from(region.rectangle.w)
                    && dy < i64::from(region.rectangle.h)
            })
            .is_some_and(|region| region.hide_cursor)
    }
}
impl VideoLayout {
    pub fn cursor_region(&self, hide_cursor: bool) -> Option<VideoCursorRegion> {
        self.background
            .map(|(rect, _)| rect)
            .or_else(|| self.video.map(|video| video.destination))
            .map(|rectangle| VideoCursorRegion {
                rectangle,
                hide_cursor,
            })
    }
}
impl VideoLayer {
    pub fn layout(&self, extent: (u32, u32)) -> Result<VideoLayout, String> {
        self.validate()?;
        let mapped;
        let layer = if self.canvas != extent {
            mapped = self.mapped(extent)?;
            &mapped
        } else {
            self
        };
        let video = layer.clipped_rect(extent).map(|rect| {
            let sx = f64::from(layer.source.w) / f64::from(layer.destination.w);
            let sy = f64::from(layer.source.h) / f64::from(layer.destination.h);
            VideoRegion {
                destination: rect,
                source_uv: [
                    ((f64::from(layer.source.x)
                        + (f64::from(rect.x) - layer.destination.x as f64) * sx)
                        / f64::from(layer.frame.width)) as f32,
                    ((f64::from(layer.source.y)
                        + (f64::from(rect.y) - layer.destination.y as f64) * sy)
                        / f64::from(layer.frame.height)) as f32,
                    (f64::from(rect.w) * sx / f64::from(layer.frame.width)) as f32,
                    (f64::from(rect.h) * sy / f64::from(layer.frame.height)) as f32,
                ],
            }
        });
        Ok(VideoLayout {
            video,
            background: layer.background.and_then(|(rect, color)| {
                layer.clip_rectangle(rect, extent).map(|rect| (rect, color))
            }),
        })
    }
    pub fn validate(&self) -> Result<(), String> {
        size(self.frame.width, self.frame.height)?;
        size(self.canvas.0, self.canvas.1)?;
        let row = self
            .frame
            .width
            .checked_mul(4)
            .ok_or("video row overflow")?;
        let bytes = u64::from(self.frame.pitch) * u64::from(self.frame.height - 1) + u64::from(row);
        if self.frame.pitch < row || bytes > self.frame.bgra.len() as u64 {
            return Err("invalid video frame layout".into());
        }
        if self.source.w == 0
            || self.source.h == 0
            || self
                .source
                .x
                .checked_add(self.source.w)
                .is_none_or(|x| x > self.frame.width)
            || self
                .source
                .y
                .checked_add(self.source.h)
                .is_none_or(|y| y > self.frame.height)
            || self
                .destination
                .x
                .checked_add(i64::from(self.destination.w))
                .is_none()
            || self
                .destination
                .y
                .checked_add(i64::from(self.destination.h))
                .is_none()
        {
            return Err("invalid video rectangle".into());
        }
        if let Some((rect, _)) = self.background
            && (rect.x > i64::from(self.clip.x)
                || rect.y > i64::from(self.clip.y)
                || rect
                    .x
                    .checked_add(i64::from(rect.w))
                    .is_none_or(|right| right < i64::from(self.clip.x) + i64::from(self.clip.w))
                || rect
                    .y
                    .checked_add(i64::from(rect.h))
                    .is_none_or(|bottom| bottom < i64::from(self.clip.y) + i64::from(self.clip.h)))
        {
            return Err("video background does not cover owner clip".into());
        }
        Ok(())
    }
    pub fn clipped_rect(&self, extent: (u32, u32)) -> Option<Rect> {
        self.clip_rectangle(self.destination, extent)
    }
    fn map_rectangle(&self, rectangle: BlitRect, extent: (u32, u32)) -> Result<BlitRect, String> {
        fn coordinate(value: i64, logical: u32, actual: u32) -> Result<i64, String> {
            if logical == 0 {
                return Err("empty logical video canvas".into());
            }
            i64::try_from((i128::from(value) * i128::from(actual)).div_euclid(i128::from(logical)))
                .map_err(|_| "video coordinate overflow".into())
        }
        let x = coordinate(rectangle.x, self.canvas.0, extent.0)?;
        let y = coordinate(rectangle.y, self.canvas.1, extent.1)?;
        let right = coordinate(
            rectangle
                .x
                .checked_add(i64::from(rectangle.w))
                .ok_or("video rectangle overflow")?,
            self.canvas.0,
            extent.0,
        )?;
        let bottom = coordinate(
            rectangle
                .y
                .checked_add(i64::from(rectangle.h))
                .ok_or("video rectangle overflow")?,
            self.canvas.1,
            extent.1,
        )?;
        Ok(BlitRect {
            x,
            y,
            w: u32::try_from(right - x).map_err(|_| "video width overflow")?,
            h: u32::try_from(bottom - y).map_err(|_| "video height overflow")?,
        })
    }
    fn mapped(&self, extent: (u32, u32)) -> Result<Self, String> {
        size(extent.0, extent.1)?;
        let mut layer = self.clone();
        layer.destination = self.map_rectangle(self.destination, extent)?;
        layer.background = self
            .background
            .map(|(rect, color)| self.map_rectangle(rect, extent).map(|rect| (rect, color)))
            .transpose()?;
        let clip = self.map_rectangle(
            BlitRect {
                x: i64::from(self.clip.x),
                y: i64::from(self.clip.y),
                w: self.clip.w,
                h: self.clip.h,
            },
            extent,
        )?;
        layer.clip = Rect {
            x: u32::try_from(clip.x).map_err(|_| "video clip overflow")?,
            y: u32::try_from(clip.y).map_err(|_| "video clip overflow")?,
            w: clip.w,
            h: clip.h,
        };
        layer.canvas = extent;
        Ok(layer)
    }
    fn clip_rectangle(&self, destination: BlitRect, extent: (u32, u32)) -> Option<Rect> {
        if self.canvas != extent {
            return self
                .mapped(extent)
                .ok()?
                .clip_rectangle(self.map_rectangle(destination, extent).ok()?, extent);
        }
        if !self.visible || destination.w == 0 || destination.h == 0 {
            return None;
        }
        let left = destination.x.max(i64::from(self.clip.x)).max(0);
        let top = destination.y.max(i64::from(self.clip.y)).max(0);
        let right = destination
            .x
            .checked_add(i64::from(destination.w))?
            .min(i64::from(self.clip.x) + i64::from(self.clip.w))
            .min(i64::from(self.canvas.0.min(extent.0)));
        let bottom = destination
            .y
            .checked_add(i64::from(destination.h))?
            .min(i64::from(self.clip.y) + i64::from(self.clip.h))
            .min(i64::from(self.canvas.1.min(extent.1)));
        (right > left && bottom > top).then_some(Rect {
            x: left as u32,
            y: top as u32,
            w: (right - left) as u32,
            h: (bottom - top) as u32,
        })
    }
    fn pixel(&self, x: u32, y: u32) -> &[u8] {
        // Sample pixel centres, matching the host GPU's nearest texture sampler.
        // Clipping keeps the original destination origin and sampling phase.
        let sx = u64::from(self.source.x)
            + (((i64::from(x) - self.destination.x) as u64 * 2 + 1) * u64::from(self.source.w))
                / (u64::from(self.destination.w) * 2);
        let sy = u64::from(self.source.y)
            + (((i64::from(y) - self.destination.y) as u64 * 2 + 1) * u64::from(self.source.h))
                / (u64::from(self.destination.h) * 2);
        let at = sy as usize * self.frame.pitch as usize + sx as usize * 4;
        &self.frame.bgra[at..at + 4]
    }
    fn compose(&self, out: &mut Snapshot) -> Result<(), String> {
        self.validate()?;
        if self.canvas != (out.width, out.height) {
            return self.mapped((out.width, out.height))?.compose(out);
        }
        if let Some((background, color)) = self.background
            && let Some(rect) = self.clip_rectangle(background, (out.width, out.height))
        {
            for y in rect.y..rect.y + rect.h {
                for x in rect.x..rect.x + rect.w {
                    let at = (y as usize * out.width as usize + x as usize) * 4;
                    out.bgra[at..at + 4].copy_from_slice(&color);
                }
            }
        }
        let Some(rect) = self.clipped_rect((out.width, out.height)) else {
            return Ok(());
        };
        for y in rect.y..rect.y + rect.h {
            for x in rect.x..rect.x + rect.w {
                let at = (y as usize * out.width as usize + x as usize) * 4;
                out.bgra[at..at + 4].copy_from_slice(self.pixel(x, y));
            }
        }
        Ok(())
    }
    /// The shell uploads this clipped patch through its existing opaque pipeline.
    /// CPU work touches decoded video only; a native base image stays on the GPU.
    pub fn rasterize(
        &self,
        extent: (u32, u32),
        out: &mut Snapshot,
    ) -> Result<Option<Rect>, String> {
        self.validate()?;
        if self.canvas != extent {
            return self.mapped(extent)?.rasterize(extent, out);
        }
        let video = self.clipped_rect(extent);
        let Some(rect) = self
            .background
            .and_then(|(rect, _)| self.clip_rectangle(rect, extent))
            .or(video)
        else {
            return Ok(None);
        };
        out.width = rect.w;
        out.height = rect.h;
        out.bgra.resize(rect.w as usize * rect.h as usize * 4, 0);
        for y in 0..rect.h {
            for x in 0..rect.w {
                let at = (y as usize * rect.w as usize + x as usize) * 4;
                let dx = rect.x + x;
                let dy = rect.y + y;
                if video.is_some_and(|v| dx >= v.x && dx < v.x + v.w && dy >= v.y && dy < v.y + v.h)
                {
                    out.bgra[at..at + 4].copy_from_slice(self.pixel(dx, dy));
                } else {
                    out.bgra[at..at + 4].copy_from_slice(&self.background.unwrap().1);
                }
            }
        }
        Ok(Some(rect))
    }
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
            + self.format.byte_offset(x)
    }
}

struct Arena {
    free: Vec<(u32, u32)>,
    regions: Vec<(u32, u32)>,
    capacity: u64,
}
const ARENA_GROWTH_BYTES: u32 = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug)]
pub struct ArenaUsage {
    pub capacity: u64,
    pub free: u64,
    pub largest_free: u32,
    pub pixel_bytes: u64,
    pub live_surfaces: usize,
}
impl Arena {
    fn contains(&self, address: u32, bytes: u32) -> bool {
        address.checked_add(bytes).is_some_and(|end| {
            self.regions
                .iter()
                .any(|&(base, len)| address >= base && end <= base + len)
        })
    }
    fn new(start: u32, len: u32) -> Result<Self, String> {
        start.checked_add(len).ok_or("arena overflow")?;
        if !start.is_multiple_of(16) || len == 0 {
            return Err("arena must be aligned and nonempty".into());
        }
        Ok(Self {
            free: vec![(start, len)],
            regions: vec![(start, len)],
            capacity: u64::from(len),
        })
    }
    fn extend(&mut self, start: u32, len: u32) -> Result<(), String> {
        let end = start.checked_add(len).ok_or("arena overflow")?;
        if len == 0
            || !start.is_multiple_of(16)
            || !len.is_multiple_of(16)
            || self
                .regions
                .iter()
                .any(|&(base, length)| start < base + length && base < end)
        {
            return Err("invalid or overlapping arena extension".into());
        }
        self.regions.push((start, len));
        self.capacity += u64::from(len);
        self.release(start, len);
        Ok(())
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
    damage_history: DamageHistory,
    software_source: Option<u32>,
    /// Damaged and total pixels of the last software publication.
    software_coverage: (u64, u64),
    software_pending: Option<SoftwareFrame>,
    #[cfg(feature = "gpu")]
    native_surfaces: BTreeMap<u32, NativeSurface>,
    #[cfg(feature = "gpu")]
    native_damage: FrameDamage,
    primary: Option<u32>,
    deferred_presentation: bool,
    pending_frame: Option<PendingFrame>,
    retained_frame: Option<PendingFrame>,
    video_layers: Arc<[(u64, VideoLayer)]>,
    captured_serial: u64,
    conversion: Snapshot,
    blit_row: Vec<u8>,
    blit_sample: Vec<u8>,
    blit_source: Vec<u8>,
    text_patch: Vec<u8>,
    frame_mailbox: Option<(FrameProducer, adv_sync::WakeEvent)>,
}

struct SoftwareFrame {
    source: u32,
    writer: u32,
    started: std::time::Instant,
    damage: Damage,
}

#[derive(Clone, Copy)]
pub enum FlushReason {
    Wait,
    Message,
    Timeout,
    Immediate,
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
            damage_history: DamageHistory::default(),
            software_source: None,
            software_coverage: (0, 0),
            software_pending: None,
            #[cfg(feature = "gpu")]
            native_surfaces: BTreeMap::new(),
            #[cfg(feature = "gpu")]
            native_damage: FrameDamage::default(),
            primary: None,
            deferred_presentation: false,
            pending_frame: None,
            retained_frame: None,
            video_layers: Arc::from([]),
            captured_serial: 0,
            conversion: Snapshot::default(),
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

    /// Cold allocation diagnostics. Released pixels remain reusable in the
    /// process pool; mappings belong to the assembly's guest address space.
    pub fn arena_usage(&self) -> ArenaUsage {
        ArenaUsage {
            capacity: self.arena.capacity,
            free: self.arena.free.iter().map(|&(_, len)| u64::from(len)).sum(),
            largest_free: self
                .arena
                .free
                .iter()
                .map(|&(_, len)| len)
                .max()
                .unwrap_or(0),
            pixel_bytes: self
                .surfaces
                .values()
                .filter(|s| s.pixels != 0)
                .map(|s| u64::from(s.pitch) * u64::from(s.height))
                .sum(),
            live_surfaces: self.surfaces.len(),
        }
    }

    /// Bytes to map when an allocation does not fit a reusable block. A zero
    /// request fits now; None means the requested byte size overflows.
    /// Mapping limits remain with the caller's shared guest address space.
    pub fn arena_growth(&self, needed: u32) -> Option<u32> {
        let needed = needed.checked_add(15)? & !15;
        if self.arena.free.iter().any(|&(_, len)| len >= needed) {
            return Some(0);
        }
        Some(needed.max(ARENA_GROWTH_BYTES))
    }

    /// Adopt a disjoint, already mapped guest range into the same allocator.
    /// The caller must retire its mapping if this validation fails.
    pub fn extend_arena(&mut self, address: u32, length: u32) -> Result<(), String> {
        self.arena.extend(address, length)
    }

    pub fn set_palette(
        &mut self,
        id: u32,
        start: u32,
        entries: &[[u8; 4]],
        m: &dyn SurfaceMemory,
    ) -> Result<usize, String> {
        self.flush(m, FlushReason::Immediate)?;
        let surface = self
            .surfaces
            .get_mut(&id)
            .ok_or("unknown palette surface")?;
        if !surface.format.indexed() {
            return Err("surface is not indexed".into());
        }
        let start = start.min(surface.format.colors() as u32) as usize;
        let count = entries.len().min(surface.format.colors() - start);
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
        if !surface.format.indexed() {
            return None;
        }
        let start = start.min(surface.format.colors() as u32) as usize;
        let count = (count as usize).min(surface.format.colors() - start);
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
        // Imported guest storage is retired by its frontend owner, not returned
        // to the surface allocator's free list.
        if self.arena.contains(s.pixels, s.pitch * s.height) {
            self.arena.release(s.pixels, s.pitch * s.height);
        }
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
            self.capture_complete()?;
        }
        self.deferred_presentation = enabled;
        Ok(())
    }

    /// Publish a movie sample or a layout/visibility change independently of a
    /// game Present. The retained base remains immutable beneath every layer.
    pub fn set_video_layer(
        &mut self,
        key: u64,
        layer: Option<VideoLayer>,
        m: &dyn SurfaceMemory,
    ) -> Result<u64, String> {
        if let Some(layer) = &layer {
            layer.validate()?;
        } else if !self.video_layers.iter().any(|(id, _)| *id == key) {
            return Ok(self.stats.presentations);
        }
        self.flush(m, FlushReason::Immediate)?;
        let serial = self
            .stats
            .presentations
            .checked_add(1)
            .ok_or("presentation serial exhausted")?;
        let mut video = self.video_layers.to_vec();
        video.retain(|(id, _)| *id != key);
        if let Some(layer) = layer {
            video.push((key, layer));
            video.sort_by_key(|(id, _)| *id);
        }
        self.video_layers = Arc::from(video);
        if self.retained_frame.is_none() {
            let (width, height) = self
                .video_layers
                .last()
                .map_or((self.width, self.height), |(_, v)| v.canvas);
            let mut frame = PendingFrame {
                width,
                height,
                pitch: width * 4,
                ..Default::default()
            };
            Arc::make_mut(&mut frame.pixels).resize(width as usize * height as usize * 4, 0);
            self.retained_frame = Some(frame);
        }
        let frame = self.retained_frame.as_mut().unwrap();
        frame.serial = serial;
        frame.history.clear(); // Video composition and removal always rebuild the base.
        frame.damage = Damage::from_rect(Rect {
            x: 0,
            y: 0,
            w: frame.width,
            h: frame.height,
        });
        frame.video = self.video_layers.clone();
        self.software_source = None;
        #[cfg(feature = "gpu")]
        if let Some(native) = &mut frame.native {
            native.serial = serial;
        }
        self.stats.presentations = serial;
        self.software_source = None;
        if let Some((mailbox, wake)) = &mut self.frame_mailbox {
            let overwritten = mailbox.publish(|slot| *slot = frame.clone());
            self.stats.coalesced_frames += u64::from(overwritten);
            if !overwritten {
                wake.signal();
            }
        } else {
            if self
                .pending_frame
                .as_ref()
                .is_some_and(|old| old.serial != self.captured_serial)
            {
                self.stats.coalesced_frames += 1;
            }
            self.pending_frame = Some(frame.clone());
            if !self.deferred_presentation {
                self.capture_complete()?;
            }
        }
        Ok(serial)
    }

    /// Pin pixels and overlay metadata together for an explicit capture.
    pub fn latest_frame(&self) -> Option<&PendingFrame> {
        self.frame_mailbox
            .as_ref()
            .and_then(|(mailbox, _)| mailbox.latest())
            .or(self.pending_frame.as_ref())
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
        let source = self.latest_frame().unwrap().clone();
        self.capture_native_frame_pixels(&source, bgra)
    }

    #[cfg(feature = "gpu")]
    pub fn capture_native_frame_pixels(
        &mut self,
        source: &PendingFrame,
        bgra: &[u8],
    ) -> Result<bool, String> {
        let mut snapshot = Snapshot::default();
        source.snapshot_native(bgra, &mut snapshot)?;
        if source.serial <= self.captured_serial || source.serial > self.stats.presentations {
            return Ok(false);
        }
        self.presenter.present(FrameView {
            width: snapshot.width,
            height: snapshot.height,
            pitch: snapshot.width * 4,
            bgra: &snapshot.bgra,
            serial: source.serial,
        })?;
        self.captured_serial = source.serial;
        self.stats.host_captures += 1;
        Ok(true)
    }

    /// Materialize a retained immutable source after a later presentation.
    /// Its own dimensions and serial remain authoritative; completed captures
    /// cannot replace a snapshot which is already newer.
    #[cfg(feature = "gpu")]
    pub fn capture_native_snapshot_pixels(
        &mut self,
        source: &crate::gpu::NativeFrame,
        bgra: &[u8],
    ) -> Result<bool, String> {
        let width = source.image.width();
        let height = source.image.height();
        let length = (width as usize)
            .checked_mul(height as usize)
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or("native capture dimensions overflow")?;
        if bgra.len() != length {
            return Err("native capture dimensions or byte length mismatch".into());
        }
        if let Some(frame) = self
            .latest_frame()
            .filter(|frame| frame.serial == source.serial)
            .cloned()
        {
            return self.capture_native_frame_pixels(&frame, bgra);
        }
        if source.serial <= self.captured_serial || source.serial > self.stats.presentations {
            return Ok(false);
        }
        self.presenter.present(FrameView {
            width,
            height,
            pitch: width * 4,
            bgra,
            serial: source.serial,
        })?;
        self.captured_serial = source.serial;
        self.stats.host_captures += 1;
        Ok(true)
    }

    /// Materialize the newest complete frame once, including after the guest stops
    /// producing frames. Raw pixels are owned, so later guest writes or surface
    /// release cannot turn this into a partially drawn frame.
    pub fn capture_latest(&mut self, m: &dyn SurfaceMemory) -> Result<bool, String> {
        self.flush(m, FlushReason::Immediate)?;
        self.capture_complete()
    }
    fn capture_complete(&mut self) -> Result<bool, String> {
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
        let mut snapshot = std::mem::take(&mut self.conversion);
        frame.snapshot(&mut snapshot)?;
        if frame.format != PixelFormat::Bgra32 {
            self.stats.converted_pixels += snapshot
                .damage
                .iter()
                .map(|r| u64::from(r.w) * u64::from(r.h))
                .sum::<u64>();
        }
        let result = self.presenter.present(FrameView {
            width: snapshot.width,
            height: snapshot.height,
            pitch: snapshot.width * 4,
            bgra: &snapshot.bgra,
            serial: frame.serial,
        });
        self.stats.converted_bytes += snapshot
            .damage
            .iter()
            .map(|r| u64::from(r.w) * u64::from(r.h) * 4)
            .sum::<u64>();
        self.conversion = snapshot;
        result?;
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
    /// Move the shell's single producer to the next foreground process. The
    /// receiving process continues the sequence observed by the same consumer.
    /// Without a mailbox, carry the capture policy and sequence instead.
    /// Both stores must be between execution leases during this handoff.
    pub fn transfer_frame_mailbox_to(
        &mut self,
        destination: &mut Self,
        m: &dyn SurfaceMemory,
    ) -> Result<bool, String> {
        if destination.frame_mailbox.is_some() {
            return Err("destination already owns a frame mailbox".into());
        }
        self.flush(m, FlushReason::Immediate)?;
        let endpoint = self.frame_mailbox.take();
        let transferred = endpoint.is_some();
        let serial = self
            .stats
            .presentations
            .max(destination.stats.presentations);
        destination.stats.presentations = serial;
        // A process-local pending frame may carry a smaller sequence. Clear it
        // before adopting the shared output so delayed captures cannot regress.
        destination.pending_frame = None;
        destination.software_source = None;
        destination.damage_history.clear();
        self.software_source = None;
        destination.captured_serial = serial;
        destination.frame_mailbox = endpoint;
        destination.deferred_presentation = self.deferred_presentation;
        // The former foreground can keep producing its local pending frame.
        self.deferred_presentation = true;
        Ok(transferred)
    }
    /// Resize the display; an existing primary surface keeps its overlapping pixels.
    pub fn configure_window(
        &mut self,
        width: u32,
        height: u32,
        m: &mut dyn SurfaceMemory,
    ) -> Result<(), String> {
        size(width, height)?;
        if (self.width, self.height) != (width, height) {
            self.flush(m, FlushReason::Immediate)?;
        }
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
    /// Register existing writable guest pixels without copying or clearing them.
    /// The caller keeps that memory alive until it deletes the surface. Imported
    /// memory must lie outside the arena, so deletion never transfers it there.
    pub fn import_guest(
        &mut self,
        surface: Surface,
        m: &mut dyn SurfaceMemory,
    ) -> Result<u32, String> {
        size(surface.width, surface.height)?;
        let row = surface.format.row_bytes(surface.width);
        let bytes = surface
            .pitch
            .checked_mul(surface.height)
            .ok_or("surface overflow")?;
        let end = surface
            .pixels
            .checked_add(bytes)
            .ok_or("surface overflow")?;
        if surface.pixels == 0
            || surface.pitch < row
            || self
                .arena
                .regions
                .iter()
                .any(|&(base, len)| surface.pixels < base + len && base < end)
        {
            return Err("invalid external surface storage".into());
        }
        m.writable(surface.pixels, bytes as usize)?;
        let id = self.new_id();
        self.surfaces.insert(id, surface);
        Ok(id)
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
        let pitch = (format.row_bytes(w) + 3) & !3;
        let bytes = pitch.checked_mul(h).ok_or("surface overflow")?;
        let p = self.arena.alloc(bytes, m)?;
        // Reused storage must not expose pixels from a previously deleted object.
        let zero = [0u8; 4096];
        let mut offset = 0;
        while offset < bytes {
            let n = (bytes - offset).min(4096);
            if let Err(error) = m.write(p + offset, &zero[..n as usize]) {
                self.arena.release(p, bytes);
                return Err(error);
            }
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
                palette: format.indexed().then(|| Arc::new(BLACK_PALETTE)),
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
    pub fn present(&mut self, id: u32, m: &dyn SurfaceMemory, damage: Rect) -> Result<(), String> {
        self.present_from(0, id, m, damage)
    }
    pub fn present_from(
        &mut self,
        context: u32,
        id: u32,
        m: &dyn SurfaceMemory,
        damage: Rect,
    ) -> Result<(), String> {
        #[cfg(feature = "gpu")]
        if self.native_surfaces.contains_key(&id) {
            return Err("native surface requires GPU presentation".into());
        }
        let damage = damage.clipped(self.surfaces[&id].rect());
        if damage.empty() {
            self.stats.empty_damage += 1;
        } else if self.frame_mailbox.is_some() {
            if self
                .software_pending
                .as_ref()
                .is_some_and(|p| p.source != id)
            {
                self.flush(m, FlushReason::Immediate)?;
            }
            let pending = self.software_pending.get_or_insert_with(|| SoftwareFrame {
                source: id,
                writer: context,
                started: std::time::Instant::now(),
                damage: Damage::default(),
            });
            pending.writer = context;
            pending.damage.add(damage);
        } else {
            return self.publish_software(id, m, Damage::from_rect(damage));
        }
        if self.software_expired() {
            self.flush(m, FlushReason::Timeout)?;
        }
        Ok(())
    }
    fn software_expired(&self) -> bool {
        self.software_pending
            .as_ref()
            .is_some_and(|p| p.started.elapsed() >= std::time::Duration::from_millis(50))
    }
    /// Only the drawing context ends a frame; other waits can enforce the age bound.
    pub fn flush_context(
        &mut self,
        context: u32,
        m: &dyn SurfaceMemory,
        reason: FlushReason,
    ) -> Result<bool, String> {
        if self
            .software_pending
            .as_ref()
            .is_some_and(|p| p.writer == context)
        {
            self.flush(m, reason)
        } else if matches!(reason, FlushReason::Wait) && self.software_expired() {
            self.flush(m, FlushReason::Timeout)
        } else {
            Ok(false)
        }
    }
    /// Damaged and total pixels of the last software publication.
    pub fn software_coverage(&self) -> (u64, u64) {
        self.software_coverage
    }
    pub fn flush(&mut self, m: &dyn SurfaceMemory, reason: FlushReason) -> Result<bool, String> {
        let Some(pending) = self.software_pending.take() else {
            return Ok(false);
        };
        self.publish_software(pending.source, m, pending.damage)?;
        self.stats.software_flushes[reason as usize] += 1;
        Ok(true)
    }
    fn publish_software(
        &mut self,
        id: u32,
        m: &dyn SurfaceMemory,
        damage: Damage,
    ) -> Result<(), String> {
        let s = &self.surfaces[&id];
        let latest = self.retained_frame.as_ref();
        let reset = self.software_source != Some(id)
            || latest.is_none_or(|f| {
                f.extent() != (s.width, s.height)
                    || f.pitch != s.pitch
                    || f.format != s.format
                    || f.bottom_up != s.bottom_up
                    || f.palette != s.palette
                    || !f.video.is_empty()
            })
            || !self.video_layers.is_empty();
        let damage = if reset {
            Damage::from_rect(s.rect())
        } else {
            damage
        };
        if reset {
            self.damage_history.clear();
        }
        self.stats.presentations += 1;
        self.stats
            .software_started
            .get_or_insert_with(std::time::Instant::now);
        self.software_source = Some(id);
        // The frame cap measures the guest's drawing, so it keeps the declared area.
        self.software_coverage = (
            damage.iter().map(|r| u64::from(r.w) * u64::from(r.h)).sum(),
            u64::from(s.width) * u64::from(s.height),
        );
        let damage = match latest {
            Some(previous) if !reset && !s.format.packed() => {
                refine_damage(s, m, &previous.pixels, &damage)?
            }
            _ => damage,
        };
        self.stats.damaged_pixels += damage
            .iter()
            .map(|r| u64::from(r.w) * u64::from(r.h))
            .sum::<u64>();
        self.stats.frame_pixels += self.software_coverage.1;
        self.damage_history.push((self.stats.presentations, damage));
        // Retire our latest reference before recycling the writer's private slot.
        self.retained_frame = None;
        if let Some((mailbox, wake)) = &mut self.frame_mailbox {
            let overwritten = mailbox.try_publish(|frame| {
                self.stats.copied_bytes += frame.store(
                    s,
                    m,
                    self.stats.presentations,
                    &self.damage_history,
                    &damage,
                )?;
                frame.video = self.video_layers.clone();
                self.retained_frame = Some(frame.clone());
                Ok::<(), String>(())
            })?;
            self.stats.published_frames += 1;
            self.stats.coalesced_frames += u64::from(overwritten);
            if !overwritten {
                wake.signal();
            }
            return Ok(());
        }
        {
            if self
                .pending_frame
                .as_ref()
                .is_some_and(|f| f.serial != self.captured_serial)
            {
                self.stats.coalesced_frames += 1;
            }
            let frame = self.pending_frame.get_or_insert_with(PendingFrame::default);
            // Retiring this reference before writing keeps the pending slot's
            // raw buffer reusable when no consumer still owns the previous base.
            self.retained_frame = None;
            self.stats.copied_bytes += frame.store(
                s,
                m,
                self.stats.presentations,
                &self.damage_history,
                &damage,
            )?;
            frame.video = self.video_layers.clone();
            self.retained_frame = Some(frame.clone());
        }
        self.stats.published_frames += 1;
        if !self.deferred_presentation {
            self.capture_complete()?;
        }
        Ok(())
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
        self.publish_native_frame(id, frame, None)?;
        Ok(())
    }

    /// A deferred store without the shared output can finish private frames.
    /// Those frames are not a baseline for eliminating a foreground Present.
    #[cfg(feature = "gpu")]
    pub fn output_serial(&self) -> Option<u64> {
        (!self.deferred_presentation || self.frame_mailbox.is_some())
            .then_some(self.stats.presentations)
    }

    /// Advance guest presentation accounting while retaining the visible frame.
    #[cfg(feature = "gpu")]
    pub fn repeat_native_frame(&mut self, id: u32) -> Result<u64, crate::gpu::Error> {
        if !self.native_surfaces.contains_key(&id) {
            return Err(crate::gpu::Error::Invalid(
                "unknown repeated surface".into(),
            ));
        }
        let serial =
            self.stats.presentations.checked_add(1).ok_or_else(|| {
                crate::gpu::Error::Invalid("presentation serial exhausted".into())
            })?;
        self.stats.presentations = serial;
        Ok(serial)
    }

    /// A discard boundary transfers the old target into the immutable frame and
    /// rotates one available target. No texture copy or Queue submit is needed.
    /// The pool is bounded; retaining all frames returns Busy before publication.
    #[cfg(feature = "gpu")]
    /// `damage` is what changed since this surface's previous presentation.
    pub fn present_native_rotating(
        &mut self,
        id: u32,
        damage: Option<Rect>,
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
        if let Err(error) = self.publish_native_frame(id, frame, damage) {
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
        damage: Option<Rect>,
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
        self.native_damage.push(id, damage);
        frame.damage = self.native_damage;
        if self.frame_mailbox.is_none() && !self.deferred_presentation {
            self.presenter.present_native(&frame)?;
            self.captured_serial = serial;
            self.stats.host_captures += 1;
        }
        self.stats.presentations = serial;
        self.software_source = None;
        if let Some((mailbox, wake)) = &mut self.frame_mailbox {
            let overwritten = mailbox.publish(|slot| {
                slot.store_native(frame);
                slot.video = self.video_layers.clone();
                self.retained_frame = Some(slot.clone());
            });
            self.stats.coalesced_frames += u64::from(overwritten);
            if !overwritten {
                wake.signal();
            }
        } else {
            if self
                .pending_frame
                .as_ref()
                .is_some_and(|old| old.serial != self.captured_serial)
            {
                self.stats.coalesced_frames += 1;
            }
            let slot = self.pending_frame.get_or_insert_with(PendingFrame::default);
            slot.store_native(frame);
            slot.video = self.video_layers.clone();
            self.retained_frame = Some(slot.clone());
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
        if surface.format == PixelFormat::Indexed1 {
            return self.draw_monochrome_text(id, &surface, bounds, memory, text, font, paint);
        }
        let row = usize::try_from(bounds.w).unwrap() * surface.format.bytes() as usize;
        let indexed = surface.format.indexed();
        let paint_format = if indexed {
            PixelFormat::Bgra32
        } else {
            surface.format
        };
        let paint_row = bounds.w as usize * paint_format.bytes() as usize;
        let pitch = (paint_row + 3) & !3;
        let paint_len = pitch
            .checked_mul(bounds.h as usize)
            .ok_or("text patch exceeds allocation limit")?;
        let original_len = if indexed { row * bounds.h as usize } else { 0 };
        let len = paint_len
            .checked_add(original_len)
            .filter(|&n| n <= 32 * 1024 * 1024)
            .ok_or("text patch exceeds allocation limit")?;
        if len > self.text_patch.capacity() {
            self.text_patch.reserve_exact(len - self.text_patch.len());
            self.stats.text_scratch_growths += 1;
        }
        if len > self.text_patch.len() {
            self.text_patch.resize(len, 0);
        }
        let (bytes, original_indices) = self.text_patch[..len].split_at_mut(paint_len);
        for y in 0..bounds.h {
            let addr = surface.row(bounds.x, bounds.y + y);
            memory.writable(addr, surface.span(bounds.x, bounds.w) as usize)?;
            let source = surface.read_pixels(memory, bounds.x, bounds.y + y, bounds.w)?;
            let destination = &mut bytes[y as usize * pitch..y as usize * pitch + paint_row];
            if indexed {
                original_indices[y as usize * row..y as usize * row + row].copy_from_slice(&source);
                for (pixel, index) in destination
                    .as_chunks_mut::<4>()
                    .0
                    .iter_mut()
                    .zip(source.iter())
                {
                    *pixel = surface.decode_pixel(&[*index]);
                }
            } else {
                destination.copy_from_slice(&source);
            }
        }
        text.paint(
            font,
            paint,
            Pixels {
                bounds,
                pitch: pitch as u32,
                format: paint_format,
                bytes,
            },
        )
        .map_err(|e| e.to_string())?;
        if indexed {
            // Quantize the same antialiased RGB output through this surface's
            // own table. Retain untouched indices even when multiple entries
            // have equal RGB values. Compact in place, with no per-pixel allocation.
            for y in 0..bounds.h as usize {
                let painted = &mut bytes[y * pitch..y * pitch + paint_row];
                for (x, &original) in original_indices[y * row..y * row + row].iter().enumerate() {
                    let pixel: [u8; 4] = painted[x * 4..x * 4 + 4].try_into().unwrap();
                    painted[x] = if pixel[..3] == surface.decode_pixel(&[original])[..3] {
                        original
                    } else {
                        surface.encode_pixel(pixel)[0]
                    };
                }
            }
        }
        let changed = (0..bounds.h).try_fold(false, |changed, y| {
            let original = surface.read_pixels(memory, bounds.x, bounds.y + y, bounds.w)?;
            Ok::<bool, String>(
                changed
                    || original.as_ref() != &bytes[y as usize * pitch..y as usize * pitch + row],
            )
        })?;
        if changed {
            for y in 0..bounds.h {
                surface.write_pixels(
                    memory,
                    bounds.x,
                    bounds.y + y,
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
        if d.format.packed() {
            for y in rect.y..rect.y + rect.h {
                m.writable(d.row(rect.x, y), d.span(rect.x, rect.w) as usize)?;
                m.read(d.row(rect.x, y), d.span(rect.x, rect.w) as usize)?;
            }
            let indices = vec![value as u8; rect.w as usize];
            for y in rect.y..rect.y + rect.h {
                d.write_pixels(m, rect.x, y, &indices)?;
            }
            self.surfaces.get_mut(&dst).unwrap().version += 1;
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
    ) -> Result<Rect, String> {
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
    ) -> Result<Rect, String> {
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
    ) -> Result<Rect, String> {
        if source.width == 0 || source.height == 0 {
            return Err("empty external surface".into());
        }
        size(source.width, source.height)?;
        let row_bytes = source.format.row_bytes(source.width);
        if source.pitch < row_bytes
            || source.pixels == 0
            || (source.format.indexed() && source.palette.is_none())
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
            return Ok(Rect::default());
        }
        // Validate every source/destination row before the first write. DIB
        // padding is not part of a pixel and need not be readable or writable.
        for y in sr.y..sr.y + sr.h {
            m.read(source.row(sr.x, y), source.span(sr.x, sr.w) as usize)?;
        }
        for y in top..bottom {
            m.writable(
                d.row(left as u32, y as u32),
                d.span(left as u32, (right - left) as u32) as usize,
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
    ) -> Result<Rect, String> {
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
            return Ok(Rect::default());
        }
        let clipped = Rect {
            x: left as u32,
            y: top as u32,
            w: (right - left) as u32,
            h: (bottom - top) as u32,
        };
        let ox = (left - dr.x) as u32;
        let oy = (top - dr.y) as u32;
        if s.format.packed() || d.format.packed() {
            return self.blit_packed(s, sr, dst, &d, dr, options, m);
        }
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
        let mut damage = clipped;
        let mut copied_rows = clipped.h;
        if unscaled
            && !options.mirror_x
            && !options.mirror_y
            && same_encoding
            && (!aliases || same_layout)
        {
            damage = Rect::default();
            copied_rows = 0;
            // Logical row order is independent of top-down/bottom-up storage.
            for row in 0..clipped.h {
                let y = if same_layout && clipped.y > sr.y + oy {
                    clipped.h - 1 - row
                } else {
                    row
                };
                let src = s.row(sr.x + ox, sr.y + oy + y);
                let dst = d.row(clipped.x, clipped.y + y);
                let n = clipped.w as usize * sb;
                if m.copy_changed(src, dst, n)? {
                    damage = damage.union(Rect {
                        y: clipped.y + y,
                        h: 1,
                        ..clipped
                    });
                    copied_rows += 1;
                }
            }
        } else if unscaled && !options.mirror_x && !aliases {
            // Every sampling mode reduces to one source texel at equal size.
            // Avoid the generic reduction/sampling buffers for common 24-bit
            // DIB -> 32-bit display transfers. Y reversal is still row-only.
            self.blit_row.resize(clipped.w as usize * db, 0);
            for y in 0..clipped.h {
                let sy = if options.mirror_y {
                    sr.h - 1 - (oy + y)
                } else {
                    oy + y
                };
                let input = m.read(s.row(sr.x + ox, sr.y + sy), clipped.w as usize * sb)?;
                match (s.format, d.format) {
                    (PixelFormat::Bgr24, PixelFormat::Bgra32) => {
                        for (src, dst) in input
                            .chunks_exact(3)
                            .zip(self.blit_row.as_chunks_mut::<4>().0)
                        {
                            *dst = [src[0], src[1], src[2], 0];
                        }
                    }
                    (PixelFormat::Indexed8, PixelFormat::Bgra32) => {
                        let palette = s.palette.as_deref().unwrap_or(&BLACK_PALETTE);
                        for (&src, dst) in input.iter().zip(self.blit_row.as_chunks_mut::<4>().0) {
                            *dst = palette[src as usize];
                        }
                    }
                    _ => {
                        for (src, dst) in input
                            .chunks_exact(sb)
                            .zip(self.blit_row.chunks_exact_mut(db))
                        {
                            if let Some(remap) = &remap {
                                dst[0] = remap[src[0] as usize];
                            } else {
                                dst.copy_from_slice(&d.encode_pixel(s.decode_pixel(src))[..db]);
                            }
                        }
                    }
                }
                m.write(d.row(clipped.x, clipped.y + y), &self.blit_row)?;
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
        if !damage.empty() {
            self.surfaces.get_mut(&dst).unwrap().version += 1;
        }
        self.stats.bytes_blitted +=
            u64::from(clipped.w) * u64::from(copied_rows) * u64::from(d.format.bytes());
        Ok(damage)
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

#[cfg(test)]
mod tests {
    use super::{FrameDamage, Rect};

    // spec: HOST-VIEW-10
    #[test]
    fn frame_damage_unions_publications_and_reports_gaps_as_unknown() {
        let rect = |x, w| Rect { x, y: 0, w, h: 1 };
        let mut damage = FrameDamage::default();
        damage.push(1, Some(rect(0, 2)));
        damage.push(1, Some(rect(10, 2)));
        assert_eq!(damage.since(Some(1)), Some(rect(10, 2)));
        assert_eq!(damage.since(Some(0)), None, "an unknown publication");
        assert_eq!(damage.since(None), None);
        assert_eq!(damage.since(Some(2)), None, "nothing new is a full redraw");
        damage.push(1, None);
        damage.push(1, Some(rect(4, 1)));
        assert_eq!(damage.since(Some(3)), Some(rect(4, 1)));
        assert_eq!(damage.since(Some(2)), None, "a full publication in between");
        damage.push(2, Some(rect(4, 1)));
        assert_eq!(damage.since(Some(4)), None, "another surface in between");
        for _ in 0..8 {
            damage.push(2, Some(rect(0, 1)));
        }
        assert_eq!(damage.since(Some(5)), None, "evicted history");
        assert_eq!(damage.since(Some(6)), Some(rect(0, 1)));
    }
}

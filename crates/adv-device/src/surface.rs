//! Software surfaces in guest memory and their presentation. Graphics fronts (GDI,
//! DirectDraw) name surfaces by id and translate their calls into these operations; the
//! presenter or the frame mailbox receives every completed frame.
use std::borrow::Cow;
use std::collections::BTreeMap;

/// Memory needed by software surfaces. PAL guests with shared storage borrow pixels;
/// other CPU backends can supply an owned buffer at this boundary.
pub trait SurfaceMemory {
    fn read(&self, addr: u32, len: usize) -> Result<Cow<'_, [u8]>, String>;
    fn write(&mut self, addr: u32, data: &[u8]) -> Result<(), String>;
    fn writable(&self, addr: u32, len: usize) -> Result<(), String>;
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
}

#[derive(Default)]
pub struct Snapshot {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
    pub frames: u64,
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
}

pub struct PendingFrame {
    width: u32,
    height: u32,
    pitch: u32,
    format: PixelFormat,
    serial: u64,
    pixels: Vec<u8>,
}
impl Default for PendingFrame {
    fn default() -> Self {
        Self {
            width: 0,
            height: 0,
            pitch: 0,
            format: PixelFormat::Bgra32,
            serial: 0,
            pixels: Vec::new(),
        }
    }
}
impl PendingFrame {
    /// Conversion is performed only by the display consumer, or an explicit capture.
    pub fn snapshot(&self, out: &mut Snapshot) -> Result<(), String> {
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
                    pixel.copy_from_slice(&self.format.decode(&src[x * 2..]));
                }
            }
        }
        Ok(())
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
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFormat {
    Bgra32,
    Rgb565,
    Rgb555,
}
impl PixelFormat {
    pub fn bytes(self) -> u32 {
        if self == Self::Bgra32 { 4 } else { 2 }
    }
    pub fn masks(self) -> (u32, u32, u32) {
        match self {
            Self::Bgra32 => (0xff0000, 0xff00, 0xff),
            Self::Rgb565 => (0xf800, 0x7e0, 0x1f),
            Self::Rgb555 => (0x7c00, 0x3e0, 0x1f),
        }
    }
    pub fn decode(self, p: &[u8]) -> [u8; 4] {
        if self == Self::Bgra32 {
            return p[..4].try_into().unwrap();
        }
        let v = u32::from(u16::from_le_bytes(p[..2].try_into().unwrap()));
        let (r, g, b) = self.masks();
        let channel = |mask: u32| {
            let bits = mask >> mask.trailing_zeros();
            ((v & mask) >> mask.trailing_zeros()) * 255 / bits
        };
        [channel(b) as u8, channel(g) as u8, channel(r) as u8, 0]
    }
    pub fn encode(self, p: [u8; 4]) -> Vec<u8> {
        if self == Self::Bgra32 {
            return p.to_vec();
        }
        let (r, g, b) = self.masks();
        let channel = |value: u8, mask: u32| {
            (u32::from(value) * (mask >> mask.trailing_zeros()) / 255) << mask.trailing_zeros()
        };
        ((channel(p[2], r) | channel(p[1], g) | channel(p[0], b)) as u16)
            .to_le_bytes()
            .to_vec()
    }
}
impl Surface {
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
    primary: Option<u32>,
    deferred_presentation: bool,
    pending_frame: Option<PendingFrame>,
    captured_serial: u64,
    conversion: Vec<u8>,
    frame_mailbox: Option<(FrameProducer, adv_sync::WakeEvent)>,
}

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
            primary: None,
            deferred_presentation: false,
            pending_frame: None,
            captured_serial: 0,
            conversion: Vec::new(),
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
                        .copy_from_slice(&frame.format.decode(&frame.pixels[at..]));
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
        let s = &self.surfaces[&id];
        self.stats.presentations += 1;
        let pixels = m.read(s.pixels, (s.pitch * s.height) as usize)?;
        if let Some((mailbox, wake)) = &mut self.frame_mailbox {
            let overwritten = mailbox.publish(|frame| {
                frame.width = s.width;
                frame.height = s.height;
                frame.pitch = s.pitch;
                frame.format = s.format;
                frame.serial = self.stats.presentations;
                frame.pixels.resize(pixels.len(), 0);
                if s.bottom_up {
                    let pitch = s.pitch as usize;
                    for y in 0..s.height as usize {
                        frame.pixels[y * pitch..(y + 1) * pitch].copy_from_slice(
                            &pixels[(s.height as usize - 1 - y) * pitch
                                ..(s.height as usize - y) * pitch],
                        );
                    }
                } else {
                    frame.pixels.copy_from_slice(pixels.as_ref());
                }
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
            let pending = self.pending_frame.get_or_insert_with(|| PendingFrame {
                width: 0,
                height: 0,
                pitch: 0,
                format: s.format,
                serial: 0,
                pixels: Vec::new(),
            });
            pending.width = s.width;
            pending.height = s.height;
            pending.pitch = s.pitch;
            pending.format = s.format;
            pending.serial = self.stats.presentations;
            pending.pixels.clear();
            pending.pixels.extend_from_slice(&pixels);
            return Ok(());
        }
        self.stats.host_captures += 1;
        self.captured_serial = self.stats.presentations;
        if s.format != PixelFormat::Bgra32 {
            // Conversion is one output buffer; the guest pixel store remains shared.
            let mut converted = vec![0; s.width as usize * s.height as usize * 4];
            self.stats.converted_pixels += u64::from(s.width) * u64::from(s.height);
            for y in 0..s.height as usize {
                for x in 0..s.width as usize {
                    let offset = y * s.pitch as usize + x * s.format.bytes() as usize;
                    converted[(y * s.width as usize + x) * 4..(y * s.width as usize + x + 1) * 4]
                        .copy_from_slice(&s.format.decode(&pixels[offset..]));
                }
            }
            return self.presenter.present(FrameView {
                width: s.width,
                height: s.height,
                pitch: s.width * 4,
                bgra: &converted,
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
    /// Copy a rectangle between surfaces, converting when their formats differ.
    pub fn copy(
        &mut self,
        src: u32,
        sr: Rect,
        dst: u32,
        dr: Rect,
        m: &mut dyn SurfaceMemory,
    ) -> Result<(), String> {
        let s = self
            .surfaces
            .get(&src)
            .ok_or("unknown source surface")?
            .clone();
        let d = self
            .surfaces
            .get(&dst)
            .ok_or("unknown destination surface")?
            .clone();
        if sr.w != dr.w || sr.h != dr.h {
            return Err("scaling not implemented".into());
        }
        // Copy bottom to top for downward overlapping same-surface transfers.
        for row in 0..sr.h {
            let y = if src == dst && dr.y > sr.y {
                sr.h - 1 - row
            } else {
                row
            };
            if s.format == d.format {
                m.copy(
                    s.row(sr.x, sr.y + y),
                    d.row(dr.x, dr.y + y),
                    (sr.w * s.format.bytes()) as usize,
                )?;
            } else {
                let input = m
                    .read(s.row(sr.x, sr.y + y), (sr.w * s.format.bytes()) as usize)?
                    .into_owned();
                let output: Vec<u8> = input
                    .chunks_exact(s.format.bytes() as usize)
                    .flat_map(|p| d.format.encode(s.format.decode(p)))
                    .collect();
                m.write(d.row(dr.x, dr.y + y), &output)?;
            }
        }
        self.surfaces.get_mut(&dst).unwrap().version += 1;
        self.stats.bytes_blitted += u64::from(sr.w) * u64::from(sr.h) * u64::from(d.format.bytes());
        Ok(())
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

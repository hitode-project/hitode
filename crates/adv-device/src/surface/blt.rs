//! DirectDraw's measured copy rules, shared by CPU and GPU execution.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryClass {
    System,
    Video,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BltError {
    InvalidRect,
    UnsupportedConversion,
    ConversionFailed,
    NoColorKeyHardware,
    InvalidArgument,
}

#[derive(Clone, Copy)]
pub enum BltKey {
    Source,
    SourceRange,
    Destination,
}

#[derive(Clone, Copy)]
pub struct BltRules {
    conversion: ConversionSupport,
}

#[derive(Clone, Copy)]
enum ConversionSupport {
    Driver,
    Upload,
    Unsupported,
    Download,
}

#[derive(Clone, Copy)]
enum Conversion {
    Identity,
    Rgb {
        source: Encoding,
        destination: Encoding,
    },
}

#[derive(Clone, Copy)]
enum Encoding {
    X888 = 1,
    Rgb555,
    Rgb565,
    Gray,
}

impl BltRules {
    pub fn for_memory(source: MemoryClass, destination: MemoryClass) -> Self {
        use MemoryClass::{System, Video};
        let conversion = match (source, destination) {
            (Video, Video) => ConversionSupport::Driver,
            (System, Video) => ConversionSupport::Upload,
            (System, System) => ConversionSupport::Unsupported,
            (Video, System) => ConversionSupport::Download,
        };
        Self { conversion }
    }

    pub fn supports_key(self, key: BltKey) -> bool {
        matches!(key, BltKey::Source)
    }

    /// Store the original key. The format mask is applied only at comparison.
    pub fn set_source_key(
        &self,
        key: &mut Option<u32>,
        value: Option<u32>,
        kind: BltKey,
    ) -> Result<(), BltError> {
        if !self.supports_key(kind) {
            return Err(BltError::NoColorKeyHardware);
        }
        *key = value;
        Ok(())
    }

    fn conversion(
        self,
        source: PixelFormat,
        destination: PixelFormat,
    ) -> Result<Conversion, BltError> {
        use PixelFormat::{Bgra32, Indexed8, Rgb555, Rgb565};
        let both_16_bit =
            matches!(source, Rgb555 | Rgb565) && matches!(destination, Rgb555 | Rgb565);
        let driver = matches!(self.conversion, ConversionSupport::Driver);
        if source == destination && !(driver && source == Rgb555) {
            return Ok(Conversion::Identity);
        }
        let convert = |source, destination| Conversion::Rgb {
            source,
            destination,
        };
        let encode = |format| match format {
            Bgra32 => Some(Encoding::X888),
            Rgb555 => Some(Encoding::Rgb555),
            Rgb565 => Some(Encoding::Rgb565),
            _ => None,
        };
        match self.conversion {
            ConversionSupport::Driver => {
                let source = if source == Indexed8 {
                    Some(Encoding::Gray)
                } else {
                    encode(source)
                };
                source
                    .zip(encode(destination))
                    .map(|(source, destination)| convert(source, destination))
                    .ok_or(BltError::ConversionFailed)
            }
            ConversionSupport::Upload | ConversionSupport::Download if both_16_bit => {
                Ok(Conversion::Identity)
            }
            ConversionSupport::Upload
                if destination == Bgra32 && matches!(source, Rgb555 | Rgb565) =>
            {
                Ok(convert(encode(source).unwrap(), Encoding::X888))
            }
            ConversionSupport::Unsupported => Err(BltError::UnsupportedConversion),
            _ => Err(BltError::ConversionFailed),
        }
    }

    fn axis(self, source: u32, destination: u32, mirror: bool) -> Axis {
        use std::cmp::Ordering::{Equal, Greater, Less};
        let driver = matches!(self.conversion, ConversionSupport::Driver);
        let (mode, a, b) = match source.cmp(&destination) {
            Equal => (Scale::Copy, 0, 0),
            Less if driver => (Scale::Grow, source - 1, 2 * (destination - 1)),
            Less => (Scale::Grow, source, 2 * destination),
            Greater if driver => {
                let step = (u64::from(source) << 24) / u64::from(destination);
                (
                    Scale::DriverReduce,
                    (step >> 24) as u32,
                    step as u32 & 0xff_ffff,
                )
            }
            Greater => (Scale::HelReduce, source / destination, source % destination),
        };
        Axis {
            source,
            destination,
            a,
            b,
            mode,
            mirror: mirror && !driver,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scale {
    Copy = 0,
    Grow = 1,
    DriverReduce = 2,
    HelReduce = 3,
}

#[derive(Clone, Copy)]
struct Axis {
    source: u32,
    destination: u32,
    a: u32,
    b: u32,
    mode: Scale,
    mirror: bool,
}

impl Axis {
    fn sample(self, x: u32) -> u32 {
        let at = match self.mode {
            Scale::Copy => x,
            Scale::Grow => ((2 * x + 1) * self.a).saturating_sub(1) / self.b,
            Scale::DriverReduce => {
                let step = (u64::from(self.a) << 24) | u64::from(self.b);
                ((step / 2 + u64::from(x) * step) >> 24) as u32
            }
            Scale::HelReduce => {
                self.a / 2 + x * self.a + (self.destination / 2 + x * self.b) / self.destination
            }
        };
        if self.mirror {
            self.source - 1 - at
        } else {
            at
        }
    }

    #[cfg(feature = "gpu")]
    fn parameters(self) -> [u32; 4] {
        [
            self.mode as u32,
            self.a,
            self.b,
            if self.mirror { self.source } else { 0 },
        ]
    }
}

impl Conversion {
    fn pixel(self, raw: u32) -> u32 {
        let Self::Rgb {
            source,
            destination,
        } = self
        else {
            return raw;
        };
        let expand5 = |n| n << 3 | n >> 2;
        let [r, g, b] = match source {
            Encoding::X888 => [(raw >> 16) & 255, (raw >> 8) & 255, raw & 255],
            Encoding::Rgb555 => [
                expand5((raw >> 10) & 31),
                expand5((raw >> 5) & 31),
                expand5(raw & 31),
            ],
            Encoding::Rgb565 => {
                let g = (raw >> 5) & 63;
                [expand5(raw >> 11), g << 2 | g >> 4, expand5(raw & 31)]
            }
            Encoding::Gray => [raw; 3],
        };
        match destination {
            Encoding::Rgb555 => (r >> 3) << 10 | (g >> 3) << 5 | (b >> 3),
            Encoding::Rgb565 => (r >> 3) << 11 | (g >> 2) << 5 | (b >> 3),
            _ => r << 16 | g << 8 | b,
        }
    }

    #[cfg(feature = "gpu")]
    fn parameters(self) -> u32 {
        match self {
            Self::Identity => 0,
            Self::Rgb {
                source,
                destination,
            } => source as u32 | (destination as u32) << 4,
        }
    }
}

#[derive(Clone, Copy)]
pub struct BltSurface {
    pub id: u32,
    pub rect: Rect,
    pub memory: MemoryClass,
}

#[derive(Clone, Copy)]
pub enum Blt {
    Copy {
        source: BltSurface,
        mirror: [bool; 2],
        key: Option<u32>,
    },
    Fill(u32),
    DestinationKey,
}

#[derive(Debug)]
pub enum BltFailure {
    Rule(BltError),
    Storage(String),
}

impl From<BltError> for BltFailure {
    fn from(value: BltError) -> Self {
        Self::Rule(value)
    }
}

impl From<String> for BltFailure {
    fn from(value: String) -> Self {
        Self::Storage(value)
    }
}

#[derive(Clone, Copy)]
struct Parameters {
    x: Axis,
    y: Axis,
    key: Option<u32>,
    mask: u32,
    conversion: Conversion,
}

impl Parameters {
    fn direct_copy(self) -> bool {
        self.x.mode == Scale::Copy
            && self.y.mode == Scale::Copy
            && !self.x.mirror
            && !self.y.mirror
            && self.key.is_none()
            && matches!(self.conversion, Conversion::Identity)
    }
}

fn check_rect(layout: &Surface, rect: Rect) -> Result<(), BltError> {
    if rect.empty() || rect.clipped(layout.rect()) != rect {
        return Err(BltError::InvalidRect);
    }
    Ok(())
}

fn raw_mask(format: PixelFormat) -> u32 {
    if format.indexed() {
        (format.colors() - 1) as u32
    } else if format == PixelFormat::Rgb555 {
        0xffff
    } else {
        let (r, g, b) = format.masks();
        r | g | b
    }
}

impl<P: Presenter> Surfaces<P> {
    /// Execute a DirectDraw Blt after its front has resolved keys and memory classes.
    pub fn blt(
        &mut self,
        destination: BltSurface,
        operation: Blt,
        memory: &mut dyn SurfaceMemory,
    ) -> Result<Rect, BltFailure> {
        let dst = self.get(destination.id).ok_or(BltError::InvalidArgument)?;
        check_rect(dst, destination.rect)?;
        let destination_format = dst.format;
        match operation {
            Blt::DestinationKey => Err(BltError::InvalidArgument.into()),
            Blt::Fill(value) => {
                let value = if destination_format == PixelFormat::Rgb555
                    && destination.memory == MemoryClass::Video
                {
                    value & 0x7fff
                } else {
                    value
                };
                #[cfg(feature = "gpu")]
                if self.surfaces[&destination.id]
                    .residency
                    .gpu_current(destination.rect)
                {
                    self.blt_gpu_fill(destination, destination_format, value, memory)?;
                    return Ok(destination.rect);
                }
                self.cpu_access(destination.id, destination.rect, Access::Write, memory)?;
                self.fill_cpu(destination.id, destination.rect, value, memory)?;
                Ok(destination.rect)
            }
            Blt::Copy {
                source,
                mirror,
                key,
            } => {
                let src = self.get(source.id).ok_or(BltError::InvalidArgument)?;
                check_rect(src, source.rect)?;
                let rules = BltRules::for_memory(source.memory, destination.memory);
                let mask = raw_mask(src.format);
                let params = Parameters {
                    x: rules.axis(source.rect.w, destination.rect.w, mirror[0]),
                    y: rules.axis(source.rect.h, destination.rect.h, mirror[1]),
                    key: key.map(|key| key & mask),
                    // The driver samples the 555 color without its unused bit,
                    // but retains that bit in the key itself.
                    mask: if src.format == PixelFormat::Rgb555
                        && matches!(rules.conversion, ConversionSupport::Driver)
                    {
                        0x7fff
                    } else {
                        mask
                    },
                    conversion: rules.conversion(src.format, destination_format)?,
                };
                #[cfg(feature = "gpu")]
                if self.surfaces[&source.id].residency.gpu_current(source.rect)
                    && self.surfaces[&destination.id]
                        .residency
                        .gpu_current(destination.rect)
                {
                    self.blt_gpu(source, destination, params, memory)?;
                    return Ok(destination.rect);
                }
                Ok(self.blt_cpu(source, destination, params, memory)?)
            }
        }
    }

    fn blt_cpu(
        &mut self,
        source: BltSurface,
        destination: BltSurface,
        params: Parameters,
        memory: &mut dyn SurfaceMemory,
    ) -> Result<Rect, String> {
        let sr = source.rect;
        let dr = destination.rect;
        let s = self
            .cpu_access(source.id, sr, Access::Read, memory)?
            .clone();
        let d = self
            .cpu_access(destination.id, dr, Access::Write, memory)?
            .clone();
        if params.direct_copy() && s.format == d.format && s.palette == d.palette {
            let dr = BlitRect {
                x: i64::from(dr.x),
                y: i64::from(dr.y),
                w: dr.w,
                h: dr.h,
            };
            return self.blit_surface(&s, sr, destination.id, dr, BlitOptions::default(), memory);
        }
        let sb = s.format.bytes() as usize;
        let db = d.format.bytes() as usize;
        let stride = sr.w as usize * sb;

        // Read the original source before writing, including aliased guest layouts.
        self.blit_source.clear();
        for y in sr.y..sr.y + sr.h {
            self.blit_source
                .extend_from_slice(&s.read_pixels(memory, sr.x, y, sr.w)?);
        }
        self.blit_columns.clear();
        self.blit_columns
            .extend((0..dr.w).map(|x| params.x.sample(x) as usize * sb));
        for y in 0..dr.h {
            let start = params.y.sample(y) as usize * stride;
            self.blit_row.clear();
            if params.key.is_some() {
                self.blit_row
                    .extend_from_slice(&d.read_pixels(memory, dr.x, dr.y + y, dr.w)?);
            } else {
                self.blit_row.resize(dr.w as usize * db, 0);
            }
            for (out, &column) in self.blit_row.chunks_exact_mut(db).zip(&self.blit_columns) {
                let mut pixel = [0; 4];
                pixel[..sb].copy_from_slice(&self.blit_source[start + column..start + column + sb]);
                let raw = u32::from_le_bytes(pixel);
                if params.key != Some(raw & params.mask) {
                    out.copy_from_slice(&params.conversion.pixel(raw).to_le_bytes()[..db]);
                }
            }
            d.write_pixels(memory, dr.x, dr.y + y, &self.blit_row)?;
        }
        self.surfaces.get_mut(&destination.id).unwrap().version += 1;
        self.stats.bytes_blitted += u64::from(dr.w) * u64::from(dr.h) * db as u64;
        Ok(dr)
    }
}

#[cfg(feature = "gpu")]
mod native;

#[cfg(test)]
mod tests;

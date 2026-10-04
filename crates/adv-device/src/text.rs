//! Font matching, immutable glyph results and a bounded raster cache. Requests
//! contain text and rendering attributes, never guest pointers or DC handles.
//! The current backend uses owned Windows fonts; other hosts report unavailable.
use std::collections::BTreeMap;
use std::sync::Arc;

#[cfg(windows)]
mod windows;

/// Legacy text encodings used to select a font's character repertoire. Reserved
/// selectors are preserved for the native matcher, never guessed as a code page.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Encoding {
    Latin,
    Automatic,
    Symbol,
    MacRoman,
    Japanese,
    Korean,
    Johab,
    SimplifiedChinese,
    TraditionalChinese,
    Greek,
    Turkish,
    Vietnamese,
    Hebrew,
    Arabic,
    Baltic,
    Cyrillic,
    Thai,
    EasternEuropean,
    Oem,
    Reserved(u8),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Precision {
    Default,
    String,
    Character,
    Stroke,
    TrueType,
    Device,
    Raster,
    PreferTrueType,
    Outline,
    ScreenOutline,
    PostScript,
    Reserved(u8),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Quality {
    Default,
    Draft,
    Proof,
    Monochrome,
    Antialiased,
    Subpixel,
    SubpixelCompatible,
    Reserved(u8),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Pitch {
    Default,
    Fixed,
    Variable,
    Reserved(u8),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Family {
    Default,
    Serif,
    SansSerif,
    Monospace,
    Script,
    Decorative,
    Reserved(u8),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ClipPrecision {
    Default,
    Character,
    Stroke,
    Reserved(u8),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Clipping {
    pub precision: ClipPrecision,
    pub transform_rotation: bool,
    pub force_outline: bool,
    pub disable_association: bool,
    pub embedded: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct FontRequest {
    pub face: String,
    pub height: i32,
    pub width: i32,
    /// Angles in tenths of a degree, preserving logical font matching precision.
    pub baseline_angle: i32,
    pub glyph_angle: i32,
    pub weight: i32,
    pub italic: bool,
    pub underline: bool,
    pub strikeout: bool,
    pub encoding: Encoding,
    pub precision: Precision,
    pub clipping: Clipping,
    pub quality: Quality,
    pub pitch: Pitch,
    pub family: Family,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Character {
    Unicode(u32),
    Encoded(u32),
    GlyphIndex(u32),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum GlyphFormat {
    Metrics,
    Monochrome,
    QuadraticOutline,
    CubicOutline,
    Gray2,
    Gray4,
    Gray8,
}
/// Signed 16.16 fixed point, in row-major order. This retains the exact caller
/// transform without float round trips or backend-specific structure layouts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Transform {
    pub xx: i32,
    pub xy: i32,
    pub yx: i32,
    pub yy: i32,
}
impl Transform {
    pub const IDENTITY: Self = Self {
        xx: 65536,
        xy: 0,
        yx: 0,
        yy: 65536,
    };
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct GlyphRequest {
    pub character: Character,
    pub format: GlyphFormat,
    pub hinted: bool,
    pub transform: Transform,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GlyphMetrics {
    pub width: u32,
    pub height: u32,
    pub origin_x: i32,
    pub origin_y: i32,
    pub advance_x: i16,
    pub advance_y: i16,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Curve {
    Line(Vec<Point>),
    Quadratic(Vec<Point>),
    Cubic(Vec<Point>),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Contour {
    pub start: Point,
    pub curves: Vec<Curve>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GlyphData {
    None,
    Bitmap(Vec<u8>),
    Outline(Vec<Contour>),
}
impl GlyphData {
    #[cfg(windows)]
    fn cache_bytes(&self) -> usize {
        match self {
            Self::None => 0,
            Self::Bitmap(bytes) => bytes.len(),
            Self::Outline(contours) => {
                contours.capacity() * std::mem::size_of::<Contour>()
                    + contours
                        .iter()
                        .map(|contour| {
                            contour.curves.capacity() * std::mem::size_of::<Curve>()
                                + contour
                                    .curves
                                    .iter()
                                    .map(|curve| {
                                        let points = match curve {
                                            Curve::Line(p)
                                            | Curve::Quadratic(p)
                                            | Curve::Cubic(p) => p,
                                        };
                                        points.capacity() * std::mem::size_of::<Point>()
                                    })
                                    .sum::<usize>()
                        })
                        .sum::<usize>()
            }
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Glyph {
    pub metrics: GlyphMetrics,
    pub required_bytes: u32,
    pub data: GlyphData,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextMetrics {
    pub height: i32,
    pub ascent: i32,
    pub descent: i32,
    pub internal_leading: i32,
    pub external_leading: i32,
    pub average_width: i32,
    pub maximum_width: i32,
    pub weight: i32,
    pub overhang: i32,
    pub aspect_x: i32,
    pub aspect_y: i32,
    pub first_character: u8,
    pub last_character: u8,
    pub default_character: u8,
    pub break_character: u8,
    pub italic: bool,
    pub underline: bool,
    pub strikeout: bool,
    pub variable_pitch: bool,
    pub vector: bool,
    pub outline: bool,
    pub device: bool,
    pub family: Family,
    pub encoding: Encoding,
}
/// Explicitly counted units. Encoded bytes follow the selected font repertoire;
/// UTF-16 keeps surrogate pairs and embedded zero units intact.
#[derive(Clone, Copy)]
pub enum TextInput<'a> {
    Encoded(&'a [u8]),
    Unicode(&'a [u16]),
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TextExtent {
    pub width: i32,
    pub height: i32,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheStats {
    pub fonts: usize,
    pub glyphs: usize,
    pub bytes: usize,
    pub hits: u64,
}
#[derive(Debug, PartialEq, Eq)]
pub enum TextError {
    Unavailable,
    Font(String),
    Glyph(String),
}
impl std::fmt::Display for TextError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable => {
                f.write_str("Font metrics and glyphs require a Windows GDI backend on this host")
            }
            Self::Font(message) | Self::Glyph(message) => f.write_str(message),
        }
    }
}

#[derive(Default)]
pub struct Text {
    #[cfg(windows)]
    fonts: BTreeMap<FontRequest, windows::Font>,
    glyphs: BTreeMap<(FontRequest, GlyphRequest), Arc<Glyph>>,
    bytes: usize,
    hits: u64,
}
/// Each cached native font owns a font and a private measurement DC. Glyph
/// results are immutable and remain valid after the native owner is evicted.
pub const FONT_CACHE_CAPACITY: usize = 128;
impl Text {
    pub fn stats(&self) -> CacheStats {
        CacheStats {
            fonts: self.font_count(),
            glyphs: self.glyphs.len(),
            bytes: self.bytes,
            hits: self.hits,
        }
    }
    fn font_count(&self) -> usize {
        #[cfg(windows)]
        {
            self.fonts.len()
        }
        #[cfg(not(windows))]
        {
            0
        }
    }
    #[cfg(windows)]
    fn font(&mut self, request: &FontRequest) -> Result<&windows::Font, TextError> {
        if !self.fonts.contains_key(request) {
            let font = windows::Font::new(request)?;
            if self.fonts.len() >= FONT_CACHE_CAPACITY {
                self.fonts.clear();
            }
            self.fonts.insert(request.clone(), font);
        }
        Ok(&self.fonts[request])
    }
    pub fn metrics(&mut self, request: &FontRequest) -> Result<TextMetrics, TextError> {
        #[cfg(windows)]
        {
            self.font(request)?.metrics()
        }
        #[cfg(not(windows))]
        {
            let _ = request;
            Err(TextError::Unavailable)
        }
    }
    /// Measure the entire run with the same owned font used for glyph requests.
    /// Individual glyph advances are insufficient for a shaped or kerned run.
    pub fn extent(
        &mut self,
        request: &FontRequest,
        input: TextInput<'_>,
    ) -> Result<TextExtent, TextError> {
        #[cfg(windows)]
        {
            self.font(request)?.extent(input)
        }
        #[cfg(not(windows))]
        {
            let _ = (request, input);
            Err(TextError::Unavailable)
        }
    }
    pub fn glyph(
        &mut self,
        font: &FontRequest,
        request: GlyphRequest,
    ) -> Result<Arc<Glyph>, TextError> {
        let key = (font.clone(), request);
        if let Some(glyph) = self.glyphs.get(&key) {
            self.hits += 1;
            return Ok(Arc::clone(glyph));
        }
        #[cfg(windows)]
        let glyph = Arc::new(self.font(font)?.glyph(request)?);
        #[cfg(not(windows))]
        {
            return Err(TextError::Unavailable);
        }
        #[cfg(windows)]
        {
            const CACHE_BYTES: usize = 8 * 1024 * 1024;
            let bytes = glyph.data.cache_bytes();
            if bytes <= CACHE_BYTES {
                if self.glyphs.len() >= 4096 || self.bytes + bytes > CACHE_BYTES {
                    self.glyphs.clear();
                    self.bytes = 0;
                }
                self.bytes += bytes;
                self.glyphs.insert(key, Arc::clone(&glyph));
            }
            Ok(glyph)
        }
    }
}

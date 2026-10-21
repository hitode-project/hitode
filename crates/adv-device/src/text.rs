//! Font matching, immutable glyph results and a bounded raster cache. Requests
//! contain text and rendering attributes, never guest pointers or DC handles.
//! Windows uses owned GDI fonts; other hosts use system or guest supplied OpenType
//! fonts through skrifa and zeno, with fontdb providing discovery.
use std::collections::BTreeMap;
use std::sync::Arc;

mod monochrome;
mod spacing;

#[cfg(not(windows))]
mod portable;
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
    /// A platform default is selected directly, rather than guessed by face name.
    pub default: Option<DefaultFont>,
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
/// Installed face discovery. Families keeps the native provider's family/style
/// grouping; Faces preserves its legacy face enumeration policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontEnumeration {
    Families,
    Faces,
    /// Extended enumeration selects a character repertoire as well as a face.
    FamiliesEx(Encoding),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontKind {
    Raster,
    Vector,
    TrueType,
}
#[derive(Clone, Debug)]
pub struct FontDesign {
    pub italic: bool,
    pub bold: bool,
    pub regular: bool,
    pub nonnegative_spacing: bool,
    pub postscript_outline: bool,
    pub truetype_outline: bool,
    pub multiple_master: bool,
    pub type1: bool,
    pub digital_signature: bool,
    pub em_units: u32,
    pub cell_height: u32,
    pub average_width: u32,
}
/// Owned, neutral font data. Text characters remain Unicode until the
/// personality chooses the guest's encoding and packs its callback ABI.
#[derive(Clone, Debug)]
pub struct EnumeratedFont {
    pub font: FontRequest,
    pub metrics: TextMetrics,
    pub characters: [u16; 4],
    pub kind: FontKind,
    pub device: bool,
    pub full_name: String,
    pub style: String,
    pub script: String,
    pub design: Option<FontDesign>,
    pub signature: Option<FontSignature>,
    /// The requested family is not installed; this is the face that draws it.
    pub substitute: bool,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FontSignature {
    pub unicode: [u32; 4],
    pub codepages: [u32; 2],
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum DefaultFont {
    Console,
    Fixed,
    Variable,
    System,
    Device,
    SystemFixed,
    Interface,
}
#[derive(Clone, Copy, Debug)]
pub enum Horizontal {
    Left,
    Center,
    Right,
}
#[derive(Clone, Copy, Debug)]
pub enum Vertical {
    Top,
    Baseline,
    Bottom,
}
#[derive(Clone, Copy, Debug)]
pub struct TextBox {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}
#[derive(Clone, Copy)]
pub struct Paint<'a> {
    pub input: TextInput<'a>,
    pub origin: Point,
    pub horizontal: Horizontal,
    pub vertical: Vertical,
    /// RGB colors resolved by the personality layer.
    pub foreground: [u8; 3],
    pub background: [u8; 3],
    pub opaque_background: bool,
    pub clip: Option<TextBox>,
    pub fill: Option<TextBox>,
}
pub const MAX_PAINT_UNITS: usize = 8192;
/// A bounded patch in logical top-down order; the original pixel format is kept.
pub struct Pixels<'a> {
    pub bounds: crate::surface::Rect,
    pub pitch: u32,
    pub format: crate::surface::PixelFormat,
    pub bytes: &'a mut [u8],
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
/// Signed 16.16 fixed point in row-major order, applied to glyph coordinates as
/// row vectors (x' = x*xx + y*yx, y' = x*xy + y*yy). This retains the exact caller
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

/// Integer logical spacing: left bearing, black width and right bearing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AbcWidths {
    pub left: i32,
    pub ink: u32,
    pub right: i32,
}

/// Coverage of the selected face, without linked-font fallback.
pub struct FontCoverage {
    pub flags: u32,
    pub glyphs: u32,
    pub ranges: Vec<(u16, u16)>,
}

/// Owned optional placement arrays; initialized values preserve native partial writes.
pub struct PlacementBuffers {
    pub text: Option<Vec<u16>>,
    pub encoded_text: Option<Vec<u8>>,
    pub order: Option<Vec<u32>>,
    pub advances: Option<Vec<i32>>,
    pub caret: Option<Vec<i32>>,
    pub classes: Option<Vec<u8>>,
    pub glyphs: Option<Vec<u16>>,
    pub capacity: u32,
    pub max_fit: i32,
}
pub struct Placement {
    pub extent: u32,
    pub buffers: Option<PlacementBuffers>,
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
/// Outline-font design metrics in the realized font's logical coordinate space.
/// Typographic ascent/descent are independent of the cell metrics above. Names
/// remain Unicode; a personality chooses its encoding and structure layout.
#[derive(Clone, Debug)]
pub struct OutlineMetrics {
    pub text: TextMetrics,
    pub panose: [u8; 10],
    pub selection: u32,
    pub embedding: u32,
    pub caret_rise: i32,
    pub caret_run: i32,
    pub italic_angle: i32,
    pub em_units: u32,
    pub ascent: i32,
    pub descent: i32,
    pub line_gap: u32,
    pub cap_height: u32,
    pub x_height: u32,
    pub bounds: TextBox,
    pub mac_ascent: i32,
    pub mac_descent: i32,
    pub mac_line_gap: u32,
    pub minimum_ppem: u32,
    pub subscript_size: Point,
    pub subscript_offset: Point,
    pub superscript_size: Point,
    pub superscript_offset: Point,
    pub strikeout_size: u32,
    pub strikeout_position: i32,
    pub underline_size: i32,
    pub underline_position: i32,
    /// Family, face, style and full names from the same selected outline font.
    pub names: [String; 4],
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
/// A Unicode pair and its signed advance adjustment in logical font units.
/// This is a backend result, not a packed guest KERNINGPAIR structure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KerningPair {
    pub first: u16,
    pub second: u16,
    pub adjustment: i32,
}
pub const MAX_KERNING_PAIRS: u32 = 1024 * 1024;
/// The actual portable face and rendering size, for reproducible references.
/// This reports the selected bytes, including a registered guest resource.
#[cfg(not(windows))]
#[derive(Clone, Debug, PartialEq)]
pub struct FontResolution {
    pub family: String,
    pub source: String,
    pub face_index: u32,
    pub sha256: String,
    pub font_bytes: usize,
    pub glyph_index: u32,
    pub default_glyph: bool,
    pub em_pixels: f32,
    pub cell_pixels: i32,
    pub horizontal_scale: f32,
    pub profile_sha256: Option<String>,
    pub backend: &'static str,
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
            Self::Unavailable => f.write_str("No font backend is available on this host"),
            Self::Font(message) | Self::Glyph(message) => f.write_str(message),
        }
    }
}

#[derive(Default)]
pub struct Text {
    overrides: Overrides,
    #[cfg(windows)]
    fonts: BTreeMap<FontRequest, windows::Font>,
    #[cfg(not(windows))]
    fonts: BTreeMap<FontRequest, portable::Font>,
    #[cfg(not(windows))]
    backend: portable::Backend,
    #[cfg(windows)]
    canvas: Option<windows::Canvas>,
    glyphs: BTreeMap<(FontRequest, GlyphRequest), Arc<Glyph>>,
    bytes: usize,
    hits: u64,
    #[cfg(windows)]
    resources: BTreeMap<FontResourceId, windows::FontResource>,
    #[cfg(windows)]
    next_resource: u64,
    #[cfg(windows)]
    resource_bytes: usize,
}

/// Explicit host font substitutions and text paint quality. Guest outline formats remain unchanged.
#[derive(Clone, Debug, Default)]
pub struct Overrides {
    pub families: BTreeMap<String, String>,
    pub antialiased: Option<bool>,
}
impl Overrides {
    fn apply(&self, request: &FontRequest) -> FontRequest {
        let mut result = request.clone();
        if let Some(face) = self.families.get(&request.face) {
            result.face = face.clone();
            result.default = None;
        }
        if let Some(smooth) = self.antialiased {
            result.quality = if smooth {
                Quality::Antialiased
            } else {
                Quality::Monochrome
            };
        }
        result
    }
}

/// A neutral, process-local font-data resource. Never a native or guest handle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct FontResourceId(u64);
pub const MAX_FONT_RESOURCES: usize = 64;
pub const MAX_FONT_RESOURCE_BYTES: usize = 64 * 1024 * 1024;
/// Each cached native font owns a font and a private measurement DC. Glyph
/// results are immutable and remain valid after the native owner is evicted.
pub const FONT_CACHE_CAPACITY: usize = 128;
/// Start the installed-font scan before the guest first needs it.
pub fn prefetch_system_fonts() {
    #[cfg(not(windows))]
    portable::prefetch_system_fonts();
}
impl Text {
    pub fn set_overrides(&mut self, overrides: Overrides) {
        self.invalidate_fonts();
        self.overrides = overrides;
    }

    /// Enumerate current backend fonts without exposing a native DC or font
    /// handle. Memory resources and hidden file resources are not enumerable.
    pub fn enumerate(
        &self,
        face: Option<&str>,
        mode: FontEnumeration,
    ) -> Result<Vec<EnumeratedFont>, TextError> {
        #[cfg(windows)]
        {
            windows::enumeration::enumerate(face, mode)
        }
        #[cfg(not(windows))]
        {
            self.backend.enumerate(face, mode)
        }
    }
    /// Install owned backend font data. Successful resource changes invalidate
    /// matching and glyph caches; immutable glyph results held by callers live on.
    pub fn add_resource(&mut self, bytes: &[u8]) -> Result<(FontResourceId, u32), TextError> {
        self.register_resource(bytes, None)
    }
    /// Register a file image read through the guest filesystem. The backend
    /// owns its private registration and copied data; file visibility is separate
    /// from memory resources, which are always hidden from enumeration.
    pub fn add_file_resource(
        &mut self,
        bytes: &[u8],
        enumerable: bool,
    ) -> Result<(FontResourceId, u32), TextError> {
        self.register_resource(bytes, Some(enumerable))
    }
    fn register_resource(
        &mut self,
        bytes: &[u8],
        file_visibility: Option<bool>,
    ) -> Result<(FontResourceId, u32), TextError> {
        #[cfg(windows)]
        {
            if bytes.is_empty()
                || self.resources.len() >= MAX_FONT_RESOURCES
                || bytes.len() > MAX_FONT_RESOURCE_BYTES.saturating_sub(self.resource_bytes)
            {
                return Err(TextError::Font("font resource budget exceeded".into()));
            }
            let next = self
                .next_resource
                .checked_add(1)
                .ok_or_else(|| TextError::Font("font resource identity exhausted".into()))?;
            let resource = windows::FontResource::new(bytes, file_visibility)?;
            let count = resource.faces();
            self.invalidate_fonts();
            let id = FontResourceId(next);
            self.resources.insert(id, resource);
            self.next_resource = next;
            self.resource_bytes += bytes.len();
            Ok((id, count))
        }
        #[cfg(not(windows))]
        {
            let result = self
                .backend
                .add_resource(bytes, file_visibility.unwrap_or(false))?;
            self.invalidate_fonts();
            Ok(result)
        }
    }

    /// Unregister before retiring the cached native font references. A failed
    /// backend operation preserves the resource and all existing cache owners.
    pub fn remove_resource(&mut self, id: FontResourceId) -> Result<bool, TextError> {
        #[cfg(windows)]
        {
            let Some(resource) = self.resources.get_mut(&id) else {
                return Ok(false);
            };
            resource.remove()?;
            self.invalidate_fonts();
            let resource = self.resources.remove(&id).unwrap();
            self.resource_bytes -= resource.bytes();
            Ok(true)
        }
        #[cfg(not(windows))]
        {
            let removed = self.backend.remove_resource(id);
            if removed {
                self.invalidate_fonts();
            }
            Ok(removed)
        }
    }
    pub fn resource_stats(&self) -> (usize, usize) {
        #[cfg(windows)]
        {
            (self.resources.len(), self.resource_bytes)
        }
        #[cfg(not(windows))]
        {
            self.backend.resource_stats()
        }
    }
    /// Refresh font matching after a resource reference changes without a new
    /// backend registration (for example a personality's repeated add/remove).
    pub fn invalidate_fonts(&mut self) {
        self.fonts.clear();
        self.glyphs.clear();
        self.bytes = 0;
    }
    /// Rasterize a complete counted run with the same selected font as queries.
    pub fn paint(
        &mut self,
        request: &FontRequest,
        paint: Paint<'_>,
        pixels: Pixels<'_>,
    ) -> Result<(), TextError> {
        self.paint_inner(request, paint, pixels, None)
    }

    /// Paint real monochrome indices, including duplicate palette colours.
    pub fn paint_monochrome(
        &mut self,
        request: &FontRequest,
        paint: Paint<'_>,
        pixels: Pixels<'_>,
        palette: [[u8; 4]; 2],
    ) -> Result<(), TextError> {
        if pixels.format != crate::surface::PixelFormat::Indexed1 {
            return Err(TextError::Font(
                "monochrome text requires one-bit storage".into(),
            ));
        }
        self.paint_inner(request, paint, pixels, Some(palette))
    }

    fn paint_inner(
        &mut self,
        request: &FontRequest,
        paint: Paint<'_>,
        pixels: Pixels<'_>,
        palette: Option<[[u8; 4]; 2]>,
    ) -> Result<(), TextError> {
        // Pixels carries no color table. Passing an indexed BITMAPINFO with only
        // the direct-color masks would make GDI read beyond that native buffer.
        if pixels.format.indexed() && palette.is_none() {
            return Err(TextError::Font("indexed text requires a palette".into()));
        }
        let units = match paint.input {
            TextInput::Encoded(bytes) => bytes.len(),
            TextInput::Unicode(units) => units.len(),
        };
        if units > MAX_PAINT_UNITS {
            return Err(TextError::Font("text run exceeds paint limit".into()));
        }
        let row = u64::from(pixels.bounds.w)
            .checked_mul(u64::from(pixels.format.bits()))
            .map(|bits| bits.div_ceil(8));
        let length = pixels.pitch.checked_mul(pixels.bounds.h);
        if row.is_none_or(|row| row > u64::from(pixels.pitch))
            || length.is_none_or(|length| {
                length as usize > pixels.bytes.len() || length > 32 * 1024 * 1024
            })
            || pixels
                .bounds
                .x
                .checked_add(pixels.bounds.w)
                .is_none_or(|n| n > i32::MAX as u32)
            || pixels
                .bounds
                .y
                .checked_add(pixels.bounds.h)
                .is_none_or(|n| n > i32::MAX as u32)
            || pixels.bounds.w == 0
            || pixels.bounds.h == 0
        {
            return Err(TextError::Font("invalid text pixel storage".into()));
        }
        #[cfg(windows)]
        {
            self.font(request)?;
            let canvas = self.canvas.get_or_insert_with(windows::Canvas::default);
            self.fonts[request].paint(canvas, paint, pixels, palette)
        }
        #[cfg(not(windows))]
        {
            if let Some(palette) = palette {
                monochrome::paint(self, request, paint, pixels, palette)
            } else {
                portable::paint(self, request, paint, pixels)
            }
        }
    }
    pub fn stats(&self) -> CacheStats {
        CacheStats {
            fonts: self.font_count(),
            glyphs: self.glyphs.len(),
            bytes: self.bytes,
            hits: self.hits,
        }
    }
    fn font_count(&self) -> usize {
        self.fonts.len()
    }
    #[cfg(windows)]
    fn font(&mut self, request: &FontRequest) -> Result<&windows::Font, TextError> {
        if !self.fonts.contains_key(request) {
            let font = windows::Font::new(&self.overrides.apply(request))?;
            if self.fonts.len() >= FONT_CACHE_CAPACITY {
                self.fonts.clear();
            }
            self.fonts.insert(request.clone(), font);
        }
        Ok(&self.fonts[request])
    }
    #[cfg(not(windows))]
    fn font(&mut self, request: &FontRequest) -> Result<&portable::Font, TextError> {
        if !self.fonts.contains_key(request) {
            let font = self.backend.font(&self.overrides.apply(request))?;
            if self.fonts.len() >= FONT_CACHE_CAPACITY {
                self.fonts.clear();
            }
            self.fonts.insert(request.clone(), font);
        }
        Ok(&self.fonts[request])
    }
    /// Name enumerated families in the guest UI language (a BCP 47 tag such
    /// as "zh-Hans") when a font provides it, as GDI does. Native GDI already
    /// follows the real UI language.
    pub fn set_language(&mut self, tag: &'static str) {
        #[cfg(not(windows))]
        {
            self.backend.language = Some(tag);
        }
        #[cfg(windows)]
        let _ = tag;
    }
    /// Use only these discovery directories on portable hosts. This provides a
    /// deterministic font environment for comparisons; guest resources still
    /// take precedence. Default construction discovers system fonts and the
    /// additional directories in ADV_FONT_DIR (the host's path-list syntax).
    #[cfg(not(windows))]
    pub fn with_font_directories(directories: &[std::path::PathBuf]) -> Self {
        let mut text = Self::default();
        text.backend = portable::Backend::with_directories(directories);
        text
    }
    /// Load a pinned, isolated portable font profile and validate all files,
    /// SHA-256 hashes, collection indices and family names before returning.
    /// Production default construction uses the same profile when
    /// ADV_FONT_PROFILE is set. Guest fonts retain exact-family precedence.
    #[cfg(not(windows))]
    pub fn with_font_profile(path: &std::path::Path) -> Result<Self, TextError> {
        let backend = portable::Backend::with_profile(path)?;
        let mut text = Self::default();
        text.backend = backend;
        Ok(text)
    }
    /// Resolve a Unicode character using the same primary/coverage/default
    /// selection as metrics, extent and raster queries. This is diagnostic data,
    /// never a font handle or a guest pointer.
    #[cfg(not(windows))]
    pub fn resolved_character(
        &mut self,
        request: &FontRequest,
        code: u32,
    ) -> Result<FontResolution, TextError> {
        self.font(request)?.resolved_character(code)
    }
    /// Describe the logical font retained by the existing provider. Default
    /// requests use the same selected face/profile as measurement and drawing.
    pub fn logical_font(&mut self, request: &FontRequest) -> Result<FontRequest, TextError> {
        #[cfg(windows)]
        {
            self.font(request)?.logical_font()
        }
        #[cfg(not(windows))]
        {
            Ok(self.font(request)?.logical_font())
        }
    }
    pub fn metrics(&mut self, request: &FontRequest) -> Result<TextMetrics, TextError> {
        #[cfg(windows)]
        {
            self.font(request)?.metrics()
        }
        #[cfg(not(windows))]
        {
            Ok(self.font(request)?.metrics())
        }
    }
    pub fn outline_metrics(&mut self, request: &FontRequest) -> Result<OutlineMetrics, TextError> {
        #[cfg(windows)]
        {
            self.font(request)?.outline_metrics()
        }
        #[cfg(not(windows))]
        {
            self.font(request)?.outline_metrics()
        }
    }
    /// Unicode range, default and break characters from the same realized font
    /// that owns numeric metrics and glyph rasterization. These units cannot be
    /// reconstructed by widening the encoded-byte metrics.
    pub fn unicode_metrics(
        &mut self,
        request: &FontRequest,
    ) -> Result<(TextMetrics, [u16; 4]), TextError> {
        self.font(request)?.unicode_metrics()
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
            self.font(request)?.extent(input)
        }
    }
    /// Query the selected font's complete Unicode kerning-pair count using the
    /// same realized-font owner as metrics, extents and glyph rasterization.
    pub fn kerning_pair_count(&mut self, request: &FontRequest) -> Result<u32, TextError> {
        #[cfg(windows)]
        {
            Ok(self.font(request)?.kerning_pair_count())
        }
        #[cfg(not(windows))]
        {
            let _ = request;
            Err(TextError::Font(
                "complete Unicode kerning-pair enumeration is unavailable on the portable font backend"
                    .into(),
            ))
        }
    }
    /// Copy at most capacity real pairs in provider order. The native provider
    /// supports partial buffers, preserving its signed logical-unit adjustments.
    pub fn kerning_pairs(
        &mut self,
        request: &FontRequest,
        capacity: u32,
    ) -> Result<Vec<KerningPair>, TextError> {
        if capacity == 0 || capacity > MAX_KERNING_PAIRS {
            return Err(TextError::Font("invalid kerning-pair capacity".into()));
        }
        #[cfg(windows)]
        {
            self.font(request)?.kerning_pairs(capacity)
        }
        #[cfg(not(windows))]
        {
            let _ = request;
            Err(TextError::Font(
                "complete Unicode kerning-pair enumeration is unavailable on the portable font backend"
                    .into(),
            ))
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
        let result = self.font(font).and_then(|font| font.glyph(request));
        let glyph = match result {
            Ok(glyph) => Arc::new(glyph),
            Err(error) => {
                if std::env::var_os("ADV_TEXT_DIAGNOSTICS").is_some() {
                    eprintln!(
                        "adv-text glyph failure: font={font:?} request={request:?} error={error}"
                    );
                }
                return Err(error);
            }
        };
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

    /// Glyph selectors belong to the selected font, without character fallback.
    pub fn glyph_widths(
        &mut self,
        font: &FontRequest,
        first: u32,
        count: u32,
        indices: Option<&[u16]>,
    ) -> Result<Vec<i32>, TextError> {
        validate_indices(count, indices)?;
        self.font(font)?.glyph_widths(first, count, indices)
    }

    pub fn glyph_abc_widths(
        &mut self,
        font: &FontRequest,
        first: u32,
        count: u32,
        indices: Option<&[u16]>,
    ) -> Result<Vec<AbcWidths>, TextError> {
        validate_indices(count, indices)?;
        if count == 0 {
            return Err(TextError::Font("empty ABC query".into()));
        }
        self.font(font)?.glyph_abc_widths(first, count, indices)
    }

    pub fn character_abc_widths(
        &mut self,
        font: &FontRequest,
        characters: &[Character],
    ) -> Result<Vec<AbcWidths>, TextError> {
        if characters.is_empty() || characters.len() > 65536 {
            return Err(TextError::Font("invalid ABC query length".into()));
        }
        self.font(font)?.character_abc_widths(characters)
    }

    pub fn unicode_ranges(&mut self, font: &FontRequest) -> Result<FontCoverage, TextError> {
        self.font(font)?.unicode_ranges()
    }

    pub fn language_info(&mut self, font: &FontRequest) -> Result<u32, TextError> {
        #[cfg(windows)]
        {
            self.font(font)?.language_info()
        }
        #[cfg(not(windows))]
        {
            let _ = font;
            Err(TextError::Font(
                "complete font language metadata is unavailable on the portable backend".into(),
            ))
        }
    }

    pub fn placement(
        &mut self,
        font: &FontRequest,
        input: TextInput<'_>,
        maximum: i32,
        flags: u32,
        buffers: Option<PlacementBuffers>,
        ansi_codepage: u32,
    ) -> Result<Placement, TextError> {
        #[cfg(windows)]
        {
            self.font(font)?
                .placement(input, maximum, flags, buffers, ansi_codepage)
        }
        #[cfg(not(windows))]
        {
            let _ = (font, input, maximum, flags, buffers, ansi_codepage);
            Err(TextError::Font(
                "native character placement and shaping are unavailable on the portable backend"
                    .into(),
            ))
        }
    }
}

fn validate_indices(count: u32, indices: Option<&[u16]>) -> Result<(), TextError> {
    if count > 65536 || indices.is_some_and(|indices| indices.len() != count as usize) {
        Err(TextError::Font("invalid glyph query length".into()))
    } else {
        Ok(())
    }
}

impl Drop for Text {
    fn drop(&mut self) {
        // Restore/delete cached selections before the resource owners retire.
        self.invalidate_fonts();
        #[cfg(windows)]
        self.resources.clear();
    }
}

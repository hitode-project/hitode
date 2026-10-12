//! Native rasterization adapter. Every GDI handle is private and owned here;
//! request/result types carry neither Win32 handles nor packed ABI structures.
use super::*;
use windows_sys::Win32::Graphics::Gdi::{self as gdi, HDC, HFONT, HGDIOBJ};

pub(super) mod enumeration;

pub(super) enum FontResource {
    Memory(MemoryFont),
    File(FileFont),
}
impl FontResource {
    pub(super) fn new(bytes: &[u8], file_visibility: Option<bool>) -> Result<Self, TextError> {
        match file_visibility {
            Some(enumerable) => FileFont::new(bytes, enumerable).map(Self::File),
            None => MemoryFont::new(bytes).map(Self::Memory),
        }
    }
    pub(super) fn faces(&self) -> u32 {
        match self {
            Self::Memory(resource) => resource.faces,
            Self::File(resource) => resource.faces,
        }
    }
    pub(super) fn bytes(&self) -> usize {
        match self {
            Self::Memory(resource) => resource.bytes,
            Self::File(resource) => resource.bytes,
        }
    }
    pub(super) fn remove(&mut self) -> Result<(), TextError> {
        match self {
            Self::Memory(resource) => resource.remove(),
            Self::File(resource) => resource.remove(),
        }
    }
}

/// GDI file registration supplies native enumeration metadata and matching.
/// The copied VFS image and its native reference retire with the resource owner.
pub(super) struct FileFont {
    path: std::path::PathBuf,
    wide_path: Vec<u16>,
    flags: u32,
    registered: bool,
    faces: u32,
    bytes: usize,
}
impl FileFont {
    fn new(bytes: &[u8], enumerable: bool) -> Result<Self, TextError> {
        use std::{
            io::Write,
            os::windows::ffi::OsStrExt,
            sync::atomic::{AtomicU64, Ordering},
        };
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let mut resource = None;
        for _ in 0..64 {
            let serial = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("adv-host-font-{}-{serial}.ttf", std::process::id()));
            let mut file = match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(TextError::Font(format!("font resource copy: {error}"))),
            };
            let owner = Self {
                wide_path: path.as_os_str().encode_wide().chain([0]).collect(),
                path,
                // Every host registration belongs to this guest's process owner;
                // guest session visibility never installs fonts for other apps.
                flags: gdi::FR_PRIVATE | if enumerable { 0 } else { gdi::FR_NOT_ENUM },
                registered: false,
                faces: 0,
                bytes: bytes.len(),
            };
            let written = file.write_all(bytes);
            drop(file);
            written.map_err(|error| TextError::Font(format!("font resource copy: {error}")))?;
            resource = Some(owner);
            break;
        }
        let Some(mut resource) = resource else {
            return Err(TextError::Font(
                "font resource copy identity exhausted".into(),
            ));
        };
        // SAFETY: terminated host path to the exclusively owned VFS copy. The
        // private flag is unconditional; the reserved parameter is NULL.
        let faces = unsafe {
            gdi::AddFontResourceExW(
                resource.wide_path.as_ptr(),
                resource.flags,
                std::ptr::null(),
            )
        };
        if faces <= 0 {
            return Err(TextError::Font("invalid font resource".into()));
        }
        resource.registered = true;
        resource.faces = faces as u32;
        Ok(resource)
    }
    fn remove(&mut self) -> Result<(), TextError> {
        // SAFETY: the same owned path and characteristics used for registration.
        if unsafe {
            gdi::RemoveFontResourceExW(self.wide_path.as_ptr(), self.flags, std::ptr::null())
        } == 0
        {
            return Err(TextError::Font("font resource removal failed".into()));
        }
        self.registered = false;
        Ok(())
    }
}
impl Drop for FileFont {
    fn drop(&mut self) {
        if self.registered
            && let Err(error) = self.remove()
        {
            log::warn!("{error}");
            return;
        }
        if let Err(error) = std::fs::remove_file(&self.path) {
            log::warn!("font resource copy cleanup: {error}");
        }
    }
}

pub(super) struct MemoryFont {
    handle: windows_sys::Win32::Foundation::HANDLE,
    pub faces: u32,
    pub bytes: usize,
}
impl MemoryFont {
    pub(super) fn new(bytes: &[u8]) -> Result<Self, TextError> {
        let length = u32::try_from(bytes.len())
            .map_err(|_| TextError::Font("font resource is too large".into()))?;
        let mut faces = 0;
        // SAFETY: bounded immutable input and writable host count; GDI copies
        // the data and makes it private to this process, never enumerable.
        let handle = unsafe {
            gdi::AddFontMemResourceEx(
                bytes.as_ptr().cast(),
                length,
                std::ptr::null(),
                &raw mut faces,
            )
        };
        if handle.is_null() {
            return Err(TextError::Font("invalid font resource".into()));
        }
        let mut resource = Self {
            handle,
            faces,
            bytes: bytes.len(),
        };
        if faces == 0 {
            resource.remove()?;
            return Err(TextError::Font("font resource contains no faces".into()));
        }
        Ok(resource)
    }
    pub(super) fn remove(&mut self) -> Result<(), TextError> {
        // SAFETY: this owner has the sole registration handle. Existing HFONT
        // objects retain their own GDI references until the Text cache is cleared.
        if unsafe { gdi::RemoveFontMemResourceEx(self.handle) } == 0 {
            return Err(TextError::Font("font resource removal failed".into()));
        }
        self.handle = std::ptr::null_mut();
        Ok(())
    }
}
impl Drop for MemoryFont {
    fn drop(&mut self) {
        if !self.handle.is_null()
            && let Err(error) = self.remove()
        {
            log::warn!("{error}");
        }
    }
}

pub(super) struct Font {
    dc: HDC,
    font: HFONT,
    old: HGDIOBJ,
    owned: bool,
}

fn encoding(value: Encoding) -> u8 {
    match value {
        Encoding::Latin => 0,
        Encoding::Automatic => 1,
        Encoding::Symbol => 2,
        Encoding::MacRoman => 77,
        Encoding::Japanese => 128,
        Encoding::Korean => 129,
        Encoding::Johab => 130,
        Encoding::SimplifiedChinese => 134,
        Encoding::TraditionalChinese => 136,
        Encoding::Greek => 161,
        Encoding::Turkish => 162,
        Encoding::Vietnamese => 163,
        Encoding::Hebrew => 177,
        Encoding::Arabic => 178,
        Encoding::Baltic => 186,
        Encoding::Cyrillic => 204,
        Encoding::Thai => 222,
        Encoding::EasternEuropean => 238,
        Encoding::Oem => 255,
        Encoding::Reserved(value) => value,
    }
}
fn precision(value: Precision) -> u8 {
    match value {
        Precision::Default => 0,
        Precision::String => 1,
        Precision::Character => 2,
        Precision::Stroke => 3,
        Precision::TrueType => 4,
        Precision::Device => 5,
        Precision::Raster => 6,
        Precision::PreferTrueType => 7,
        Precision::Outline => 8,
        Precision::ScreenOutline => 9,
        Precision::PostScript => 10,
        Precision::Reserved(value) => value,
    }
}
fn quality(value: Quality) -> u8 {
    match value {
        Quality::Default => 0,
        Quality::Draft => 1,
        Quality::Proof => 2,
        Quality::Monochrome => 3,
        Quality::Antialiased => 4,
        Quality::Subpixel => 5,
        Quality::SubpixelCompatible => 6,
        Quality::Reserved(value) => value,
    }
}
fn pitch(value: Pitch) -> u8 {
    match value {
        Pitch::Default => 0,
        Pitch::Fixed => 1,
        Pitch::Variable => 2,
        Pitch::Reserved(value) => value,
    }
}
fn clip_precision(value: ClipPrecision) -> u8 {
    match value {
        ClipPrecision::Default => 0,
        ClipPrecision::Character => 1,
        ClipPrecision::Stroke => 2,
        ClipPrecision::Reserved(value) => value,
    }
}
fn family(value: Family) -> u8 {
    match value {
        Family::Default => 0,
        Family::Serif => 1,
        Family::SansSerif => 2,
        Family::Monospace => 3,
        Family::Script => 4,
        Family::Decorative => 5,
        Family::Reserved(value) => value,
    }
}
fn decode_encoding(value: u8) -> Encoding {
    match value {
        0 => Encoding::Latin,
        1 => Encoding::Automatic,
        2 => Encoding::Symbol,
        77 => Encoding::MacRoman,
        128 => Encoding::Japanese,
        129 => Encoding::Korean,
        130 => Encoding::Johab,
        134 => Encoding::SimplifiedChinese,
        136 => Encoding::TraditionalChinese,
        161 => Encoding::Greek,
        162 => Encoding::Turkish,
        163 => Encoding::Vietnamese,
        177 => Encoding::Hebrew,
        178 => Encoding::Arabic,
        186 => Encoding::Baltic,
        204 => Encoding::Cyrillic,
        222 => Encoding::Thai,
        238 => Encoding::EasternEuropean,
        255 => Encoding::Oem,
        value => Encoding::Reserved(value),
    }
}
fn decode_family(value: u8) -> Family {
    match value {
        0 => Family::Default,
        1 => Family::Serif,
        2 => Family::SansSerif,
        3 => Family::Monospace,
        4 => Family::Script,
        5 => Family::Decorative,
        value => Family::Reserved(value),
    }
}
impl Font {
    pub(super) fn encoded_character_width(&self, code: u16) -> Result<i32, TextError> {
        let mut width = 0;
        // SAFETY: the selected private font/DC belongs to this provider, and
        // the initialized scalar covers the one-character output completely.
        if unsafe { gdi::GetCharWidth32A(self.dc, u32::from(code), u32::from(code), &mut width) }
            == 0
        {
            return Err(TextError::Font(
                "encoded character width query failed".into(),
            ));
        }
        Ok(width)
    }
    pub(super) fn character_widths(&self, first: u16, last: u16) -> Result<Vec<i32>, TextError> {
        let mut widths = vec![0; usize::from(last) - usize::from(first) + 1];
        // SAFETY: private selected font/DC, and a writable allocation covering
        // the entire inclusive Unicode range. No guest address reaches GDI.
        if unsafe {
            gdi::GetCharWidth32W(
                self.dc,
                u32::from(first),
                u32::from(last),
                widths.as_mut_ptr(),
            )
        } == 0
        {
            return Err(TextError::Font("character width query failed".into()));
        }
        Ok(widths)
    }
    pub(super) fn paint(
        &self,
        canvas: &mut Canvas,
        paint: Paint<'_>,
        pixels: Pixels<'_>,
        palette: Option<[[u8; 4]; 2]>,
    ) -> Result<(), TextError> {
        canvas.ensure(pixels.bounds.w, pixels.bounds.h, pixels.format, palette)?;
        let row = pixels.format.row_bytes(pixels.bounds.w) as usize;
        // SAFETY: ensure owns a DIB of at least these dimensions; its storage is
        // unselected between calls, and GdiFlush completes each render before read.
        unsafe {
            for y in 0..pixels.bounds.h as usize {
                std::ptr::copy_nonoverlapping(
                    pixels.bytes.as_ptr().add(y * pixels.pitch as usize),
                    canvas.bits.add(y * canvas.pitch),
                    row,
                );
            }
            let saved = gdi::SaveDC(self.dc);
            if saved == 0 {
                return Err(TextError::Font("SaveDC failed".into()));
            }
            let previous = gdi::GetCurrentObject(self.dc, gdi::OBJ_BITMAP as u32);
            let result = (|| {
                let old = gdi::SelectObject(self.dc, canvas.bitmap);
                if old.is_null() || old as usize == usize::MAX {
                    return Err(TextError::Font("text bitmap selection failed".into()));
                }
                gdi::SetViewportOrgEx(
                    self.dc,
                    -(pixels.bounds.x as i32),
                    -(pixels.bounds.y as i32),
                    std::ptr::null_mut(),
                );
                gdi::IntersectClipRect(
                    self.dc,
                    pixels.bounds.x as i32,
                    pixels.bounds.y as i32,
                    (pixels.bounds.x + pixels.bounds.w) as i32,
                    (pixels.bounds.y + pixels.bounds.h) as i32,
                );
                if let Some(r) = paint.clip {
                    gdi::IntersectClipRect(self.dc, r.left, r.top, r.right, r.bottom);
                }
                let color = |rgb: [u8; 3]| {
                    u32::from(rgb[0]) | (u32::from(rgb[1]) << 8) | (u32::from(rgb[2]) << 16)
                };
                gdi::SetTextColor(self.dc, color(paint.foreground));
                gdi::SetBkColor(self.dc, color(paint.background));
                gdi::SetBkMode(self.dc, if paint.opaque_background { 2 } else { 1 });
                let align = match paint.horizontal {
                    Horizontal::Left => 0,
                    Horizontal::Center => 6,
                    Horizontal::Right => 2,
                } | match paint.vertical {
                    Vertical::Top => 0,
                    Vertical::Baseline => 24,
                    Vertical::Bottom => 8,
                };
                gdi::SetTextAlign(self.dc, align);
                let rect = paint.fill.map(|r| windows_sys::Win32::Foundation::RECT {
                    left: r.left,
                    top: r.top,
                    right: r.right,
                    bottom: r.bottom,
                });
                let rectptr = rect.as_ref().map_or(std::ptr::null(), |r| r as *const _);
                let options = if rect.is_some() { 2 } else { 0 };
                let ok = match paint.input {
                    TextInput::Encoded(bytes) => gdi::ExtTextOutA(
                        self.dc,
                        paint.origin.x,
                        paint.origin.y,
                        options,
                        rectptr,
                        bytes.as_ptr(),
                        bytes.len() as u32,
                        std::ptr::null(),
                    ),
                    TextInput::Unicode(units) => gdi::ExtTextOutW(
                        self.dc,
                        paint.origin.x,
                        paint.origin.y,
                        options,
                        rectptr,
                        units.as_ptr(),
                        units.len() as u32,
                        std::ptr::null(),
                    ),
                };
                if ok == 0 {
                    return Err(TextError::Font("native text paint failed".into()));
                }
                if gdi::GdiFlush() == 0 {
                    return Err(TextError::Font("native text flush failed".into()));
                }
                Ok(())
            })();
            // Restore before any error return; no cached DC retains the bitmap.
            if gdi::RestoreDC(self.dc, saved) == 0 {
                // Even a failed state restoration must not retain a scratch
                // selection when its owner is later replaced or dropped.
                gdi::SelectObject(self.dc, previous);
                return Err(TextError::Font("RestoreDC failed".into()));
            }
            result?;
            for y in 0..pixels.bounds.h as usize {
                std::ptr::copy_nonoverlapping(
                    canvas.bits.add(y * canvas.pitch),
                    pixels.bytes.as_mut_ptr().add(y * pixels.pitch as usize),
                    row,
                );
            }
        }
        Ok(())
    }
    pub(super) fn new(request: &FontRequest) -> Result<Self, TextError> {
        if request.height.unsigned_abs() > 16384 || request.width.unsigned_abs() > 16384 {
            return Err(TextError::Font("invalid font dimensions".into()));
        }
        let clip = request.clipping;
        let mut logical = gdi::LOGFONTW {
            lfHeight: request.height,
            lfWidth: request.width,
            lfEscapement: request.baseline_angle,
            lfOrientation: request.glyph_angle,
            lfWeight: request.weight,
            lfItalic: u8::from(request.italic),
            lfUnderline: u8::from(request.underline),
            lfStrikeOut: u8::from(request.strikeout),
            lfCharSet: encoding(request.encoding),
            lfOutPrecision: precision(request.precision),
            lfClipPrecision: clip_precision(clip.precision)
                | (u8::from(clip.transform_rotation) << 4)
                | (u8::from(clip.force_outline) << 5)
                | (u8::from(clip.disable_association) << 6)
                | (u8::from(clip.embedded) << 7),
            lfQuality: quality(request.quality),
            lfPitchAndFamily: pitch(request.pitch) | (family(request.family) << 4),
            ..Default::default()
        };
        for (slot, unit) in logical
            .lfFaceName
            .iter_mut()
            .take(31)
            .zip(request.face.encode_utf16())
        {
            *slot = unit;
        }
        // SAFETY: initialized host LOGFONT; each acquired handle is released on
        // failure or by Drop after restoring the original DC selection.
        unsafe {
            let dc = gdi::CreateCompatibleDC(std::ptr::null_mut());
            if dc.is_null() {
                return Err(TextError::Font("CreateCompatibleDC failed".into()));
            }
            let owned = request.default.is_none();
            let font = if let Some(kind) = request.default {
                gdi::GetStockObject(match kind {
                    DefaultFont::Console => 10,
                    DefaultFont::Fixed => 11,
                    DefaultFont::Variable => 12,
                    DefaultFont::System => 13,
                    DefaultFont::Device => 14,
                    DefaultFont::SystemFixed => 16,
                    DefaultFont::Interface => 17,
                })
            } else {
                gdi::CreateFontIndirectW(&logical)
            };
            if font.is_null() {
                gdi::DeleteDC(dc);
                return Err(TextError::Font("CreateFontIndirectW failed".into()));
            }
            let old = gdi::SelectObject(dc, font);
            if old.is_null() || old as usize == usize::MAX {
                if owned {
                    gdi::DeleteObject(font);
                }
                gdi::DeleteDC(dc);
                return Err(TextError::Font("SelectObject failed".into()));
            }
            Ok(Self {
                dc,
                font,
                old,
                owned,
            })
        }
    }
    pub(super) fn logical_font(&self) -> Result<FontRequest, TextError> {
        let mut logical = gdi::LOGFONTW::default();
        // SAFETY: this font is owned or retained as a stock object by this
        // provider; the output covers exactly one initialized host LOGFONTW.
        if unsafe {
            gdi::GetObjectW(
                self.font,
                std::mem::size_of_val(&logical) as i32,
                (&mut logical as *mut gdi::LOGFONTW).cast(),
            )
        } != std::mem::size_of_val(&logical) as i32
        {
            return Err(TextError::Font("native logical font query failed".into()));
        }
        Ok(enumeration::request(&logical))
    }
    pub(super) fn metrics(&self) -> Result<TextMetrics, TextError> {
        let mut m = gdi::TEXTMETRICA::default();
        // SAFETY: self owns the DC; m is initialized writable host storage.
        if unsafe { gdi::GetTextMetricsA(self.dc, &mut m) } == 0 {
            return Err(TextError::Font("GetTextMetricsA failed".into()));
        }
        Ok(TextMetrics {
            height: m.tmHeight,
            ascent: m.tmAscent,
            descent: m.tmDescent,
            internal_leading: m.tmInternalLeading,
            external_leading: m.tmExternalLeading,
            average_width: m.tmAveCharWidth,
            maximum_width: m.tmMaxCharWidth,
            weight: m.tmWeight,
            overhang: m.tmOverhang,
            aspect_x: m.tmDigitizedAspectX,
            aspect_y: m.tmDigitizedAspectY,
            first_character: m.tmFirstChar,
            last_character: m.tmLastChar,
            default_character: m.tmDefaultChar,
            break_character: m.tmBreakChar,
            italic: m.tmItalic != 0,
            underline: m.tmUnderlined != 0,
            strikeout: m.tmStruckOut != 0,
            variable_pitch: m.tmPitchAndFamily & 1 != 0,
            vector: m.tmPitchAndFamily & 2 != 0,
            outline: m.tmPitchAndFamily & 4 != 0,
            device: m.tmPitchAndFamily & 8 != 0,
            family: decode_family(m.tmPitchAndFamily >> 4),
            encoding: decode_encoding(m.tmCharSet),
        })
    }
    pub(super) fn unicode_metrics(&self) -> Result<(TextMetrics, [u16; 4]), TextError> {
        let mut wide = gdi::TEXTMETRICW::default();
        // SAFETY: the font's existing private DC is selected, and wide is
        // initialized writable storage for the complete native structure.
        if unsafe { gdi::GetTextMetricsW(self.dc, &mut wide) } == 0 {
            return Err(TextError::Font("GetTextMetricsW failed".into()));
        }
        // Numeric metrics, styles and realized charset are shared by A/W on
        // this same selected font. Only the four character fields differ;
        // return those native UTF-16 units without host-ACP conversion.
        Ok((
            self.metrics()?,
            [
                wide.tmFirstChar,
                wide.tmLastChar,
                wide.tmDefaultChar,
                wide.tmBreakChar,
            ],
        ))
    }
    pub(super) fn outline_metrics(&self) -> Result<OutlineMetrics, TextError> {
        // SAFETY: this font's private DC retains the same realized HFONT used
        // by glyph rasterization; NULL output only queries the required size.
        let size = unsafe { gdi::GetOutlineTextMetricsW(self.dc, 0, std::ptr::null_mut()) };
        if size < std::mem::size_of::<gdi::OUTLINETEXTMETRICW>() as u32 || size > 65536 {
            return Err(TextError::Font("outline font metrics unavailable".into()));
        }
        // u64 storage supplies the native structure's pointer alignment on all
        // supported Windows hosts, including the trailing UTF-16 names.
        let mut storage = vec![0u64; (size as usize).div_ceil(8)];
        let pointer = storage.as_mut_ptr().cast::<gdi::OUTLINETEXTMETRICW>();
        // SAFETY: initialized, aligned host storage covers the queried byte
        // count. GDI receives no guest pointer or guest-owned handle.
        if unsafe { gdi::GetOutlineTextMetricsW(self.dc, size, pointer) } != size {
            return Err(TextError::Font("outline font metrics read failed".into()));
        }
        // SAFETY: the complete initialized structure was returned above.
        let m = unsafe { &*pointer };
        let names = [
            m.otmpFamilyName,
            m.otmpFaceName,
            m.otmpStyleName,
            m.otmpFullName,
        ]
        .map(|offset| {
            let start = offset as usize;
            if start % 2 != 0 || start >= size as usize {
                return Err(TextError::Font("invalid outline font name offset".into()));
            }
            // SAFETY: the offset and count stay within the initialized native
            // buffer, and UTF-16 alignment follows from the even offset.
            let units = unsafe {
                std::slice::from_raw_parts(
                    storage.as_ptr().cast::<u8>().add(start).cast::<u16>(),
                    (size as usize - start) / 2,
                )
            };
            let end = units
                .iter()
                .position(|&unit| unit == 0)
                .ok_or_else(|| TextError::Font("unterminated outline font name".into()))?;
            String::from_utf16(&units[..end])
                .map_err(|_| TextError::Font("invalid outline font name".into()))
        });
        let [family, face, style, full] = names;
        let point = |p: windows_sys::Win32::Foundation::POINT| Point { x: p.x, y: p.y };
        let p = m.otmPanoseNumber;
        Ok(OutlineMetrics {
            text: self.metrics()?,
            panose: [
                p.bFamilyType,
                p.bSerifStyle,
                p.bWeight,
                p.bProportion,
                p.bContrast,
                p.bStrokeVariation,
                p.bArmStyle,
                p.bLetterform,
                p.bMidline,
                p.bXHeight,
            ],
            selection: m.otmfsSelection,
            embedding: m.otmfsType,
            caret_rise: m.otmsCharSlopeRise,
            caret_run: m.otmsCharSlopeRun,
            italic_angle: m.otmItalicAngle,
            em_units: m.otmEMSquare,
            ascent: m.otmAscent,
            descent: m.otmDescent,
            line_gap: m.otmLineGap,
            cap_height: m.otmsCapEmHeight,
            x_height: m.otmsXHeight,
            bounds: TextBox {
                left: m.otmrcFontBox.left,
                top: m.otmrcFontBox.top,
                right: m.otmrcFontBox.right,
                bottom: m.otmrcFontBox.bottom,
            },
            mac_ascent: m.otmMacAscent,
            mac_descent: m.otmMacDescent,
            mac_line_gap: m.otmMacLineGap,
            minimum_ppem: m.otmusMinimumPPEM,
            subscript_size: point(m.otmptSubscriptSize),
            subscript_offset: point(m.otmptSubscriptOffset),
            superscript_size: point(m.otmptSuperscriptSize),
            superscript_offset: point(m.otmptSuperscriptOffset),
            strikeout_size: m.otmsStrikeoutSize,
            strikeout_position: m.otmsStrikeoutPosition,
            underline_size: m.otmsUnderscoreSize,
            underline_position: m.otmsUnderscorePosition,
            names: [family?, face?, style?, full?],
        })
    }
    pub(super) fn kerning_pair_count(&self) -> u32 {
        // SAFETY: this font's private measurement DC retains its selected HFONT;
        // NULL output queries the complete native count without allocating.
        unsafe { gdi::GetKerningPairsW(self.dc, 0, std::ptr::null_mut()) }
    }
    pub(super) fn kerning_pairs(&self, capacity: u32) -> Result<Vec<KerningPair>, TextError> {
        if capacity == 0 || capacity > MAX_KERNING_PAIRS {
            return Err(TextError::Font("invalid kerning-pair capacity".into()));
        }
        let mut pairs = vec![gdi::KERNINGPAIR::default(); capacity as usize];
        // SAFETY: bounded initialized native storage, with the same DC and
        // selected native font as the preceding count and other text queries.
        let written = unsafe { gdi::GetKerningPairsW(self.dc, capacity, pairs.as_mut_ptr()) };
        if written > capacity {
            return Err(TextError::Font(
                "native kerning-pair count exceeds capacity".into(),
            ));
        }
        pairs.truncate(written as usize);
        Ok(pairs
            .into_iter()
            .map(|pair| KerningPair {
                first: pair.wFirst,
                second: pair.wSecond,
                adjustment: pair.iKernAmount,
            })
            .collect())
    }
    pub(super) fn extent(&self, input: TextInput<'_>) -> Result<TextExtent, TextError> {
        let mut size = windows_sys::Win32::Foundation::SIZE::default();
        let result = match input {
            TextInput::Encoded(bytes) => {
                let count = i32::try_from(bytes.len())
                    .map_err(|_| TextError::Font("encoded run length overflow".into()))?;
                // SAFETY: the counted input covers every byte GDI may consume;
                // no terminator scan. The selected private font defines its repertoire.
                unsafe { gdi::GetTextExtentPoint32A(self.dc, bytes.as_ptr(), count, &mut size) }
            }
            TextInput::Unicode(units) => {
                let count = i32::try_from(units.len())
                    .map_err(|_| TextError::Font("UTF-16 run length overflow".into()))?;
                // SAFETY: same owned DC and explicit count of initialized UTF-16 units.
                unsafe { gdi::GetTextExtentPoint32W(self.dc, units.as_ptr(), count, &mut size) }
            }
        };
        if result == 0 {
            return Err(TextError::Font("native text extent query failed".into()));
        }
        Ok(TextExtent {
            width: size.cx,
            height: size.cy,
        })
    }
    pub(super) fn glyph(&self, request: GlyphRequest) -> Result<Glyph, TextError> {
        let (code, ansi, index) = match request.character {
            Character::Unicode(code) => (code, false, false),
            Character::Encoded(code) => (code, true, false),
            Character::GlyphIndex(code) => (code, false, true),
        };
        let mut format = match request.format {
            GlyphFormat::Metrics => 0,
            GlyphFormat::Monochrome => 1,
            GlyphFormat::QuadraticOutline => 2,
            GlyphFormat::CubicOutline => 3,
            GlyphFormat::Gray2 => 4,
            GlyphFormat::Gray4 => 5,
            GlyphFormat::Gray8 => 6,
        };
        if index {
            format |= 0x80;
        }
        if !request.hinted {
            format |= 0x100;
        }
        let fixed = |value: i32| gdi::FIXED {
            fract: value as u16,
            value: (value >> 16) as i16,
        };
        let transform = gdi::MAT2 {
            eM11: fixed(request.transform.xx),
            eM12: fixed(request.transform.xy),
            eM21: fixed(request.transform.yx),
            eM22: fixed(request.transform.yy),
        };
        let mut metrics = gdi::GLYPHMETRICS::default();
        let get_outline = if ansi {
            gdi::GetGlyphOutlineA
        } else {
            gdi::GetGlyphOutlineW
        };
        // SAFETY: owned DC and initialized host metrics/transform; query has no output buffer.
        let size = unsafe {
            get_outline(
                self.dc,
                code,
                format,
                &mut metrics,
                0,
                std::ptr::null_mut(),
                &transform,
            )
        };
        if size == u32::MAX || size > 16 * 1024 * 1024 {
            return Err(TextError::Glyph("GetGlyphOutline failed".into()));
        }
        let mut bytes = Vec::new();
        if request.format != GlyphFormat::Metrics && size != 0 {
            bytes.resize(size as usize, 0);
            // SAFETY: bytes covers exactly the size returned by this same native query.
            if unsafe {
                get_outline(
                    self.dc,
                    code,
                    format,
                    &mut metrics,
                    size,
                    bytes.as_mut_ptr().cast(),
                    &transform,
                )
            } == u32::MAX
            {
                return Err(TextError::Glyph("GetGlyphOutline data failed".into()));
            }
        }
        let data = match request.format {
            GlyphFormat::Metrics => GlyphData::None,
            GlyphFormat::QuadraticOutline | GlyphFormat::CubicOutline => {
                GlyphData::Outline(contours(&bytes)?)
            }
            _ => GlyphData::Bitmap(bytes),
        };
        Ok(Glyph {
            metrics: GlyphMetrics {
                width: metrics.gmBlackBoxX,
                height: metrics.gmBlackBoxY,
                origin_x: metrics.gmptGlyphOrigin.x,
                origin_y: metrics.gmptGlyphOrigin.y,
                advance_x: metrics.gmCellIncX,
                advance_y: metrics.gmCellIncY,
            },
            required_bytes: size,
            data,
        })
    }
}

/// One bounded scratch bitmap shared by the font cache, never by guest handles.
pub(super) struct Canvas {
    bitmap: gdi::HBITMAP,
    bits: *mut u8,
    width: u32,
    height: u32,
    pitch: usize,
    format: crate::surface::PixelFormat,
    palette: Option<[[u8; 4]; 2]>,
}
impl Default for Canvas {
    fn default() -> Self {
        Self {
            bitmap: std::ptr::null_mut(),
            bits: std::ptr::null_mut(),
            width: 0,
            height: 0,
            pitch: 0,
            format: crate::surface::PixelFormat::Bgra32,
            palette: None,
        }
    }
}
impl Canvas {
    fn ensure(
        &mut self,
        width: u32,
        height: u32,
        format: crate::surface::PixelFormat,
        palette: Option<[[u8; 4]; 2]>,
    ) -> Result<(), TextError> {
        if !self.bitmap.is_null()
            && self.width >= width
            && self.height >= height
            && self.format == format
            && self.palette == palette
        {
            return Ok(());
        }
        let width = width
            .checked_next_power_of_two()
            .ok_or_else(|| TextError::Font("text canvas width overflow".into()))?;
        let height = height
            .checked_next_power_of_two()
            .ok_or_else(|| TextError::Font("text canvas height overflow".into()))?;
        let pitch = ((format.row_bytes(width) + 3) & !3) as usize;
        if pitch
            .checked_mul(height as usize)
            .is_none_or(|n| n > 32 * 1024 * 1024)
        {
            return Err(TextError::Font(
                "text canvas exceeds allocation limit".into(),
            ));
        }
        #[repr(C)]
        struct Info {
            header: gdi::BITMAPINFOHEADER,
            masks: [u32; 3],
        }
        let masks = format.masks();
        let info = Info {
            header: gdi::BITMAPINFOHEADER {
                biSize: 40,
                biWidth: width as i32,
                biHeight: -(height as i32),
                biPlanes: 1,
                biBitCount: format.bits() as u16,
                biCompression: if format.bytes() == 2 { 3 } else { 0 },
                ..Default::default()
            },
            masks: if let Some(palette) = palette {
                [
                    u32::from_le_bytes(palette[0]),
                    u32::from_le_bytes(palette[1]),
                    0,
                ]
            } else {
                [masks.0, masks.1, masks.2]
            },
        };
        let mut bits = std::ptr::null_mut();
        // SAFETY: info includes the header and either two monochrome RGBQUADs
        // or three bitfield masks when needed;
        // acquired bitmap is not selected, and replacement is transactional.
        let bitmap = unsafe {
            gdi::CreateDIBSection(
                std::ptr::null_mut(),
                &info as *const Info as *const gdi::BITMAPINFO,
                0,
                &mut bits,
                std::ptr::null_mut(),
                0,
            )
        };
        if bitmap.is_null() || bits.is_null() {
            if !bitmap.is_null() {
                unsafe {
                    gdi::DeleteObject(bitmap);
                }
            }
            return Err(TextError::Font("text canvas allocation failed".into()));
        }
        let old = std::mem::replace(
            self,
            Self {
                bitmap,
                bits: bits.cast(),
                width,
                height,
                pitch,
                format,
                palette,
            },
        );
        drop(old);
        Ok(())
    }
}
impl Drop for Canvas {
    fn drop(&mut self) {
        if !self.bitmap.is_null() {
            unsafe {
                gdi::DeleteObject(self.bitmap);
            }
        }
    }
}
fn contours(bytes: &[u8]) -> Result<Vec<Contour>, TextError> {
    let invalid = || TextError::Glyph("invalid native outline result".into());
    let u32_at = |at: usize| -> Result<u32, TextError> {
        Ok(u32::from_le_bytes(
            bytes
                .get(at..at + 4)
                .ok_or_else(invalid)?
                .try_into()
                .unwrap(),
        ))
    };
    let point = |at: usize| -> Result<Point, TextError> {
        Ok(Point {
            x: u32_at(at)? as i32,
            y: u32_at(at + 4)? as i32,
        })
    };
    let mut output = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let end = at
            .checked_add(u32_at(at)? as usize)
            .filter(|&end| end <= bytes.len() && end >= at + 16)
            .ok_or_else(invalid)?;
        if u32_at(at + 4)? != 24 {
            return Err(invalid());
        }
        let mut contour = Contour {
            start: point(at + 8)?,
            curves: Vec::new(),
        };
        at += 16;
        while at < end {
            let kind = u16::from_le_bytes(
                bytes
                    .get(at..at + 2)
                    .ok_or_else(invalid)?
                    .try_into()
                    .unwrap(),
            );
            let count = u16::from_le_bytes(
                bytes
                    .get(at + 2..at + 4)
                    .ok_or_else(invalid)?
                    .try_into()
                    .unwrap(),
            ) as usize;
            let next = at + 4 + count * 8;
            if next > end {
                return Err(invalid());
            }
            let points = (0..count)
                .map(|i| point(at + 4 + i * 8))
                .collect::<Result<Vec<_>, _>>()?;
            contour.curves.push(match kind {
                1 => Curve::Line(points),
                2 => Curve::Quadratic(points),
                3 => Curve::Cubic(points),
                _ => return Err(invalid()),
            });
            at = next;
        }
        output.push(contour);
    }
    Ok(output)
}
impl Drop for Font {
    fn drop(&mut self) {
        // SAFETY: restore the owned DC's previous selection before releasing its font.
        unsafe {
            gdi::SelectObject(self.dc, self.old);
            if self.owned {
                gdi::DeleteObject(self.font);
            }
            gdi::DeleteDC(self.dc);
        }
    }
}

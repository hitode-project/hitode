//! CoreText outline extraction for Apple font formats (such as hvgl) that
//! skrifa cannot outline directly. Metrics, cell bounds, synthetic styles and
//! rasterization stay in the shared portable pipeline.
use super::{Face, FontRequest, Pen, TextError};
use fontdb::{FaceInfo, Source};
use objc2_core_foundation::{
    CFArray, CFData, CFDictionary, CFNumber, CFRetained, CFString, CFURL, Type,
};
use objc2_core_graphics::{CGDataProvider, CGFont, CGPath, CGPathElement, CGPathElementType};
use objc2_core_text::{
    CTFont, CTFontDescriptor, CTFontManagerCreateFontDescriptorsFromData,
    CTFontManagerCreateFontDescriptorsFromURL, kCTFontFamilyNameAttribute, kCTFontNameAttribute,
    kCTFontVariationAttribute,
};
use skrifa::{FontRef, GlyphId, MetadataProvider, Tag, outline::OutlinePen};
use std::{ffi::c_void, ptr::NonNull};

#[derive(Clone)]
pub(super) struct CoreDescriptor(CFRetained<CTFontDescriptor>);
// SAFETY: CTFontDescriptor is an immutable CoreText value type safe to share
// across threads and accessed under FaceStore synchronization.
unsafe impl Send for CoreDescriptor {}
unsafe impl Sync for CoreDescriptor {}

#[derive(Clone)]
pub(super) struct CoreFont(CFRetained<CTFont>);
// SAFETY: CTFont is an immutable CoreText value type safe for concurrent
// glyph path queries and accessed under Font's cache mutex.
unsafe impl Send for CoreFont {}
unsafe impl Sync for CoreFont {}

fn descriptor_attribute_string(descriptor: &CTFontDescriptor, key: &CFString) -> Option<String> {
    // SAFETY: `key` is a static CoreText attribute key for `descriptor`.
    unsafe { descriptor.attribute(key) }
        .and_then(|value| value.downcast::<CFString>().ok())
        .map(|name| name.to_string())
}

fn select_descriptor(
    descriptors: &CFArray<CTFontDescriptor>,
    info: &FaceInfo,
) -> Option<CoreDescriptor> {
    // SAFETY: kCTFontNameAttribute and kCTFontFamilyNameAttribute are static CoreText keys.
    let (name_key, family_key) = unsafe { (kCTFontNameAttribute, kCTFontFamilyNameAttribute) };
    let count = descriptors.len();
    let mut best: Option<(u8, u8, CFRetained<CTFontDescriptor>)> = None;
    for descriptor in descriptors.iter() {
        let family = descriptor_attribute_string(&descriptor, family_key);
        let ps = descriptor_attribute_string(&descriptor, name_key);
        let family_score = match family.as_deref() {
            Some(name) if info.families.iter().any(|(known, _)| known == name) => 2,
            Some(name)
                if info
                    .families
                    .iter()
                    .any(|(known, _)| known.eq_ignore_ascii_case(name)) =>
            {
                1
            }
            _ => 0,
        };
        let ps_score = match ps.as_deref() {
            Some(name) if name == info.post_script_name => 2,
            Some(name) if name.eq_ignore_ascii_case(&info.post_script_name) => 1,
            _ => 0,
        };
        if family_score == 0 && ps_score == 0 && count != 1 {
            continue;
        }
        let replace = best
            .as_ref()
            .is_none_or(|(best_fam, best_ps, _)| (family_score, ps_score) > (*best_fam, *best_ps));
        if replace {
            best = Some((family_score, ps_score, descriptor));
        }
    }
    best.map(|(_, _, descriptor)| CoreDescriptor(descriptor))
}

pub(super) fn descriptor_for_face(face: &Face) -> Result<Option<CoreDescriptor>, TextError> {
    let descriptors = match &face.info.source {
        Source::File(path) => {
            let Some(url) = CFURL::from_file_path(path) else {
                return Ok(None);
            };
            // SAFETY: `url` is a valid CFURL pointing to the font file.
            unsafe { CTFontManagerCreateFontDescriptorsFromURL(&url) }
        }
        Source::Binary(_) => {
            let data = CFData::from_bytes(face.data.as_ref().as_ref());
            // SAFETY: `data` is a valid CFData holding the OpenType bytes.
            Some(unsafe { CTFontManagerCreateFontDescriptorsFromData(&data) })
        }
    };
    let Some(descriptors) = descriptors else {
        return Ok(None);
    };
    // SAFETY: CTFontManagerCreateFontDescriptorsFrom* returns a CFArray of CTFontDescriptor.
    let descriptors =
        unsafe { CFRetained::cast_unchecked::<CFArray<CTFontDescriptor>>(descriptors) };
    Ok(select_descriptor(&descriptors, &face.info))
}

fn variation_attributes(
    font: &FontRef<'_>,
    request: &FontRequest,
) -> Option<CFRetained<CFDictionary>> {
    let axes = font.axes();
    let requested = [
        (
            b"wght",
            if request.weight == 0 {
                400.0
            } else {
                request.weight as f32
            },
        ),
        (b"ital", f32::from(u8::from(request.italic))),
        (b"slnt", if request.italic { -12.0 } else { 0.0 }),
    ];
    let mut pairs = Vec::new();
    for (tag_bytes, value) in requested {
        if let Some(axis) = axes.get_by_tag(Tag::new(tag_bytes)) {
            let clamped = value.clamp(axis.min_value(), axis.max_value());
            pairs.push((
                CFNumber::new_i32(u32::from_be_bytes(*tag_bytes) as i32),
                CFNumber::new_f64(f64::from(clamped)),
            ));
        }
    }
    if pairs.is_empty() {
        return None;
    }
    let key_refs: Vec<&CFNumber> = pairs.iter().map(|(k, _)| k.as_ref()).collect();
    let val_refs: Vec<&CFNumber> = pairs.iter().map(|(_, v)| v.as_ref()).collect();
    let variations = CFDictionary::from_slices(&key_refs, &val_refs);
    // SAFETY: kCTFontVariationAttribute is a static CoreText descriptor attribute key.
    let var_key = unsafe { kCTFontVariationAttribute };
    let outer = CFDictionary::from_slices(&[var_key], &[variations.as_opaque()]);
    Some(outer.as_opaque().retain())
}

pub(super) fn create_font(
    face: &Face,
    font: &FontRef<'_>,
    descriptor: Option<&CoreDescriptor>,
    em: f32,
    request: &FontRequest,
) -> Result<CoreFont, TextError> {
    let attrs = variation_attributes(font, request);
    if let Some(descriptor) = descriptor {
        let matched = match &attrs {
            // SAFETY: `attrs` maps kCTFontVariationAttribute to a CFNumber->CFNumber dictionary.
            Some(attrs) => unsafe { descriptor.0.copy_with_attributes(attrs) },
            None => descriptor.0.clone(),
        };
        // SAFETY: `matched` is a valid CTFontDescriptor and matrix is null.
        return Ok(CoreFont(unsafe {
            CTFont::with_font_descriptor(&matched, f64::from(em), std::ptr::null())
        }));
    }
    if matches!(&face.info.source, Source::Binary(_)) && face.index == 0 {
        let data = CFData::from_bytes(face.data.as_ref().as_ref());
        if let Some(provider) = CGDataProvider::with_cf_data(Some(&data))
            && let Some(cg_font) = CGFont::with_data_provider(&provider)
        {
            let attr_desc = attrs
                .as_ref()
                // SAFETY: `attrs` is a valid CoreText attributes dictionary.
                .map(|attrs| unsafe { CTFontDescriptor::with_attributes(attrs) });
            // SAFETY: `cg_font` is valid, matrix is null, `attr_desc` is valid or None.
            return Ok(CoreFont(unsafe {
                CTFont::with_graphics_font(
                    &cg_font,
                    f64::from(em),
                    std::ptr::null(),
                    attr_desc.as_deref(),
                )
            }));
        }
    }
    Err(TextError::Glyph(
        "CoreText descriptor unavailable for Apple outline face".into(),
    ))
}

struct PathState<'a> {
    pen: &'a mut Pen,
    start: Option<(f32, f32)>,
    current: Option<(f32, f32)>,
}

unsafe extern "C-unwind" fn visit_element(info: *mut c_void, element: NonNull<CGPathElement>) {
    // SAFETY: `info` points to `PathState` for the synchronous `CGPath::apply` walk,
    // and `element` is provided by CoreGraphics with point counts defined by its type.
    let state = unsafe { &mut *info.cast::<PathState<'_>>() };
    let element = unsafe { element.as_ref() };
    let near =
        |ax: f32, ay: f32, bx: f32, by: f32| (ax - bx).abs() <= 1e-5 && (ay - by).abs() <= 1e-5;
    match element.r#type {
        CGPathElementType::MoveToPoint => {
            // SAFETY: MoveToPoint carries 1 CGPoint.
            let p = unsafe { *element.points.as_ptr() };
            let (x, y) = (p.x as f32, p.y as f32);
            state.pen.move_to(x, y);
            state.start = Some((x, y));
            state.current = Some((x, y));
        }
        CGPathElementType::AddLineToPoint => {
            // SAFETY: AddLineToPoint carries 1 CGPoint.
            let p = unsafe { *element.points.as_ptr() };
            let (x, y) = (p.x as f32, p.y as f32);
            state.pen.line_to(x, y);
            state.current = Some((x, y));
        }
        CGPathElementType::AddQuadCurveToPoint => {
            // SAFETY: AddQuadCurveToPoint carries 2 CGPoints (control, end).
            let points = unsafe { std::slice::from_raw_parts(element.points.as_ptr(), 2) };
            let (cx, cy) = (points[0].x as f32, points[0].y as f32);
            let (x, y) = (points[1].x as f32, points[1].y as f32);
            if state.current.is_some_and(|(px, py)| near(cx, cy, px, py)) || near(cx, cy, x, y) {
                state.pen.line_to(x, y);
            } else {
                state.pen.quad_to(cx, cy, x, y);
            }
            state.current = Some((x, y));
        }
        CGPathElementType::AddCurveToPoint => {
            // SAFETY: AddCurveToPoint carries 3 CGPoints (c1, c2, end).
            let points = unsafe { std::slice::from_raw_parts(element.points.as_ptr(), 3) };
            let (x1, y1) = (points[0].x as f32, points[0].y as f32);
            let (x2, y2) = (points[1].x as f32, points[1].y as f32);
            let (x, y) = (points[2].x as f32, points[2].y as f32);
            state.pen.curve_to(x1, y1, x2, y2, x, y);
            state.current = Some((x, y));
        }
        CGPathElementType::CloseSubpath => {
            state.pen.close();
            state.current = state.start;
        }
        _ => {}
    }
}

pub(super) fn draw_outline(
    ct_font: &CoreFont,
    glyph: GlyphId,
    pen: &mut Pen,
) -> Result<(), TextError> {
    let gid = u16::try_from(glyph.to_u32())
        .map_err(|_| TextError::Glyph("glyph index exceeds CoreText range".into()))?;
    // SAFETY: `ct_font.0` is a valid CTFont and matrix is null (Y-up pixel coordinates at `em`).
    let Some(path) = (unsafe { ct_font.0.path_for_glyph(gid, std::ptr::null()) }) else {
        return Ok(());
    };
    let mut state = PathState {
        pen,
        start: None,
        current: None,
    };
    // SAFETY: `state` outlives `CGPath::apply` and `visit_element` matches `CGPathApplierFunction`.
    unsafe {
        CGPath::apply(
            Some(&path),
            (&raw mut state).cast::<c_void>(),
            Some(visit_element),
        );
    }
    Ok(())
}

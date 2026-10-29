//! GCP arrays and Unicode visual order on the same realized OpenType face.
use super::*;

impl Font {
    pub(in crate::text) fn language_info(&self) -> Result<u32, TextError> {
        let cp = self.signature()?.codepages[0];
        Ok(u32::from(cp & 0x003e0000 != 0)
            | if cp & (1 << 6) != 0 { 0x10 } else { 0 }
            | if self.kerning_pairs()?.is_empty() {
                0
            } else {
                8
            })
    }
    pub(in crate::text) fn placement(
        &self,
        input: TextInput<'_>,
        maximum: i32,
        flags: u32,
        mut buffers: Option<PlacementBuffers>,
        acp: u32,
    ) -> Result<Placement, TextError> {
        let (string, encoded) = match input {
            TextInput::Unicode(units) => (String::from_utf16(units).map_err(font_error)?, false),
            TextInput::Encoded(bytes) => {
                let encoding = match self.request.encoding {
                    Encoding::Automatic | Encoding::Latin | Encoding::Oem => match acp {
                        932 => encoding_rs::SHIFT_JIS,
                        936 => encoding_rs::GBK,
                        65001 => encoding_rs::UTF_8,
                        _ => encoding_rs::WINDOWS_1252,
                    },
                    other => codec(other)?,
                };
                (
                    encoding.decode_without_bom_handling(bytes).0.into_owned(),
                    true,
                )
            }
        };
        let logical: Vec<_> = string.encode_utf16().collect();
        let mut visual = logical.clone();
        let mut order: Vec<u32> = (0..logical.len() as u32).collect();
        if flags & 2 != 0 && !string.is_empty() {
            let bidi = unicode_bidi::BidiInfo::new(&string, Some(unicode_bidi::Level::ltr()));
            visual.clear();
            let mut visual_index = 0;
            for para in &bidi.paragraphs {
                let levels = bidi.reordered_levels_per_char(para, para.range.clone());
                let map = unicode_bidi::BidiInfo::reorder_visual(&levels);
                let chars: Vec<_> = string[para.range.clone()].chars().collect();
                let starts: Vec<_> = chars
                    .iter()
                    .scan(
                        string[..para.range.start].encode_utf16().count(),
                        |at, c| {
                            let old = *at;
                            *at += c.len_utf16();
                            Some(old)
                        },
                    )
                    .collect();
                for index in map {
                    let c = chars[index];
                    for i in 0..c.len_utf16() {
                        order[starts[index] + i] = visual_index + i as u32;
                    }
                    let mut units = [0; 2];
                    visual.extend_from_slice(c.encode_utf16(&mut units));
                    visual_index += c.len_utf16() as u32;
                }
            }
        }
        let mut glyphs = self.glyph_indices(&visual, 0)?;
        let mut advances = Vec::with_capacity(visual.len());
        for &code in &visual {
            let (face, glyph) = if (0xd800..=0xdfff).contains(&code) {
                self.default_glyph()?
            } else {
                self.resolve(u32::from(code))?
            };
            advances.push(self.advance(&face, glyph, Some(u32::from(code)))? as i32);
        }
        if flags & (0x10 | 0x20) != 0
            || (flags & 2 != 0 && logical.iter().any(|&c| matches!(c, 0x590..=0x8ff)))
        {
            let font = rustybuzz::Face::from_slice(
                self.primary.data.as_ref().as_ref(),
                self.primary.index,
            )
            .ok_or_else(|| TextError::Font("invalid shaping face".into()))?;
            let mut buffer = rustybuzz::UnicodeBuffer::new();
            buffer.push_str(&string);
            buffer.guess_segment_properties();
            let shaped = rustybuzz::shape(&font, &[], buffer);
            glyphs = shaped
                .glyph_infos()
                .iter()
                .map(|g| g.glyph_id as u16)
                .collect();
            advances = glyphs
                .iter()
                .map(|&g| {
                    self.advance(&self.primary, GlyphId::new(u32::from(g)), None)
                        .map(|v| v as i32)
                })
                .collect::<Result<_, _>>()?;
        }
        if flags & 8 != 0 {
            let pairs: BTreeMap<_, _> = self
                .kerning_pairs()?
                .into_iter()
                .map(|p| ((p.first, p.second), p.adjustment))
                .collect();
            for (i, pair) in visual.windows(2).enumerate() {
                if let Some(amount) = pairs.get(&(pair[0], pair[1]))
                    && let Some(advance) = advances.get_mut(i)
                {
                    *advance += amount;
                }
            }
        }
        let width: i32 = advances.iter().sum();
        let extent = if logical.is_empty() {
            0
        } else {
            (width as u32 & 65535) | ((self.metrics.height as u32 & 65535) << 16)
        };
        if let Some(output) = &mut buffers {
            let count = (output.capacity as usize).min(glyphs.len());
            output.capacity = count as u32;
            if let Some(text) = &mut output.text {
                for (dst, src) in text.iter_mut().zip(visual.iter()).take(count) {
                    *dst = *src;
                }
            }
            if encoded && let Some(text) = &mut output.encoded_text {
                let encoding = match acp {
                    932 => encoding_rs::SHIFT_JIS,
                    936 => encoding_rs::GBK,
                    65001 => encoding_rs::UTF_8,
                    _ => encoding_rs::WINDOWS_1252,
                };
                let string = String::from_utf16_lossy(&visual);
                let bytes = encoding.encode(&string).0;
                for (dst, &src) in text.iter_mut().zip(bytes.iter()).take(count) {
                    *dst = src;
                }
            }
            if let Some(target) = &mut output.order {
                for (dst, &src) in target.iter_mut().zip(order.iter()).take(count) {
                    *dst = src;
                }
            }
            let mut cursor = 0;
            let mut fit = 0;
            for (i, &width) in advances.iter().take(count).enumerate() {
                if let Some(target) = &mut output.caret {
                    target[i] = cursor;
                }
                if let Some(target) = &mut output.advances {
                    target[i] = width;
                }
                if let Some(target) = &mut output.glyphs {
                    target[i] = glyphs[i];
                }
                if flags & 0x80000 == 0
                    && let Some(target) = &mut output.classes
                {
                    let code = visual.get(i).copied().unwrap_or(32);
                    target[i] = match code {
                        0x590..=0x8ff => 2,
                        48..=57 => 5,
                        32 | 9 | 10 | 13 => 3,
                        _ => 1,
                    };
                }
                cursor += width;
                if cursor <= maximum {
                    fit = i + 1;
                }
            }
            if flags & 0x100000 != 0 {
                output.max_fit = fit as i32;
            }
        }
        Ok(Placement { extent, buffers })
    }
}

//! Queries of the realized face; all data shares its immutable source buffer.
use super::*;

impl Font {
    pub(in crate::text) fn is_linked(&self) -> bool {
        self.candidates.len() > 1 && !self.request.clipping.disable_association
    }

    pub(in crate::text) fn file(&self) -> FontFile {
        FontFile {
            data: self.primary.data.clone(),
            index: self.primary.index,
        }
    }
    pub(in crate::text) fn signature(&self) -> Result<FontSignature, TextError> {
        let os2 = self.primary.font()?.os2().map_err(font_error)?;
        Ok(FontSignature {
            unicode: [
                os2.ul_unicode_range_1(),
                os2.ul_unicode_range_2(),
                os2.ul_unicode_range_3(),
                os2.ul_unicode_range_4(),
            ],
            codepages: [
                os2.ul_code_page_range_1().unwrap_or(0),
                os2.ul_code_page_range_2().unwrap_or(0),
            ],
        })
    }
    pub(in crate::text) fn glyph_indices(
        &self,
        units: &[u16],
        missing: u16,
    ) -> Result<Vec<u16>, TextError> {
        let font = self.primary.font()?;
        let cmap = font.charmap();
        Ok(units
            .iter()
            .map(|&unit| {
                cmap.map(u32::from(unit))
                    .map_or(missing, |g| g.to_u32() as u16)
            })
            .collect())
    }
    pub(in crate::text) fn width_info(&self) -> Result<[i32; 2], TextError> {
        let font = self.primary.font()?;
        let hhea = font.hhea().map_err(font_error)?;
        let scale =
            self.em * self.xscale / f32::from(font.head().map_err(font_error)?.units_per_em());
        Ok([
            (f32::from(hhea.min_left_side_bearing().to_i16()) * scale).round() as i32,
            (f32::from(hhea.min_right_side_bearing().to_i16()) * scale).round() as i32,
        ])
    }
    pub(in crate::text) fn simulations(&self) -> u32 {
        u32::from(normalized_weight(&self.request) >= 600 && self.primary.info.weight.0 < 600)
            | (u32::from(self.request.italic && self.primary.info.style == Style::Normal) << 1)
    }
    pub(in crate::text) fn kerning_pairs(&self) -> Result<Vec<KerningPair>, TextError> {
        // The legacy GDI query exposes the horizontal format-0 kern pairs,
        // expanded through the primary Unicode cmap, in character order.
        let font = self.primary.font()?;
        let Some(data) = self.file().table(*b"kern").map(<[u8]>::to_vec) else {
            return Ok(Vec::new());
        };
        let mut codes: BTreeMap<u16, Vec<u16>> = BTreeMap::new();
        for (code, glyph) in font.charmap().mappings().filter(|(code, _)| *code <= 65535) {
            codes
                .entry(glyph.to_u32() as u16)
                .or_default()
                .push(code as u16);
        }
        let be16 = |at: usize| {
            data.get(at..at + 2)
                .map(|s| u16::from_be_bytes(s.try_into().unwrap()))
        };
        let mut values: BTreeMap<(u16, u16), i32> = BTreeMap::new();
        let mut at = 4;
        let scale =
            self.em * self.xscale / f32::from(font.head().map_err(font_error)?.units_per_em());
        for _ in 0..be16(2).unwrap_or(0) {
            let Some(length) = be16(at + 2).map(usize::from) else {
                break;
            };
            let Some(coverage) = be16(at + 4) else { break };
            if coverage & 0xff01 == 1 {
                for pair in 0..be16(at + 6).unwrap_or(0) as usize {
                    let p = at + 14 + pair * 6;
                    let (Some(left), Some(right), Some(value)) =
                        (be16(p), be16(p + 2), be16(p + 4))
                    else {
                        break;
                    };
                    if let (Some(left), Some(right)) = (codes.get(&left), codes.get(&right)) {
                        for &a in left {
                            for &b in right {
                                let value = (f32::from(value as i16) * scale).round() as i32;
                                let entry = values.entry((a, b)).or_default();
                                if coverage & 8 != 0 {
                                    *entry = value;
                                } else {
                                    *entry += value;
                                }
                            }
                        }
                    }
                }
            }
            if length == 0 {
                break;
            }
            at += length;
        }
        Ok(values
            .into_iter()
            .map(|((first, second), amount)| KerningPair {
                first,
                second,
                adjustment: amount,
            })
            .collect())
    }
}

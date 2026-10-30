//! Unshaped character advances over the same selected, owned font as rasterization.
use super::*;

impl Text {
    /// One encoded character in the selected font's repertoire. This also
    /// preserves the provider's default-glyph handling for incomplete sequences.
    pub fn encoded_character_width(
        &mut self,
        request: &FontRequest,
        code: u16,
    ) -> Result<i32, TextError> {
        self.glyph(
            request,
            GlyphRequest {
                character: Character::Encoded(u32::from(code)),
                format: GlyphFormat::Metrics,
                hinted: true,
                transform: Transform::IDENTITY,
            },
        )
        .map(|g| i32::from(g.metrics.advance_x))
    }
    /// Consecutive Unicode units, including NUL and missing/default characters.
    /// This measures individual advances, independently of string shaping.
    pub fn character_widths(
        &mut self,
        request: &FontRequest,
        first: u16,
        last: u16,
    ) -> Result<Vec<i32>, TextError> {
        if last < first {
            return Err(TextError::Glyph("invalid character range".into()));
        }

        {
            // Character widths are horizontal advances, not rotated ink bounds.
            let mut horizontal = request.clone();
            horizontal.baseline_angle = 0;
            horizontal.glyph_angle = 0;
            (first..=last)
                .map(|unit| {
                    // A surrogate unit is an unmapped individual character,
                    // not half of a shaped run. NUL resolves the default glyph
                    // in the portable provider, without font fallback search.
                    let unit = if (0xd800..=0xdfff).contains(&unit) {
                        0
                    } else {
                        unit
                    };
                    self.extent(&horizontal, TextInput::Unicode(&[unit]))
                        .map(|extent| extent.width)
                })
                .collect()
        }
    }
}

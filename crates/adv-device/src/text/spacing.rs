//! Unshaped character advances over the same selected, owned font as rasterization.
use super::*;

impl Text {
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
        #[cfg(windows)]
        {
            self.font(request)?.character_widths(first, last)
        }
        #[cfg(not(windows))]
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

//! Text records (WINE-UNIX-8): the lines the host font frontend draws and the
//! glyph bytes it hands the guest, kept only while a host consumer asks.
//!
//! Text extraction reads these at the points where a character turns into
//! pixels or glyph bytes and would lose its identity. A drawn line keeps its
//! characters, or glyph indices when the guest drew indices, with the device
//! origin of each. A produced glyph keeps the guest range its bytes went to, so
//! later uses of those bytes can be traced back to the character.
use std::collections::VecDeque;

/// What one glyph of a drawn line stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Code {
    /// A Unicode scalar value as the guest gave it, which may be a lone surrogate.
    Unicode(u32),
    /// A code in the font's own encoding, such as a symbol font's.
    Encoded(u32),
    /// The guest drew a glyph index; the character is not known here.
    Index(u32),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DrawnGlyph {
    pub code: Code,
    /// Device coordinates of the glyph origin on the drawing surface.
    pub origin: [i32; 2],
}

/// One line drawn through the GDI text entry points.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DrawnLine {
    pub sequence: u64,
    /// The drawing guest thread's ID.
    pub thread: u32,
    pub dc: u32,
    /// The guest DIB device the line was composed into; `None` for metafile,
    /// path and other drivers, which receive the run instead.
    pub surface: Option<u32>,
    pub face: String,
    pub height: i32,
    /// Device coordinates the line was clipped to.
    pub clip: Option<[i32; 4]>,
    pub glyphs: Vec<DrawnGlyph>,
}

/// Glyph bytes GetGlyphOutline wrote into a guest buffer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProducedGlyph {
    pub sequence: u64,
    pub thread: u32,
    pub dc: u32,
    pub face: String,
    pub height: i32,
    pub code: Code,
    pub buffer: u32,
    pub len: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextRecord {
    Line(DrawnLine),
    Glyph(ProducedGlyph),
}

/// Records taken since the previous take.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TextBatch {
    pub records: Vec<TextRecord>,
    /// Records that did not fit before this take, so the batch is incomplete.
    pub dropped: u64,
}

/// Records held for the consumer, bounded so a consumer that stops taking
/// them costs bounded memory.
#[derive(Default)]
pub(crate) struct TextRecords {
    records: VecDeque<TextRecord>,
    next: u64,
    dropped: u64,
}

impl TextRecords {
    const CAPACITY: usize = 4096;

    /// The sequence number the next record carries.
    pub(crate) fn sequence(&mut self) -> u64 {
        let sequence = self.next;
        self.next += 1;
        sequence
    }

    pub(crate) fn push(&mut self, record: TextRecord) {
        if self.records.len() == Self::CAPACITY {
            self.records.pop_front();
            self.dropped += 1;
        }
        self.records.push_back(record);
    }

    pub(crate) fn take(&mut self) -> TextBatch {
        TextBatch {
            records: self.records.drain(..).collect(),
            dropped: std::mem::take(&mut self.dropped),
        }
    }
}

impl From<adv_device::text::Character> for Code {
    fn from(character: adv_device::text::Character) -> Self {
        match character {
            adv_device::text::Character::Unicode(c) => Code::Unicode(c),
            adv_device::text::Character::Encoded(c) => Code::Encoded(c),
            adv_device::text::Character::GlyphIndex(i) => Code::Index(i),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // spec: WINE-UNIX-8
    #[test]
    fn records_beyond_capacity_are_counted_as_dropped() {
        let mut records = TextRecords::default();
        for _ in 0..TextRecords::CAPACITY + 2 {
            let sequence = records.sequence();
            records.push(TextRecord::Glyph(ProducedGlyph {
                sequence,
                thread: 1,
                dc: 2,
                face: String::new(),
                height: 0,
                code: Code::Unicode(65),
                buffer: 0,
                len: 1,
            }));
        }
        let batch = records.take();
        assert_eq!(
            (batch.records.len(), batch.dropped),
            (TextRecords::CAPACITY, 2)
        );
        assert!(matches!(&batch.records[0], TextRecord::Glyph(g) if g.sequence == 2));
        assert_eq!(records.take(), TextBatch::default());
    }
}

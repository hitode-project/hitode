//! Text records (WINE-UNIX-8): the lines the host font frontend draws, the
//! glyph bytes it hands the guest, and the host pixel copies that move drawn
//! text, kept only while a host consumer asks.
//!
//! Text extraction reads these at the points where a character turns into
//! pixels or glyph bytes and would lose its identity. A drawn line keeps its
//! characters, or glyph indices when the guest drew indices, with the device
//! origin of each on the plane it went to. A produced glyph keeps the guest
//! range its bytes went to, so later uses of those bytes can be traced back to
//! the character. A copy is kept only when its source plane has held text, and
//! then marks its destination plane as holding text, so records follow drawn
//! text from plane to plane and ignore unrelated blits.
//!
//! The store is shared with the DIB bodies through `Services::text_state`,
//! which is null while records are off.
use adv_machine::{Ext, Sys};
use std::{cell::RefCell, collections::VecDeque, rc::Rc};

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

/// A guest DIB as Wine's dib_info addresses it: device point `(x, y)` is the
/// pixel at `bits + (origin[1] + y) * stride + (origin[0] + x) * bpp / 8`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plane {
    pub bits: u32,
    /// Bytes from one row to the next; negative for bottom-up DIBs.
    pub stride: i32,
    pub bpp: u32,
    /// The DIB rectangle's top-left corner within the bits.
    pub origin: [i32; 2],
    /// The bits' width and height in pixels.
    pub size: [i32; 2],
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
    /// The DIB the line was composed into; `None` for metafile, path and
    /// other drivers, which receive the run instead.
    pub plane: Option<Plane>,
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

/// The DIB primitive that copied pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyKind {
    /// copy_rect: BitBlt and its relatives, with a raster operation.
    Copy,
    /// blend_rects: AlphaBlend.
    Blend,
    /// convert_to: a format conversion, as GetDIBits and SetDIBits make.
    Convert,
}

/// Pixels a host DIB primitive copied from a plane that has held text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CopiedPixels {
    pub sequence: u64,
    pub thread: u32,
    pub kind: CopyKind,
    pub destination: Plane,
    /// Destination device rectangle the copy wrote, as left, top, right, bottom.
    pub rect: [i32; 4],
    pub source: Plane,
    /// Source device point = destination device point + offset.
    pub offset: [i32; 2],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextRecord {
    Line(DrawnLine),
    Glyph(ProducedGlyph),
    Copy(CopiedPixels),
}

impl TextRecord {
    pub fn sequence(&self) -> u64 {
        match self {
            TextRecord::Line(line) => line.sequence,
            TextRecord::Glyph(glyph) => glyph.sequence,
            TextRecord::Copy(copy) => copy.sequence,
        }
    }
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
pub struct TextRecords {
    records: VecDeque<TextRecord>,
    next: u64,
    dropped: u64,
    /// Bits of the planes that have held text, sorted.
    planes: Vec<u32>,
}

impl TextRecords {
    const CAPACITY: usize = 4096;

    /// The sequence number the next record carries.
    pub fn sequence(&mut self) -> u64 {
        let sequence = self.next;
        self.next += 1;
        sequence
    }

    pub fn push(&mut self, record: TextRecord) {
        if let TextRecord::Line(DrawnLine {
            plane: Some(plane), ..
        }) = &record
        {
            self.mark(plane.bits);
        }
        if self.records.len() == Self::CAPACITY {
            self.records.pop_front();
            self.dropped += 1;
        }
        self.records.push_back(record);
    }

    /// Keep a copy when its source has held text.
    fn copied(
        &mut self,
        thread: u32,
        kind: CopyKind,
        destination: Plane,
        rect: [i32; 4],
        source: Plane,
        offset: [i32; 2],
    ) {
        if self.planes.binary_search(&source.bits).is_err()
            || rect[0] >= rect[2]
            || rect[1] >= rect[3]
        {
            return;
        }
        let sequence = self.sequence();
        self.push(TextRecord::Copy(CopiedPixels {
            sequence,
            thread,
            kind,
            destination,
            rect,
            source,
            offset,
        }));
        self.mark(destination.bits);
    }

    fn mark(&mut self, bits: u32) {
        if let Err(at) = self.planes.binary_search(&bits) {
            self.planes.insert(at, bits);
        }
    }

    pub fn take(&mut self) -> TextBatch {
        TextBatch {
            records: self.records.drain(..).collect(),
            dropped: std::mem::take(&mut self.dropped),
        }
    }
}

/// The shared store while records are on.
#[derive(Default)]
pub struct TextServices(Option<Rc<RefCell<TextRecords>>>);

fn services(_: &Sys, ext: &Ext, services: &mut adv_cpu::body::Services) {
    services.text_state = ext
        .try_get::<TextServices>()
        .and_then(|text| text.0.as_ref())
        .map_or(std::ptr::null(), |store| Rc::as_ptr(store).cast());
}

/// Turn records on or off. Turning them on again keeps pending records;
/// turning them off discards them.
pub fn set(sys: &mut Sys, ext: &mut Ext, enabled: bool) {
    sys.add_body_services(services);
    let text = &mut ext.get_or_default::<TextServices>().0;
    if !enabled {
        *text = None;
    } else if text.is_none() {
        *text = Some(Default::default());
    }
}

/// Records since the previous take; empty while records are off.
pub fn take(ext: &mut Ext) -> TextBatch {
    ext.try_get::<TextServices>()
        .and_then(|text| text.0.as_ref())
        .map(|store| store.borrow_mut().take())
        .unwrap_or_default()
}

/// The store, while records are on.
pub fn records(ext: &Ext) -> Option<std::cell::RefMut<'_, TextRecords>> {
    ext.try_get::<TextServices>()?
        .0
        .as_ref()
        .map(|store| store.borrow_mut())
}

/// Note a copy a DIB body completed.
pub(super) fn copied(
    services: adv_cpu::body::Services,
    kind: CopyKind,
    destination: Plane,
    rect: [i32; 4],
    source: Plane,
    offset: [i32; 2],
) {
    let pointer = services.text_state.cast::<RefCell<TextRecords>>();
    if pointer.is_null() {
        return;
    }
    // SAFETY: `set` keeps this allocation in the extension while the pointer
    // is published, and body services are captured afresh for each entry.
    let store = unsafe { &*pointer };
    if let Ok(mut store) = store.try_borrow_mut() {
        store.copied(services.thread, kind, destination, rect, source, offset);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plane(bits: u32) -> Plane {
        Plane {
            bits,
            stride: 64,
            bpp: 32,
            origin: [0, 0],
            size: [16, 16],
        }
    }

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
        assert_eq!(batch.records[0].sequence(), 2);
        assert_eq!(records.take(), TextBatch::default());
    }

    // spec: WINE-UNIX-8
    #[test]
    fn copies_are_kept_only_from_planes_that_have_held_text() {
        let mut records = TextRecords::default();
        let rect = [0, 0, 4, 4];
        records.copied(
            7,
            CopyKind::Copy,
            plane(0x2000),
            rect,
            plane(0x1000),
            [0, 0],
        );
        let sequence = records.sequence();
        records.push(TextRecord::Line(DrawnLine {
            sequence,
            thread: 7,
            dc: 1,
            plane: Some(plane(0x1000)),
            face: String::new(),
            height: 0,
            clip: None,
            glyphs: Vec::new(),
        }));
        records.copied(
            7,
            CopyKind::Copy,
            plane(0x2000),
            [0, 0, 0, 4],
            plane(0x1000),
            [0, 0],
        );
        records.copied(
            7,
            CopyKind::Blend,
            plane(0x2000),
            rect,
            plane(0x1000),
            [1, 2],
        );
        // The destination now holds text, so a copy from it is kept too.
        records.copied(
            7,
            CopyKind::Convert,
            plane(0x3000),
            rect,
            plane(0x2000),
            [0, 0],
        );
        let kinds: Vec<_> = records
            .take()
            .records
            .into_iter()
            .map(|record| match record {
                TextRecord::Copy(copy) => Some((copy.kind, copy.source.bits, copy.offset)),
                _ => None,
            })
            .collect();
        assert_eq!(
            kinds,
            [
                None,
                Some((CopyKind::Blend, 0x1000, [1, 2])),
                Some((CopyKind::Convert, 0x2000, [0, 0]))
            ]
        );
    }
}

//! Text records (WINE-UNIX-8): the lines the host font frontend draws, the
//! glyph bytes it hands the guest, and the host pixel moves that carry drawn
//! text on, kept only while a host consumer asks.
//!
//! Text extraction reads these at the points where a character turns into
//! pixels or glyph bytes and would lose its identity. A drawn line keeps its
//! characters, or glyph indices when the guest drew indices, with the device
//! origin of each on the plane it went to. A produced glyph keeps the guest
//! range its bytes went to, so later uses of those bytes can be traced back to
//! the character. A move (a DIB copy, a device upload, draw or copy, or a
//! present) is kept only when its source holds text, and then marks its
//! destination as holding text, so records follow drawn text from store to
//! store and ignore unrelated pixels.
//!
//! The records are shared with the DIB bodies through `Services::text_state`,
//! which is null while records are off.
use adv_cpu::backend::GuestExt;
use adv_machine::{Ext, Sys};
use std::{
    cell::RefCell,
    collections::{HashSet, VecDeque},
    rc::Rc,
};

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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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

impl Plane {
    /// The guest bytes the plane's rows occupy, as start and end.
    pub fn span(&self) -> [u32; 2] {
        let rows = i64::from(self.size[1].max(0));
        let stride = i64::from(self.stride);
        let start = i64::from(self.bits) + stride.min(0) * (rows - 1).max(0);
        let end = start + stride.abs() * rows;
        [start, end].map(|at| at.clamp(0, i64::from(u32::MAX)) as u32)
    }
}

/// A device resource's subresource, as the DirectX frontends key it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Resource {
    pub allocation: (u64, u64),
    pub subresource: u64,
}

/// Where pixels are kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Store {
    /// Guest memory, as a DIB or a locked surface addresses it.
    Memory(Plane),
    /// A device resource the guest cannot address.
    Resource(Resource),
    /// A window's client area, which a swap chain presents to.
    Window(u32),
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
    /// The session's key for the font, as `Session::glyph_characters` takes it.
    pub font: u32,
    pub face: String,
    pub height: i32,
    /// Device coordinates the line was clipped to.
    pub clip: Option<[i32; 4]>,
    pub glyphs: Vec<DrawnGlyph>,
}

/// Glyph bytes GetGlyphOutline or DirectWrite wrote into a guest buffer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProducedGlyph {
    pub sequence: u64,
    pub thread: u32,
    /// The DC GetGlyphOutline used; DirectWrite has none.
    pub dc: Option<u32>,
    /// The session's key for the font, as `Session::glyph_characters` takes it.
    pub font: u32,
    /// The requested face; for DirectWrite, the font resource's name.
    pub face: String,
    pub height: i32,
    pub code: Code,
    pub buffer: u32,
    pub len: u32,
}

/// What moved pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MoveKind {
    /// copy_rect: BitBlt and its relatives, with a raster operation.
    Copy,
    /// blend_rects: AlphaBlend.
    Blend,
    /// convert_to: a format conversion, as GetDIBits and SetDIBits make.
    Convert,
    /// Guest memory to a device resource.
    Upload,
    /// A device copy between resources.
    DeviceCopy,
    /// A device draw that sampled the source.
    Draw,
    /// A swap chain presented the source to a window.
    Present,
    /// A device resource to guest memory.
    Readback,
}

/// Which source pixel each destination pixel reads: the source point is
/// the destination point times `scale`, plus `offset`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Map {
    pub scale: [f64; 2],
    pub offset: [f64; 2],
}

impl Map {
    pub fn offset(x: i32, y: i32) -> Self {
        Self {
            scale: [1.0; 2],
            offset: [f64::from(x), f64::from(y)],
        }
    }
}

/// Pixels moved from a source that holds text.
#[derive(Clone, Debug, PartialEq)]
pub struct MovedPixels {
    pub sequence: u64,
    pub thread: u32,
    pub kind: MoveKind,
    pub destination: Store,
    /// The destination rectangle written, as left, top, right, bottom.
    pub rect: [i32; 4],
    pub source: Store,
    /// `None` when the move reads the source in a way no affine map
    /// describes, such as a transformed draw.
    pub map: Option<Map>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TextRecord {
    Line(DrawnLine),
    Glyph(ProducedGlyph),
    Move(MovedPixels),
}

impl TextRecord {
    pub fn sequence(&self) -> u64 {
        match self {
            TextRecord::Line(line) => line.sequence,
            TextRecord::Glyph(glyph) => glyph.sequence,
            TextRecord::Move(moved) => moved.sequence,
        }
    }
}

/// Records taken since the previous take.
#[derive(Clone, Debug, Default, PartialEq)]
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
    holdings: Holdings,
}

/// The stores that have held text since records went on.
#[derive(Default)]
struct Holdings {
    /// Sorted, disjoint guest byte ranges.
    memory: Vec<[u32; 2]>,
    stores: HashSet<Store>,
}

impl Holdings {
    fn holds(&self, store: &Store) -> bool {
        match store {
            Store::Memory(plane) => {
                let [start, end] = plane.span();
                let at = self.memory.partition_point(|range| range[1] <= start);
                self.memory.get(at).is_some_and(|range| range[0] < end)
            }
            other => self.stores.contains(other),
        }
    }

    fn mark(&mut self, store: Store) {
        let Store::Memory(plane) = store else {
            self.stores.insert(store);
            return;
        };
        let [mut start, mut end] = plane.span();
        if start == end {
            return;
        }
        // Merge every range that touches the new one.
        let first = self.memory.partition_point(|range| range[1] < start);
        let last = self.memory.partition_point(|range| range[0] <= end);
        for range in &self.memory[first..last] {
            start = start.min(range[0]);
            end = end.max(range[1]);
        }
        self.memory.splice(first..last, [[start, end]]);
    }
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
            self.holdings.mark(Store::Memory(*plane));
        }
        if self.records.len() == Self::CAPACITY {
            self.records.pop_front();
            self.dropped += 1;
        }
        self.records.push_back(record);
    }

    /// Keep a move when its source holds text; its destination then does.
    pub fn moved(
        &mut self,
        thread: u32,
        kind: MoveKind,
        destination: Store,
        rect: [i32; 4],
        source: Store,
        map: Option<Map>,
    ) {
        if rect[0] >= rect[2] || rect[1] >= rect[3] || !self.holdings.holds(&source) {
            return;
        }
        let sequence = self.sequence();
        self.push(TextRecord::Move(MovedPixels {
            sequence,
            thread,
            kind,
            destination,
            rect,
            source,
            map,
        }));
        self.holdings.mark(destination);
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

/// The calling guest thread's ID, from its TEB client ID.
pub fn thread(env: &adv_machine::Env<'_>) -> u32 {
    let teb = env.sys.contexts[env.context as usize].teb;
    env.cpu.read_u32(teb + 0x24).unwrap_or(0)
}

/// Note pixels a host handler moved, while records are on.
pub fn moved(
    env: &adv_machine::Env<'_>,
    kind: MoveKind,
    destination: Store,
    rect: [i32; 4],
    source: Store,
    map: Option<Map>,
) {
    if let Some(mut records) = records(env.ext) {
        records.moved(thread(env), kind, destination, rect, source, map);
    }
}

/// Note a copy a DIB body completed.
pub(super) fn copied(
    services: adv_cpu::body::Services,
    kind: MoveKind,
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
        store.moved(
            services.thread,
            kind,
            Store::Memory(destination),
            rect,
            Store::Memory(source),
            Some(Map::offset(offset[0], offset[1])),
        );
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
                dc: Some(2),
                font: 0,
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
    fn moves_are_kept_only_from_stores_that_hold_text() {
        let mut records = TextRecords::default();
        let rect = [0, 0, 4, 4];
        let memory = |bits| Store::Memory(plane(bits));
        let texture = Store::Resource(Resource {
            allocation: (1, 2),
            subresource: 0,
        });
        let map = Some(Map::offset(1, 2));
        records.moved(7, MoveKind::Copy, memory(0x2000), rect, memory(0x1000), map);
        let sequence = records.sequence();
        records.push(TextRecord::Line(DrawnLine {
            sequence,
            thread: 7,
            dc: 1,
            plane: Some(plane(0x1000)),
            font: 0,
            face: String::new(),
            height: 0,
            clip: None,
            glyphs: Vec::new(),
        }));
        records.moved(
            7,
            MoveKind::Copy,
            memory(0x2000),
            [0, 0, 0, 4],
            memory(0x1000),
            map,
        );
        // A plane inside the text's bytes holds text, one past them does not.
        let inner = Store::Memory(Plane {
            size: [4, 4],
            ..plane(0x1100)
        });
        records.moved(7, MoveKind::Upload, texture, rect, inner, None);
        records.moved(7, MoveKind::Copy, memory(0x5000), rect, memory(0x1400), map);
        records.moved(7, MoveKind::Present, Store::Window(9), rect, texture, map);
        let kept: Vec<_> = records
            .take()
            .records
            .into_iter()
            .filter_map(|record| match record {
                TextRecord::Move(moved) => Some((moved.kind, moved.source, moved.destination)),
                _ => None,
            })
            .collect();
        assert_eq!(
            kept,
            [
                (MoveKind::Upload, inner, texture),
                (MoveKind::Present, texture, Store::Window(9)),
            ]
        );
    }

    #[test]
    fn plane_spans_cover_bottom_up_rows() {
        let up = Plane {
            stride: -64,
            ..plane(0x1000 + 64 * 15)
        };
        assert_eq!(up.span(), [0x1000, 0x1400]);
        assert_eq!(plane(0x1000).span(), [0x1000, 0x1400]);
    }

    #[test]
    fn marked_memory_merges_touching_ranges() {
        let mut holdings = Holdings::default();
        for bits in [0x3000, 0x1000, 0x1400] {
            holdings.mark(Store::Memory(plane(bits)));
        }
        assert_eq!(holdings.memory, [[0x1000, 0x1800], [0x3000, 0x3400]]);
    }
}

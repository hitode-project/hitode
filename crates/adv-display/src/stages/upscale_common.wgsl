// Shared by compare and bookkeeping (HOST-VIEW-10).
// The frame's upload, bound at its ring offset: words, since the Vulkan heap
// tier applies a binding offset to the outer array only. A header, then lists
// addressed by word offsets from FRAME_DATA.
const FRAME_EPOCH: u32 = 0u;
const FRAME_EXACT: u32 = 1u;
const FRAME_COMPARED: u32 = 2u;
const FRAME_LIST: u32 = 3u;
const FRAME_TILE_LIST: u32 = 4u;
const FRAME_BITMAP: u32 = 5u;
const FRAME_DATA: u32 = 8u;
// Static shape of the model at this canvas size.
struct Shape {
    size: vec2<u32>, passes: u32, halo: u32,
    columns: u32, rows: u32, delta: u32, valid0: u32,
    valid1: u32, arguments0: u32, arguments1: u32, set_words: u32,
    records: u32, chunk: u32,
}
struct Grid {
    columns: u32, rows: u32, unit: u32, affected: u32,
    radius: u32, consumers: u32, aliases: u32, valid: u32,
    compacted: u32, arguments: u32, counter: u32, arena: u32,
}
struct Metadata { shape: Shape, grids: array<Grid> }

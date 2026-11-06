// Shared by compare, bookkeeping, refresh and the final model pass
// (HOST-VIEW-10, HOST-VIEW-13). Nothing here is cleared per frame: facts
// carry the epoch of the compare that wrote them, leaders an epoch tag.
struct Fact {
    // Epoch of the compare that found this 8x8 cell changed.
    stamp: u32,
    // Bit k: every pixel equals the previous input at the frame's offset k.
    matches: u32,
    constant: u32,
    // Epoch in which this cell publishes its colour's pattern into `slot`.
    owner: u32,
    colour: vec2<u32>,
    slot: u32,
    padding: u32,
}
struct Colour {
    key: vec2<u32>,
    valid: u32,
    touched: atomic<u32>,
    claimed: atomic<u32>,
    padding0: u32,
    padding1: u32,
    padding2: u32,
}
struct References {
    epoch: u32,
    // Scalars: a vec3 would align to 16 and move the arrays.
    padding0: u32,
    padding1: u32,
    padding2: u32,
    // Tag (epoch) << 17 | cell of each new colour's owner this frame.
    leaders: array<atomic<u32>, 4096>,
    colours: array<Colour, 4096>,
    facts: array<Fact>,
}
// The frame's upload, bound at its ring offset: words, since the Vulkan heap
// tier applies a binding offset to the outer array only. A header, then lists
// addressed by word offsets from FRAME_DATA.
const FRAME_EPOCH: u32 = 0u;
const FRAME_EXACT: u32 = 1u;
const FRAME_PROPOSALS: u32 = 2u;
const FRAME_COMPARED: u32 = 3u;
const FRAME_TILES: u32 = 4u;
const FRAME_LIST: u32 = 5u;
const FRAME_TILE_LIST: u32 = 6u;
const FRAME_BITMAP: u32 = 7u;
// Eight vec4<i32>: xy offset k, z nonzero when proposed; entry 0 is the
// global motion.
const FRAME_OFFSETS: u32 = 8u;
const FRAME_DATA: u32 = 40u;
// Static shape of the model at this canvas size.
struct Shape {
    size: vec2<u32>, passes: u32, halo: u32,
    columns: u32, rows: u32, threshold: u32, last_unit: u32,
    delta: u32, valid0: u32, valid1: u32, written: u32,
    arguments0: u32, arguments1: u32, set_words: u32, refresh: u32,
    refresh_counter: u32, refresh_arguments: u32, chunk: u32, reach: u32,
    translations: u32, erosion: u32, records: u32, padding: u32,
}
struct Grid {
    columns: u32, rows: u32, unit: u32, affected: u32,
    radius: u32, consumers: u32, aliases: u32, valid: u32,
    compacted: u32, arguments: u32, counter: u32, arena: u32,
}
struct Metadata { shape: Shape, grids: array<Grid> }
// State words: the motion header, its two indirect sets, then the rest at
// offsets from `Shape`.
const MOTION_OFFSET: u32 = 0u;
const MOTION_SHARP: u32 = 2u;
const MOTION_EPOCH: u32 = 3u;
const MOTION_FAILURES: u32 = 4u;
const MOTION_SKIP: u32 = 5u;
const MOTION_SETS: u32 = 8u;
const MOTION_SET_WORDS: u32 = 16u;
const MOTION_COUNTER: u32 = 12u;
// Refresh operations; rebuilds add the offset index.
const REFRESH_COPY: u32 = 0u;
const REFRESH_REBUILD: u32 = 1u;
const REFRESH_CONSTANT: u32 = 0x80000000u;

fn colour_hash(key: vec2<u32>) -> u32 {
    return (key.x * 0x9e3779b9u ^ key.y * 0x85ebca6bu) & 4095u;
}

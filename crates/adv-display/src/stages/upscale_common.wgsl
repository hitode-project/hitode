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
const FRAME_ANCHORED: u32 = 6u;
const FRAME_ANCHOR_LIST: u32 = 7u;
const FRAME_DATA: u32 = 8u;
// A list offset meaning every source cell in order: whole-canvas changes.
const WHOLE: u32 = 0xffffffffu;
// Per source cell, what compare found: the epochs it last found the cell
// changed and last matched hypotheses, and the bits of those that matched.
struct Fact { changed: u32, compared: u32, matches: u32 }
// Anchors, in words: per source cell the latest input's (hash, position),
// from PENDING per cell the anchor pass's (epoch, hash, position, vote), and
// from INDEX per cell the index's (hash, position) slots. Positions are
// x | y << 16; a vote is an offset key, zero for none.
const PENDING: u32 = 2u;
const PENDING_WORDS: u32 = 4u;
const INDEX: u32 = 6u;
const PRESENT: u32 = 0x80000000u;
const NO_ANCHOR: u32 = 0xffffffffu;
const WINDOW: u32 = 4u;
// Offset votes: VOTES keys, then their counts; the key hash takes six bits.
const VOTES: u32 = 64u;
const PROPOSALS: u32 = 4u;
const VOTE_CAP: u32 = 32u;
// Per frame parity: the count, the bits of those that copied a cell, up to
// HYPOTHESES packed offsets, the first zero, then the epoch that chose them.
const HYPOTHESES: u32 = 8u;
const HYPOTHESIS_WORDS: u32 = 11u;
// Copy entries: the cell's x and 13 bits of y, the hypothesis above.
const COPY_SHIFT: u32 = 29u;
const COPY_ROWS: u32 = 0x1fffu;

// Entry `index` of the list at header word `list`.
fn listed(list: u32, index: u32) -> u32 {
    if frame[list] == WHOLE {
        let columns = metadata.shape.columns;
        return index % columns | (index / columns) << 16u;
    }
    return frame[FRAME_DATA + frame[list] + index];
}

// Whether the anchor pass hashes: in exact first frames and frames seeking
// offsets. Otherwise it only finds the change, and bookkeeping drops the
// anchors whose windows meet it.
fn hashing() -> bool {
    return frame[FRAME_ANCHORED] != 0u && (frame[FRAME_EXACT] != 0u || frame[FRAME_COMPARED] != 0u);
}

// An offset packed as x | y << 16, both signed.
fn hypothesis(key: u32) -> vec2<i32> {
    return vec2(i32(key << 16u) >> 16u, i32(key) >> 16u);
}
// Static shape of the model at this canvas size.
struct Shape {
    size: vec2<u32>, passes: u32, halo: u32,
    columns: u32, rows: u32, delta: u32, valid0: u32,
    valid1: u32, arguments0: u32, arguments1: u32, set_words: u32,
    records: u32, chunk: u32, reach: u32, hypotheses: u32,
    votes: u32, copies: u32, copy_arguments: u32, copy_counter: u32,
    index_mask: u32, neighbourhood: u32,
    // Compare's indirect record, which the hypotheses write.
    compare_record: u32, pad: u32,
}
// One pass's grid. `affected`: input pixels a change reaches in its results;
// `producers`: passes whose results it reads; `margin`: input pixels by which
// the output cells' need grows back to its results.
struct Grid {
    columns: u32, rows: u32, unit: u32, affected: u32,
    radius: u32, producers: u32, aliases: u32, valid: u32,
    compacted: u32, arguments: u32, counter: u32, arena: u32,
    margin: u32,
}
struct Metadata { shape: Shape, grids: array<Grid> }

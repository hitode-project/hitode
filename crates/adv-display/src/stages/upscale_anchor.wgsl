// Anchors: one workgroup per source cell whose windows the change reaches
// (HOST-VIEW-13). A cell's anchor is its unique least window hash; looking it
// up in the previous frame's index votes for the offset it moved by. Only the
// previous frame's anchors are read here: bookkeeping commits these.
@group(1) @binding(0) var current: texture_2d<f32>;
@group(3) @binding(0) var<storage, read> frame: array<u32>;
@group(3) @binding(1) var<storage, read_write> anchors: array<u32>;
@group(3) @binding(2) var<storage, read_write> state: array<atomic<u32>>;
@group(3) @binding(3) var<storage, read> metadata: Metadata;
const PROBES: u32 = 8u;
var<workgroup> least: atomic<u32>;
var<workgroup> ties: atomic<u32>;
// The cell and the pixels its windows reach beyond it, as mixed pixel values,
// then each row's hashes of WINDOW pixels.
const SIDE: u32 = 8u + WINDOW - 1u;
var<workgroup> pixels: array<u32, SIDE * SIDE>;
var<workgroup> rows: array<u32, SIDE * 8u>;

fn mixed(v: u32) -> u32 {
    var h = v;
    h ^= h >> 16u;
    h *= 0x7feb352du;
    h ^= h >> 15u;
    h *= 0x846ca68bu;
    return h ^ (h >> 16u);
}

// A polynomial over WINDOW values, so a window's hash is the content's alone.
fn polynomial(a: u32, b: u32, c: u32, d: u32, base: u32) -> u32 {
    return ((d * base + c) * base + b) * base + a;
}

// One vote for `offset`, lost when PROBES slots from its own are taken; the
// offset's key either way.
// Counts saturate at VOTE_CAP: ranking needs no more, and further votes only
// read.
fn vote(offset: vec2<i32>) -> u32 {
    let votes = metadata.shape.votes;
    let key = (u32(offset.x) & 0xffffu) | (u32(offset.y) << 16u);
    var slot = (key * 0x9e3779b1u) >> 26u;
    for (var probes = 0u; probes < PROBES; probes++) {
        var held = atomicLoad(&state[votes + slot]);
        if held == 0u {
            held = atomicCompareExchangeWeak(&state[votes + slot], 0u, key).old_value;
            // A weak exchange may fail spuriously; the slot then stays empty.
            if held == 0u && atomicLoad(&state[votes + slot]) == key { held = key; }
        }
        if held == key {
            if atomicLoad(&state[votes + VOTES + slot]) < VOTE_CAP { atomicAdd(&state[votes + VOTES + slot], 1u); }
            return key;
        }
        if held != 0u { slot = (slot + 1u) % VOTES; }
    }
    return key;
}

@compute @workgroup_size(64)
fn main(@builtin(workgroup_id) group: vec3<u32>, @builtin(local_invocation_index) lane: u32) {
    let index = group.y * 65535u + group.x;
    if index >= frame[FRAME_ANCHORED] { return; }
    let shape = metadata.shape;
    let packed = listed(FRAME_ANCHOR_LIST, index);
    let cell = vec2(packed & 65535u, packed >> 16u);
    let pixel = cell * 8u + vec2(lane % 8u, lane / 8u);
    if lane == 0u { atomicStore(&least, NO_ANCHOR); }
    // Each pixel the windows read once; beyond the canvas it is never used.
    let limit = vec2<i32>(shape.size) - 1;
    for (var i = lane; i < SIDE * SIDE; i += 64u) {
        let at = min(vec2<i32>(cell * 8u + vec2(i % SIDE, i / SIDE)), limit);
        let p = bitcast<vec4<u32>>(textureLoad(current, at, 0));
        pixels[i] = mixed(p.x ^ mixed(p.y ^ mixed(p.z ^ mixed(p.w))));
    }
    workgroupBarrier();
    for (var i = lane; i < SIDE * 8u; i += 64u) {
        let at = (i / 8u) * SIDE + i % 8u;
        rows[i] = polynomial(pixels[at], pixels[at + 1u], pixels[at + 2u], pixels[at + 3u], 0x01000193u);
    }
    workgroupBarrier();
    var hash = NO_ANCHOR;
    if all(pixel + WINDOW <= shape.size) {
        let at = lane;
        hash = min(mixed(polynomial(rows[at], rows[at + 8u], rows[at + 16u], rows[at + 24u], 0x9e3779b1u)), NO_ANCHOR - 1u);
        atomicMin(&least, hash);
    }
    workgroupBarrier();
    let found = atomicLoad(&least);
    if hash == found && found != NO_ANCHOR { atomicAdd(&ties, 1u); }
    workgroupBarrier();
    let unique = found != NO_ANCHOR && atomicLoad(&ties) == 1u;
    let at = cell.y * shape.columns + cell.x;
    let pending = PENDING * shape.columns * shape.rows + at * PENDING_WORDS;
    if !unique {
        // Flat areas, lines and periodic texture have no anchor.
        if lane == 0u {
            anchors[pending] = frame[FRAME_EPOCH];
            anchors[pending + 1u] = 0u;
            anchors[pending + 2u] = 0u;
            anchors[pending + 3u] = 0u;
        }
        return;
    }
    if hash != found { return; }
    let position = pixel.x | pixel.y << 16u | PRESENT;
    anchors[pending] = frame[FRAME_EPOCH];
    anchors[pending + 1u] = hash;
    anchors[pending + 2u] = position;
    anchors[pending + 3u] = 0u;
    // The index may hold anything; an entry counts only while its cell's
    // anchor is still that entry.
    let entry = INDEX * shape.columns * shape.rows + (hash & shape.index_mask) * 2u;
    let old = anchors[entry + 1u];
    if anchors[entry] != hash || (old & PRESENT) == 0u { return; }
    let source = vec2(old & 0xffffu, (old >> 16u) & 0x7fffu);
    let owner = (source.y / 8u * shape.columns + source.x / 8u) * 2u;
    if anchors[owner] != hash || anchors[owner + 1u] != old { return; }
    let offset = vec2<i32>(pixel) - vec2<i32>(source);
    if any(offset != vec2(0)) {
        // Compare verifies the offset only near the cells that voted for it.
        anchors[pending + 3u] = vote(offset);
    }
}

// Bookkeeping: one dispatch per frame, one workgroup per 128-pixel tile
// (HOST-VIEW-10). Each tile selects every pass's cells over itself and a halo,
// in workgroup memory, from read-only inputs: delta and the validity the
// previous frame left; then settles which computed cells are exact. Halo
// results equal the neighbour's own, so only interior cells are written: each
// pass's compacted list, its validity, the copies and the delta bitmap; the
// interior's anchors commit here (HOST-VIEW-13).
const TILE: u32 = 128u;
const ARENA: u32 = 2048u;
const LANES: u32 = 256u;
// Source cells across a tile's delta region, in words of 32 cells.
const DELTA_WORDS: u32 = 128u;
@group(3) @binding(0) var<storage, read> frame: array<u32>;
@group(3) @binding(1) var<storage, read> facts: array<Fact>;
@group(3) @binding(2) var<storage, read_write> state: array<atomic<u32>>;
@group(3) @binding(3) var<storage, read> metadata: Metadata;
@group(3) @binding(4) var<storage, read_write> compacted: array<u32>;
@group(3) @binding(5) var<storage, read_write> anchors: array<u32>;
var<workgroup> required: array<atomic<u32>, ARENA>;
var<workgroup> validity: array<atomic<u32>, ARENA>;
// Delta over the tile, its halo and the largest affected reach.
var<workgroup> delta: array<atomic<u32>, DELTA_WORDS>;
// Per list, a pass's or the copies': entries counted, then written.
var<workgroup> counts: array<atomic<u32>, 33>;
// The last pass's required local cells: first x, y and last-plus-one x, y.
var<workgroup> bounds: array<atomic<u32>, 4>;
var<workgroup> bases: array<u32, 33>;
// The changed delta cells: complemented first x and y, then last-plus-one x
// and y, so that zero-initialized memory starts empty.
var<workgroup> changes: array<atomic<u32>, 4>;
// This frame's hypotheses, unless it seeks none.
var<workgroup> seeking: u32;
var<workgroup> offsets: array<vec2<i32>, HYPOTHESES>;
// Whether delta meets the tile's region and margin.
var<workgroup> stirred: u32;

fn tile_origin(group: vec3<u32>) -> vec2<i32> {
    let packed = frame[FRAME_DATA + frame[FRAME_TILE_LIST] + group.y * 65535u + group.x];
    return vec2<i32>(vec2(packed & 65535u, packed >> 16u) * TILE);
}

// Cells of `unit` across the tile and halo, starting at `origin` cells.
fn span(unit: u32) -> u32 {
    return (TILE + 2u * metadata.shape.halo) >> countTrailingZeros(unit);
}

// The halo, the largest affected reach and a pass unit, in whole cells.
fn margin() -> u32 {
    return metadata.shape.halo + 32u;
}

fn delta_span() -> u32 {
    return (TILE + 2u * margin()) / 8u;
}

fn bit(words: u32, local: vec2<u32>) -> vec2<u32> {
    return vec2(local.y * words + local.x / 32u, 1u << (local.x % 32u));
}

// Two adjacent words' 32 bits from bit `shift` of the first on.
fn funnel(low: u32, high: u32, shift: u32) -> u32 {
    return select((low >> shift) | (high << (32u - shift)), low, shift == 0u);
}

// Whether each of the 32 cells from `at` on changed; none beyond the canvas.
// A lane's reads here do not wait on each other.
fn changed_bits(at: vec2<i32>) -> u32 {
    let shape = metadata.shape;
    if frame[FRAME_EXACT] != 0u {
        let stride = (shape.columns + 31u) / 32u;
        let row = FRAME_DATA + frame[FRAME_BITMAP] + u32(at.y) * stride;
        let word = at.x >> 5u;
        var low = 0u;
        var high = 0u;
        if word >= 0 && word < i32(stride) { low = frame[row + u32(word)]; }
        if word + 1 >= 0 && word + 1 < i32(stride) { high = frame[row + u32(word + 1)]; }
        return funnel(low, high, u32(at.x) & 31u);
    }
    var bits = 0u;
    for (var x = 0u; x < 32u; x++) {
        let c = at.x + i32(x);
        if c >= 0 && c < i32(shape.columns)
            && facts[u32(at.y) * shape.columns + u32(c)].changed == frame[FRAME_EPOCH] { bits |= 1u << x; }
    }
    return bits;
}

// Per component: SPIR-V readers reject naga's vector signed-division wrapper.
fn quotient(v: vec2<i32>, d: i32) -> vec2<i32> {
    return vec2(v.x / d, v.y / d);
}

// Whether delta meets the pixels [lo, hi), clipped to the canvas.
fn meets(origin: vec2<i32>, lo: vec2<i32>, hi: vec2<i32>) -> bool {
    let size = vec2<i32>(metadata.shape.size);
    let first = quotient(max(lo, vec2(0)) - (origin - i32(margin())), 8);
    let last = quotient(min(hi, size) - 1 - (origin - i32(margin())), 8);
    if any(last < first) { return false; }
    let words = (delta_span() + 31u) / 32u;
    for (var y = first.y; y <= last.y; y++) {
        for (var word = u32(first.x) / 32u; word <= u32(last.x) / 32u; word++) {
            let lo_bit = select(0u, u32(first.x) % 32u, word == u32(first.x) / 32u);
            let hi_bit = select(31u, u32(last.x) % 32u, word == u32(last.x) / 32u);
            let mask = (0xffffffffu << lo_bit) & (0xffffffffu >> (31u - hi_bit));
            if (atomicLoad(&delta[u32(y) * words + word]) & mask) != 0u { return true; }
        }
    }
    return false;
}

fn grid_origin(origin: vec2<i32>, unit: u32) -> vec2<i32> {
    return (origin - i32(metadata.shape.halo)) >> vec2(countTrailingZeros(unit));
}

// The bits [lo, hi) that fall in word `word` of a row.
fn row_mask(word: u32, lo: u32, hi: u32) -> u32 {
    let first = max(lo, word * 32u) - word * 32u;
    let width = min(hi, word * 32u + 32u) - word * 32u - first;
    return select((1u << width) - 1u, 0xffffffffu, width == 32u) << first;
}

// Region cells of grid `index` meeting the pixels [lo, hi) clipped to the
// canvas, as local first and last-plus-one cells.
fn local_cells(index: u32, origin: vec2<i32>, lo: vec2<i32>, hi: vec2<i32>) -> vec4<i32> {
    let unit = metadata.grids[index].unit;
    let shift = vec2(countTrailingZeros(unit));
    let size = vec2<i32>(metadata.shape.size);
    let start = grid_origin(origin, unit);
    let first = (max(lo, vec2(0)) >> shift) - start;
    let last = ((min(hi, size) + i32(unit) - 1) >> shift) - start;
    return vec4(first, last);
}

// The low `count` bits of a word, `count` at most 32.
fn low_bits(count: u32) -> u32 {
    return select((1u << count) - 1u, 0xffffffffu, count >= 32u);
}

// The item of a per-pass loop a lane takes first when `before` items preceded
// it: consecutive short loops then spread over every lane.
fn first_item(lane: u32, before: u32) -> u32 {
    return (lane + LANES - before % LANES) % LANES;
}

// 32 bits of a canvas validity row from cell `x` on. Cells left of the canvas
// read as invalid; bits beyond the last column are never set.
fn canvas_bits(row: u32, stride: u32, x: i32) -> u32 {
    let word = x >> 5u;
    let shift = u32(x) & 31u;
    var low = 0u;
    var high = 0u;
    if word >= 0 && word < i32(stride) { low = atomicLoad(&state[row + u32(word)]); }
    if shift != 0u && word + 1 >= 0 && word + 1 < i32(stride) { high = atomicLoad(&state[row + u32(word + 1)]); }
    return funnel(low, high, shift);
}

// `count` bits of a region validity row from local cell `x` on.
fn region_bits(row: u32, x: u32, count: u32) -> u32 {
    let shift = x % 32u;
    var bits = atomicLoad(&validity[row + x / 32u]) >> shift;
    if shift + count > 32u { bits |= atomicLoad(&validity[row + x / 32u + 1u]) << (32u - shift); }
    return bits & low_bits(count);
}

fn joined(a: vec4<i32>, b: vec4<i32>) -> vec4<i32> {
    if any(a.zw <= a.xy) { return b; }
    if any(b.zw <= b.xy) { return a; }
    return vec4(min(a.xy, b.xy), max(a.zw, b.zw));
}

// Local cells of pass `index` this change can select or invalidate, as first
// and last-plus-one: those whose results meet the changed pixels [lo, hi),
// and in inner passes those meeting the need of the last pass's required
// cells. Selection outside them keeps nothing and changes no validity.
fn selectable(index: u32, origin: vec2<i32>, lo: vec2<i32>, hi: vec2<i32>) -> vec4<i32> {
    let shape = metadata.shape;
    let grid = metadata.grids[index];
    let n = vec2(i32(span(grid.unit)));
    var cells = local_cells(index, origin, lo - i32(grid.affected), hi + i32(grid.affected));
    if index + 1u < shape.passes {
        let low = vec2(atomicLoad(&bounds[0]), atomicLoad(&bounds[1]));
        let high = vec2(atomicLoad(&bounds[2]), atomicLoad(&bounds[3]));
        if all(low < high) {
            let unit = i32(metadata.grids[shape.passes - 1u].unit);
            let first = grid_origin(origin, u32(unit));
            let m = i32(grid.margin);
            let need = local_cells(index, origin, (first + vec2<i32>(low)) * unit - m, (first + vec2<i32>(high)) * unit + m);
            cells = joined(cells, need);
        }
    }
    return vec4(max(cells.xy, vec2(0)), min(cells.zw, n));
}

// The interior cells of region cells `cells` of pass `index`.
fn interior(index: u32, cells: vec4<i32>) -> vec4<i32> {
    let unit = metadata.grids[index].unit;
    let offset = i32(metadata.shape.halo / unit);
    return vec4(max(cells.xy, vec2(offset)), min(cells.zw, vec2(offset + i32(TILE / unit))));
}

fn area(cells: vec4<i32>) -> u32 {
    return u32(max(cells.z - cells.x, 0) * max(cells.w - cells.y, 0));
}

// Local cell `i` of `cells`, row by row.
fn nth(cells: vec4<i32>, i: u32) -> vec2<u32> {
    let w = u32(cells.z - cells.x);
    return vec2<u32>(cells.xy) + vec2(i % w, i / w);
}

// Whether the last pass requires a cell meeting the pixels [pixel - radius,
// pixel + unit + radius); false beyond its required cells' bounds. One word
// per row.
fn required_by(index: u32, origin: vec2<i32>, pixel: vec2<i32>, unit: u32, radius: u32) -> bool {
    let grid = metadata.grids[index];
    let n = i32(span(grid.unit));
    let cells = local_cells(index, origin, pixel - i32(radius), pixel + i32(unit + radius));
    let low = vec2(atomicLoad(&bounds[0]), atomicLoad(&bounds[1]));
    let high = vec2(atomicLoad(&bounds[2]), atomicLoad(&bounds[3]));
    let first = max(cells.xy, vec2<i32>(min(low, vec2(u32(n)))));
    let last = min(min(cells.zw, vec2(n)), vec2<i32>(high));
    if any(last <= first) { return false; }
    let words = (u32(n) + 31u) / 32u;
    for (var y = first.y; y < last.y; y++) {
        for (var w = u32(first.x) / 32u; w <= u32(last.x - 1) / 32u; w++) {
            let mask = row_mask(w, u32(first.x), u32(last.x));
            if (atomicLoad(&required[grid.arena + u32(y) * words + w]) & mask) != 0u { return true; }
        }
    }
    return false;
}

// Whether every cell of grid `index` meeting the pixels [lo, hi), clipped to
// the canvas, holds exact results; false beyond the region.
fn all_valid(index: u32, origin: vec2<i32>, lo: vec2<i32>, hi: vec2<i32>) -> bool {
    let grid = metadata.grids[index];
    let n = i32(span(grid.unit));
    let cells = local_cells(index, origin, lo, hi);
    if any(cells.zw <= cells.xy) { return true; }
    if any(cells.xy < vec2(0)) || any(cells.zw > vec2(n)) { return false; }
    let words = (u32(n) + 31u) / 32u;
    for (var y = cells.y; y < cells.w; y++) {
        for (var w = u32(cells.x) / 32u; w <= u32(cells.z - 1) / 32u; w++) {
            let mask = row_mask(w, u32(cells.x), u32(cells.z));
            if (atomicLoad(&validity[grid.arena + u32(y) * words + w]) & mask) != mask { return false; }
        }
    }
    return true;
}

fn hypotheses() -> u32 {
    return metadata.shape.hypotheses + (frame[FRAME_EPOCH] % 2u) * HYPOTHESIS_WORDS;
}

// Whether a field [lo, hi) along one axis, moved by h, keeps the border's
// clamp: either it does not move, or neither it nor its source meets it.
fn clear(lo: i32, hi: i32, h: i32, size: i32) -> bool {
    return h == 0 || (lo >= 0 && hi <= size && lo - h >= 0 && hi - h <= size);
}

// The first hypothesis h != 0 under which last-pass cell `cell` copies the
// previous output at 2h, or zero: every source cell meeting its field matched
// the old input at h, and on each axis where h moves, both fields stay off
// the canvas border.
fn decide(cell: vec2<i32>) -> u32 {
    let shape = metadata.shape;
    let n = seeking;
    if n < 2u { return 0u; }
    let size = vec2<i32>(shape.size);
    let unit = i32(metadata.grids[shape.passes - 1u].unit);
    let reach = i32(shape.reach);
    let lo = cell * unit - reach;
    let hi = min(cell * unit + unit, size) + reach;
    var mask = 0u;
    for (var i = 1u; i < n; i++) {
        let h = offsets[i];
        if clear(lo.x, hi.x, h.x, size.x) && clear(lo.y, hi.y, h.y, size.y) { mask |= 1u << i; }
    }
    let first = vec2<u32>(max(lo, vec2(0))) / 8u;
    let last = vec2<u32>(min(hi, size) - 1) / 8u;
    for (var y = first.y; y <= last.y && mask != 0u; y++) {
        for (var x = first.x; x <= last.x && mask != 0u; x++) {
            let fact = facts[y * shape.columns + x];
            mask &= select(0u, fact.matches, fact.compared == frame[FRAME_EPOCH]);
        }
    }
    return select(0u, firstTrailingBit(mask), mask != 0u);
}

// The output cells a change reaches and no offset copies; with `last` false,
// in every other pass the cells meeting their need that hold no exact result.
// The need is in pixels, so a cell's outer pixels may stay inexact, and it
// depends on the last pass alone: passes select without barriers between.
fn select_cells(index: u32, origin: vec2<i32>, start: u32, cells: vec4<i32>) {
    let shape = metadata.shape;
    let grid = metadata.grids[index];
    let n = span(grid.unit);
    let words = (n + 31u) / 32u;
    let first = grid_origin(origin, grid.unit);
    let last = index + 1u == shape.passes;
    var low = vec2(n);
    var high = vec2(0u);
    for (var i = start; i < area(cells); i += LANES) {
        let local = nth(cells, i);
        let cell = first + vec2<i32>(local);
        if any(cell < vec2(0)) || cell.x >= i32(grid.columns) || cell.y >= i32(grid.rows) { continue; }
        let pixel = cell * i32(grid.unit);
        let b = bit(words, local);
        let slot = grid.arena + b.x;
        let affected = meets(origin, pixel - i32(grid.affected), pixel + i32(grid.unit + grid.affected));
        var keep: bool;
        if last {
            keep = affected && decide(cell) == 0u;
            if keep {
                low = min(low, local);
                high = max(high, local + 1u);
            }
        } else {
            if affected { atomicAnd(&validity[slot], ~b.y); }
            let valid = (atomicLoad(&validity[slot]) & b.y) != 0u;
            keep = !valid && required_by(shape.passes - 1u, origin, pixel, grid.unit, grid.margin);
            // Without producers, a computed cell is exact.
            if keep && grid.producers == 0u { atomicOr(&validity[slot], b.y); }
        }
        if keep { atomicOr(&required[slot], b.y); }
    }
    if last && all(low < high) {
        atomicMin(&bounds[0], low.x);
        atomicMin(&bounds[1], low.y);
        atomicMax(&bounds[2], high.x);
        atomicMax(&bounds[3], high.y);
    }
}

// Forwards: a computed cell is exact where every producer cell it read was.
fn settle(index: u32, origin: vec2<i32>, lane: u32, cells: vec4<i32>) {
    let grid = metadata.grids[index];
    let n = span(grid.unit);
    let words = (n + 31u) / 32u;
    let first = grid_origin(origin, grid.unit);
    let radius = i32(grid.radius);
    for (var i = lane; i < area(cells); i += LANES) {
        let local = nth(cells, i);
        let b = bit(words, local);
        let slot = grid.arena + b.x;
        if (atomicLoad(&required[slot]) & b.y) == 0u { continue; }
        let pixel = (first + vec2<i32>(local)) * i32(grid.unit);
        var exact = true;
        for (var rest = grid.producers; rest != 0u && exact; rest &= rest - 1u) {
            exact = all_valid(firstTrailingBit(rest), origin, pixel - radius, pixel + i32(grid.unit) + radius);
        }
        if exact { atomicOr(&validity[slot], b.y); }
    }
    workgroupBarrier();
}

// The list `index`, a pass or the copies after them, as (counter, records,
// first entry).
fn list(index: u32) -> vec3<u32> {
    let shape = metadata.shape;
    if index == shape.passes { return vec3(shape.copy_counter, shape.copy_arguments, shape.copies); }
    let grid = metadata.grids[index];
    return vec3(grid.counter, grid.arguments, grid.compacted);
}

// Reserve `count` list entries of a counter and size its indirect records.
fn reserve(counter: u32, records: u32, kept_count: u32) -> u32 {
    let start = atomicAdd(&state[counter], kept_count);
    let chunk = metadata.shape.chunk;
    let end = start + kept_count;
    for (var slice = start / chunk; slice * chunk < end; slice++) {
        atomicMax(&state[records + slice * 4u], min(chunk, end - slice * chunk));
    }
    return start;
}

fn arguments(parity: u32) -> u32 {
    return select(metadata.shape.arguments0, metadata.shape.arguments1, parity == 1u);
}

// Group 0 resets what the next frame accumulates into.
fn reset_next(lane: u32) {
    let shape = metadata.shape;
    let next = arguments((frame[FRAME_EPOCH] + 1u) % 2u);
    // Indirect records (x, 1, 1, 0) first, then the counters.
    for (var i = lane; i < shape.set_words; i += LANES) {
        let unit = i < shape.records && (i % 4u == 1u || i % 4u == 2u);
        atomicStore(&state[next + i], select(0u, 1u, unit));
    }
    // Compare read this frame's votes; the next frame's anchors cast new ones.
    for (var i = lane; i < 2u * VOTES; i += LANES) { atomicStore(&state[shape.votes + i], 0u); }
}

fn write_bits(global: u32, stride: u32, cell: vec2<u32>, count: u32, bits: u32) {
    // `count` interior cells starting at `cell`, within one word.
    let word = global + cell.y * stride + cell.x / 32u;
    let shift = cell.x % 32u;
    let mask = select((1u << count) - 1u, 0xffffffffu, count == 32u) << shift;
    atomicAnd(&state[word], ~mask);
    atomicOr(&state[word], (bits << shift) & mask);
}

// A tile whose margin delta never meets selects nothing and keeps its
// validity: the interior's previous bits carry over, its delta bits clear.
fn carry(origin: vec2<i32>, lane: u32, previous_valid: u32, current_valid: u32) {
    let shape = metadata.shape;
    for (var p = 0u; p + 1u < shape.passes; p++) {
        let grid = metadata.grids[p];
        if grid.aliases != 0u { continue; }
        let inner = TILE / grid.unit;
        let start = vec2<u32>(origin) / grid.unit;
        let stride = (grid.columns + 31u) / 32u;
        let run = min(inner, 32u);
        let per_row = inner / run;
        for (var i = lane; i < inner * per_row; i += LANES) {
            let c = start + vec2((i % per_row) * run, i / per_row);
            if c.x >= grid.columns || c.y >= grid.rows { continue; }
            let word = atomicLoad(&state[previous_valid + grid.valid + c.y * stride + c.x / 32u]);
            write_bits(current_valid + grid.valid, stride, c, min(run, grid.columns - c.x), word >> (c.x % 32u));
        }
    }
    let first = vec2<u32>(origin) / 8u;
    let stride = (shape.columns + 31u) / 32u;
    for (var row = lane; row < TILE / 8u; row += LANES) {
        let c = first + vec2(0u, row);
        if c.x >= shape.columns || c.y >= shape.rows { continue; }
        write_bits(shape.delta, stride, c, min(TILE / 8u, shape.columns - c.x), 0u);
    }
}

@compute @workgroup_size(256)
fn main(@builtin(workgroup_id) group: vec3<u32>, @builtin(local_invocation_index) lane: u32) {
    let shape = metadata.shape;
    let origin = tile_origin(group);
    if group.x == 0u && group.y == 0u { reset_next(lane); }
    if lane < 2u { atomicStore(&bounds[lane], 0xffffffffu); }
    let parity = frame[FRAME_EPOCH] % 2u;
    let previous_valid = select(shape.valid0, shape.valid1, parity == 0u);
    let current_valid = select(shape.valid0, shape.valid1, parity == 1u);
    // Delta over the region and margin.
    let dn = delta_span();
    let dwords = (dn + 31u) / 32u;
    let dfirst = quotient(origin - i32(margin()), 8);
    var low = vec2(dn);
    var high = vec2(0u);
    for (var i = lane; i < dn * dwords; i += LANES) {
        let row = i / dwords;
        let w = i % dwords;
        let y = dfirst.y + i32(row);
        if y < 0 || y >= i32(shape.rows) { continue; }
        let bits = changed_bits(vec2(dfirst.x + i32(w * 32u), y)) & low_bits(dn - w * 32u);
        if bits != 0u {
            atomicStore(&delta[i], bits);
            low = min(low, vec2(w * 32u + firstTrailingBit(bits), row));
            high = max(high, vec2(w * 32u + firstLeadingBit(bits) + 1u, row + 1u));
        }
    }
    if lane == 0u {
        // Hypotheses this frame chose, for every decision.
        let block = hypotheses();
        let n = min(atomicLoad(&state[block]), HYPOTHESES);
        // Frames seeking no offsets leave an older frame's hypotheses here.
        seeking = select(0u, n, atomicLoad(&state[block + 2u + HYPOTHESES]) == frame[FRAME_EPOCH]);
        for (var i = 1u; i < seeking; i++) { offsets[i] = hypothesis(atomicLoad(&state[block + 2u + i])); }
    }
    if all(low < high) {
        atomicMax(&changes[0], ~low.x);
        atomicMax(&changes[1], ~low.y);
        atomicMax(&changes[2], high.x);
        atomicMax(&changes[3], high.y);
    }
    workgroupBarrier();
    if lane == 0u { stirred = atomicLoad(&changes[2]); }
    if workgroupUniformLoad(&stirred) == 0u {
        carry(origin, lane, previous_valid, current_valid);
        return;
    }
    // The changed pixels' bounds.
    let dlo = (dfirst + vec2<i32>(~vec2(atomicLoad(&changes[0]), atomicLoad(&changes[1])))) * 8;
    let dhi = (dfirst + vec2<i32>(vec2(atomicLoad(&changes[2]), atomicLoad(&changes[3])))) * 8;
    // Validity the previous frame left, over the region of every inner pass
    // whose storage no other pass shares: what a shared tensor holds outside
    // this frame's writes may be another pass's.
    // Word by word, every pass's words spread over all lanes.
    var before = 0u;
    for (var p = 0u; p + 1u < shape.passes; p++) {
        let grid = metadata.grids[p];
        if grid.aliases != 0u { continue; }
        let n = span(grid.unit);
        let words = (n + 31u) / 32u;
        let first = grid_origin(origin, grid.unit);
        let stride = (grid.columns + 31u) / 32u;
        for (var i = first_item(lane, before); i < n * words; i += LANES) {
            let row = first.y + i32(i / words);
            if row < 0 || row >= i32(grid.rows) { continue; }
            let w = i % words;
            let bits = canvas_bits(previous_valid + grid.valid + u32(row) * stride, stride, first.x + i32(w * 32u));
            atomicStore(&validity[grid.arena + i], bits & low_bits(n - w * 32u));
        }
        before += n * words;
    }
    workgroupBarrier();
    let final_cells = selectable(shape.passes - 1u, origin, dlo, dhi);
    select_cells(shape.passes - 1u, origin, lane, final_cells);
    workgroupBarrier();
    before = 0u;
    for (var p = 0u; p + 1u < shape.passes; p++) {
        let cells = selectable(p, origin, dlo, dhi);
        select_cells(p, origin, first_item(lane, before), cells);
        before += area(cells);
    }
    workgroupBarrier();
    for (var p = 1u; p + 1u < shape.passes; p++) {
        if metadata.grids[p].producers != 0u { settle(p, origin, lane, selectable(p, origin, dlo, dhi)); }
    }
    // Interior output cells copied at an offset, with their hypothesis.
    let last = metadata.grids[shape.passes - 1u];
    let start = vec2<u32>(origin) / last.unit;
    let offset = vec2(shape.halo / last.unit);
    let words = (span(last.unit) + 31u) / 32u;
    var copies: array<u32, 4>;
    var found = 0u;
    var won = 0u;
    let copied = interior(shape.passes - 1u, final_cells);
    for (var i = lane; i < area(copied); i += LANES) {
        let local = nth(copied, i);
        let c = start + local - offset;
        if c.x >= last.columns || c.y >= last.rows { continue; }
        let b = bit(words, local);
        let pixel = vec2<i32>(c * last.unit);
        if (atomicLoad(&required[last.arena + b.x]) & b.y) != 0u
            || !meets(origin, pixel - i32(last.affected), pixel + i32(last.unit + last.affected)) { continue; }
        let h = decide(vec2<i32>(c));
        copies[found] = c.x | c.y << 16u | h << COPY_SHIFT;
        found++;
        won |= 1u << h;
    }
    if won != 0u { atomicOr(&state[hypotheses() + 1u], won); }
    if found != 0u { atomicAdd(&counts[shape.passes], found); }
    // Interior lists: count every pass's entries, reserve them, write them.
    for (var p = 0u; p < shape.passes; p++) {
        let grid = metadata.grids[p];
        let n = span(grid.unit);
        let start = vec2<u32>(origin) / grid.unit;
        let offset = vec2(shape.halo / grid.unit);
        let cells = interior(p, selectable(p, origin, dlo, dhi));
        var count = 0u;
        for (var i = lane; i < area(cells); i += LANES) {
            let local = nth(cells, i);
            let c = start + local - offset;
            if c.x >= grid.columns || c.y >= grid.rows { continue; }
            let b = bit((n + 31u) / 32u, local);
            if (atomicLoad(&required[grid.arena + b.x]) & b.y) != 0u { count++; }
        }
        if count != 0u { atomicAdd(&counts[p], count); }
    }
    workgroupBarrier();
    let current = arguments(parity);
    if lane <= shape.passes {
        let total = atomicLoad(&counts[lane]);
        let at = list(lane);
        if total != 0u { bases[lane] = reserve(current + at.x, current + at.y, total); }
        atomicStore(&counts[lane], 0u);
    }
    workgroupBarrier();
    if found != 0u {
        let at = bases[shape.passes] + atomicAdd(&counts[shape.passes], found);
        for (var i = 0u; i < found; i++) { compacted[shape.copies + at + i] = copies[i]; }
    }
    before = 0u;
    for (var p = 0u; p < shape.passes; p++) {
        let grid = metadata.grids[p];
        let n = span(grid.unit);
        let words = (n + 31u) / 32u;
        let inner = TILE / grid.unit;
        let start = vec2<u32>(origin) / grid.unit;
        let offset = vec2(shape.halo / grid.unit);
        let cells = interior(p, selectable(p, origin, dlo, dhi));
        for (var i = lane; i < area(cells); i += LANES) {
            let local = nth(cells, i);
            let c = start + local - offset;
            if c.x >= grid.columns || c.y >= grid.rows { continue; }
            let b = bit(words, local);
            if (atomicLoad(&required[grid.arena + b.x]) & b.y) != 0u {
                compacted[grid.compacted + bases[p] + atomicAdd(&counts[p], 1u)] = c.x | c.y << 16u;
            }
        }
        // Validity rows of the interior, at most one word per row and lane;
        // shared storage keeps none.
        if grid.aliases != 0u || p + 1u == shape.passes { continue; }
        let stride = (grid.columns + 31u) / 32u;
        let run = min(inner, 32u);
        let per_row = inner / run;
        for (var i = first_item(lane, before); i < inner * per_row; i += LANES) {
            let row = i / per_row;
            let column = (i % per_row) * run;
            let c = start + vec2(column, row);
            if c.x >= grid.columns || c.y >= grid.rows { continue; }
            let count = min(run, grid.columns - c.x);
            let bits = region_bits(grid.arena + (offset.y + row) * words, offset.x + column, count);
            write_bits(current_valid + grid.valid, stride, c, count, bits);
        }
        before += inner * per_row;
    }
    // Commit the interior's new anchors to the per-cell array and the index,
    // or drop those whose windows meet the change when none were hashed. The
    // windows read a cell and those right of and below it.
    let interior_first = vec2<u32>(origin) / 8u;
    let cells = shape.columns * shape.rows;
    let hashed = hashing();
    for (var i = lane; i < (TILE / 8u) * (TILE / 8u); i += LANES) {
        let c = interior_first + vec2(i % (TILE / 8u), i / (TILE / 8u));
        if c.x >= shape.columns || c.y >= shape.rows { continue; }
        let at = c.y * shape.columns + c.x;
        if !hashed {
            let local = vec2(margin() / 8u) + vec2(i % (TILE / 8u), i / (TILE / 8u));
            var reached = false;
            for (var d = 0u; d < 4u; d++) {
                let b = bit(dwords, local + vec2(d % 2u, d / 2u));
                reached = reached || (atomicLoad(&delta[b.x]) & b.y) != 0u;
            }
            if reached { anchors[at * 2u + 1u] = 0u; }
            continue;
        }
        let pending = PENDING * cells + at * PENDING_WORDS;
        if anchors[pending] != frame[FRAME_EPOCH] { continue; }
        let hash = anchors[pending + 1u];
        let position = anchors[pending + 2u];
        anchors[at * 2u] = hash;
        anchors[at * 2u + 1u] = position;
        if (position & PRESENT) != 0u {
            let entry = INDEX * cells + (hash & shape.index_mask) * 2u;
            anchors[entry] = hash;
            anchors[entry + 1u] = position;
        }
    }
    // The delta bitmap of the interior, for the damage overlay.
    let stride = (shape.columns + 31u) / 32u;
    for (var row = lane; row < TILE / 8u; row += LANES) {
        let c = interior_first + vec2(0u, row);
        if c.x >= shape.columns || c.y >= shape.rows { continue; }
        let count = min(TILE / 8u, shape.columns - c.x);
        // From this tile's delta, whose region starts a margin before it.
        let at = (margin() / 8u + row) * dwords;
        let x = margin() / 8u;
        var bits = atomicLoad(&delta[at + x / 32u]) >> (x % 32u);
        if x % 32u + count > 32u { bits |= atomicLoad(&delta[at + x / 32u + 1u]) << (32u - x % 32u); }
        write_bits(shape.delta, stride, c, count, bits);
    }
}

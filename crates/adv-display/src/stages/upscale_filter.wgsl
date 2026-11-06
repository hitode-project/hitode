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

fn changed(cell: vec2<u32>) -> bool {
    let shape = metadata.shape;
    if frame[FRAME_EXACT] != 0u {
        let stride = (shape.columns + 31u) / 32u;
        return (frame[FRAME_DATA + frame[FRAME_BITMAP] + cell.y * stride + cell.x / 32u] & (1u << (cell.x % 32u))) != 0u;
    }
    return facts[cell.y * shape.columns + cell.x].changed == frame[FRAME_EPOCH];
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
    let block = hypotheses();
    let n = min(atomicLoad(&state[block]), HYPOTHESES);
    if n < 2u { return 0u; }
    let size = vec2<i32>(shape.size);
    let unit = i32(metadata.grids[shape.passes - 1u].unit);
    let reach = i32(shape.reach);
    let lo = cell * unit - reach;
    let hi = min(cell * unit + unit, size) + reach;
    var mask = 0u;
    for (var i = 1u; i < n; i++) {
        let h = hypothesis(atomicLoad(&state[block + 2u + i]));
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
fn select_cells(index: u32, origin: vec2<i32>, lane: u32) {
    let shape = metadata.shape;
    let grid = metadata.grids[index];
    let n = span(grid.unit);
    let words = (n + 31u) / 32u;
    let first = grid_origin(origin, grid.unit);
    let last = index + 1u == shape.passes;
    var low = vec2(n);
    var high = vec2(0u);
    for (var i = lane; i < n * n; i += LANES) {
        let local = vec2(i % n, i / n);
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
fn settle(index: u32, origin: vec2<i32>, lane: u32) {
    let grid = metadata.grids[index];
    let n = span(grid.unit);
    let words = (n + 31u) / 32u;
    let first = grid_origin(origin, grid.unit);
    let radius = i32(grid.radius);
    for (var i = lane; i < n * n; i += LANES) {
        let local = vec2(i % n, i / n);
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
    for (var i = lane; i < dn * dn; i += LANES) {
        let local = vec2(i % dn, i / dn);
        let cell = dfirst + vec2<i32>(local);
        if any(cell < vec2(0)) || cell.x >= i32(shape.columns) || cell.y >= i32(shape.rows) { continue; }
        if changed(vec2<u32>(cell)) {
            let b = bit(dwords, local);
            atomicOr(&delta[b.x], b.y);
        }
    }
    // Validity the previous frame left, over the region of every inner pass
    // whose storage no other pass shares: what a shared tensor holds outside
    // this frame's writes may be another pass's.
    for (var p = 0u; p < shape.passes; p++) {
        let grid = metadata.grids[p];
        if grid.aliases != 0u || p + 1u == shape.passes { continue; }
        let n = span(grid.unit);
        let words = (n + 31u) / 32u;
        let first = grid_origin(origin, grid.unit);
        let stride = (grid.columns + 31u) / 32u;
        for (var i = lane; i < n * n; i += LANES) {
            let local = vec2(i % n, i / n);
            let cell = first + vec2<i32>(local);
            if any(cell < vec2(0)) || cell.x >= i32(grid.columns) || cell.y >= i32(grid.rows) { continue; }
            let c = vec2<u32>(cell);
            let word = atomicLoad(&state[previous_valid + grid.valid + c.y * stride + c.x / 32u]);
            if (word & (1u << (c.x % 32u))) != 0u {
                let b = bit(words, local);
                atomicOr(&validity[grid.arena + b.x], b.y);
            }
        }
    }
    workgroupBarrier();
    select_cells(shape.passes - 1u, origin, lane);
    workgroupBarrier();
    for (var p = 0u; p + 1u < shape.passes; p++) { select_cells(p, origin, lane); }
    workgroupBarrier();
    for (var p = 1u; p + 1u < shape.passes; p++) {
        if metadata.grids[p].producers != 0u { settle(p, origin, lane); }
    }
    // Interior output cells copied at an offset, with their hypothesis.
    let last = metadata.grids[shape.passes - 1u];
    let inner = TILE / last.unit;
    let start = vec2<u32>(origin) / last.unit;
    let offset = vec2(shape.halo / last.unit);
    let words = (span(last.unit) + 31u) / 32u;
    var copies: array<u32, 4>;
    var found = 0u;
    var won = 0u;
    for (var i = lane; i < inner * inner; i += LANES) {
        let c = start + vec2(i % inner, i / inner);
        if c.x >= last.columns || c.y >= last.rows { continue; }
        let b = bit(words, offset + vec2(i % inner, i / inner));
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
        let inner = TILE / grid.unit;
        let start = vec2<u32>(origin) / grid.unit;
        let offset = vec2(shape.halo / grid.unit);
        var count = 0u;
        for (var i = lane; i < inner * inner; i += LANES) {
            let c = start + vec2(i % inner, i / inner);
            if c.x >= grid.columns || c.y >= grid.rows { continue; }
            let b = bit((n + 31u) / 32u, offset + vec2(i % inner, i / inner));
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
    for (var p = 0u; p < shape.passes; p++) {
        let grid = metadata.grids[p];
        let n = span(grid.unit);
        let words = (n + 31u) / 32u;
        let inner = TILE / grid.unit;
        let start = vec2<u32>(origin) / grid.unit;
        let offset = vec2(shape.halo / grid.unit);
        for (var i = lane; i < inner * inner; i += LANES) {
            let c = start + vec2(i % inner, i / inner);
            if c.x >= grid.columns || c.y >= grid.rows { continue; }
            let b = bit(words, offset + vec2(i % inner, i / inner));
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
        for (var i = lane; i < inner * per_row; i += LANES) {
            let row = i / per_row;
            let column = (i % per_row) * run;
            let c = start + vec2(column, row);
            if c.x >= grid.columns || c.y >= grid.rows { continue; }
            let count = min(run, grid.columns - c.x);
            var bits = 0u;
            for (var x = 0u; x < count; x++) {
                let b = bit(words, offset + vec2(column + x, row));
                if (atomicLoad(&validity[grid.arena + b.x]) & b.y) != 0u { bits |= 1u << x; }
            }
            write_bits(current_valid + grid.valid, stride, c, count, bits);
        }
    }
    // Commit the interior's new anchors to the per-cell array and the index.
    let interior_first = vec2<u32>(origin) / 8u;
    let cells = shape.columns * shape.rows;
    for (var i = lane; i < (TILE / 8u) * (TILE / 8u); i += LANES) {
        let c = interior_first + vec2(i % (TILE / 8u), i / (TILE / 8u));
        if c.x >= shape.columns || c.y >= shape.rows { continue; }
        let at = c.y * shape.columns + c.x;
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
        var bits = 0u;
        for (var x = 0u; x < count; x++) {
            if changed(c + vec2(x, 0u)) { bits |= 1u << x; }
        }
        write_bits(shape.delta, stride, c, count, bits);
    }
}

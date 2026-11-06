// Bookkeeping: one dispatch per frame, one workgroup per 128-pixel tile
// (HOST-VIEW-10). Each tile evaluates every pass backwards over itself and a
// halo, in workgroup memory, from read-only inputs: delta and the validity the
// previous frame left. Halo results equal the neighbour's own, so only
// interior cells are written: each pass's compacted list, its validity and
// the delta bitmap.
const TILE: u32 = 128u;
const ARENA: u32 = 2048u;
const LANES: u32 = 256u;
// Source cells across a tile's delta region, in words of 32 cells.
const DELTA_WORDS: u32 = 128u;
@group(3) @binding(0) var<storage, read> frame: array<u32>;
@group(3) @binding(1) var<storage, read> stamps: array<u32>;
@group(3) @binding(2) var<storage, read_write> state: array<atomic<u32>>;
@group(3) @binding(3) var<storage, read> metadata: Metadata;
@group(3) @binding(4) var<storage, read_write> compacted: array<u32>;
var<workgroup> required: array<atomic<u32>, ARENA>;
var<workgroup> validity: array<atomic<u32>, ARENA>;
// Delta over the tile, its halo and the largest affected reach.
var<workgroup> delta: array<atomic<u32>, DELTA_WORDS>;
var<workgroup> kept: atomic<u32>;
var<workgroup> base: u32;

fn tile_origin(group: vec3<u32>) -> vec2<i32> {
    let packed = frame[FRAME_DATA + frame[FRAME_TILE_LIST] + group.y * 65535u + group.x];
    return vec2<i32>(vec2(packed & 65535u, packed >> 16u) * TILE);
}

// Cells of `unit` across the tile and halo, starting at `origin` cells.
fn span(unit: u32) -> u32 {
    return (TILE + 2u * metadata.shape.halo) / unit;
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
    return stamps[cell.y * shape.columns + cell.x] == frame[FRAME_EPOCH];
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
    return quotient(origin - i32(metadata.shape.halo), i32(unit));
}

// Required bit of grid `index` at its cell `cell`, false outside the region.
fn required_at(index: u32, origin: vec2<i32>, cell: vec2<i32>) -> bool {
    let grid = metadata.grids[index];
    let local = cell - grid_origin(origin, grid.unit);
    let n = i32(span(grid.unit));
    if any(local < vec2(0)) || any(local >= vec2(n)) { return false; }
    let b = bit((u32(n) + 31u) / 32u, vec2<u32>(local));
    return (atomicLoad(&required[grid.arena + b.x]) & b.y) != 0u;
}

fn required_by(index: u32, origin: vec2<i32>, pixel: vec2<i32>, unit: u32, radius: u32) -> bool {
    let grid = metadata.grids[index];
    let size = vec2<i32>(metadata.shape.size);
    let lo = quotient(max(pixel - i32(radius), vec2(0)), i32(grid.unit));
    let hi = quotient(min(pixel + i32(unit + radius), size) + i32(grid.unit) - 1, i32(grid.unit));
    for (var y = lo.y; y < hi.y; y++) {
        for (var x = lo.x; x < hi.x; x++) {
            if required_at(index, origin, vec2(x, y)) { return true; }
        }
    }
    return false;
}

fn clear_validity(index: u32, origin: vec2<i32>, cell: vec2<i32>) {
    let grid = metadata.grids[index];
    let local = cell - grid_origin(origin, grid.unit);
    let n = i32(span(grid.unit));
    if any(local < vec2(0)) || any(local >= vec2(n)) { return; }
    let b = bit((u32(n) + 31u) / 32u, vec2<u32>(local));
    atomicAnd(&validity[grid.arena + b.x], ~b.y);
}

// One pass over its region cells, as the per-pass filter decided them.
fn select_pass(index: u32, origin: vec2<i32>, lane: u32) {
    let shape = metadata.shape;
    let grid = metadata.grids[index];
    let n = span(grid.unit);
    let words = (n + 31u) / 32u;
    let first = grid_origin(origin, grid.unit);
    let last = index + 1u == shape.passes;
    for (var i = lane; i < n * n; i += LANES) {
        let local = vec2(i % n, i / n);
        let cell = first + vec2<i32>(local);
        if any(cell < vec2(0)) || cell.x >= i32(grid.columns) || cell.y >= i32(grid.rows) { continue; }
        let pixel = cell * i32(grid.unit);
        let b = bit(words, local);
        let slot = grid.arena + b.x;
        let affected = meets(origin, pixel - i32(grid.affected), pixel + i32(grid.unit + grid.affected));
        let was_valid = grid.aliases == 0u && (atomicLoad(&validity[slot]) & b.y) != 0u;
        var keep = affected;
        if !last {
            var needed = false;
            for (var j = index + 1u; j < shape.passes; j++) {
                if (grid.consumers & (1u << j)) != 0u && required_by(j, origin, pixel, grid.unit, metadata.grids[j].radius) {
                    needed = true;
                    break;
                }
            }
            keep = needed && (affected || !was_valid);
        }
        if affected { atomicAnd(&validity[slot], ~b.y); }
        if !keep { continue; }
        atomicOr(&required[slot], b.y);
        var overwritten = false;
        for (var j = 0u; j < shape.passes; j++) {
            if (grid.aliases & (1u << j)) == 0u { continue; }
            let other = metadata.grids[j];
            let lo = quotient(pixel, i32(other.unit));
            let hi = quotient(min(pixel + i32(grid.unit), vec2<i32>(shape.size)) + i32(other.unit) - 1, i32(other.unit));
            for (var y = lo.y; y < hi.y; y++) {
                for (var x = lo.x; x < hi.x; x++) {
                    if j > index && required_at(j, origin, vec2(x, y)) {
                        overwritten = true;
                    } else {
                        clear_validity(j, origin, vec2(x, y));
                    }
                }
            }
        }
        if !overwritten { atomicOr(&validity[slot], b.y); }
    }
    workgroupBarrier();
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
    // Validity the previous frame left, over the region of every pass.
    for (var p = 0u; p < shape.passes; p++) {
        let grid = metadata.grids[p];
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
    for (var p = shape.passes; p > 0u; p--) { select_pass(p - 1u, origin, lane); }
    // Interior results: lists, validity and delta.
    let current = arguments(parity);
    for (var p = 0u; p < shape.passes; p++) {
        let grid = metadata.grids[p];
        let n = span(grid.unit);
        let words = (n + 31u) / 32u;
        let inner = TILE / grid.unit;
        let start = vec2<u32>(origin) / grid.unit;
        let offset = vec2(shape.halo / grid.unit);
        var cells: array<u32, 4>;
        var found = 0u;
        if lane == 0u { atomicStore(&kept, 0u); }
        workgroupBarrier();
        for (var i = lane; i < inner * inner; i += LANES) {
            let c = start + vec2(i % inner, i / inner);
            if c.x >= grid.columns || c.y >= grid.rows { continue; }
            let b = bit(words, offset + vec2(i % inner, i / inner));
            if (atomicLoad(&required[grid.arena + b.x]) & b.y) != 0u {
                cells[found] = c.x | c.y << 16u;
                found++;
            }
        }
        var local = 0u;
        if found != 0u { local = atomicAdd(&kept, found); }
        workgroupBarrier();
        if lane == 0u {
            let total = atomicLoad(&kept);
            base = 0u;
            if total != 0u { base = reserve(current + grid.counter, current + grid.arguments, total); }
        }
        let at = workgroupUniformLoad(&base);
        for (var i = 0u; i < found; i++) { compacted[grid.compacted + at + local + i] = cells[i]; }
        // Validity rows of the interior, at most one word per row and lane.
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
        workgroupBarrier();
    }
    // The delta bitmap of the interior, for the damage overlay.
    let interior_first = vec2<u32>(origin) / 8u;
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

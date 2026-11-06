// Bookkeeping: one dispatch per frame, one workgroup per 128-pixel tile
// (HOST-VIEW-10, HOST-VIEW-13). Each tile evaluates every pass backwards over
// itself and a halo, in workgroup memory, from read-only inputs: delta, the
// facts compare wrote, and the validity the previous frame left. Halo results
// equal the neighbour's own, so only interior cells are written: each pass's
// compacted list, its validity, the refresh list and the delta bitmap.
const TILE: u32 = 128u;
const ARENA: u32 = 2048u;
const LANES: u32 = 256u;
// Source cells across a tile's delta region, in words of 32 cells.
const DELTA_WORDS: u32 = 128u;
@group(3) @binding(0) var<storage, read> frame: array<u32>;
@group(3) @binding(1) var<storage, read_write> references: References;
@group(3) @binding(2) var<storage, read_write> state: array<atomic<u32>>;
@group(3) @binding(3) var<storage, read> metadata: Metadata;
@group(3) @binding(4) var<storage, read_write> compacted: array<u32>;
var<workgroup> required: array<atomic<u32>, ARENA>;
var<workgroup> validity: array<atomic<u32>, ARENA>;
// Delta over the tile, its halo and the largest affected reach.
var<workgroup> delta: array<atomic<u32>, DELTA_WORDS>;
// Source cells over the tile and halo whose output comes from a reference.
var<workgroup> decided: array<atomic<u32>, DELTA_WORDS>;
// Interior source cells: the refresh operation of each decided cell.
var<workgroup> actions: array<u32, 256>;
var<workgroup> kept: atomic<u32>;
var<workgroup> base: u32;
var<workgroup> unexplained: atomic<u32>;

fn offset_entry(k: u32) -> vec4<i32> {
    let at = FRAME_OFFSETS + k * 4u;
    return bitcast<vec4<i32>>(vec4(frame[at], frame[at + 1u], frame[at + 2u], frame[at + 3u]));
}

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
    return references.facts[cell.y * shape.columns + cell.x].stamp == frame[FRAME_EPOCH];
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

fn offset_of(k: u32) -> vec2<i32> {
    if k == 0u {
        return vec2(i32(atomicLoad(&state[MOTION_OFFSET])), i32(atomicLoad(&state[MOTION_OFFSET + 1u])));
    }
    return offset_entry(k).xy;
}

fn offered(k: u32) -> bool {
    if k == 0u {
        return atomicLoad(&state[MOTION_SHARP]) != 0u && atomicLoad(&state[MOTION_EPOCH]) == frame[FRAME_EPOCH];
    }
    return offset_entry(k).z != 0;
}

// The cell's whole receptive field equals the previous input at offset k.
fn qualified(cell: vec2<u32>, k: u32) -> bool {
    let shape = metadata.shape;
    let size = vec2<i32>(shape.size);
    let offset = offset_of(k);
    let origin = vec2<i32>(cell * 8u);
    let reach = i32(shape.reach);
    let lo = origin - reach;
    let hi = min(origin + 8, size) - 1 + reach;
    for (var axis = 0u; axis < 2u; axis++) {
        if offset[axis] != 0 && (lo[axis] < 0 || hi[axis] >= size[axis] || lo[axis] - offset[axis] < 0 || hi[axis] - offset[axis] >= size[axis]) {
            return false;
        }
    }
    let source = origin - offset;
    let end = min(origin + 8, size) - offset;
    if any(source < vec2(0)) || any(end > size) { return false; }
    let first = max(cell, vec2(shape.erosion)) - shape.erosion;
    let last = min(cell + shape.erosion + 1u, vec2(shape.columns, shape.rows));
    for (var y = first.y; y < last.y; y++) {
        for (var x = first.x; x < last.x; x++) {
            let at = y * shape.columns + x;
            if references.facts[at].stamp != frame[FRAME_EPOCH] || (references.facts[at].matches & (1u << k)) == 0u {
                return false;
            }
        }
    }
    return true;
}

fn constant_field(cell: vec2<u32>, key: vec2<u32>) -> bool {
    let shape = metadata.shape;
    let first = max(cell, vec2(shape.erosion)) - shape.erosion;
    let last = min(cell + shape.erosion + 1u, vec2(shape.columns, shape.rows));
    for (var y = first.y; y < last.y; y++) {
        for (var x = first.x; x < last.x; x++) {
            let at = y * shape.columns + x;
            if references.facts[at].constant == 0u || any(references.facts[at].colour != key) { return false; }
        }
    }
    return true;
}

// The published pattern of `key`; read-only, so every tile agrees.
fn cached(key: vec2<u32>) -> u32 {
    let hash = colour_hash(key);
    for (var probe = 0u; probe < 8u; probe++) {
        let slot = (hash + probe) & 4095u;
        if references.colours[slot].valid != 0u && all(references.colours[slot].key == key) {
            return slot;
        }
    }
    return 0xffffffffu;
}

fn tag() -> u32 {
    return (frame[FRAME_EPOCH] % 0x7fffu + 1u) << 17u;
}

// Elect one owner per new colour, which claims a cache slot to publish into.
fn elect(cell: vec2<u32>, key: vec2<u32>) {
    let shape = metadata.shape;
    let at = cell.y * shape.columns + cell.x;
    let hash = colour_hash(key);
    var owner = false;
    for (var probe = 0u; probe < 64u && !owner; probe++) {
        let slot = (hash + probe) & 4095u;
        loop {
            let value = atomicLoad(&references.leaders[slot]);
            if (value & 0xfffe0000u) == tag() {
                if all(references.facts[value & 0x1ffffu].colour == key) { return; }
                break;
            }
            let claim = atomicCompareExchangeWeak(&references.leaders[slot], value, tag() | at);
            if claim.exchanged { owner = true; break; }
        }
    }
    if !owner { return; }
    // Replace the least recently used of the colour's probe slots.
    var best = 0xffffffffu;
    var oldest = 0xffffffffu;
    for (var probe = 0u; probe < 8u; probe++) {
        let slot = (hash + probe) & 4095u;
        let touched = select(0u, atomicLoad(&references.colours[slot].touched), references.colours[slot].valid != 0u);
        if atomicLoad(&references.colours[slot].claimed) != frame[FRAME_EPOCH] && touched < oldest {
            oldest = touched;
            best = slot;
        }
    }
    if best == 0xffffffffu { return; }
    let previous = atomicLoad(&references.colours[best].claimed);
    if previous == frame[FRAME_EPOCH] { return; }
    if !atomicCompareExchangeWeak(&references.colours[best].claimed, previous, frame[FRAME_EPOCH]).exchanged { return; }
    references.facts[at].owner = frame[FRAME_EPOCH];
    references.facts[at].slot = best;
}

// A changed source cell's output from a reference, as a refresh operation:
// a verified offset, a cached colour, or none.
fn decide(cell: vec2<u32>, interior: bool) -> u32 {
    if frame[FRAME_EXACT] != 0u || !changed(cell) { return 0xffffffffu; }
    for (var k = 0u; k < 8u; k++) {
        if offered(k) && qualified(cell, k) { return REFRESH_REBUILD + k; }
    }
    let at = cell.y * metadata.shape.columns + cell.x;
    if references.facts[at].constant == 0u { return 0xffffffffu; }
    let key = references.facts[at].colour;
    if !constant_field(cell, key) { return 0xffffffffu; }
    let slot = cached(key);
    if slot != 0xffffffffu {
        if interior { atomicMax(&references.colours[slot].touched, frame[FRAME_EPOCH]); }
        return REFRESH_CONSTANT | slot;
    }
    if interior { elect(cell, key); }
    return 0xffffffffu;
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
        if last {
            let source = vec2<u32>(pixel) / 8u - vec2<u32>(grid_origin(origin, 8u));
            let d = bit((span(8u) + 31u) / 32u, source);
            if (atomicLoad(&decided[d.x]) & d.y) != 0u {
                atomicAnd(&validity[slot], ~b.y);
                continue;
            }
        } else {
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
    if lane == 0u {
        references.epoch = frame[FRAME_EPOCH];
        let motion = MOTION_SETS + (frame[FRAME_EPOCH] % 2u) * MOTION_SET_WORDS;
        for (var i = 0u; i < MOTION_SET_WORDS; i++) { atomicStore(&state[motion + i], 0u); }
    }
}

// Open the next frame's projection once enough changed cells went
// unexplained, unless failed projections are backing off.
fn need_motion(count: u32) {
    if count == 0u { return; }
    let motion = MOTION_SETS + ((frame[FRAME_EPOCH] + 1u) % 2u) * MOTION_SET_WORDS;
    let threshold = metadata.shape.threshold;
    let before = atomicAdd(&state[motion + MOTION_COUNTER], count);
    if before >= threshold || before + count < threshold { return; }
    let skip = atomicLoad(&state[MOTION_SKIP]);
    if skip != 0u {
        atomicStore(&state[MOTION_SKIP], skip - 1u);
        return;
    }
    let length = max(metadata.shape.size.x, metadata.shape.size.y);
    atomicStore(&state[motion], length);
    atomicStore(&state[motion + 1u], 2u);
    atomicStore(&state[motion + 2u], 1u);
    atomicStore(&state[motion + 4u], length * 2u - 1u);
    atomicStore(&state[motion + 5u], 2u);
    atomicStore(&state[motion + 6u], 1u);
    atomicStore(&state[motion + 8u], 1u);
    atomicStore(&state[motion + 9u], 1u);
    atomicStore(&state[motion + 10u], 1u);
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
    // Reference decisions over the region's source cells.
    let sn = span(8u);
    let swords = (sn + 31u) / 32u;
    let sfirst = grid_origin(origin, 8u);
    let interior_first = vec2<u32>(origin) / 8u;
    for (var i = lane; i < sn * sn; i += LANES) {
        let local = vec2(i % sn, i / sn);
        let cell = sfirst + vec2<i32>(local);
        if any(cell < vec2(0)) || cell.x >= i32(shape.columns) || cell.y >= i32(shape.rows) { continue; }
        let c = vec2<u32>(cell);
        let inner = c - interior_first;
        let interior = all(c >= interior_first) && all(inner < vec2(TILE / 8u));
        let action = decide(c, interior);
        if action != 0xffffffffu {
            let b = bit(swords, local);
            atomicOr(&decided[b.x], b.y);
        }
        if interior {
            actions[inner.y * (TILE / 8u) + inner.x] = action;
            // Only proposals explain a change; global motion is re-measured.
            if changed(c) && !(action > REFRESH_REBUILD && action < REFRESH_CONSTANT) {
                atomicAdd(&unexplained, 1u);
            }
            if frame[FRAME_EXACT] != 0u && changed(c) {
                references.facts[c.y * shape.columns + c.x].constant = 0u;
            }
        }
    }
    workgroupBarrier();
    for (var p = shape.passes; p > 0u; p--) { select_pass(p - 1u, origin, lane); }
    // Interior results: lists, validity, refresh entries and delta.
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
    // Refresh entries and written stamps of the interior's last-pass cells.
    let grid = metadata.grids[shape.passes - 1u];
    let inner = TILE / grid.unit;
    let start = vec2<u32>(origin) / grid.unit;
    let offset = vec2(shape.halo / grid.unit);
    let words = (span(grid.unit) + 31u) / 32u;
    var entries: array<vec2<u32>, 4>;
    var found = 0u;
    if lane == 0u { atomicStore(&kept, 0u); }
    workgroupBarrier();
    for (var i = lane; i < inner * inner; i += LANES) {
        let c = start + vec2(i % inner, i / inner);
        if c.x >= grid.columns || c.y >= grid.rows { continue; }
        let at = shape.written + c.y * grid.columns + c.x;
        let b = bit(words, offset + vec2(i % inner, i / inner));
        let source = c * grid.unit / 8u - interior_first;
        let action = actions[source.y * (TILE / 8u) + source.x];
        var op = 0xffffffffu;
        if (atomicLoad(&required[grid.arena + b.x]) & b.y) != 0u {
            atomicStore(&state[at], frame[FRAME_EPOCH]);
        } else if action != 0xffffffffu {
            atomicStore(&state[at], frame[FRAME_EPOCH]);
            op = action;
        } else if atomicLoad(&state[at]) == frame[FRAME_EPOCH] - 1u {
            op = REFRESH_COPY;
        }
        if op != 0xffffffffu {
            entries[found] = vec2(c.x | c.y << 16u, op);
            found++;
        }
    }
    var local = 0u;
    if found != 0u { local = atomicAdd(&kept, found); }
    workgroupBarrier();
    if lane == 0u {
        let total = atomicLoad(&kept);
        base = 0u;
        if total != 0u {
            base = reserve(current + shape.refresh_counter, current + shape.refresh_arguments, total);
        }
        need_motion(atomicLoad(&unexplained));
    }
    let at = workgroupUniformLoad(&base);
    for (var i = 0u; i < found; i++) {
        compacted[shape.refresh + (at + local + i) * 2u] = entries[i].x;
        compacted[shape.refresh + (at + local + i) * 2u + 1u] = entries[i].y;
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

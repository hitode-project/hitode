struct Parameters {
    size: vec2<u32>, unit: u32, reach: u32,
    index: u32, passes: u32, exact: u32, valid: u32,
    required: u32, argument_stride: u32, threshold: u32, padding: u32,
}
struct Grid {
    columns: u32, rows: u32, unit: u32, offset: u32,
    affected: u32, radius: u32, consumers: u32, aliases: u32,
}
struct Arguments { x: atomic<u32>, y: u32, z: u32, count: u32 }
@group(0) @binding(0) var<uniform> parameters: Parameters;
@group(3) @binding(0) var<storage, read> cells: array<u32>;
@group(3) @binding(1) var<storage, read> delta: array<u32>;
@group(3) @binding(2) var<storage, read_write> compacted: array<u32>;
@group(3) @binding(3) var<storage, read_write> arguments: Arguments;
@group(3) @binding(4) var<storage, read_write> state: array<atomic<u32>>;
@group(3) @binding(5) var<storage, read> grids: array<Grid>;
@group(3) @binding(6) var<storage, read_write> verified: Verification;
@group(1) @binding(0) var input: texture_2d<f32>;
@group(1) @binding(2) var constant_output: texture_2d<f32>;
@group(3) @binding(7) var previous_input: texture_storage_2d<rgba16float, write>;
@group(3) @binding(9) var<storage, read_write> motion_arguments: array<atomic<u32>>;
@group(3) @binding(8) var output: texture_storage_2d<rgba8unorm, write>;
var<workgroup> count: atomic<u32>;
var<workgroup> base: u32;

fn meets_delta(origin: vec2<u32>, reach: u32) -> bool {
    let lo = (max(origin, vec2(reach)) - reach) / 8u;
    let hi = (min(origin + parameters.unit + reach, parameters.size) + 7u) / 8u;
    let stride = ((parameters.size.x + 7u) / 8u + 31u) / 32u;
    for (var y = lo.y; y < hi.y; y++) {
        for (var word = lo.x / 32u; word <= (hi.x - 1u) / 32u; word++) {
            let first = select(0u, lo.x % 32u, word == lo.x / 32u);
            let last = select(31u, (hi.x - 1u) % 32u, word == (hi.x - 1u) / 32u);
            let mask = (0xffffffffu << first) & (0xffffffffu >> (31u - last));
            if (delta[y * stride + word] & mask) != 0u { return true; }
        }
    }
    return false;
}

fn bit_at(grid: Grid, cell: vec2<u32>) -> u32 {
    return grid.offset + cell.y * ((grid.columns + 31u) / 32u) + cell.x / 32u;
}

fn required_by(index: u32, origin: vec2<u32>, radius: u32) -> bool {
    let grid = grids[index];
    let lo = (max(origin, vec2(radius)) - radius) / grid.unit;
    let hi = (min(origin + parameters.unit + radius, parameters.size) + grid.unit - 1u) / grid.unit;
    for (var y = lo.y; y < hi.y; y++) {
        for (var x = lo.x; x < hi.x; x++) {
            let word = parameters.required + bit_at(grid, vec2(x, y));
            if (atomicLoad(&state[word]) & (1u << (x % 32u))) != 0u { return true; }
        }
    }
    return false;
}

fn matches(cell: vec2<u32>, offset: vec2<i32>) -> bool {
    let at = cell.y * ((parameters.size.x + 7u) / 8u) + cell.x;
    if verified.cells[at].data.x != verified.epoch { return false; }
    for (var i = 0u; i < 4u; i++) {
        let term = verified.cells[at].terms[i];
        if term.z != 0 && all(term.xy == offset) { return true; }
    }
    return false;
}

fn qualified(cell: vec2<u32>, offset: vec2<i32>) -> bool {
    let origin = vec2<i32>(cell * 8u);
    let reach = i32(grids[parameters.passes - 1u].affected);
    let lo = origin - reach;
    let hi = min(origin + 8, vec2<i32>(parameters.size)) - 1 + reach;
    for (var axis = 0u; axis < 2u; axis++) {
        if offset[axis] != 0 && (lo[axis] < 0 || hi[axis] >= i32(parameters.size[axis]) || lo[axis] - offset[axis] < 0 || hi[axis] - offset[axis] >= i32(parameters.size[axis])) {
            return false;
        }
    }
    let erosion = (u32(reach) + 7u) / 8u;
    let first = (max(cell, vec2(erosion)) - erosion);
    let last = min(cell + erosion + 1u, (parameters.size + 7u) / 8u);
    for (var y = first.y; y < last.y; y++) {
        for (var x = first.x; x < last.x; x++) {
            if !matches(vec2(x, y), offset) { return false; }
        }
    }
    let source = origin - offset;
    let end = min(origin + 8, vec2<i32>(parameters.size)) - offset;
    if any(source < vec2(0)) || any(end > vec2<i32>(parameters.size)) { return false; }
    let grid = grids[parameters.passes - 1u];
    for (var y = u32(source.y) / grid.unit; y < (u32(end.y) + grid.unit - 1u) / grid.unit; y++) {
        for (var x = u32(source.x) / grid.unit; x < (u32(end.x) + grid.unit - 1u) / grid.unit; x++) {
            let at = parameters.exact + y * ((grid.columns + 31u) / 32u) + x / 32u;
            if (atomicLoad(&state[at]) & (1u << (x % 32u))) == 0u { return false; }
        }
    }
    return true;
}

fn previous_pixel(pixel: vec2<i32>) -> vec4<f32> {
    let p = vec2<u32>(pixel);
    let cell = p / 16u;
    let at = cell.y * ((parameters.size.x + 7u) / 8u) + cell.x;
    let local = p % 16u;
    return unpack4x8unorm(atomicLoad(&verified.cells[at].output[local.y * 16u + local.x]));
}

fn reconstruct(origin: vec2<u32>) -> bool {
    let cell = origin / 8u;
    let at = cell.y * ((parameters.size.x + 7u) / 8u) + cell.x;
    if verified.cells[at].data.x != verified.epoch { return false; }
    for (var i = 0u; i < 4u; i++) {
        let term = verified.cells[at].terms[i];
        if term.z == 0 || !qualified(cell, term.xy) { continue; }
        for (var y = 0u; y < parameters.unit * 2u; y++) {
            for (var x = 0u; x < parameters.unit * 2u; x++) {
                let pixel = origin * 2u + vec2(x, y);
                if all(pixel < parameters.size * 2u) {
                    let source = vec2<i32>(pixel) - term.xy * 2;
                    textureStore(output, vec2<i32>(pixel), previous_pixel(source));
                }
            }
        }
        atomicStore(&state[0], 1u);
        let stride = ((parameters.size.x + 7u) / 8u + 31u) / 32u;
        atomicOr(&state[1u + cell.y * stride + cell.x / 32u], 1u << (cell.x % 32u));
        return true;
    }
    return false;
}

fn motion_need(origin: vec2<u32>) {
    if any(origin % 8u != vec2(0u)) { return; }
    let cell = origin / 8u;
    let stride = ((parameters.size.x + 7u) / 8u + 31u) / 32u;
    if (delta[cell.y * stride + cell.x / 32u] & (1u << (cell.x % 32u))) == 0u { return; }
    let at = cell.y * ((parameters.size.x + 7u) / 8u) + cell.x;
    if verified.cells[at].data.x == verified.epoch {
        for (var i = 0u; i < 4u; i++) {
            let term = verified.cells[at].terms[i];
            if term.z != 0 && term.w == 1 && qualified(cell, term.xy) { return; }
        }
    }
    let base = (verified.epoch % 2u) * parameters.argument_stride * 4u;
    if atomicAdd(&motion_arguments[base + parameters.argument_stride * 3u], 1u) + 1u == parameters.threshold {
        let length = max(parameters.size.x, parameters.size.y);
        for (var i = 0u; i < 3u; i++) {
            let at = base + i * parameters.argument_stride;
            var x = 1u;
            var y = 1u;
            if i == 0u { x = length; y = 2u; }
            if i == 1u { x = length * 2u - 1u; y = 2u; }
            atomicStore(&motion_arguments[at], x);
            atomicStore(&motion_arguments[at + 1u], y);
            atomicStore(&motion_arguments[at + 2u], 1u);
        }
    }
}

fn copy_input_history(origin: vec2<u32>) {
    if any(origin % 8u != vec2(0u)) { return; }
    let cell = origin / 8u;
    let stride = ((parameters.size.x + 7u) / 8u + 31u) / 32u;
    if (delta[cell.y * stride + cell.x / 32u] & (1u << (cell.x % 32u))) == 0u { return; }
    let first = textureLoad(input, vec2<i32>(origin), 0);
    let colour = vec2(pack2x16float(first.xy), pack2x16float(first.zw));
    var constant = true;
    var digest0 = 0u;
    var digest1 = 0u;
    for (var y = 0u; y < 8u; y++) {
        for (var x = 0u; x < 8u; x++) {
            let pixel = origin + vec2(x, y);
            if all(pixel < parameters.size) {
                let value = textureLoad(input, vec2<i32>(pixel), 0);
                textureStore(previous_input, vec2<i32>(pixel), value);
                if verified.padding0 != 0u {
                    let bits = bitcast<vec4<u32>>(value);
                    constant = constant && all(bits == bitcast<vec4<u32>>(first));
                    let a = bits.x * 0x9e3779b9u ^ bits.y * 0x85ebca6bu ^ bits.z * 0xc2b2ae35u ^ bits.w;
                    let b = a ^ (a >> 16u) ^ ((y * 8u + x) * 0x27d4eb2du);
                    digest0 ^= b * 0x165667b1u;
                    digest1 += (b ^ (b >> 13u)) * 0x9e3779b9u;
                }
            }
        }
    }
    if verified.padding0 != 0u {
        let at = cell.y * ((parameters.size.x + 7u) / 8u) + cell.x;
        verified.cells[at].data.y = digest0;
        verified.cells[at].data.z = digest1;
        verified.cells[at].data.w = u32(constant);
        verified.cells[at].colour = colour;
    }
}

fn constant_field(cell: vec2<u32>, colour: vec2<u32>) -> bool {
    let radius = (grids[parameters.passes - 1u].affected + 7u) / 8u;
    let lo = max(cell, vec2(radius)) - radius;
    let hi = min(cell + radius + 1u, (parameters.size + 7u) / 8u);
    let columns = (parameters.size.x + 7u) / 8u;
    for (var y = lo.y; y < hi.y; y++) {
        for (var x = lo.x; x < hi.x; x++) {
            let at = y * columns + x;
            if verified.cells[at].data.w == 0u || any(verified.cells[at].colour != colour) { return false; }
        }
    }
    return true;
}

fn colour_cache_cell() -> u32 {
    let index = atomicAdd(&verified.colour_count, 1u);
    if index < 4096u {
        atomicStore(&verified.colours[index].touched, 0x80000000u | verified.epoch);
        return index;
    }
    atomicMin(&verified.colour_count, 4096u);
    var cell = 0u;
    loop {
        var oldest = 0x80000000u;
        for (var i = 0u; i < 4096u; i++) {
            let touched = atomicLoad(&verified.colours[i].touched);
            if verified.colours[i].valid != 0u && touched < oldest {
                oldest = touched;
                cell = i;
            }
        }
        let claim = atomicCompareExchangeWeak(&verified.colours[cell].touched, oldest, oldest | 0x80000000u);
        if claim.exchanged { break; }
    }
    return cell;
}

// Current-frame owners point into immutable input metadata, so competing
// workgroups can identify a colour without a lock or a partially published key.
fn colour_owner(key: vec2<u32>, cell: u32) -> u32 {
    let hash = (key.x * 0x9e3779b9u ^ key.y * 0x85ebca6bu) & 4095u;
    for (var probe = 0u; probe < 4096u; probe++) {
        let slot = (hash + probe) & 4095u;
        loop {
            let claim = atomicCompareExchangeWeak(&verified.leaders[slot].owner, 0xffffffffu, cell);
            if claim.exchanged {
                verified.leaders[slot].cache = colour_cache_cell();
                return slot;
            }
            if claim.old_value != 0xffffffffu {
                if all(verified.cells[claim.old_value].colour == key) { return slot; }
                break;
            }
        }
    }
    return 0xffffffffu;
}

// 1 when a cached colour rebuilt the cell, 2 for a new colour's owner, else 0.
fn constant_reconstruction(origin: vec2<u32>) -> u32 {
    if verified.padding0 != 0u { return 0u; }
    let cell = origin / 8u;
    let stride = ((parameters.size.x + 7u) / 8u + 31u) / 32u;
    if (delta[cell.y * stride + cell.x / 32u] & (1u << (cell.x % 32u))) == 0u { return 0u; }
    let at = cell.y * ((parameters.size.x + 7u) / 8u) + cell.x;
    if verified.cells[at].data.w == 0u { return 0u; }
    let key = verified.cells[at].colour;
    if !constant_field(cell, key) { return 0u; }
    let count = min(atomicLoad(&verified.colour_count), 4096u);
    for (var slot = 0u; slot < count; slot++) {
        if verified.colours[slot].valid != 0u && all(verified.colours[slot].key == key) {
            atomicMax(&verified.colours[slot].touched, verified.epoch & 0x7fffffffu);
            let base = vec2(slot % 64u, slot / 64u) * 2u;
            for (var y = 0u; y < parameters.unit * 2u; y++) {
                for (var x = 0u; x < parameters.unit * 2u; x++) {
                    let pixel = origin * 2u + vec2(x, y);
                    if all(pixel < parameters.size * 2u) {
                        textureStore(output, vec2<i32>(pixel), textureLoad(constant_output, vec2<i32>(base + (pixel & vec2(1u))), 0));
                    }
                }
            }
            atomicStore(&state[0], 1u);
            return 1u;
        }
    }
    // A new colour's owner computes and publishes it. Other cells of that
    // colour compute it this once, so no cell waits on another's run.
    let slot = colour_owner(key, at);
    if slot == 0xffffffffu || atomicLoad(&verified.leaders[slot].owner) != at || any(origin != cell * 8u) {
        return 0u;
    }
    atomicStore(&verified.cells[at].slot, slot);
    atomicStore(&verified.cells[at].constant_epoch, verified.epoch);
    return 2u;
}

fn select_cell(packed: u32) -> bool {
    let cell = vec2(packed & 65535u, packed >> 16u);
    let origin = cell * parameters.unit;
    let grid = grids[parameters.index];
    let at = bit_at(grid, cell);
    let mask = 1u << (cell.x % 32u);
    let affected = meets_delta(origin, grid.affected);
    if parameters.index + 1u == parameters.passes {
        copy_input_history(origin);
        motion_need(origin);
        if reconstruct(origin) {
            atomicAnd(&state[parameters.valid + at], ~mask);
            atomicOr(&state[parameters.exact + at - grid.offset], mask);
            return false;
        }
        let colour = constant_reconstruction(origin);
        if colour == 1u {
            atomicAnd(&state[parameters.valid + at], ~mask);
            atomicOr(&state[parameters.exact + at - grid.offset], mask);
            return false;
        }
    }
    // Shared storage has another logical lifetime in the forward run. Its
    // earlier writer can overwrite a valid later value after this selection.
    let was_valid = grid.aliases == 0u && (atomicLoad(&state[parameters.valid + at]) & mask) != 0u;
    var keep = meets_delta(origin, parameters.reach);
    if atomicLoad(&state[0]) != 0u {
        if parameters.index + 1u == parameters.passes {
            let source = origin / 8u;
            let stride = ((parameters.size.x + 7u) / 8u + 31u) / 32u;
            let rebuilt = (atomicLoad(&state[1u + source.y * stride + source.x / 32u]) & (1u << (source.x % 32u))) != 0u;
            keep = affected && !rebuilt;
        } else {
            var needed = false;
            for (var i = parameters.index + 1u; i < parameters.passes; i++) {
                if (grid.consumers & (1u << i)) != 0u && required_by(i, origin, grids[i].radius) {
                    needed = true;
                    break;
                }
            }
            keep = needed && (affected || !was_valid);
        }
    }
    if affected { atomicAnd(&state[parameters.valid + at], ~mask); }
    if !keep { return false; }
    atomicOr(&state[parameters.required + at], mask);
    var overwritten = false;
    for (var i = 0u; i < parameters.passes; i++) {
        if (grid.aliases & (1u << i)) == 0u { continue; }
        let other = grids[i];
        let lo = origin / other.unit;
        let hi = (min(origin + parameters.unit, parameters.size) + other.unit - 1u) / other.unit;
        for (var y = lo.y; y < hi.y; y++) {
            for (var x = lo.x; x < hi.x; x++) {
                let word = bit_at(other, vec2(x, y));
                let bit = 1u << (x % 32u);
                let later = i > parameters.index && (atomicLoad(&state[parameters.required + word]) & bit) != 0u;
                if later {
                    overwritten = true;
                } else {
                    atomicAnd(&state[parameters.valid + word], ~bit);
                }
            }
        }
    }
    if !overwritten { atomicOr(&state[parameters.valid + at], mask); }
    if parameters.index + 1u == parameters.passes {
        atomicOr(&state[parameters.exact + at - grid.offset], mask);
    }
    return true;
}

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) id: vec3<u32>, @builtin(local_invocation_index) lane: u32) {
    var packed = 0u;
    var keep = false;
    var local = 0u;
    if id.x < arguments.count {
        packed = cells[id.x];
        keep = select_cell(packed);
        if keep { local = atomicAdd(&count, 1u); }
    }
    workgroupBarrier();
    if lane == 0u {
        let kept = atomicLoad(&count);
        if kept != 0u { base = atomicAdd(&arguments.x, kept); }
    }
    workgroupBarrier();
    if keep { compacted[base + local] = packed; }
}

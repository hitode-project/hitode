struct Parameters {
    size: vec2<u32>, unit: u32, reach: u32,
    index: u32, passes: u32, exact: u32, valid: u32,
    required: u32, padding0: u32, padding1: u32, padding2: u32,
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

fn select_cell(packed: u32) -> bool {
    let cell = vec2(packed & 65535u, packed >> 16u);
    let origin = cell * parameters.unit;
    let grid = grids[parameters.index];
    let at = bit_at(grid, cell);
    let mask = 1u << (cell.x % 32u);
    let affected = meets_delta(origin, grid.affected);
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

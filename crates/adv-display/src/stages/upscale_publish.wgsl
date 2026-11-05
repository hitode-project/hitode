@group(3) @binding(16) var<storage, read_write> references: Verification;
@group(3) @binding(17) var colour_output: texture_storage_2d<rgba8unorm, write>;

// The final model writer also publishes one new constant pattern and all cells
// waiting for it. Native UNORM stores quantize both destinations identically.
fn publish(pixel: vec2<u32>, value: vec4<f32>) {
    textureStore(OUTPUT_0, vec2<i32>(pixel), value);
    let size = Params_0.dimensions_0.xy;
    let columns = (size.x + 7u) / 8u;
    let cell = pixel / 16u;
    let at = cell.y * columns + cell.x;
    if atomicLoad(&references.cells[at].constant_epoch) != references.epoch { return; }
    let slot = atomicLoad(&references.cells[at].slot);
    let owner = atomicLoad(&references.leaders[slot].owner);
    let origin = vec2(owner % columns, owner / columns) * 16u;
    if any(pixel < origin) || any(pixel >= origin + 2u) { return; }
    let phase = pixel - origin;
    let cache = references.leaders[slot].cache;
    let base = vec2(cache % 64u, cache / 64u) * 2u;
    textureStore(colour_output, vec2<i32>(base + phase), value);
    if all(phase == vec2(0u)) {
        references.colours[cache].key = references.cells[at].colour;
        references.colours[cache].valid = 1u;
        atomicStore(&references.colours[cache].touched, references.epoch & 0x7fffffffu);
    }
    let count = columns * ((size.y + 7u) / 8u);
    for (var i = 0u; i < count; i++) {
        if atomicLoad(&references.cells[i].constant_epoch) != references.epoch || atomicLoad(&references.cells[i].slot) != slot { continue; }
        let destination = vec2(i % columns, i / columns) * 16u;
        for (var y = phase.y; y < 16u; y += 2u) {
            for (var x = phase.x; x < 16u; x += 2u) {
                let p = destination + vec2(x, y);
                if all(p < size * 2u) { textureStore(OUTPUT_0, vec2<i32>(p), value); }
            }
        }
    }
}

@group(3) @binding(16) var<storage, read_write> references: References;
@group(3) @binding(17) var colour_output: texture_storage_2d<rgba8unorm, write>;

// The final model writer of a new colour's owner also publishes its constant
// pattern. Native UNORM stores quantize both destinations identically.
fn publish(pixel: vec2<u32>, value: vec4<f32>) {
    textureStore(OUTPUT_0, vec2<i32>(pixel), value);
    let size = Params_0.dimensions_0.xy;
    let cell = pixel / 16u;
    let at = cell.y * ((size.x + 7u) / 8u) + cell.x;
    if references.facts[at].owner != references.epoch { return; }
    let phase = pixel - cell * 16u;
    if any(phase >= vec2(2u)) { return; }
    let slot = references.facts[at].slot;
    textureStore(colour_output, vec2<i32>(vec2(slot % 64u, slot / 64u) * 2u + phase), value);
    if all(phase == vec2(0u)) {
        references.colours[slot].key = references.facts[at].colour;
        references.colours[slot].valid = 1u;
        atomicStore(&references.colours[slot].touched, references.epoch);
    }
}

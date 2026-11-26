fn legacy_stage(op: u32, a: vec4<f32>, b: vec4<f32>, current: vec4<f32>,
    diffuse: vec4<f32>, texture: vec4<f32>) -> vec4<f32> {
    var result = current;
    let product = round(a * b * 255.0) / 255.0;
    switch op {
        case 2u: { return a; }
        case 3u: { return b; }
        case 4u: { result = product; }
        case 5u: { result = product * 2.0; }
        case 6u: { result = product * 4.0; }
        case 7u: { result = a + b; }
        case 8u: { result = a + b - (128.0 / 255.0); }
        case 9u: { result = (a + b - (128.0 / 255.0)) * 2.0; }
        case 10u: { result = a - b; }
        case 11u: { result = a + b * (1.0 - a); }
        case 12u: { result = mix(b, a, diffuse.a); }
        case 13u: { result = mix(b, a, texture.a); }
        case 14u: { result = mix(b, a, bitcast<vec4<f32>>(draw_parameters.values[36]).a); }
        case 15u: { result = a + b * (1.0 - texture.a); }
        case 16u: { result = mix(b, a, current.a); }
        case 18u: { result = a + a.a * b; }
        case 19u: { result = a * b + a.a; }
        case 20u: { result = a + (1.0 - a.a) * b; }
        case 21u: { result = (1.0 - a) * b + a.a; }
        case 24u: { result = vec4<f32>(dot(a.rgb * 2.0 - 1.0, b.rgb * 2.0 - 1.0)); }
        default: {}
    }
    return clamp(result, vec4<f32>(0.0), vec4<f32>(1.0));
}

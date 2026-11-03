// Output coordinates and tiles are in the target, including pre-rotation.
struct Output { extent: vec2<f32>, source: vec2<f32>, dst: vec4<f32>, prescale: vec2<f32>, rotation: u32, linear: u32, source_rotation: u32 }
@group(0) @binding(0) var<uniform> output: Output;
@group(3) @binding(2) var<storage, read> tiles: array<vec4<u32>>;
@group(1) @binding(0) var image: texture_2d<f32>;
@vertex fn vs(@builtin(vertex_index) vertex: u32, @builtin(instance_index) tile: u32) -> @builtin(position) vec4<f32> {
    let corners = array<vec2<u32>, 6>(vec2(0u,0u),vec2(1u,0u),vec2(0u,1u),vec2(0u,1u),vec2(1u,0u),vec2(1u,1u));
    let r = tiles[tile];
    let xy = vec2<f32>(r.xy + corners[vertex] * r.zw) / output.extent;
    return vec4(xy.x*2.0-1.0, 1.0-xy.y*2.0, 0.0, 1.0);
}
fn source_position(position: vec2<f32>) -> vec2<f32> {
    let uv = (position - output.dst.xy) / output.dst.zw;
    switch output.rotation {
        case 1u: { return vec2(uv.y, 1.0-uv.x); }
        case 2u: { return 1.0-uv; }
        case 3u: { return vec2(1.0-uv.y, uv.x); }
        default: { return uv; }
    }
}
fn texel(p: vec2<f32>) -> vec4<f32> {
    var uv = (clamp(p,vec2(0.0),output.source-1.0)+0.5)/output.source;
    switch output.source_rotation {
        case 1u: { uv=vec2(1.0-uv.y,uv.x); }
        case 2u: { uv=1.0-uv; }
        case 3u: { uv=vec2(uv.y,1.0-uv.x); }
        default: {}
    }
    return textureLoad(image,vec2<i32>(min(floor(uv*vec2<f32>(textureDimensions(image))),vec2<f32>(textureDimensions(image))-1.0)),0);
}
fn soft_texel(p: vec2<f32>) -> vec4<f32> {
    let xy = vec2<u32>(clamp(p, vec2(0.0), output.source-1.0));
    return software_pixel(xy.x, xy.y);
}
fn sample_position(position: vec2<f32>) -> vec2<f32> {
    return source_position(position) * output.source;
}
@fragment fn fs(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let p = sample_position(position.xy);
    var c: vec4<f32>;
    if output.linear == 0u { c = texel(floor(p)); }
    else {
        let q = p * output.prescale - 0.5;
        let a = floor(q); let f = fract(q);
        let lo = floor(a / output.prescale); let hi = floor((a+1.0) / output.prescale);
        c = mix(mix(texel(lo),texel(vec2(hi.x,lo.y)),f.x),mix(texel(vec2(lo.x,hi.y)),texel(hi),f.x),f.y);
    }
    return vec4(c.rgb,1.0);
}
@fragment fn fs_software(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let p = sample_position(position.xy);
    var c: vec4<f32>;
    if output.linear == 0u { c = soft_texel(floor(p)); }
    else {
        let q = p * output.prescale - 0.5;
        let a = floor(q); let f = fract(q);
        let lo = floor(a / output.prescale); let hi = floor((a+1.0) / output.prescale);
        c = mix(mix(soft_texel(lo),soft_texel(vec2(hi.x,lo.y)),f.x),mix(soft_texel(vec2(lo.x,hi.y)),soft_texel(hi),f.x),f.y);
    }
    return vec4(c.rgb,1.0);
}

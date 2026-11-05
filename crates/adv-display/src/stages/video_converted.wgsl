@group(4) @binding(0) var video: texture_2d<f32>;
@group(4) @binding(1) var filtering: sampler;
fn sample_video(uv: vec2<f32>) -> vec4<f32> {
    return textureSampleLevel(video, filtering, uv, 0.);
}

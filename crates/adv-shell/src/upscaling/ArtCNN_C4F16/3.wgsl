// Generated from pinned upstream sources; see vendor/upscaling/NOTICE.
struct SLANG_ParameterGroup_Params_std140_0
{
    @align(16) dimensions_0 : vec4<u32>,
};

@binding(0) @group(0) var<uniform> Params_0 : SLANG_ParameterGroup_Params_std140_0;
@binding(1) @group(0) var LUMA_0 : texture_2d<f32>;

@binding(3) @group(0) var sam_0 : sampler;

@binding(2) @group(0) var conv2d_2_0 : texture_storage_2d<rgba16float, write>;

fn LUMA_tex_0( uv_0 : vec2<f32>) -> vec4<f32>
{
    return vec4<f32>(dot((textureSampleLevel((LUMA_0), (sam_0), (uv_0), (0.0f))).xyz, vec3<f32>(0.2125999927520752f, 0.71520000696182251f, 0.07220000028610229f)), 0.0f, 0.0f, 1.0f);
}

fn hook_0( pos_0 : vec2<f32>) -> vec4<f32>
{
    return vec4<f32>(0.03118281066417694f, -0.00269942660816014f, -0.00372878066264093f, 0.0003732584300451f) + vec4<f32>(-0.07207971066236496f, 0.12400944530963898f, -0.18263049423694611f, -0.06461377441883087f) * vec4<f32>(LUMA_tex_0(pos_0 + vec2<f32>(-1.0f, -1.0f) / vec2<f32>(Params_0.dimensions_0.xy)).x) + vec4<f32>(0.13202652335166931f, -0.02463600225746632f, 0.17531424760818481f, 0.02642096392810345f) * vec4<f32>(LUMA_tex_0(pos_0 + vec2<f32>(0.0f, -1.0f) / vec2<f32>(Params_0.dimensions_0.xy)).x) + vec4<f32>(0.08179203420877457f, -0.01628864742815495f, 0.00493208831176162f, 0.03564971312880516f) * vec4<f32>(LUMA_tex_0(pos_0 + vec2<f32>(1.0f, -1.0f) / vec2<f32>(Params_0.dimensions_0.xy)).x) + vec4<f32>(-0.21004730463027954f, 0.36062362790107727f, 0.3787519633769989f, 0.12862665951251984f) * vec4<f32>(LUMA_tex_0(pos_0 + vec2<f32>(-1.0f, 0.0f) / vec2<f32>(Params_0.dimensions_0.xy)).x) + vec4<f32>(-0.35443854331970215f, -0.84707659482955933f, -0.33541738986968994f, -0.10354519635438919f) * vec4<f32>(LUMA_tex_0(pos_0 + vec2<f32>(0.0f, 0.0f) / vec2<f32>(Params_0.dimensions_0.xy)).x) + vec4<f32>(0.20664091408252716f, 0.17807371914386749f, -0.0573745034635067f, -0.01497742626816034f) * vec4<f32>(LUMA_tex_0(pos_0 + vec2<f32>(1.0f, 0.0f) / vec2<f32>(Params_0.dimensions_0.xy)).x) + vec4<f32>(0.06737475097179413f, 0.11318455636501312f, -0.17001555860042572f, -0.0686633363366127f) * vec4<f32>(LUMA_tex_0(pos_0 + vec2<f32>(-1.0f, 1.0f) / vec2<f32>(Params_0.dimensions_0.xy)).x) + vec4<f32>(0.2910485565662384f, 0.13395407795906067f, 0.15402670204639435f, 0.11570620536804199f) * vec4<f32>(LUMA_tex_0(pos_0 + vec2<f32>(0.0f, 1.0f) / vec2<f32>(Params_0.dimensions_0.xy)).x) + vec4<f32>(-0.20120938122272491f, -0.01795956119894981f, 0.03381584584712982f, -0.05401330813765526f) * vec4<f32>(LUMA_tex_0(pos_0 + vec2<f32>(1.0f, 1.0f) / vec2<f32>(Params_0.dimensions_0.xy)).x);
}

@compute
@workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) id_0 : vec3<u32>)
{
    var _S1 : vec2<u32> = id_0.xy;
    if((any((_S1 >= (Params_0.dimensions_0.xy)))))
    {
        return;
    }
    textureStore((conv2d_2_0), (_S1), (hook_0((vec2<f32>(_S1) + vec2<f32>(0.5f)) / vec2<f32>(Params_0.dimensions_0.xy))));
    return;
}

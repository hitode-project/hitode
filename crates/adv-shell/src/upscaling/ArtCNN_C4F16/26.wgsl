// Generated from pinned upstream sources; see vendor/upscaling/NOTICE.
struct SLANG_ParameterGroup_Params_std140_0
{
    @align(16) dimensions_0 : vec4<u32>,
};

@binding(0) @group(0) var<uniform> Params_0 : SLANG_ParameterGroup_Params_std140_0;
@binding(1) @group(0) var conv2d_6_0_0 : texture_2d<f32>;

@binding(4) @group(0) var sam_0 : sampler;

@binding(2) @group(0) var INPUT_0 : texture_2d<f32>;

@binding(3) @group(0) var OUTPUT_0 : texture_storage_2d<rgba16float, write>;

fn conv2d_6_0_tex_0( uv_0 : vec2<f32>) -> vec4<f32>
{
    return (textureSampleLevel((conv2d_6_0_0), (sam_0), (uv_0), (0.0f)));
}

fn hook_0( pos_0 : vec2<f32>) -> vec4<f32>
{
    var result_0 : vec4<f32> = vec4<f32>(0.0f, 0.0f, 0.0f, 1.0f);
    var f0_0 : vec2<f32> = fract(pos_0 * vec2<f32>(Params_0.dimensions_0.xy));
    var _S1 : vec2<i32> = vec2<i32>(f0_0 * vec2<f32>(2.0f));
    result_0[i32(0)] = conv2d_6_0_tex_0((vec2<f32>(0.5f) - f0_0) * (vec2<f32>(1.0f) / vec2<f32>(Params_0.dimensions_0.xy)) + pos_0)[_S1.y * i32(2) + _S1.x];
    return clamp(result_0, vec4<f32>(0.0f), vec4<f32>(1.0f));
}

@compute
@workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) id_0 : vec3<u32>)
{
    var _S2 : vec2<u32> = id_0.xy;
    if((any((_S2 >= (Params_0.dimensions_0.zw)))))
    {
        return;
    }
    var pos_1 : vec2<f32> = (vec2<f32>(_S2) + vec2<f32>(0.5f)) / vec2<f32>(Params_0.dimensions_0.zw);
    var rgb_0 : vec3<f32> = (textureSampleLevel((INPUT_0), (sam_0), (pos_1), (0.0f))).xyz;
    textureStore((OUTPUT_0), (_S2), (vec4<f32>(rgb_0 + vec3<f32>((hook_0(pos_1).x - dot(rgb_0, vec3<f32>(0.2125999927520752f, 0.71520000696182251f, 0.07220000028610229f)))), 1.0f)));
    return;
}

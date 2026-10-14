// Generated from pinned upstream sources; see vendor/upscaling/NOTICE.
struct SLANG_ParameterGroup_Params_std140_0
{
    @align(16) dimensions_0 : vec4<u32>,
};

@binding(0) @group(0) var<uniform> Params_0 : SLANG_ParameterGroup_Params_std140_0;
@binding(1) @group(0) var LUMA_0 : texture_2d<f32>;

@binding(3) @group(0) var sam_0 : sampler;

@binding(2) @group(0) var conv2d_3_0 : texture_storage_2d<rgba16float, write>;

fn LUMA_tex_0( uv_0 : vec2<f32>) -> vec4<f32>
{
    return vec4<f32>(dot((textureSampleLevel((LUMA_0), (sam_0), (uv_0), (0.0f))).xyz, vec3<f32>(0.2125999927520752f, 0.71520000696182251f, 0.07220000028610229f)), 0.0f, 0.0f, 1.0f);
}

fn hook_0( pos_0 : vec2<f32>) -> vec4<f32>
{
    return vec4<f32>(-0.0049566337838769f, -0.0008637509890832f, 0.00938562769442797f, -0.05450365319848061f) + vec4<f32>(0.08766466379165649f, -0.14293834567070007f, 0.24944287538528442f, -0.1190890297293663f) * vec4<f32>(LUMA_tex_0(pos_0 + vec2<f32>(-1.0f, -1.0f) / vec2<f32>(Params_0.dimensions_0.xy)).x) + vec4<f32>(-0.20067194104194641f, 0.21825326979160309f, 0.43851405382156372f, 0.23163676261901855f) * vec4<f32>(LUMA_tex_0(pos_0 + vec2<f32>(0.0f, -1.0f) / vec2<f32>(Params_0.dimensions_0.xy)).x) + vec4<f32>(0.01229582354426384f, -0.06864415854215622f, 0.05650031939148903f, 0.04072269424796104f) * vec4<f32>(LUMA_tex_0(pos_0 + vec2<f32>(1.0f, -1.0f) / vec2<f32>(Params_0.dimensions_0.xy)).x) + vec4<f32>(0.04752621054649353f, 0.00018846755847335f, 0.05499293655157089f, -0.27092695236206055f) * vec4<f32>(LUMA_tex_0(pos_0 + vec2<f32>(-1.0f, 0.0f) / vec2<f32>(Params_0.dimensions_0.xy)).x) + vec4<f32>(0.01579387672245502f, -0.1493777334690094f, -0.51547569036483765f, -0.02119462564587593f) * vec4<f32>(LUMA_tex_0(pos_0 + vec2<f32>(0.0f, 0.0f) / vec2<f32>(Params_0.dimensions_0.xy)).x) + vec4<f32>(0.0373893529176712f, 0.15699309110641479f, -0.17308734357357025f, 0.23973947763442993f) * vec4<f32>(LUMA_tex_0(pos_0 + vec2<f32>(1.0f, 0.0f) / vec2<f32>(Params_0.dimensions_0.xy)).x) + vec4<f32>(-0.08452466130256653f, 0.13827866315841675f, -0.05384433642029762f, -0.11077354103326797f) * vec4<f32>(LUMA_tex_0(pos_0 + vec2<f32>(-1.0f, 1.0f) / vec2<f32>(Params_0.dimensions_0.xy)).x) + vec4<f32>(0.12855713069438934f, -0.06819382309913635f, -0.16493387520313263f, -0.01538566313683987f) * vec4<f32>(LUMA_tex_0(pos_0 + vec2<f32>(0.0f, 1.0f) / vec2<f32>(Params_0.dimensions_0.xy)).x) + vec4<f32>(-0.03437929973006248f, -0.08640728890895844f, 0.03329051285982132f, 0.1497901976108551f) * vec4<f32>(LUMA_tex_0(pos_0 + vec2<f32>(1.0f, 1.0f) / vec2<f32>(Params_0.dimensions_0.xy)).x);
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
    textureStore((conv2d_3_0), (_S1), (hook_0((vec2<f32>(_S1) + vec2<f32>(0.5f)) / vec2<f32>(Params_0.dimensions_0.xy))));
    return;
}

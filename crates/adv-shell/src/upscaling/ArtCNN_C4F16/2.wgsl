// Generated from pinned upstream sources; see vendor/upscaling/NOTICE.
struct SLANG_ParameterGroup_Params_std140_0
{
    @align(16) dimensions_0 : vec4<u32>,
};

@binding(0) @group(0) var<uniform> Params_0 : SLANG_ParameterGroup_Params_std140_0;
@binding(1) @group(0) var LUMA_0 : texture_2d<f32>;

@binding(3) @group(0) var sam_0 : sampler;

@binding(2) @group(0) var conv2d_1_0 : texture_storage_2d<rgba16float, write>;

fn LUMA_tex_0( uv_0 : vec2<f32>) -> vec4<f32>
{
    return vec4<f32>(dot((textureSampleLevel((LUMA_0), (sam_0), (uv_0), (0.0f))).xyz, vec3<f32>(0.2125999927520752f, 0.71520000696182251f, 0.07220000028610229f)), 0.0f, 0.0f, 1.0f);
}

fn hook_0( pos_0 : vec2<f32>) -> vec4<f32>
{
    return vec4<f32>(-0.0266644973307848f, -0.00375956413336098f, 0.00117590907029808f, 0.00083150708815083f) + vec4<f32>(0.10171354562044144f, -0.11118058860301971f, -0.08754985779523849f, 0.04185217618942261f) * vec4<f32>(LUMA_tex_0(pos_0 + vec2<f32>(-1.0f, -1.0f) / vec2<f32>(Params_0.dimensions_0.xy)).x) + vec4<f32>(-0.08920513838529587f, -0.01556373666971922f, 0.01543291565030813f, -0.22755816578865051f) * vec4<f32>(LUMA_tex_0(pos_0 + vec2<f32>(0.0f, -1.0f) / vec2<f32>(Params_0.dimensions_0.xy)).x) + vec4<f32>(0.08262278139591217f, 0.13759559392929077f, 0.06919368356466293f, 0.1847749799489975f) * vec4<f32>(LUMA_tex_0(pos_0 + vec2<f32>(1.0f, -1.0f) / vec2<f32>(Params_0.dimensions_0.xy)).x) + vec4<f32>(-0.06889422237873077f, 0.10659056901931763f, 0.06685658544301987f, -0.03940286487340927f) * vec4<f32>(LUMA_tex_0(pos_0 + vec2<f32>(-1.0f, 0.0f) / vec2<f32>(Params_0.dimensions_0.xy)).x) + vec4<f32>(0.11116014420986176f, -0.20927311480045319f, 0.11305384337902069f, 0.20958375930786133f) * vec4<f32>(LUMA_tex_0(pos_0 + vec2<f32>(0.0f, 0.0f) / vec2<f32>(Params_0.dimensions_0.xy)).x) + vec4<f32>(-0.13418367505073547f, 0.28347292542457581f, -0.17690807580947876f, -0.16033287346363068f) * vec4<f32>(LUMA_tex_0(pos_0 + vec2<f32>(1.0f, 0.0f) / vec2<f32>(Params_0.dimensions_0.xy)).x) + vec4<f32>(0.06449020653963089f, 0.00703831110149622f, 0.0254477895796299f, -0.00129604560788721f) * vec4<f32>(LUMA_tex_0(pos_0 + vec2<f32>(-1.0f, 1.0f) / vec2<f32>(Params_0.dimensions_0.xy)).x) + vec4<f32>(0.07631179690361023f, -0.27137014269828796f, -0.13406933844089508f, 0.02124913409352303f) * vec4<f32>(LUMA_tex_0(pos_0 + vec2<f32>(0.0f, 1.0f) / vec2<f32>(Params_0.dimensions_0.xy)).x) + vec4<f32>(-0.09120962768793106f, 0.07497385889291763f, 0.10840508341789246f, -0.02889464609324932f) * vec4<f32>(LUMA_tex_0(pos_0 + vec2<f32>(1.0f, 1.0f) / vec2<f32>(Params_0.dimensions_0.xy)).x);
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
    textureStore((conv2d_1_0), (_S1), (hook_0((vec2<f32>(_S1) + vec2<f32>(0.5f)) / vec2<f32>(Params_0.dimensions_0.xy))));
    return;
}

// Generated from pinned upstream sources; see vendor/upscaling/NOTICE.
struct SLANG_ParameterGroup_Params_std140_0
{
    @align(16) dimensions_0 : vec4<u32>,
    @align(16) origin_0 : vec4<u32>,
};

@binding(0) @group(0) var<uniform> Params_0 : SLANG_ParameterGroup_Params_std140_0;
@binding(1) @group(0) var LUMA_0 : texture_2d<f32>;

@binding(3) @group(0) var sam_0 : sampler;

@binding(2) @group(0) var conv2d_0_0 : texture_storage_2d<rgba16float, write>;

fn LUMA_tex_0( uv_0 : vec2<f32>) -> vec4<f32>
{
    return vec4<f32>(dot((textureSampleLevel((LUMA_0), (sam_0), (uv_0), (0.0f))).xyz, vec3<f32>(0.2125999927520752f, 0.71520000696182251f, 0.07220000028610229f)), 0.0f, 0.0f, 1.0f);
}

fn hook_0( pos_0 : vec2<f32>) -> vec4<f32>
{
    return vec4<f32>(-0.00492900749668479f, -0.01222846750169992f, -0.00566070526838303f, -0.07538485527038574f) + vec4<f32>(-0.03172564879059792f, -0.17579308152198792f, 0.02376881241798401f, 0.00271212309598923f) * vec4<f32>(LUMA_tex_0(pos_0 + vec2<f32>(-1.0f, -1.0f) / vec2<f32>(Params_0.dimensions_0.xy)).x) + vec4<f32>(0.01935435645282269f, 0.19664944708347321f, 0.14559915661811829f, 0.13385973870754242f) * vec4<f32>(LUMA_tex_0(pos_0 + vec2<f32>(0.0f, -1.0f) / vec2<f32>(Params_0.dimensions_0.xy)).x) + vec4<f32>(0.01243773009628057f, 0.07908594608306885f, -0.13799697160720825f, 0.04767274856567383f) * vec4<f32>(LUMA_tex_0(pos_0 + vec2<f32>(1.0f, -1.0f) / vec2<f32>(Params_0.dimensions_0.xy)).x) + vec4<f32>(0.02844352647662163f, 0.11310484260320663f, 0.03962155058979988f, 0.17636382579803467f) * vec4<f32>(LUMA_tex_0(pos_0 + vec2<f32>(-1.0f, 0.0f) / vec2<f32>(Params_0.dimensions_0.xy)).x) + vec4<f32>(-0.03735103458166122f, -0.07099626213312149f, -0.38756141066551208f, 0.33549466729164124f) * vec4<f32>(LUMA_tex_0(pos_0 + vec2<f32>(0.0f, 0.0f) / vec2<f32>(Params_0.dimensions_0.xy)).x) + vec4<f32>(-0.2427942156791687f, -0.14805968105792999f, 0.15254049003124237f, 0.11202972382307053f) * vec4<f32>(LUMA_tex_0(pos_0 + vec2<f32>(1.0f, 0.0f) / vec2<f32>(Params_0.dimensions_0.xy)).x) + vec4<f32>(0.00344173843041062f, 0.17604658007621765f, -0.04915207251906395f, 0.09447337687015533f) * vec4<f32>(LUMA_tex_0(pos_0 + vec2<f32>(-1.0f, 1.0f) / vec2<f32>(Params_0.dimensions_0.xy)).x) + vec4<f32>(0.05195661261677742f, -0.16056416928768158f, 0.25666967034339905f, -0.05485959351062775f) * vec4<f32>(LUMA_tex_0(pos_0 + vec2<f32>(0.0f, 1.0f) / vec2<f32>(Params_0.dimensions_0.xy)).x) + vec4<f32>(0.20419681072235107f, 0.01003602333366871f, -0.04147930070757866f, 0.09628883749246597f) * vec4<f32>(LUMA_tex_0(pos_0 + vec2<f32>(1.0f, 1.0f) / vec2<f32>(Params_0.dimensions_0.xy)).x);
}

@compute
@workgroup_size(8, 8, 1)
fn main(@builtin(global_invocation_id) group_0 : vec3<u32>)
{
    var id_0 : vec2<u32> = group_0.xy + Params_0.origin_0.xy;
    if((any((id_0 >= (Params_0.dimensions_0.xy)))))
    {
        return;
    }
    textureStore((conv2d_0_0), (id_0), (hook_0((vec2<f32>(id_0) + vec2<f32>(0.5f)) / vec2<f32>(Params_0.dimensions_0.xy))));
    return;
}

interface StageColor { float4 shade(); };
class StageTint : StageColor {
    float4 tint;
    Texture2D<float4> tex;
    SamplerState samp;
    float4 shade() { return tint * tex.Sample(samp, float2(0.5, 0.5)); }
};
StageTint namedTint[2];
StageColor stagePaint[2];
cbuffer StageSelection : register(b4) { uint selected; };
float4 stage_members() : SV_Target {
    return stagePaint[selected & 1].shade();
}

struct StagePatch {
    float edges[3] : SV_TessFactor;
    float inside : SV_InsideTessFactor;
};
struct StageVertex {
    float4 position : SV_Position;
};
StagePatch stage_patch_constants(InputPatch<StageVertex, 3> patch) {
    StagePatch result = {{64, 64, 64}, 64};
    return result;
}
[domain("tri")]
[partitioning("fractional_even")]
[outputtopology("triangle_cw")]
[outputcontrolpoints(3)]
[patchconstantfunc("stage_patch_constants")]
StageVertex stage_wide_hull(InputPatch<StageVertex, 3> patch, uint i : SV_OutputControlPointID) {
    return patch[i];
}

[maxvertexcount(64)]
void stage_wide_geometry(triangle StageVertex input[3], inout TriangleStream<StageVertex> stream) {
    [unroll] for (uint i = 0; i < 3; i++) stream.Append(input[i]);
    stream.RestartStrip();
}

// Authored shader corpus for the SM4/5 token codec.
cbuffer Scene : register(b0) {
    float4 scale;
    float4 offset;
};

Texture2D<float4> image : register(t0);
SamplerState filtering : register(s0);

struct Vertex {
    float4 position : SV_Position;
    float2 uv : TEXCOORD0;
};

Vertex vs(float4 position : POSITION, float2 uv : TEXCOORD0) {
    Vertex result;
    result.position = position * scale + offset;
    result.uv = uv;
    return result;
}

float4 ps(Vertex input) : SV_Target {
    float4 color = image.Sample(filtering, input.uv);
    return saturate(color * scale + offset);
}

#ifndef LEVEL9
float4 indexed_ps(Vertex input) : SV_Target {
    static const float4 colors[4] = {
        float4(1, 0, 0, 0), float4(0, 1, 0, 1),
        float4(0, 0, 1, 1), float4(1, 1, 0, 0)
    };
    uint index = uint(input.position.x) & 3;
    float4 color = image.Sample(filtering, input.uv, int2(1, -2));
    return color * colors[index];
}

[maxvertexcount(3)]
void gs(triangle Vertex input[3], inout TriangleStream<Vertex> stream) {
    [unroll]
    for (int i = 0; i < 3; i++) {
        stream.Append(input[i]);
    }
    stream.RestartStrip();
}

RWStructuredBuffer<uint> output : register(u0);
groupshared uint values[8];

[numthreads(8, 1, 1)]
void cs(uint3 id : SV_DispatchThreadID, uint index : SV_GroupIndex) {
    values[index] = id.x * id.x;
    GroupMemoryBarrierWithGroupSync();
    output[id.x] = values[7 - index];
}

struct Patch {
    float edges[3] : SV_TessFactor;
    float inside : SV_InsideTessFactor;
};

Patch patch_constants(InputPatch<Vertex, 3> patch) {
    Patch result;
    result.edges[0] = patch[0].uv.x * scale.x + 1;
    result.edges[1] = patch[1].uv.x * scale.x + 1;
    result.edges[2] = patch[2].uv.x * scale.x + 1;
    result.inside = 3;
    return result;
}

[domain("tri")]
[partitioning("fractional_even")]
[outputtopology("triangle_cw")]
[outputcontrolpoints(3)]
[patchconstantfunc("patch_constants")]
Vertex hs(InputPatch<Vertex, 3> patch, uint i : SV_OutputControlPointID) {
    return patch[i];
}

[domain("tri")]
Vertex ds(Patch constants, float3 uvw : SV_DomainLocation, const OutputPatch<Vertex, 3> patch) {
    Vertex result;
    result.position = patch[0].position * uvw.x + patch[1].position * uvw.y + patch[2].position * uvw.z;
    result.uv = patch[0].uv * uvw.x + patch[1].uv * uvw.y + patch[2].uv * uvw.z;
    return result;
}
#endif

#ifndef LEVEL9
[domain("tri")]
[partitioning("fractional_even")]
[outputtopology("triangle_cw")]
[outputcontrolpoints(3)]
[patchconstantfunc("patch_constants")]
[maxtessfactor(32.0)]
Vertex limited_hull(InputPatch<Vertex, 3> patch, uint i : SV_OutputControlPointID) {
    return patch[i];
}
#endif

Texture2D t : register(t0);
SamplerState s : register(s0);
cbuffer Coordinates : register(b0) { float4 parameters; };
float4 main(float4 p : SV_Position) : SV_Target
{
    float g = exp2((p.x - 0.5) / 8) / 64;
    return t.SampleGrad(s, parameters.xy, float2(g, 0), float2(0, parameters.z == 0 ? g : parameters.z));
}

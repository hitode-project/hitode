Texture2D t : register(t0);
SamplerState s : register(s0);
cbuffer Coordinates : register(b0) { float4 parameters; };
float4 main(float4 p : SV_Position) : SV_Target
{
    return t.SampleLevel(s, float2((p.x - 0.5 + parameters.x) * parameters.y, 0.5), parameters.z);
}

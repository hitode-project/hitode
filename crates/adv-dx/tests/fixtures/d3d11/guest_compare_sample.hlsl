Texture2D<float> t : register(t0);
SamplerComparisonState s : register(s0);
cbuffer Comparison : register(b0) { float4 parameters; };
float4 main(float4 p : SV_Position) : SV_Target
{
    float i = p.x - 0.5;
    return t.SampleCmpLevelZero(s, float2(parameters.x + i * parameters.y, 0.5), parameters.z + i * parameters.w);
}

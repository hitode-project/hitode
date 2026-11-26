Texture2D t : register(t0);
SamplerState s : register(s0);
float4 main(float4 p : SV_Position) : SV_Target
{
    return t.SampleLevel(s, float2((0.5 + (p.x - 0.5) / 64) / 2, 0.5), 0);
}

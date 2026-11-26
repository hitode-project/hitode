Texture2D<uint2> t : register(t0);
float4 main(float4 p : SV_Position) : SV_Target
{
    return float(t.Load(int3(int(p.x), 0, 0)).g);
}

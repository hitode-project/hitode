Texture2D t : register(t0);
float4 main(float4 p : SV_Position) : SV_Target
{
    uint i = uint(p.x);
    return t.Load(int3(i % 4, i / 4, 0));
}

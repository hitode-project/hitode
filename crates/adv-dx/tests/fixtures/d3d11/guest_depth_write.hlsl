cbuffer Depths : register(b0) { float4 depths[16]; };
float main(float4 p : SV_Position) : SV_Depth
{
    uint i = uint(p.x);
    return depths[i / 4][i % 4];
}

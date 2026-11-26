cbuffer Depths : register(b0) { float4 depths; };
float4 main(uint id : SV_VertexID) : SV_Position
{
    return float4(id == 1 ? 3.0 : -1.0, id == 2 ? -3.0 : 1.0, depths[id], 1.0);
}

cbuffer C : register(b0) { float4 color; };

float4 read_mask(uint coverage : SV_Coverage) : SV_Target
{
    return float4(float(coverage * 4u) / 255.0, 1.0, 0.0, color.a);
}

float4 write_mask(out uint mask : SV_Coverage) : SV_Target
{
    mask = asuint(color.z);
    return color;
}

float4 read_write_mask(uint coverage : SV_Coverage, out uint mask : SV_Coverage) : SV_Target
{
    mask = asuint(color.z);
    return read_mask(coverage);
}

[earlydepthstencil]
float4 early_color() : SV_Target
{
    return color;
}

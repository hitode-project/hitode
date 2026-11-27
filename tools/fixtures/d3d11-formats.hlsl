// Compile each entry with fxc /T ps_5_0 /O3 /Qstrip_debug /Qstrip_priv.
Texture2D<uint4> rgb_uint : register(t0);
float4 format_rgb_uint(float4 p : SV_Position) : SV_Target
{
    return float4(rgb_uint.Load(int3(int(p.x), 0, 0)));
}

Buffer<float4> rgb_buffer : register(t0);
float4 format_rgb_buffer(float4 p : SV_Position) : SV_Target
{
    return rgb_buffer.Load(int(p.x));
}

Texture1DArray<float4> narrow : register(t0);
float4 format_narrow_array(float4 p : SV_Position) : SV_Target
{
    uint width, layers, mips;
    narrow.GetDimensions(1, width, layers, mips);
    float4 color = narrow.Load(int3(int(p.x), 0, 1));
    return float4(color.xy, width, layers * 16 + mips);
}

Texture2DMSArray<float4> ms_array : register(t0);
float4 format_ms_array(float4 p : SV_Position) : SV_Target
{
    uint width, height, layers, samples;
    ms_array.GetDimensions(width, height, layers, samples);
    return float4(ms_array.Load(int3(int(p.x), 0, 1), 3).xy, layers, samples);
}

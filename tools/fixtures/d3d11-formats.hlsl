// Compile format_* entries with fxc /O3 /Qstrip_debug /Qstrip_priv.
// Use ps_5_0 for the pixel entries and cs_5_0 for the compute entries.
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

#ifdef ATOMIC_SCALAR
// Compile twice with /D ATOMIC_SCALAR=uint /D ATOMIC_MIN=5 and
// /D ATOMIC_SCALAR=int /D ATOMIC_MIN=-2.
RWTexture2DArray<ATOMIC_SCALAR> atomic_image : register(u0);

[numthreads(1, 1, 1)]
void format_atomic_set()
{
    uint3 p = uint3(0, 1, 1);
    ATOMIC_SCALAR old;
    InterlockedAdd(atomic_image[p], 3, old);
    atomic_image[p + uint3(1, 0, 0)] = old;
    InterlockedAnd(atomic_image[p], 7, old);
    atomic_image[p + uint3(2, 0, 0)] = old;
    InterlockedOr(atomic_image[p], 8, old);
    atomic_image[p + uint3(3, 0, 0)] = old;
    InterlockedXor(atomic_image[p], 4, old);
    atomic_image[p + uint3(4, 0, 0)] = old;
    InterlockedMin(atomic_image[p], ATOMIC_SCALAR(ATOMIC_MIN), old);
    atomic_image[p + uint3(5, 0, 0)] = old;
    InterlockedMax(atomic_image[p], ATOMIC_SCALAR(6), old);
    atomic_image[p + uint3(6, 0, 0)] = old;
    InterlockedExchange(atomic_image[p], 2, old);
    atomic_image[p + uint3(7, 0, 0)] = old;
    InterlockedCompareExchange(atomic_image[p], 2, 3, old);
    atomic_image[p + uint3(8, 0, 0)] = old;
    InterlockedCompareExchange(atomic_image[p], 1, 7, old);
    atomic_image[p + uint3(9, 0, 0)] = old;
    InterlockedCompareStore(atomic_image[p], 3, 8);
    InterlockedCompareStore(atomic_image[p], 3, 99);
    InterlockedAdd(atomic_image[p], 4);
    InterlockedAnd(atomic_image[p], 13);
    InterlockedOr(atomic_image[p], 2);
    InterlockedXor(atomic_image[p], 4);
    InterlockedMin(atomic_image[p], ATOMIC_SCALAR(9));
    InterlockedMax(atomic_image[p], ATOMIC_SCALAR(7));
}
#endif

RWTexture2D<float4> alpha_uav : register(u0);

[numthreads(1, 1, 1)]
void format_alpha_uav()
{
    alpha_uav[uint2(1, 0)] = float4(1, 0.5, 0, 0.25);
}

RWTexture2D<float> float_uav : register(u0);

[numthreads(1, 1, 1)]
void format_atomic_float()
{
    float old;
    InterlockedExchange(float_uav[uint2(0, 1)], 2.5, old);
    float_uav[uint2(1, 1)] = old;
}

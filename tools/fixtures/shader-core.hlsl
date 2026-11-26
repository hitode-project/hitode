RWStructuredBuffer<uint> output_words : register(u0);
StructuredBuffer<uint> input_words : register(t0);

[numthreads(8, 1, 1)]
void arithmetic(uint3 id : SV_DispatchThreadID) {
    uint i = id.x;
    uint x = input_words[i];
    uint y = input_words[i + 8];
    float a = asfloat(x);
    float b = asfloat(y);
    output_words[i * 4] = asuint(round(-abs(a)) + b);
    output_words[i * 4 + 1] = f32tof16(a);
    output_words[i * 4 + 2] = (x << y) ^ reversebits(x);
    output_words[i * 4 + 3] = x / y;
}

[numthreads(8, 1, 1)]
void control(uint3 id : SV_DispatchThreadID) {
    uint n = input_words[id.x] & 7;
    uint result = 0;
    [loop] for (uint i = 0; i < n; ++i) {
        if (i & 1)
            result += i;
        else
            result ^= n;
    }
    switch (n) {
        case 0: result = 9; break;
        case 1: result = 6; break;
        case 2: result *= 4; break;
        default: result += 7; break;
    }
    output_words[id.x] = result;
}

float4 color(float4 position : SV_Position) : SV_Target {
    return float4(position.x * 0.03125, position.y * 0.0625, 0.5, 1.0);
}

float4 discard_pixel(float4 position : SV_Position) : SV_Target {
    if (position.y > 4.0)
        discard;
    return float4(0.25, position.x * 0.03125, 0.75, 1.0);
}

RWTexture2D<uint> typed_output : register(u1);

[numthreads(8, 1, 1)]
void typed_uav(uint3 id : SV_DispatchThreadID) {
    typed_output[id.xy] = typed_output[id.xy] + id.x;
}

[numthreads(8, 1, 1)]
void doubles(uint3 id : SV_DispatchThreadID) {
    double value = (double)asfloat(input_words[id.x]);
    uint2 bits;
    asuint(value * value, bits.x, bits.y);
    output_words[id.x * 2] = bits.x;
    output_words[id.x * 2 + 1] = bits.y;
}

[numthreads(8, 1, 1)]
void atomics(uint3 id : SV_DispatchThreadID) {
    output_words[id.x] = 0;
    uint original;
    InterlockedAdd(output_words[id.x], input_words[id.x], original);
    InterlockedCompareExchange(output_words[id.x], input_words[id.x], original + 17, original);
    output_words[id.x + 8] = original;
}

ByteAddressBuffer raw_input : register(t0);
RWByteAddressBuffer raw_output : register(u0);

[numthreads(8, 1, 1)]
void raw(uint3 id : SV_DispatchThreadID) {
    uint source = raw_input.Load(id.x * 4);
    uint zero = raw_input.Load((id.x + 8) * 4);
    raw_output.Store(id.x * 4, source ^ zero);
}

Texture2D<float4> color_texture : register(t0);
Texture2D<float> depth_texture : register(t1);
SamplerState texture_filter : register(s0);
SamplerComparisonState shadow_filter : register(s1);

float4 texture_ops(float2 uv : TEXCOORD0) : SV_Target {
    uint width, height, levels;
    color_texture.GetDimensions(1, width, height, levels);
    float4 gathered = color_texture.GatherGreen(texture_filter, uv, int2(1, 1));
    float4 shadow = depth_texture.GatherCmp(shadow_filter, uv, 0.5);
    float4 loaded = color_texture.Load(int3(uv * float2(width, height), 1));
    return gathered + shadow + loaded + (float)levels;
}

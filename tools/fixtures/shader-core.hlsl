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
    uint x = input_words[id.x], y = input_words[id.x + 8];
    double a = (double)asfloat(x), b = (double)asfloat(y);
    double rows[10] = {
        a * b, a + b, min(a, b), max(a, b), a / b, rcp(a),
        fma(a, b, (double)1), x ? a : b, (double)asint(x), (double)y
    };
    [unroll] for (uint i = 0; i < 10; ++i) {
        uint2 bits;
        asuint(rows[i], bits.x, bits.y);
        output_words[id.x * 26 + 2 * i] = bits.x;
        output_words[id.x * 26 + 2 * i + 1] = bits.y;
    }
    output_words[id.x * 26 + 20] = a == b ? 1 : 0;
    output_words[id.x * 26 + 21] = a != b ? 1 : 0;
    output_words[id.x * 26 + 22] = a >= b ? 1 : 0;
    output_words[id.x * 26 + 23] = a < b ? 1 : 0;
    output_words[id.x * 26 + 24] = (uint)(int)a;
    output_words[id.x * 26 + 25] = (uint)b ^ asuint((float)(a + b));
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

Buffer<uint2> typed_input : register(t0);
RWBuffer<uint> typed_buffer_output : register(u0);
AppendStructuredBuffer<uint> append_output : register(u1);
ConsumeStructuredBuffer<uint> consume_input : register(u2);
RWStructuredBuffer<uint> counter_output : register(u3);

[numthreads(1, 1, 1)]
void buffer_views(uint3 id : SV_DispatchThreadID) {
    uint count;
    typed_input.GetDimensions(count);
    typed_buffer_output[0] = typed_input[id.x].x + count;
    typed_buffer_output[1] = typed_input[id.x].y;
    typed_buffer_output[2] = typed_buffer_output[id.x] + typed_input[count].x;
}

[numthreads(1, 1, 1)]
void counters(uint3 id : SV_DispatchThreadID) {
    append_output.Append(consume_input.Consume() + id.x);
    uint index = counter_output.IncrementCounter();
    counter_output[index] = index + 19;
}

[numthreads(8, 1, 1)]
void texture_atomic(uint3 id : SV_DispatchThreadID) {
    InterlockedAdd(typed_output[id.xy], id.x + 1);
}

[numthreads(8, 1, 1)]
void texture_atomic_return(uint3 id : SV_DispatchThreadID) {
    uint old;
    InterlockedAdd(typed_output[id.xy], id.x + 1, old);
    typed_output[id.xy] = old + id.x * 2;
}

float4 raster_queries(noperspective float2 uv : TEXCOORD0, float4 position : SV_Position) : SV_Target {
    int2 offset = int2(3, -2);
    float2 snapped = EvaluateAttributeSnapped(uv, offset);
    float2 sample = EvaluateAttributeAtSample(uv, uint(position.x) & 3);
    float2 centroid_value = EvaluateAttributeCentroid(uv);
    return float4(snapped.x, sample.y, centroid_value.x, centroid_value.y);
}

float4 raster_samples(float4 position : SV_Position, uint covered : SV_Coverage,
                      out uint output_mask : SV_Coverage) : SV_Target {
    output_mask = covered;
    return float4(GetRenderTargetSamplePosition(uint(position.x)), GetRenderTargetSampleCount(), 1);
}

float4 dynamic_gather(float2 uv : TEXCOORD0, float4 position : SV_Position) : SV_Target {
    int2 offset = int2(position.xy) + int2(31, -32);
    return color_texture.GatherGreen(texture_filter, uv, offset);
}

float4 dynamic_gather_compare(float2 uv : TEXCOORD0, float4 position : SV_Position) : SV_Target {
    int2 offset = int2(position.xy) + int2(31, -32);
    return depth_texture.GatherCmp(shadow_filter, uv, 0.5, offset);
}

float4 level_query(float2 uv : TEXCOORD0) : SV_Target {
    return float4(color_texture.CalculateLevelOfDetail(texture_filter, uv),
                  color_texture.CalculateLevelOfDetailUnclamped(texture_filter, uv), 0, 1);
}

float4 derivative_queries(float4 input : TEXCOORD0) : SV_Target {
    return float4(ddx_coarse(input.x), ddy_coarse(input.y), ddx_fine(input.z), ddy_fine(input.w));
}

Texture2DMS<float4, 4> multisampled : register(t0);

float4 multisample_queries(float4 position : SV_Position, uint sample : SV_SampleIndex) : SV_Target {
    uint width, height, samples;
    multisampled.GetDimensions(width, height, samples);
    return multisampled.Load(int2(position.xy), sample) +
        float4(multisampled.GetSamplePosition(sample), width + height, samples);
}

[numthreads(8, 1, 1)]
void integer_ops(uint3 id : SV_DispatchThreadID) {
    uint x = input_words[id.x], y = input_words[id.x + 8];
    int a = asint(x), b = asint(y);
    uint u = id.x * 16;
    output_words[u + 0] = x & y;
    output_words[u + 1] = x | y;
    output_words[u + 2] = x ^ y;
    output_words[u + 3] = x == y ? -1 : 0;
    output_words[u + 4] = x != y ? -1 : 0;
    output_words[u + 5] = a >= b ? -1 : 0;
    output_words[u + 6] = x >= y ? -1 : 0;
    output_words[u + 7] = max(a, b);
    output_words[u + 8] = min(x, y);
    output_words[u + 9] = a * b + 7;
    output_words[u + 10] = x ? y : 7;
    output_words[u + 11] = asuint(abs(asfloat(x)));
    output_words[u + 12] = asuint(-asfloat(x));
    output_words[u + 13] = asuint(dot(asfloat(uint3(x, y, x)), asfloat(uint3(y, x, y))));
    output_words[u + 14] = asuint(dot(asfloat(uint4(x, x, y, y)), asfloat(uint4(y, y, x, x))));
    output_words[u + 15] = msad4(x, uint2(y, id.x), uint4(3, 5, 7, 9)).x;
}

float4 sample_modes(float2 uv : TEXCOORD0) : SV_Target {
    float4 value = color_texture.Sample(texture_filter, uv);
    value += color_texture.SampleBias(texture_filter, uv, uv.x);
    value += color_texture.SampleGrad(texture_filter, uv, uv, uv.yx);
    value += color_texture.SampleLevel(texture_filter, uv, uv.y);
    value += depth_texture.SampleCmp(shadow_filter, uv, uv.x);
    value += depth_texture.SampleCmpLevelZero(shadow_filter, uv, uv.y);
    return value;
}

float4 vertex_sample(uint id : SV_VertexID) : SV_Position {
    return color_texture.SampleLevel(texture_filter, float2(id & 1, id >> 1), 0);
}

RWStructuredBuffer<int> signed_output : register(u1);

[numthreads(1, 1, 1)]
void atomic_modes(uint3 id : SV_DispatchThreadID) {
    uint i = id.x;
    uint x = input_words[i], old;
    int sx = asint(x), sold;
    InterlockedAnd(output_words[i], x);
    InterlockedOr(output_words[i], x);
    InterlockedXor(output_words[i], x);
    InterlockedMin(output_words[i], x);
    InterlockedMax(output_words[i], x);
    InterlockedCompareStore(output_words[i], x, 7);
    InterlockedAnd(output_words[i], x, old);
    output_words[8] = old;
    InterlockedOr(output_words[i], x, old);
    output_words[9] = old;
    InterlockedXor(output_words[i], x, old);
    output_words[10] = old;
    InterlockedMin(output_words[i], x, old);
    output_words[11] = old;
    InterlockedMax(output_words[i], x, old);
    output_words[12] = old;
    InterlockedExchange(output_words[i], x, old);
    output_words[13] = old;
    InterlockedMin(signed_output[i], sx);
    InterlockedMax(signed_output[i], sx);
    InterlockedMin(signed_output[i], sx, sold);
    output_words[14] = asuint(sold);
    InterlockedMax(signed_output[i], sx, sold);
    output_words[15] = asuint(sold);
}

uint4 coverage(uint sample : SV_SampleIndex, uint mask : SV_Coverage) : SV_Target {
    return uint4(sample, mask, GetRenderTargetSampleCount(), 0);
}

[earlydepthstencil]
float4 early_depth(float4 position : SV_Position) : SV_Target {
    return position;
}

Buffer<uint> packed_input : register(t0);

[numthreads(2, 1, 1)]
void packed_buffer(uint3 id : SV_DispatchThreadID) {
    typed_buffer_output[id.x] = packed_input[id.x];
}

[numthreads(8, 1, 1)]
void float_ops(uint3 id : SV_DispatchThreadID) {
    float a = asfloat(input_words[id.x]), b = asfloat(input_words[id.x + 8]);
    uint i = id.x * 24;
    output_words[i + 0] = asuint(a / b);
    output_words[i + 1] = asuint(dot(float2(a, a), float2(b, b)));
    output_words[i + 2] = a == b ? -1 : 0;
    output_words[i + 3] = a != b ? -1 : 0;
    output_words[i + 4] = a >= b ? -1 : 0;
    output_words[i + 5] = a < b ? -1 : 0;
    output_words[i + 6] = asuint(exp2(a));
    output_words[i + 7] = asuint(log2(b));
    output_words[i + 8] = asuint(frac(a));
    output_words[i + 9] = asuint(min(a, b));
    output_words[i + 10] = asuint(max(a, b));
    output_words[i + 11] = asuint(floor(a));
    output_words[i + 12] = asuint(ceil(a));
    output_words[i + 13] = asuint(trunc(a));
    output_words[i + 14] = asuint(rsqrt(a));
    output_words[i + 15] = asuint(sqrt(a));
    output_words[i + 16] = asuint(sin(a));
    output_words[i + 17] = asuint(cos(a));
    output_words[i + 18] = asuint(rcp(b));
    output_words[i + 19] = asuint(saturate(a));
    output_words[i + 20] = asuint(mad(a, b, 1));
    output_words[i + 21] = asuint(f16tof32(asuint(a)));
    output_words[i + 22] = (uint)(int)b;
    output_words[i + 23] = (uint)b;
}

[numthreads(8, 1, 1)]
void bit_ops(uint3 id : SV_DispatchThreadID) {
    uint x = input_words[id.x], y = input_words[id.x + 8];
    int a = asint(x), b = asint(y);
    uint i = id.x * 18;
    output_words[i + 0] = a < b ? -1 : 0;
    output_words[i + 1] = x < y ? -1 : 0;
    output_words[i + 2] = min(a, b);
    output_words[i + 3] = max(x, y);
    output_words[i + 4] = -a;
    output_words[i + 5] = ~x;
    output_words[i + 6] = a >> b;
    output_words[i + 7] = x >> y;
    output_words[i + 8] = asuint((float)a);
    output_words[i + 9] = asuint((float)y);
    output_words[i + 10] = countbits(x);
    output_words[i + 11] = firstbithigh(x);
    output_words[i + 12] = firstbithigh(a);
    output_words[i + 13] = firstbitlow(x);
    output_words[i + 14] = (x >> 4) & 0xff;
    output_words[i + 15] = (a << 20) >> 24;
    output_words[i + 16] = (x & ~0xff0u) | ((y << 4) & 0xff0u);
    output_words[i + 17] = x % y;
}

[numthreads(8, 1, 1)]
void branches(uint3 id : SV_DispatchThreadID) {
    uint flag = input_words[id.x];
    [branch] if (flag == 7) {
        output_words[id.x] = 17;
        return;
    } else {
        output_words[id.x] = 13;
    }
    [loop] for (uint i = 0; i < (flag & 7); ++i) {
        if (i & 1) continue;
        if (i == 4) break;
        output_words[id.x] += i;
    }
}

cbuffer RuntimeIndices : register(b0) {
    float4 palette[8];
};

float4 indexed_inputs(float4 first : TEXCOORD0, float4 second : TEXCOORD1, uint selector : TEXCOORD2) : SV_Target {
    float4 choices[2] = {first, second};
    return choices[selector & 1] + palette[selector & 7];
}

float4 derivatives_4_0(float4 input : TEXCOORD0) : SV_Target {
    return ddx(input) - ddy(input);
}

ByteAddressBuffer tiled_raw : register(t3);
StructuredBuffer<float4> tiled_structured : register(t4);

Texture2DMS<float4, 4> tiled_multisampled : register(t2);

float4 residency(float4 position : SV_Position, float2 uv : TEXCOORD0) : SV_Target {
    uint status;
    int2 offset = int2(position.xy);
    float2 gradient = float2(1.0 / 64, 1.0 / 64);
    float4 value = color_texture.Load(int3(position.xy, 0), int2(1, 1), status) * CheckAccessFullyMapped(status);
    value += tiled_multisampled.Load(int2(position.xy), 0, int2(1, 1), status) * CheckAccessFullyMapped(status);
    value += color_texture.Sample(texture_filter, uv, int2(1, 1), uv.y, status) * CheckAccessFullyMapped(status);
    value += color_texture.SampleBias(texture_filter, uv, 0.5, int2(1, 1), uv.y, status) * CheckAccessFullyMapped(status);
    value += color_texture.SampleGrad(texture_filter, uv, gradient, gradient.yx, int2(1, 1), uv.y, status) * CheckAccessFullyMapped(status);
    value += color_texture.SampleLevel(texture_filter, uv, uv.x, int2(1, 1), status) * CheckAccessFullyMapped(status);
    value += depth_texture.SampleCmp(shadow_filter, uv, 0.5, int2(1, 1), uv.y, status) * CheckAccessFullyMapped(status);
    value += depth_texture.SampleCmpLevelZero(shadow_filter, uv, 0.5, int2(1, 1), status) * CheckAccessFullyMapped(status);
    value += color_texture.GatherGreen(texture_filter, uv, int2(1, 1), status) * CheckAccessFullyMapped(status);
    value += color_texture.GatherRed(texture_filter, uv, offset, status) * CheckAccessFullyMapped(status);
    value += depth_texture.GatherCmp(shadow_filter, uv, 0.5, int2(1, 1), status) * CheckAccessFullyMapped(status);
    value += depth_texture.GatherCmp(shadow_filter, uv, 0.5, offset, status) * CheckAccessFullyMapped(status);
    value += tiled_raw.Load(uint(position.x) * 4, status) * CheckAccessFullyMapped(status);
    value += tiled_structured.Load(uint(position.x), status) * CheckAccessFullyMapped(status);
    value += typed_output.Load(int2(position.xy), status) * CheckAccessFullyMapped(status);
    return value;
}

interface ShaderColor {
    float4 shade(float4 position);
};
class Red : ShaderColor {
    float4 shade(float4 position) { return float4(position.x, 0, 0, 1); }
};
class Blue : ShaderColor {
    float4 shade(float4 position) { return float4(0, 0, position.y, 1); }
};
ShaderColor paint;
Red red;
Blue blue;
float4 linkage(float4 position : SV_Position) : SV_Target {
    return paint.shade(position);
}

[numthreads(1, 1, 1)]
void swap_values(uint3 id : SV_DispatchThreadID) {
    uint a = input_words[id.x], b = input_words[id.x + 1];
    bool swap = (id.x & 1) != 0;
    output_words[id.x * 2] = swap ? a : b;
    output_words[id.x * 2 + 1] = swap ? b : a;
}

float4 diagnostic_pixel(float4 position : SV_Position) : SV_Target {
    if (position.x < 1) abort();
    return position;
}

[numthreads(1, 1, 1)]
void min_precision(uint3 id : SV_DispatchThreadID) {
    min16float a = (min16float)(float(id.x) * 0.25);
    min16int b = (min16int)(int(id.x) - 4);
    min16uint c = (min16uint)id.x;
    output_words[id.x * 3] = asuint(float(abs(a) * a));
    output_words[id.x * 3 + 1] = uint(b * b);
    output_words[id.x * 3 + 2] = uint(c * c);
}

struct StreamVertex {
    float4 position : SV_Position;
    uint primitive : SV_PrimitiveID;
};

[maxvertexcount(2)]
[instance(2)]
void instanced_geometry(point float4 input[1] : SV_Position, uint id : SV_GSInstanceID,
                        inout LineStream<StreamVertex> stream) {
    StreamVertex result;
    result.position = input[0];
    result.primitive = id;
    stream.Append(result);
    result.position.x += 0.5;
    stream.Append(result);
    stream.RestartStrip();
}

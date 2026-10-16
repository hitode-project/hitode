#include <metal_stdlib>
using namespace metal;

// Integer decoding matches the software surface formats, including zero alpha
// for RGB and the complete stored alpha byte in indexed palettes.
kernel void decode(device const uchar *pixels [[buffer(0)]],
                   constant uint *p [[buffer(1)]],
                   constant uchar4 *palette [[buffer(2)]],
                   texture2d<float, access::write> output [[texture(0)]],
                   uint2 tid [[thread_position_in_grid]]) {
    if (tid.x >= p[3] || tid.y >= p[4]) return;
    uint2 xy = tid + uint2(p[1], p[2]);
    device const uchar *row = pixels + xy.y * p[0];
    uint x = xy.x, format = p[5];
    uchar4 bgra;
    if (format == 1) {
        bgra = uchar4(row[x * 3], row[x * 3 + 1], row[x * 3 + 2], 0);
    } else if (format == 2 || format == 3) {
        uint v = uint(row[x * 2]) | (uint(row[x * 2 + 1]) << 8);
        uint g = format == 2 ? ((v >> 5) & 63) * 255 / 63 : ((v >> 5) & 31) * 255 / 31;
        uint r = (v >> (format == 2 ? 11 : 10)) & 31;
        bgra = uchar4((v & 31) * 255 / 31, g, r * 255 / 31, 0);
    } else {
        uint index = format == 4 ? row[x] : format == 5
            ? (row[x / 2] >> ((1 - (x & 1)) * 4)) & 15
            : (row[x / 8] >> (7 - (x & 7))) & 1;
        bgra = palette[index];
    }
    output.write(float4(bgra.zyxw) / 255.0f, xy);
}

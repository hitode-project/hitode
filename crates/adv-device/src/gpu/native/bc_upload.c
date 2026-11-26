/* Use the same BC decoder as D3DX, with signed BC4/5 and BC6H/7 enabled
 * for modern texture uploads. */
#include <stdint.h>
#include <string.h>

#define BCDEC_IMPLEMENTATION
#define BCDECDEF static inline
#define WINE_UNUSED
#include "bcdec.h"

static void signed_channel(const uint8_t *src, uint8_t *dst, unsigned stride)
{
    int v[8] = {(int8_t)src[0], (int8_t)src[1]};
    if (v[0] == -128) v[0] = -127;
    if (v[1] == -128) v[1] = -127;
    unsigned n = v[0] > v[1] ? 7 : 5;
    for (unsigned i = 1; i < n; ++i) {
        int weighted = (int)(n - i) * v[0] + (int)i * v[1];
        v[i + 1] = (weighted + (weighted < 0 ? -(int)(n / 2) : (int)(n / 2))) / (int)n;
    }
    if (n == 5) {
        v[6] = -127;
        v[7] = 127;
    }
    uint64_t bits = 0;
    memcpy(&bits, src, 8);
    bits >>= 16;
    for (unsigned i = 0; i < 16; ++i) {
        dst[i * stride] = (uint8_t)v[bits & 7];
        bits >>= 3;
    }
}

static void bc1(const uint8_t *src, uint32_t *dst)
{
    bcdec_bc1(src, dst, 16);
    uint16_t endpoints[2];
    memcpy(endpoints, src, 4);
    if (endpoints[0] > endpoints[1]) return;

    /* The DXGI three-color mode truncates the 8-bit midpoint. */
    uint32_t indices;
    memcpy(&indices, src + 4, 4);
    uint8_t *colors = (uint8_t *)dst;
    uint32_t endpoint_pixels[2];
    uint64_t palette_block;
    memcpy(&palette_block, src, 8);
    palette_block = (palette_block & UINT64_C(0xffffffff)) | (UINT64_C(1) << 34);
    uint32_t palette[16];
    bcdec_bc1(&palette_block, palette, 16);
    endpoint_pixels[0] = palette[0];
    endpoint_pixels[1] = palette[1];
    const uint8_t *p = (const uint8_t *)endpoint_pixels;
    for (unsigned i = 0; i < 16; ++i, indices >>= 2) {
        if ((indices & 3) != 2) continue;
        for (unsigned c = 0; c < 3; ++c) colors[i * 4 + c] = (p[c] + p[4 + c]) / 2;
    }
}

void adv_decode_bc(unsigned kind, const uint8_t *src, unsigned src_pitch,
                   uint8_t *dst, unsigned width, unsigned height, unsigned pitch)
{
    const unsigned block_size = kind == 1 || kind == 4 || kind == 8 ? 8 : 16;
    const unsigned pixel_size = kind == 4 || kind == 8 ? 1 :
                                kind == 5 || kind == 9 ? 2 :
                                kind == 6 || kind == 10 ? 8 : 4;
    for (unsigned y = 0; y < height; y += 4) {
        for (unsigned x = 0; x < width; x += 4) {
            uint64_t block[2];
            uint32_t pixels[32];
            memcpy(block, src + y / 4 * src_pitch + x / 4 * block_size, block_size);
            switch (kind) {
            case 1: bc1((const uint8_t *)block, pixels); break;
            case 2: bcdec_bc2(block, pixels, 16); break;
            case 3: bcdec_bc3(block, pixels, 16); break;
            case 4: bcdec_bc4(block, pixels, 4); break;
            case 5: bcdec_bc5(block, pixels, 8); break;
            case 6: case 10: {
                uint16_t rgb[48];
                uint16_t *rgba = (uint16_t *)pixels;
                bcdec_bc6h_half(block, rgb, 12, kind == 10);
                for (unsigned i = 0; i < 16; ++i) {
                    memcpy(rgba + 4 * i, rgb + 3 * i, 6);
                    rgba[4 * i + 3] = 0x3c00;
                }
                break;
            }
            case 7: bcdec_bc7(block, pixels, 16); break;
            case 8: signed_channel((const uint8_t *)block, (uint8_t *)pixels, 1); break;
            case 9:
                signed_channel((const uint8_t *)block, (uint8_t *)pixels, 2);
                signed_channel((const uint8_t *)block + 8, (uint8_t *)pixels + 1, 2);
                break;
            }
            unsigned rows = height - y < 4 ? height - y : 4;
            unsigned count = width - x < 4 ? width - x : 4;
            for (unsigned row = 0; row < rows; ++row)
                memcpy(dst + (y + row) * pitch + x * pixel_size,
                       (const uint8_t *)pixels + row * 4 * pixel_size,
                       count * pixel_size);
        }
    }
}

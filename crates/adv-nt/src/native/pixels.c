/*
 * Copyright 2025 Connor McAdams for CodeWeavers
 *
 * This library is free software; you can redistribute it and/or
 * modify it under the terms of the GNU Lesser General Public
 * License as published by the Free Software Foundation; either
 * version 2.1 of the License, or (at your option) any later version.
 *
 * This library is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the GNU
 * Lesser General Public License for more details.
 *
 * You should have received a copy of the GNU Lesser General Public
 * License along with this library; if not, write to the Free Software
 * Foundation, Inc., 51 Franklin St, Fifth Floor, Boston, MA 02110-1301, USA
 *
 */

/* Pixel conversion subset of Wine 11.0 d3dx_helpers.c. Guest layouts and
 * memory admission are handled by pixels.rs; these kernels receive host spans. */
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <math.h>
#include <assert.h>
#include <setjmp.h>
typedef uint8_t BYTE;
typedef uint16_t WORD;
typedef uint32_t DWORD, UINT;
typedef int32_t BOOL, HRESULT, INT, LONG;
typedef struct { LONG left, top, right, bottom; } RECT;
typedef struct { BYTE peRed, peGreen, peBlue, peFlags; } PALETTEENTRY;
#define ZeroMemory(p,n) memset(p,0,n)
#define D3DX_PIXEL_FORMAT_R8G8_SNORM D3DX_PIXEL_FORMAT_U8V8_SNORM
#define TRUE 1
#define FALSE 0
#define S_OK 0
#define E_NOTIMPL ((HRESULT)0x80004001)
#define E_OUTOFMEMORY ((HRESULT)0x8007000e)
#define FAILED(hr) ((hr) < 0)
#define SUCCEEDED(hr) ((hr) >= 0)
#define min(a,b) ((a)<(b)?(a):(b))
#define max(a,b) ((a)>(b)?(a):(b))
#define ARRAY_SIZE(a) (sizeof(a)/sizeof((a)[0]))
#define TRACE(...) ((void)0)
#define WARN(...) ((void)0)
#define FIXME(...) ((void)0)
static void SetRect(RECT *r, LONG l, LONG t, LONG rr, LONG b) { *r = (RECT){l,t,rr,b}; }
static BOOL EqualRect(const RECT *a, const RECT *b) { return !memcmp(a,b,sizeof(*a)); }
static void OffsetRect(RECT *r, LONG x, LONG y) { r->left+=x; r->right+=x; r->top+=y; r->bottom+=y; }
static BOOL IntersectRect(RECT *out, const RECT *a, const RECT *b) {
    RECT r = {max(a->left,b->left),max(a->top,b->top),min(a->right,b->right),min(a->bottom,b->bottom)};
    BOOL valid = r.left < r.right && r.top < r.bottom;
    *out = valid ? r : (RECT){0,0,0,0}; return valid;
}
#define BCDEC_IMPLEMENTATION
#define BCDEC_STATIC
#include "bcdec.h"
#define STB_DXT_IMPLEMENTATION
#define STB_DXT_STATIC
#include "stb_dxt.h"
struct vec4
{
    float x, y, z, w;
};

enum range {
    RANGE_FULL  = 0,
    RANGE_UNORM = 1,
    RANGE_SNORM = 2,
};

struct d3dx_color
{
    struct vec4 value;
    enum range rgb_range;
    enum range a_range;
};

struct volume
{
    UINT width;
    UINT height;
    UINT depth;
};

static inline void set_volume_struct(struct volume *volume, uint32_t width, uint32_t height, uint32_t depth)
{
    volume->width = width;
    volume->height = height;
    volume->depth = depth;
}

/* These values act as indexes into the pixel_format_desc table. */
enum d3dx_pixel_format_id
{
    D3DX_PIXEL_FORMAT_B8G8R8_UNORM,
    D3DX_PIXEL_FORMAT_B8G8R8A8_UNORM,
    D3DX_PIXEL_FORMAT_B8G8R8X8_UNORM,
    D3DX_PIXEL_FORMAT_R8G8B8A8_UNORM,
    D3DX_PIXEL_FORMAT_R8G8B8X8_UNORM,
    D3DX_PIXEL_FORMAT_B5G6R5_UNORM,
    D3DX_PIXEL_FORMAT_B5G5R5X1_UNORM,
    D3DX_PIXEL_FORMAT_B5G5R5A1_UNORM,
    D3DX_PIXEL_FORMAT_B2G3R3_UNORM,
    D3DX_PIXEL_FORMAT_B2G3R3A8_UNORM,
    D3DX_PIXEL_FORMAT_B4G4R4A4_UNORM,
    D3DX_PIXEL_FORMAT_B4G4R4X4_UNORM,
    D3DX_PIXEL_FORMAT_B10G10R10A2_UNORM,
    D3DX_PIXEL_FORMAT_R10G10B10A2_UNORM,
    D3DX_PIXEL_FORMAT_R16G16B16_UNORM,
    D3DX_PIXEL_FORMAT_R16G16B16A16_UNORM,
    D3DX_PIXEL_FORMAT_R8_UNORM,
    D3DX_PIXEL_FORMAT_R8_SNORM,
    D3DX_PIXEL_FORMAT_R8G8_UNORM,
    D3DX_PIXEL_FORMAT_R16_UNORM,
    D3DX_PIXEL_FORMAT_R16G16_UNORM,
    D3DX_PIXEL_FORMAT_A8_UNORM,
    D3DX_PIXEL_FORMAT_L8A8_UNORM,
    D3DX_PIXEL_FORMAT_L4A4_UNORM,
    D3DX_PIXEL_FORMAT_L8_UNORM,
    D3DX_PIXEL_FORMAT_L16_UNORM,
    D3DX_PIXEL_FORMAT_DXT1_UNORM,
    D3DX_PIXEL_FORMAT_DXT2_UNORM,
    D3DX_PIXEL_FORMAT_DXT3_UNORM,
    D3DX_PIXEL_FORMAT_DXT4_UNORM,
    D3DX_PIXEL_FORMAT_DXT5_UNORM,
    D3DX_PIXEL_FORMAT_BC4_UNORM,
    D3DX_PIXEL_FORMAT_BC4_SNORM,
    D3DX_PIXEL_FORMAT_BC5_UNORM,
    D3DX_PIXEL_FORMAT_BC5_SNORM,
    D3DX_PIXEL_FORMAT_R16_FLOAT,
    D3DX_PIXEL_FORMAT_R16G16_FLOAT,
    D3DX_PIXEL_FORMAT_R16G16B16A16_FLOAT,
    D3DX_PIXEL_FORMAT_R32_FLOAT,
    D3DX_PIXEL_FORMAT_R32G32_FLOAT,
    D3DX_PIXEL_FORMAT_R32G32B32_FLOAT,
    D3DX_PIXEL_FORMAT_R32G32B32A32_FLOAT,
    D3DX_PIXEL_FORMAT_P1_UINT,
    D3DX_PIXEL_FORMAT_P2_UINT,
    D3DX_PIXEL_FORMAT_P4_UINT,
    D3DX_PIXEL_FORMAT_P8_UINT,
    D3DX_PIXEL_FORMAT_P8_UINT_A8_UNORM,
    D3DX_PIXEL_FORMAT_U8V8W8Q8_SNORM,
    D3DX_PIXEL_FORMAT_U16V16W16Q16_SNORM,
    D3DX_PIXEL_FORMAT_U8V8_SNORM,
    D3DX_PIXEL_FORMAT_U8V8_SNORM_Cx,
    D3DX_PIXEL_FORMAT_U16V16_SNORM,
    D3DX_PIXEL_FORMAT_U8V8_SNORM_L8X8_UNORM,
    D3DX_PIXEL_FORMAT_U10V10W10_SNORM_A2_UNORM,
    D3DX_PIXEL_FORMAT_R8G8_B8G8_UNORM,
    D3DX_PIXEL_FORMAT_G8R8_G8B8_UNORM,
    D3DX_PIXEL_FORMAT_UYVY,
    D3DX_PIXEL_FORMAT_YUY2,
    D3DX_PIXEL_FORMAT_COUNT,
};


/* for internal use */
enum component_type
{
    CTYPE_EMPTY,
    CTYPE_UNORM,
    CTYPE_SNORM,
    CTYPE_FLOAT,
    CTYPE_LUMA,
    CTYPE_INDEX,
    CTYPE_SHILO, /* Signed HILO. */
};

enum format_flag
{
    FMT_FLAG_DXT  = 0x01,
    FMT_FLAG_PACKED = 0x02,
    /* Internal only format, has no exact D3DFORMAT equivalent. */
    FMT_FLAG_INTERNAL = 0x04,
    /*
     * For formats that only have a DXGI_FORMAT mapping, no D3DFORMAT
     * equivalent.
     */
    FMT_FLAG_DXGI     = 0x08,
};

struct pixel_format_desc {
    enum d3dx_pixel_format_id format;
    BYTE bits[4];
    BYTE shift[4];
    UINT bytes_per_pixel;
    UINT block_width;
    UINT block_height;
    UINT block_byte_count;
    enum component_type a_type;
    enum component_type rgb_type;
    uint32_t flags;
};

struct d3dx_pixels
{
    const void *data;
    uint32_t row_pitch;
    uint32_t slice_pitch;
    const PALETTEENTRY *palette;

    struct volume size;
    RECT unaligned_rect;
};


static inline void set_d3dx_pixels(struct d3dx_pixels *pixels, const void *data, uint32_t row_pitch,
        uint32_t slice_pitch, const PALETTEENTRY *palette, uint32_t width, uint32_t height, uint32_t depth,
        const RECT *unaligned_rect)
{
    pixels->data = data;
    pixels->row_pitch = row_pitch;
    pixels->slice_pitch = slice_pitch;
    pixels->palette = palette;
    set_volume_struct(&pixels->size, width, height, depth);
    pixels->unaligned_rect = *unaligned_rect;
}

static inline BOOL is_unknown_format(const struct pixel_format_desc *format)
{
    return (format->format == D3DX_PIXEL_FORMAT_COUNT);
}

static inline BOOL is_index_format(const struct pixel_format_desc *format)
{
    return (format->a_type == CTYPE_INDEX || format->rgb_type == CTYPE_INDEX);
}

static inline BOOL is_compressed_format(const struct pixel_format_desc *format)
{
    return !!(format->flags & FMT_FLAG_DXT);
}

static inline BOOL is_packed_format(const struct pixel_format_desc *format)
{
    return !!(format->flags & FMT_FLAG_PACKED);
}

static inline BOOL is_signed_hilo_format(const struct pixel_format_desc *format)
{
    return format->rgb_type == CTYPE_SHILO;
}

static inline BOOL format_types_match(const struct pixel_format_desc *src, const struct pixel_format_desc *dst)
{
    if ((src->a_type && dst->a_type) && (src->a_type != dst->a_type))
        return FALSE;

    if ((src->rgb_type && dst->rgb_type) && (src->rgb_type != dst->rgb_type))
        return FALSE;

    if (src->flags != dst->flags)
        return FALSE;

    return (src->rgb_type == dst->rgb_type || src->a_type == dst->a_type);
}

static inline BOOL is_internal_format(const struct pixel_format_desc *format)
{
    return !!(format->flags & FMT_FLAG_INTERNAL);
}

static inline BOOL is_dxgi_format(const struct pixel_format_desc *format)
{
    return !!(format->flags & FMT_FLAG_DXGI);
}

static inline BOOL is_conversion_from_supported(const struct pixel_format_desc *format)
{
    return !is_packed_format(format) && !is_unknown_format(format);
}

static inline BOOL is_conversion_to_supported(const struct pixel_format_desc *format)
{
    return !is_index_format(format) && !is_packed_format(format) && !is_signed_hilo_format(format)
        && !is_unknown_format(format);
}


/* Returns TRUE if num is a power of 2, FALSE if not, or if 0 */
static inline BOOL is_pow2(UINT num)
{
    return !(num & (num - 1));
}

HRESULT d3dx_validate_filter(uint32_t filter);


#define D3DX_FILTER_NONE             0x00000001
#define D3DX_FILTER_POINT            0x00000002
#define D3DX_FILTER_LINEAR           0x00000003
#define D3DX_FILTER_TRIANGLE         0x00000004
#define D3DX_FILTER_BOX              0x00000005
#define D3DX_FILTER_MIRROR_U         0x00010000
#define D3DX_FILTER_MIRROR_V         0x00020000
#define D3DX_FILTER_MIRROR_W         0x00040000
#define D3DX_FILTER_MIRROR           0x00070000
#define D3DX_FILTER_DITHER           0x00080000
#define D3DX_FILTER_DITHER_DIFFUSION 0x00100000
#define D3DX_FILTER_SRGB_IN          0x00200000
#define D3DX_FILTER_SRGB_OUT         0x00400000
#define D3DX_FILTER_SRGB             0x00600000

static const struct pixel_format_desc formats[] =
{
    /* format                                    bpc               shifts             bpp blocks   alpha type   rgb type     flags */
    {D3DX_PIXEL_FORMAT_B8G8R8_UNORM,             { 0,  8,  8,  8}, { 0, 16,  8,  0},  3, 1, 1,  3, CTYPE_EMPTY, CTYPE_UNORM, 0           },
    {D3DX_PIXEL_FORMAT_B8G8R8A8_UNORM,           { 8,  8,  8,  8}, {24, 16,  8,  0},  4, 1, 1,  4, CTYPE_UNORM, CTYPE_UNORM, 0           },
    {D3DX_PIXEL_FORMAT_B8G8R8X8_UNORM,           { 0,  8,  8,  8}, { 0, 16,  8,  0},  4, 1, 1,  4, CTYPE_EMPTY, CTYPE_UNORM, 0           },
    {D3DX_PIXEL_FORMAT_R8G8B8A8_UNORM,           { 8,  8,  8,  8}, {24,  0,  8, 16},  4, 1, 1,  4, CTYPE_UNORM, CTYPE_UNORM, 0           },
    {D3DX_PIXEL_FORMAT_R8G8B8X8_UNORM,           { 0,  8,  8,  8}, { 0,  0,  8, 16},  4, 1, 1,  4, CTYPE_EMPTY, CTYPE_UNORM, 0           },
    {D3DX_PIXEL_FORMAT_B5G6R5_UNORM,             { 0,  5,  6,  5}, { 0, 11,  5,  0},  2, 1, 1,  2, CTYPE_EMPTY, CTYPE_UNORM, 0           },
    {D3DX_PIXEL_FORMAT_B5G5R5X1_UNORM,           { 0,  5,  5,  5}, { 0, 10,  5,  0},  2, 1, 1,  2, CTYPE_EMPTY, CTYPE_UNORM, 0           },
    {D3DX_PIXEL_FORMAT_B5G5R5A1_UNORM,           { 1,  5,  5,  5}, {15, 10,  5,  0},  2, 1, 1,  2, CTYPE_UNORM, CTYPE_UNORM, 0           },
    {D3DX_PIXEL_FORMAT_B2G3R3_UNORM,             { 0,  3,  3,  2}, { 0,  5,  2,  0},  1, 1, 1,  1, CTYPE_EMPTY, CTYPE_UNORM, 0           },
    {D3DX_PIXEL_FORMAT_B2G3R3A8_UNORM,           { 8,  3,  3,  2}, { 8,  5,  2,  0},  2, 1, 1,  2, CTYPE_UNORM, CTYPE_UNORM, 0           },
    {D3DX_PIXEL_FORMAT_B4G4R4A4_UNORM,           { 4,  4,  4,  4}, {12,  8,  4,  0},  2, 1, 1,  2, CTYPE_UNORM, CTYPE_UNORM, 0           },
    {D3DX_PIXEL_FORMAT_B4G4R4X4_UNORM,           { 0,  4,  4,  4}, { 0,  8,  4,  0},  2, 1, 1,  2, CTYPE_EMPTY, CTYPE_UNORM, 0           },
    {D3DX_PIXEL_FORMAT_B10G10R10A2_UNORM,        { 2, 10, 10, 10}, {30, 20, 10,  0},  4, 1, 1,  4, CTYPE_UNORM, CTYPE_UNORM, 0           },
    {D3DX_PIXEL_FORMAT_R10G10B10A2_UNORM,        { 2, 10, 10, 10}, {30,  0, 10, 20},  4, 1, 1,  4, CTYPE_UNORM, CTYPE_UNORM, 0           },
    {D3DX_PIXEL_FORMAT_R16G16B16_UNORM,          { 0, 16, 16, 16}, { 0,  0, 16, 32},  6, 1, 1,  6, CTYPE_EMPTY, CTYPE_UNORM, FMT_FLAG_INTERNAL},
    {D3DX_PIXEL_FORMAT_R16G16B16A16_UNORM,       {16, 16, 16, 16}, {48,  0, 16, 32},  8, 1, 1,  8, CTYPE_UNORM, CTYPE_UNORM, 0           },
    {D3DX_PIXEL_FORMAT_R8_UNORM,                 { 0,  8,  0,  0}, { 0,  0,  0,  0},  1, 1, 1,  1, CTYPE_EMPTY, CTYPE_UNORM, FMT_FLAG_DXGI},
    {D3DX_PIXEL_FORMAT_R8_SNORM,                 { 0,  8,  0,  0}, { 0,  0,  0,  0},  1, 1, 1,  1, CTYPE_EMPTY, CTYPE_SNORM, FMT_FLAG_DXGI},
    {D3DX_PIXEL_FORMAT_R8G8_UNORM,               { 0,  8,  8,  0}, { 0,  0,  8,  0},  2, 1, 1,  2, CTYPE_EMPTY, CTYPE_UNORM, FMT_FLAG_DXGI},
    {D3DX_PIXEL_FORMAT_R16_UNORM,                { 0, 16,  0,  0}, { 0,  0,  0,  0},  2, 1, 1,  2, CTYPE_EMPTY, CTYPE_UNORM, FMT_FLAG_DXGI},
    {D3DX_PIXEL_FORMAT_R16G16_UNORM,             { 0, 16, 16,  0}, { 0,  0, 16,  0},  4, 1, 1,  4, CTYPE_EMPTY, CTYPE_UNORM, 0           },
    {D3DX_PIXEL_FORMAT_A8_UNORM,                 { 8,  0,  0,  0}, { 0,  0,  0,  0},  1, 1, 1,  1, CTYPE_UNORM, CTYPE_EMPTY, 0           },
    {D3DX_PIXEL_FORMAT_L8A8_UNORM,               { 8,  8,  0,  0}, { 8,  0,  0,  0},  2, 1, 1,  2, CTYPE_UNORM, CTYPE_LUMA,  0           },
    {D3DX_PIXEL_FORMAT_L4A4_UNORM,               { 4,  4,  0,  0}, { 4,  0,  0,  0},  1, 1, 1,  1, CTYPE_UNORM, CTYPE_LUMA,  0           },
    {D3DX_PIXEL_FORMAT_L8_UNORM,                 { 0,  8,  0,  0}, { 0,  0,  0,  0},  1, 1, 1,  1, CTYPE_EMPTY, CTYPE_LUMA,  0           },
    {D3DX_PIXEL_FORMAT_L16_UNORM,                { 0, 16,  0,  0}, { 0,  0,  0,  0},  2, 1, 1,  2, CTYPE_EMPTY, CTYPE_LUMA,  0           },
    {D3DX_PIXEL_FORMAT_DXT1_UNORM,               { 0,  0,  0,  0}, { 0,  0,  0,  0},  1, 4, 4,  8, CTYPE_UNORM, CTYPE_UNORM, FMT_FLAG_DXT},
    {D3DX_PIXEL_FORMAT_DXT2_UNORM,               { 0,  0,  0,  0}, { 0,  0,  0,  0},  1, 4, 4, 16, CTYPE_UNORM, CTYPE_UNORM, FMT_FLAG_DXT},
    {D3DX_PIXEL_FORMAT_DXT3_UNORM,               { 0,  0,  0,  0}, { 0,  0,  0,  0},  1, 4, 4, 16, CTYPE_UNORM, CTYPE_UNORM, FMT_FLAG_DXT},
    {D3DX_PIXEL_FORMAT_DXT4_UNORM,               { 0,  0,  0,  0}, { 0,  0,  0,  0},  1, 4, 4, 16, CTYPE_UNORM, CTYPE_UNORM, FMT_FLAG_DXT},
    {D3DX_PIXEL_FORMAT_DXT5_UNORM,               { 0,  0,  0,  0}, { 0,  0,  0,  0},  1, 4, 4, 16, CTYPE_UNORM, CTYPE_UNORM, FMT_FLAG_DXT},
    {D3DX_PIXEL_FORMAT_BC4_UNORM,                { 0,  0,  0,  0}, { 0,  0,  0,  0},  1, 4, 4,  8, CTYPE_EMPTY, CTYPE_UNORM, FMT_FLAG_DXT|FMT_FLAG_DXGI},
    {D3DX_PIXEL_FORMAT_BC4_SNORM,                { 0,  0,  0,  0}, { 0,  0,  0,  0},  1, 4, 4,  8, CTYPE_EMPTY, CTYPE_SNORM, FMT_FLAG_DXT|FMT_FLAG_DXGI},
    {D3DX_PIXEL_FORMAT_BC5_UNORM,                { 0,  0,  0,  0}, { 0,  0,  0,  0},  1, 4, 4, 16, CTYPE_EMPTY, CTYPE_UNORM, FMT_FLAG_DXT|FMT_FLAG_DXGI},
    {D3DX_PIXEL_FORMAT_BC5_SNORM,                { 0,  0,  0,  0}, { 0,  0,  0,  0},  1, 4, 4, 16, CTYPE_EMPTY, CTYPE_SNORM, FMT_FLAG_DXT|FMT_FLAG_DXGI},
    {D3DX_PIXEL_FORMAT_R16_FLOAT,                { 0, 16,  0,  0}, { 0,  0,  0,  0},  2, 1, 1,  2, CTYPE_EMPTY, CTYPE_FLOAT, 0           },
    {D3DX_PIXEL_FORMAT_R16G16_FLOAT,             { 0, 16, 16,  0}, { 0,  0, 16,  0},  4, 1, 1,  4, CTYPE_EMPTY, CTYPE_FLOAT, 0           },
    {D3DX_PIXEL_FORMAT_R16G16B16A16_FLOAT,       {16, 16, 16, 16}, {48,  0, 16, 32},  8, 1, 1,  8, CTYPE_FLOAT, CTYPE_FLOAT, 0           },
    {D3DX_PIXEL_FORMAT_R32_FLOAT,                { 0, 32,  0,  0}, { 0,  0,  0,  0},  4, 1, 1,  4, CTYPE_EMPTY, CTYPE_FLOAT, 0           },
    {D3DX_PIXEL_FORMAT_R32G32_FLOAT,             { 0, 32, 32,  0}, { 0,  0, 32,  0},  8, 1, 1,  8, CTYPE_EMPTY, CTYPE_FLOAT, 0           },
    {D3DX_PIXEL_FORMAT_R32G32B32_FLOAT,          { 0, 32, 32, 32}, { 0,  0, 32, 64}, 12, 1, 1, 12, CTYPE_EMPTY, CTYPE_FLOAT, FMT_FLAG_DXGI},
    {D3DX_PIXEL_FORMAT_R32G32B32A32_FLOAT,       {32, 32, 32, 32}, {96,  0, 32, 64}, 16, 1, 1, 16, CTYPE_FLOAT, CTYPE_FLOAT, 0           },
    {D3DX_PIXEL_FORMAT_P1_UINT,                  { 8,  8,  8,  8}, { 0,  0,  0,  0},  1, 8, 1,  1, CTYPE_INDEX, CTYPE_INDEX, FMT_FLAG_INTERNAL},
    {D3DX_PIXEL_FORMAT_P2_UINT,                  { 8,  8,  8,  8}, { 0,  0,  0,  0},  1, 4, 1,  1, CTYPE_INDEX, CTYPE_INDEX, FMT_FLAG_INTERNAL},
    {D3DX_PIXEL_FORMAT_P4_UINT,                  { 8,  8,  8,  8}, { 0,  0,  0,  0},  1, 2, 1,  1, CTYPE_INDEX, CTYPE_INDEX, FMT_FLAG_INTERNAL},
    {D3DX_PIXEL_FORMAT_P8_UINT,                  { 8,  8,  8,  8}, { 0,  0,  0,  0},  1, 1, 1,  1, CTYPE_INDEX, CTYPE_INDEX, 0           },
    {D3DX_PIXEL_FORMAT_P8_UINT_A8_UNORM,         { 8,  8,  8,  8}, { 8,  0,  0,  0},  2, 1, 1,  2, CTYPE_UNORM, CTYPE_INDEX, 0           },
    {D3DX_PIXEL_FORMAT_U8V8W8Q8_SNORM,           { 8,  8,  8,  8}, {24,  0,  8, 16},  4, 1, 1,  4, CTYPE_SNORM, CTYPE_SNORM, 0           },
    {D3DX_PIXEL_FORMAT_U16V16W16Q16_SNORM,       {16, 16, 16, 16}, {48,  0, 16, 32},  8, 1, 1,  8, CTYPE_SNORM, CTYPE_SNORM, 0           },
    {D3DX_PIXEL_FORMAT_U8V8_SNORM,               { 0,  8,  8,  0}, { 0,  0,  8,  0},  2, 1, 1,  2, CTYPE_EMPTY, CTYPE_SNORM, 0           },
    {D3DX_PIXEL_FORMAT_U8V8_SNORM_Cx,            { 0,  8,  8,  0}, { 0,  0,  8,  0},  2, 1, 1,  2, CTYPE_EMPTY, CTYPE_SHILO, 0           },
    {D3DX_PIXEL_FORMAT_U16V16_SNORM,             { 0, 16, 16,  0}, { 0,  0, 16,  0},  4, 1, 1,  4, CTYPE_EMPTY, CTYPE_SNORM, 0           },
    {D3DX_PIXEL_FORMAT_U8V8_SNORM_L8X8_UNORM,    { 8,  8,  8,  0}, {16,  0,  8,  0},  4, 1, 1,  4, CTYPE_UNORM, CTYPE_SNORM, 0           },
    {D3DX_PIXEL_FORMAT_U10V10W10_SNORM_A2_UNORM, { 2, 10, 10, 10}, {30,  0, 10, 20},  4, 1, 1,  4, CTYPE_UNORM, CTYPE_SNORM, 0           },
    {D3DX_PIXEL_FORMAT_R8G8_B8G8_UNORM,          { 0,  0,  0,  0}, { 0,  0,  0,  0},  1, 2, 1,  4, CTYPE_EMPTY, CTYPE_UNORM, FMT_FLAG_PACKED},
    {D3DX_PIXEL_FORMAT_G8R8_G8B8_UNORM,          { 0,  0,  0,  0}, { 0,  0,  0,  0},  1, 2, 1,  4, CTYPE_EMPTY, CTYPE_UNORM, FMT_FLAG_PACKED},
    {D3DX_PIXEL_FORMAT_UYVY,                     { 0,  0,  0,  0}, { 0,  0,  0,  0},  1, 2, 1,  4, CTYPE_EMPTY, CTYPE_UNORM, FMT_FLAG_PACKED},
    {D3DX_PIXEL_FORMAT_YUY2,                     { 0,  0,  0,  0}, { 0,  0,  0,  0},  1, 2, 1,  4, CTYPE_EMPTY, CTYPE_UNORM, FMT_FLAG_PACKED},
    /* marks last element */
    {D3DX_PIXEL_FORMAT_COUNT,                    { 0,  0,  0,  0}, { 0,  0,  0,  0},  0, 1, 1,  0, CTYPE_EMPTY, CTYPE_EMPTY, 0           },
};

const struct pixel_format_desc *adv_pixel_get_d3dx_pixel_format_info(enum d3dx_pixel_format_id format)
{
    return &formats[min(format, D3DX_PIXEL_FORMAT_COUNT)];
}

static BOOL d3dx_size_is_half(struct volume *src, struct volume *dst)
{
    return (max(src->width / 2, 1) == dst->width) && (max(src->height / 2, 1) == dst->height)
        && (max(src->depth / 2, 1) == dst->depth);
}

static BOOL d3dx_size_is_pow2(struct volume *size)
{
    return is_pow2(size->width) && is_pow2(size->height) && is_pow2(size->depth);
}

HRESULT adv_pixel_d3dx_load_pixels_from_pixels(struct d3dx_pixels *, const struct pixel_format_desc *, struct d3dx_pixels *, const struct pixel_format_desc *, uint32_t, uint32_t);
/* Wine's classification helper inspects the encoding without arithmetic. */
static int pixel_isnan(float value) { union { float value; uint32_t bits; } v={.value=value}; return (v.bits&0x7fffffff)>0x7f800000; }
static int pixel_isinf(float value) { union { float value; uint32_t bits; } v={.value=value}; return (v.bits&0x7fffffff)==0x7f800000; }
#undef isnan
#undef isinf
#define isnan pixel_isnan
#define isinf pixel_isinf

static _Thread_local uint32_t pixel_controls = 0x037f1f80;
static _Thread_local uint32_t pixel_flags;
static _Thread_local jmp_buf pixel_jump;
static _Thread_local void *pixel_allocations[2];
static _Thread_local unsigned pixel_trap;
static uint32_t pixel_status(void)
{
#if defined(__aarch64__)
    uint64_t status; __asm__ volatile("mrs %0, fpsr" : "=r"(status));
    return (status & 1) | ((status & 0x80) >> 6) | ((status & 0x1e) << 1);
#elif defined(__x86_64__)
    uint32_t status; __asm__ volatile("stmxcsr %0" : "=m"(status)); return status & 0x3f;
#endif
}
static uint32_t pixel_save_status(void)
{
    uint32_t status;
#if defined(__aarch64__)
    uint64_t value; __asm__ volatile("mrs %0, fpsr" : "=r"(value)); status = value;
    __asm__ volatile("msr fpsr, xzr");
#elif defined(__x86_64__)
    __asm__ volatile("stmxcsr %0" : "=m"(status)); uint32_t cleared = status & ~0x3f;
    __asm__ volatile("ldmxcsr %0" : : "m"(cleared));
#endif
    return status;
}
static void pixel_restore_status(uint32_t status)
{
#if defined(__aarch64__)
    __asm__ volatile("msr fpsr, %0" : : "r"((uint64_t)status));
#elif defined(__x86_64__)
    __asm__ volatile("ldmxcsr %0" : : "m"(status));
#endif
}
static void pixel_raise(unsigned vector) { pixel_trap = vector; longjmp(pixel_jump, 1); }
static void pixel_check(void)
{
    if ((pixel_controls & 0x1f80) != 0x1f80 &&
        ((pixel_status() | pixel_flags) & ~(pixel_controls >> 7) & 0x3f)) pixel_raise(19);
}
static void *pixel_malloc(size_t size)
{
    void *value = malloc(size);
    for (unsigned i=0; i<2; ++i) if (!pixel_allocations[i]) { pixel_allocations[i]=value; break; }
    return value;
}
static void pixel_free(void *value)
{
    for (unsigned i=0; i<2; ++i) if (pixel_allocations[i]==value) pixel_allocations[i]=NULL;
    free(value);
}
#define malloc pixel_malloc
#define free pixel_free
static float pixel_round(float value)
{
    uint32_t status = pixel_save_status();
    float result;
#if defined(__aarch64__)
    uint64_t saved; __asm__ volatile("mrs %0, fpcr" : "=r"(saved));
    const unsigned modes[4]={0,2,1,3};
    uint64_t control = (saved & ~(3ull<<22)) | ((uint64_t)modes[(pixel_controls>>26)&3]<<22);
    __asm__ volatile("msr fpcr, %0" : : "r"(control)); result = rintf(value);
    __asm__ volatile("msr fpcr, %0" : : "r"(saved));
#elif defined(__x86_64__)
    uint32_t control=(status & ~0x6000) | ((pixel_controls>>13)&0x6000);
    __asm__ volatile("ldmxcsr %0" : : "m"(control)); result = rintf(value);
#endif
    pixel_restore_status(status);
    if (!(pixel_controls & (1u<<21)) && result != value) pixel_raise(16);
    return result;
}
extern uint64_t adv_pixel_x87_sqrt(uint32_t, uint32_t);
static float pixel_sqrt(float value)
{
    union { uint32_t bits; float value; } v={ .value=value };
    uint64_t result=adv_pixel_x87_sqrt(v.bits,pixel_controls);
    if (((result>>48) & ~(pixel_controls>>16) & 0x3f)) pixel_raise(16);
    pixel_flags |= (result>>32)&0x3f; pixel_check();
    v.bits=result; return v.value;
}

void adv_pixel_set_controls(uint32_t controls) { pixel_controls = controls; }
static float pixel_input(float value)
{
    union { uint32_t bits; float value; } v = { .value = value };
    if ((pixel_controls & 0x40) && (v.bits & 0x7fffffff) && !(v.bits & 0x7f800000))
        { v.bits &= 0x80000000; }
    else if ((v.bits & 0x7fffffff) && !(v.bits & 0x7f800000)) pixel_flags |= 2;
    return v.value;
}
static float pixel_output(float value)
{
    union { uint32_t bits; float value; } v = { .value = value };
    if ((pixel_controls & 0x8000) && (v.bits & 0x7fffffff) && !(v.bits & 0x7f800000))
        { v.bits &= 0x80000000; pixel_flags |= 0x30; }
    return v.value;
}
static float pixel_add(float, float);
static float pixel_mul(float a, float b) { float result=pixel_output(pixel_input(a) * pixel_input(b)); pixel_check(); return result; }
static float pixel_div(float a, float b) { float result=pixel_output(pixel_input(a) / pixel_input(b)); pixel_check(); return result; }

unsigned short adv_pixel_float_32_to_16(const float in)
{
    int exp = 0, origexp;
    float tmp = fabsf(in);
    int sign = (copysignf(1, in) < 0);
    unsigned int mantissa;
    unsigned short ret;

    /* Deal with special numbers */
    if (isinf(in)) return (sign ? 0xffff : 0x7fff);
    if (isnan(in)) return (sign ? 0xffff : 0x7fff);
    if (pixel_input(in) == 0.0f) return (sign ? 0x8000 : 0x0000);

    if (tmp < (float)(1u << 10))
    {
        do
        {
            tmp = pixel_mul(tmp,2.0f);
            exp--;
        } while (tmp < (float)(1u << 10));
    }
    else if (tmp >= (float)(1u << 11))
    {
        do
        {
            tmp = pixel_div(tmp,2.0f);
            exp++;
        } while (tmp >= (float)(1u << 11));
    }

    exp += 10;  /* Normalize the mantissa */
    exp += 15;  /* Exponent is encoded with excess 15 */

    origexp = exp;

    mantissa = (unsigned int) tmp;
    if ((tmp - mantissa == 0.5f && mantissa % 2 == 1) || /* round half to even */
        (tmp - mantissa > 0.5f))
    {
        mantissa++; /* round to nearest, away from zero */
    }
    if (mantissa == 2048)
    {
        mantissa = 1024;
        exp++;
    }

    if (exp > 31)
    {
        /* too big */
        ret = 0x7fff; /* INF */
    }
    else if (exp <= 0)
    {
        unsigned int rounding = 0;

        /* Denormalized half float */

        /* return 0x0000 (=0.0) for numbers too small to represent in half floats */
        if (exp < -11)
            return (sign ? 0x8000 : 0x0000);

        exp = origexp;

        /* the 13 extra bits from single precision are used for rounding */
        mantissa = (unsigned int)(tmp * (1u << 13));
        mantissa >>= 1 - exp; /* denormalize */

        mantissa -= ~(mantissa >> 13) & 1; /* round half to even */
        /* remove 13 least significant bits to get half float precision */
        mantissa >>= 12;
        rounding = mantissa & 1;
        mantissa >>= 1;

        ret = mantissa + rounding;
    }
    else
    {
        ret = (exp << 10) | (mantissa & 0x3ff);
    }

    ret |= ((sign ? 1 : 0) << 15); /* Add the sign */
    return ret;
}

/* Native d3dx9's D3DXFloat16to32Array lacks support for NaN and Inf. Specifically, e = 16 is treated as a
 * regular number - e.g., 0x7fff is converted to 131008.0 and 0xffff to -131008.0. */
float adv_pixel_float_16_to_32(const unsigned short in)
{
    const unsigned short s = (in & 0x8000);
    const unsigned short e = (in & 0x7C00) >> 10;
    const unsigned short m = in & 0x3FF;
    const float sgn = (s ? -1.0f : 1.0f);

    if (e == 0)
    {
        if (m == 0) return sgn * 0.0f; /* +0.0 or -0.0 */
        else return sgn * powf(2, -14.0f) * (m / 1024.0f);
    }
    else
    {
        return sgn * powf(2, e - 15.0f) * (1.0f + (m / 1024.0f));
    }
}

struct argb_conversion_info
{
    const struct pixel_format_desc *srcformat;
    const struct pixel_format_desc *destformat;
    DWORD srcshift[4], destshift[4];
    DWORD srcmask[4], destmask[4];
    BOOL process_channel[4];
    DWORD channelmask;
};

static void init_argb_conversion_info(const struct pixel_format_desc *srcformat, const struct pixel_format_desc *destformat, struct argb_conversion_info *info)
{
    UINT i;
    ZeroMemory(info->process_channel, 4 * sizeof(BOOL));
    info->channelmask = 0;

    info->srcformat  =  srcformat;
    info->destformat = destformat;

    for(i = 0;i < 4;i++) {
        /* srcshift is used to extract the _relevant_ components */
        info->srcshift[i]  =  srcformat->shift[i] + max( srcformat->bits[i] - destformat->bits[i], 0);

        /* destshift is used to move the components to the correct position */
        info->destshift[i] = destformat->shift[i] + max(destformat->bits[i] -  srcformat->bits[i], 0);

        info->srcmask[i]  = ((1 <<  srcformat->bits[i]) - 1) <<  srcformat->shift[i];
        info->destmask[i] = ((1 << destformat->bits[i]) - 1) << destformat->shift[i];

        /* channelmask specifies bits which aren't used in the source format but in the destination one */
        if(destformat->bits[i]) {
            if(srcformat->bits[i]) info->process_channel[i] = TRUE;
            else info->channelmask |= info->destmask[i];
        }
    }
}

/************************************************************
 * get_relevant_argb_components
 *
 * Extracts the relevant components from the source color and
 * drops the less significant bits if they aren't used by the destination format.
 */
static void get_relevant_argb_components(const struct argb_conversion_info *info, const BYTE *col, DWORD *out)
{
    unsigned int i, j;
    unsigned int component, mask;

    for (i = 0; i < 4; ++i)
    {
        if (!info->process_channel[i])
            continue;

        component = 0;
        mask = info->srcmask[i];
        for (j = 0; j < 4 && mask; ++j)
        {
            if (info->srcshift[i] < j * 8)
                component |= (col[j] & mask) << (j * 8 - info->srcshift[i]);
            else
                component |= (col[j] & mask) >> (info->srcshift[i] - j * 8);
            mask >>= 8;
        }
        out[i] = component;
    }
}

static float d3dx_clamp(float value, float min_value, float max_value)
{
    if (isnan(value))
        return max_value;
    return value < min_value ? min_value : value > max_value ? max_value : value;
}

/************************************************************
 * make_argb_color
 *
 * Recombines the output of get_relevant_argb_components and converts
 * it to the destination format.
 */
static DWORD make_argb_color(const struct argb_conversion_info *info, const DWORD *in)
{
    UINT i;
    DWORD val = 0;

    for(i = 0;i < 4;i++) {
        if(info->process_channel[i]) {
            /* necessary to make sure that e.g. an X4R4G4B4 white maps to an R8G8B8 white instead of 0xf0f0f0 */
            signed int shift;
            for(shift = info->destshift[i]; shift > info->destformat->shift[i]; shift -= info->srcformat->bits[i]) val |= in[i] << shift;
            val |= (in[i] >> (info->destformat->shift[i] - shift)) << info->destformat->shift[i];
        }
    }
    val |= info->channelmask;   /* new channels are set to their maximal value */
    return val;
}

static enum range get_range_for_component_type(enum component_type type)
{
    switch (type)
    {
        case CTYPE_SNORM:
        case CTYPE_SHILO:
            return RANGE_SNORM;

        case CTYPE_LUMA:
        case CTYPE_INDEX:
        case CTYPE_UNORM:
            return RANGE_UNORM;

        case CTYPE_EMPTY:
        case CTYPE_FLOAT:
            return RANGE_FULL;

        default:
            assert(0);
            return RANGE_FULL;
    }
}

/* It doesn't work for components bigger than 32 bits (or somewhat smaller but unaligned). */
void adv_pixel_format_to_d3dx_color(const struct pixel_format_desc *format, const BYTE *src, const PALETTEENTRY *palette,
        struct d3dx_color *dst)
{
    DWORD mask, tmp;
    unsigned int c;

    dst->rgb_range = get_range_for_component_type(format->rgb_type);
    dst->a_range = get_range_for_component_type(format->a_type);
    for (c = 0; c < 4; ++c)
    {
        const enum component_type dst_ctype = !c ? format->a_type : format->rgb_type;
        static const unsigned int component_offsets[4] = {3, 0, 1, 2};
        float *dst_component = &dst->value.x + component_offsets[c];

        if (format->bits[c])
        {
            mask = ~0u >> (32 - format->bits[c]);

            memmove(&tmp, src + format->shift[c] / 8,
                    min(sizeof(DWORD), (format->shift[c] % 8 + format->bits[c] + 7) / 8));
            tmp = (tmp >> (format->shift[c] % 8)) & mask;

            switch (dst_ctype)
            {
            case CTYPE_FLOAT:
                if (format->bits[c] == 16)
                    *dst_component = adv_pixel_float_16_to_32(tmp);
                else
                    *dst_component = *(float *)&tmp;
                break;

            case CTYPE_INDEX:
                *dst_component = (&palette[tmp].peRed)[component_offsets[c]] / 255.0f;
                break;

            case CTYPE_LUMA:
            case CTYPE_UNORM:
                *dst_component = (float)tmp / mask;
                break;

            case CTYPE_SHILO:
            case CTYPE_SNORM:
            {
                const uint32_t sign_bit = (1u << (format->bits[c] - 1));
                uint32_t tmp_extended = (tmp & sign_bit) ? (tmp | ~(sign_bit - 1)) : tmp;

                /*
                 * In order to clamp to an even range, we need to ignore
                 * the maximum negative value.
                 */
                if (tmp == sign_bit)
                    tmp_extended |= 1;

                *dst_component = (float)(((int32_t)tmp_extended)) / (sign_bit - 1);
                break;
            }

            default:
                break;
            }
        }
        else if (dst_ctype == CTYPE_LUMA)
        {
            assert(format->bits[1]);
            *dst_component = dst->value.x;
        }
        else if (dst_ctype == CTYPE_SHILO)
        {
            float cx_val;

            assert(format->bits[1] && format->bits[2] && c == 3);
            cx_val = 1.0f - dst->value.x * dst->value.x - dst->value.y * dst->value.y;
            if (cx_val <= 0.0f || cx_val > 1.0f)
                *dst_component = 0.0f;
            else
                *dst_component = pixel_sqrt(cx_val);
        }
        else
        {
            *dst_component = 1.0f;
        }
        pixel_check();
    }
}

/* It doesn't work for components bigger than 32 bits. */
void adv_pixel_format_from_d3dx_color(const struct pixel_format_desc *format, const struct d3dx_color *src, BYTE *dst)
{
    DWORD v, mask32;
    unsigned int c, i;

    memset(dst, 0, format->bytes_per_pixel);

    for (c = 0; c < 4; ++c)
    {
        const enum component_type dst_ctype = !c ? format->a_type : format->rgb_type;
        static const unsigned int component_offsets[4] = {3, 0, 1, 2};
        const float src_component = *(&src->value.x + component_offsets[c]);
        const enum range src_range = !c ? src->a_range : src->rgb_range;

        if (!format->bits[c])
            continue;

        mask32 = ~0u >> (32 - format->bits[c]);

        switch (dst_ctype)
        {
        case CTYPE_FLOAT:
            if (format->bits[c] == 16)
                v = adv_pixel_float_32_to_16(src_component);
            else
                v = *(DWORD *)&src_component;
            break;

        case CTYPE_LUMA:
        {
            float val = pixel_add(pixel_add(pixel_mul(src->value.x, 0.2125f), pixel_mul(src->value.y, 0.7154f)), pixel_mul(src->value.z, 0.0721f));

            if (src_range == RANGE_SNORM)
                val = (val + 1.0f) / 2.0f;

            v = d3dx_clamp(val, 0.0f, 1.0f) * ((1u << format->bits[c]) - 1) + 0.5f;
            break;
        }

        case CTYPE_UNORM:
        {
            float val = pixel_input(src_component);

            if (src_range == RANGE_SNORM)
                val = (val + 1.0f) / 2.0f;

            v = d3dx_clamp(val, 0.0f, 1.0f) * ((1u << format->bits[c]) - 1) + 0.5f;
            break;
        }

        case CTYPE_SNORM:
        {
            const uint32_t max_value = (1u << (format->bits[c] - 1)) - 1;
            float val = pixel_input(src_component);

            if (src_range == RANGE_UNORM)
                val = (val * 2.0f) - 1.0f;

            /* The pinned i386 conversion truncates through a signed integer.
             * An AArch64 unsigned conversion would clamp negative values to zero. */
            v = (uint32_t)(int32_t)(d3dx_clamp(val, -1.0f, 1.0f) * max_value + 0.5f);
            break;
        }

        /* We shouldn't be trying to output to CTYPE_INDEX or CTYPE_SHILO. */
        case CTYPE_INDEX:
        case CTYPE_SHILO:
            assert(0);
            break;

        default:
            v = 0;
            break;
        }

        pixel_check();
        for (i = format->shift[c] / 8 * 8; i < format->shift[c] + format->bits[c]; i += 8)
        {
            BYTE mask, byte;

            if (format->shift[c] > i)
            {
                mask = mask32 << (format->shift[c] - i);
                byte = (v << (format->shift[c] - i)) & mask;
            }
            else
            {
                mask = mask32 >> (i - format->shift[c]);
                byte = (v >> (i - format->shift[c])) & mask;
            }
            dst[i / 8] |= byte;
        }
    }
}

struct d3dx_color_key
{
    uint8_t color_key_min[4];
    uint8_t color_key_max[4];
};

static void d3dx_init_color_key(const struct pixel_format_desc *src_fmt, uint32_t color_key,
        struct d3dx_color_key *color_key_out)
{
    unsigned int i;

    for (i = 0; i < 4; ++i)
    {
        const enum component_type src_ctype = !i ? src_fmt->a_type : src_fmt->rgb_type;
        const uint8_t channel_bits = (src_ctype == CTYPE_LUMA) ? src_fmt->bits[1] : src_fmt->bits[i];
        const uint8_t ck_channel = (color_key >> (24 - (i * 8))) & 0xff;
        float slop, channel_conv, unique_values;

        if (!channel_bits)
        {
            color_key_out->color_key_min[i] = 0x00;
            color_key_out->color_key_max[i] = 0xff;
            continue;
        }

        /*
         * If the source format channel can represent all unique channel
         * values in the color key, no extra processing is necessary.
         */
        if (src_ctype == CTYPE_FLOAT || (src_ctype == CTYPE_SNORM && channel_bits > 8)
                || (src_ctype != CTYPE_SNORM && channel_bits >= 8))
        {
            color_key_out->color_key_min[i] = color_key_out->color_key_max[i] = ck_channel;
            continue;
        }

        channel_conv = ck_channel / 255.0f;
        if (src_ctype == CTYPE_SNORM)
        {
            const uint32_t max_value = (1u << (channel_bits - 1)) - 1;

            unique_values = (1u << channel_bits) - 2;
            channel_conv = (channel_conv * 2.0f) - 1.0f;
            channel_conv = pixel_round(channel_conv * max_value) / max_value;
            channel_conv = (channel_conv + 1.0f) / 2.0f;
        }
        else
        {
            unique_values = (1u << channel_bits) - 1;
            channel_conv = pixel_round(channel_conv * unique_values) / unique_values;
        }

        channel_conv = channel_conv * 255.0f;
        slop = (255.0f - unique_values) / unique_values / 2.0f;
        color_key_out->color_key_min[i] = pixel_round(d3dx_clamp(channel_conv - slop, 0.0f, 255.0f));
        color_key_out->color_key_max[i] = pixel_round(d3dx_clamp(channel_conv + slop, 0.0f, 255.0f));
        pixel_check();
    }
}

/************************************************************
 * copy_pixels
 *
 * Copies the source buffer to the destination buffer.
 * Works for any pixel format.
 * The source and the destination must be block-aligned.
 */
static void copy_pixels(const BYTE *src, UINT src_row_pitch, UINT src_slice_pitch,
        BYTE *dst, UINT dst_row_pitch, UINT dst_slice_pitch, const struct volume *size,
        const struct pixel_format_desc *format)
{
    UINT row, slice;
    BYTE *dst_addr;
    const BYTE *src_addr;
    UINT row_block_count = (size->width + format->block_width - 1) / format->block_width;
    UINT row_count = (size->height + format->block_height - 1) / format->block_height;

    for (slice = 0; slice < size->depth; slice++)
    {
        src_addr = src + slice * src_slice_pitch;
        dst_addr = dst + slice * dst_slice_pitch;

        for (row = 0; row < row_count; row++)
        {
            memmove(dst_addr, src_addr, row_block_count * format->block_byte_count);
            src_addr += src_row_pitch;
            dst_addr += dst_row_pitch;
        }
    }
}

static void convert_argb_pixel(const uint8_t *src_ptr, const struct pixel_format_desc *src_fmt,
        uint8_t *dst_ptr, const struct pixel_format_desc *dst_fmt, const PALETTEENTRY *palette,
        struct argb_conversion_info *conv_info, const struct d3dx_color_key *color_key,
        const struct pixel_format_desc *ck_format, struct argb_conversion_info *ck_conv_info)
{
    unsigned int i;

    if (format_types_match(src_fmt, dst_fmt) && src_fmt->bytes_per_pixel <= 4 && dst_fmt->bytes_per_pixel <= 4)
    {
        DWORD channels[4];
        DWORD val;

        get_relevant_argb_components(conv_info, src_ptr, channels);
        val = make_argb_color(conv_info, channels);

        if (color_key)
        {
            DWORD ck_pixel;

            get_relevant_argb_components(ck_conv_info, src_ptr, channels);
            ck_pixel = make_argb_color(ck_conv_info, channels);
            for (i = 0; i < 4; ++i)
            {
                const uint8_t ck_channel = (ck_pixel >> (24 - (i * 8))) & 0xff;

                if ((ck_channel < color_key->color_key_min[i]) || (ck_channel > color_key->color_key_max[i]))
                    break;
            }
            if (i == 4)
                val = 0;
        }
        memmove(dst_ptr, &val, dst_fmt->bytes_per_pixel);
    }
    else
    {
        struct d3dx_color color, tmp;

        adv_pixel_format_to_d3dx_color(src_fmt, src_ptr, palette, &color);
        tmp = color;

        if (color_key)
        {
            DWORD ck_pixel = 0;

            adv_pixel_format_from_d3dx_color(ck_format, &tmp, (BYTE *)&ck_pixel);
            for (i = 0; i < 4; ++i)
            {
                const uint8_t ck_channel = (ck_pixel >> (24 - (i * 8))) & 0xff;

                if ((ck_channel < color_key->color_key_min[i]) || (ck_channel > color_key->color_key_max[i]))
                    break;
            }
            if (i == 4)
                tmp.value.x = tmp.value.y = tmp.value.z = tmp.value.w = 0.0f;
        }

        color = tmp;
        adv_pixel_format_from_d3dx_color(dst_fmt, &color, dst_ptr);
    }
}

/************************************************************
 * convert_argb_pixels
 *
 * Copies the source buffer to the destination buffer, performing
 * any necessary format conversion and color keying.
 * Pixels outsize the source rect are blacked out.
 */
static void convert_argb_pixels(const BYTE *src, UINT src_row_pitch, UINT src_slice_pitch, const struct volume *src_size,
        const struct pixel_format_desc *src_format, BYTE *dst, UINT dst_row_pitch, UINT dst_slice_pitch,
        const struct volume *dst_size, const struct pixel_format_desc *dst_format, const struct d3dx_color_key *color_key,
        const PALETTEENTRY *palette)
{
    /* Color keys are always represented in D3DFMT_A8R8G8B8 format. */
    const struct pixel_format_desc *ck_format = color_key ? adv_pixel_get_d3dx_pixel_format_info(D3DX_PIXEL_FORMAT_B8G8R8A8_UNORM) : NULL;
    struct argb_conversion_info conv_info, ck_conv_info;
    UINT min_width, min_height, min_depth;
    UINT x, y, z;

    TRACE("src %p, src_row_pitch %u, src_slice_pitch %u, src_size %p, src_format %p, dst %p, "
            "dst_row_pitch %u, dst_slice_pitch %u, dst_size %p, dst_format %p, color_key %s, palette %p.\n",
            src, src_row_pitch, src_slice_pitch, src_size, src_format, dst, dst_row_pitch, dst_slice_pitch, dst_size,
            dst_format, debug_d3dx_color_key(color_key), palette);

    init_argb_conversion_info(src_format, dst_format, &conv_info);

    min_width = min(src_size->width, dst_size->width);
    min_height = min(src_size->height, dst_size->height);
    min_depth = min(src_size->depth, dst_size->depth);

    if (color_key)
        init_argb_conversion_info(src_format, ck_format, &ck_conv_info);

    for (z = 0; z < min_depth; z++) {
        const BYTE *src_slice_ptr = src + z * src_slice_pitch;
        BYTE *dst_slice_ptr = dst + z * dst_slice_pitch;

        for (y = 0; y < min_height; y++) {
            const BYTE *src_ptr = src_slice_ptr + y * src_row_pitch;
            BYTE *dst_ptr = dst_slice_ptr + y * dst_row_pitch;

            for (x = 0; x < min_width; x++) {
                convert_argb_pixel(src_ptr, src_format, dst_ptr, dst_format, palette,
                        &conv_info, color_key, ck_format, &ck_conv_info);

                src_ptr += src_format->bytes_per_pixel;
                dst_ptr += dst_format->bytes_per_pixel;
            }

            if (src_size->width < dst_size->width) /* black out remaining pixels */
                memset(dst_ptr, 0, dst_format->bytes_per_pixel * (dst_size->width - src_size->width));
        }

        if (src_size->height < dst_size->height) /* black out remaining pixels */
            memset(dst + src_size->height * dst_row_pitch, 0, dst_row_pitch * (dst_size->height - src_size->height));
    }
    if (src_size->depth < dst_size->depth) /* black out remaining pixels */
        memset(dst + src_size->depth * dst_slice_pitch, 0, dst_slice_pitch * (dst_size->depth - src_size->depth));
}

/************************************************************
 * point_filter_argb_pixels
 *
 * Copies the source buffer to the destination buffer, performing
 * any necessary format conversion, color keying and stretching
 * using a point filter.
 */
static void point_filter_argb_pixels(const BYTE *src, UINT src_row_pitch, UINT src_slice_pitch,
        const struct volume *src_size, const struct pixel_format_desc *src_format, BYTE *dst, UINT dst_row_pitch,
        UINT dst_slice_pitch, const struct volume *dst_size, const struct pixel_format_desc *dst_format,
        const struct d3dx_color_key *color_key, const PALETTEENTRY *palette)
{
    /* Color keys are always represented in D3DFMT_A8R8G8B8 format. */
    const struct pixel_format_desc *ck_format = color_key ? adv_pixel_get_d3dx_pixel_format_info(D3DX_PIXEL_FORMAT_B8G8R8A8_UNORM) : NULL;
    struct argb_conversion_info conv_info, ck_conv_info;
    UINT x, y, z;

    TRACE("src %p, src_row_pitch %u, src_slice_pitch %u, src_size %p, src_format %p, dst %p, "
            "dst_row_pitch %u, dst_slice_pitch %u, dst_size %p, dst_format %p, color_key %s, palette %p.\n",
            src, src_row_pitch, src_slice_pitch, src_size, src_format, dst, dst_row_pitch, dst_slice_pitch, dst_size,
            dst_format, debug_d3dx_color_key(color_key), palette);

    init_argb_conversion_info(src_format, dst_format, &conv_info);

    if (color_key)
        init_argb_conversion_info(src_format, ck_format, &ck_conv_info);

    for (z = 0; z < dst_size->depth; z++)
    {
        BYTE *dst_slice_ptr = dst + z * dst_slice_pitch;
        const BYTE *src_slice_ptr = src + src_slice_pitch * (z * src_size->depth / dst_size->depth);

        for (y = 0; y < dst_size->height; y++)
        {
            BYTE *dst_ptr = dst_slice_ptr + y * dst_row_pitch;
            const BYTE *src_row_ptr = src_slice_ptr + src_row_pitch * (y * src_size->height / dst_size->height);

            for (x = 0; x < dst_size->width; x++)
            {
                const BYTE *src_ptr = src_row_ptr + (x * src_size->width / dst_size->width) * src_format->bytes_per_pixel;

                convert_argb_pixel(src_ptr, src_format, dst_ptr, dst_format, palette,
                        &conv_info, color_key, ck_format, &ck_conv_info);
                dst_ptr += dst_format->bytes_per_pixel;
            }
        }
    }
}

static void check_color_key(struct d3dx_color *color, const struct d3dx_color_key *color_key,
        const struct pixel_format_desc *ck_format)
{
    struct d3dx_color tmp;
    DWORD ck_pixel = 0;
    unsigned int i;

    tmp = *color;
    adv_pixel_format_from_d3dx_color(ck_format, &tmp, (BYTE *)&ck_pixel);
    for (i = 0; i < 4; ++i)
    {
        const uint8_t ck_channel = (ck_pixel >> (24 - (i * 8))) & 0xff;

        if ((ck_channel < color_key->color_key_min[i]) || (ck_channel > color_key->color_key_max[i]))
            break;
    }

    if (i == 4)
        tmp.value.x = tmp.value.y = tmp.value.z = tmp.value.w = 0.0f;

    *color = tmp;
}

/* SSE's invalid sum of opposite infinities is a negative indefinite NaN.
 * Its sign remains observable when BOX writes a floating-point destination. */
static float pixel_add(float a, float b)
{
    a = pixel_input(a); b = pixel_input(b);
    union { uint32_t bits; float value; } av = { .value = a }, bv = { .value = b };
    if (((av.bits & 0x7fffffff)>0x7f800000 && !(av.bits & 0x00400000)) ||
        ((bv.bits & 0x7fffffff)>0x7f800000 && !(bv.bits & 0x00400000))) pixel_flags |= 1;
    /* SSE propagates the first NaN, including its payload, before the second. */
    if ((av.bits & 0x7fffffff) > 0x7f800000) { av.bits |= 0x00400000; pixel_check(); return av.value; }
    if ((bv.bits & 0x7fffffff) > 0x7f800000) { bv.bits |= 0x00400000; pixel_check(); return bv.value; }
    if (isinf(a) && isinf(b) && signbit(a) != signbit(b))
    {
        union { uint32_t bits; float value; } indefinite = { .bits = 0xffc00000 };
        pixel_flags |= 1; pixel_check(); return indefinite.value;
    }
    float result=pixel_output(a+b); pixel_check(); return result;
}

static inline void vec4_add(struct vec4 *out, const struct vec4 *in)
{
    out->x = pixel_add(out->x, in->x);
    out->y = pixel_add(out->y, in->y);
    out->z = pixel_add(out->z, in->z);
    out->w = pixel_add(out->w, in->w);
}

static inline void vec4_scale(struct vec4 *out, const float scale)
{
    out->x = pixel_mul(out->x, scale);
    out->y = pixel_mul(out->y, scale);
    out->z = pixel_mul(out->z, scale);
    out->w = pixel_mul(out->w, scale);
}

static void box_filter_argb_pixels(const BYTE *src, UINT src_row_pitch, UINT src_slice_pitch,
        const struct volume *src_size, const struct pixel_format_desc *src_format, BYTE *dst, UINT dst_row_pitch,
        UINT dst_slice_pitch, const struct volume *dst_size, const struct pixel_format_desc *dst_format,
        const struct d3dx_color_key *color_key, const PALETTEENTRY *palette)
{
    /* Color keys are always represented in D3DFMT_A8R8G8B8 format. */
    const struct pixel_format_desc *ck_format = color_key ? adv_pixel_get_d3dx_pixel_format_info(D3DX_PIXEL_FORMAT_B8G8R8A8_UNORM) : NULL;
    struct argb_conversion_info conv_info, ck_conv_info;
    struct d3dx_color color, tmp;
    unsigned int i;
    UINT x, y, z;

    TRACE("src %p, src_row_pitch %u, src_slice_pitch %u, src_size %p, src_format %p, dst %p, "
            "dst_row_pitch %u, dst_slice_pitch %u, dst_size %p, dst_format %p, color_key %s, palette %p.\n",
            src, src_row_pitch, src_slice_pitch, src_size, src_format, dst, dst_row_pitch, dst_slice_pitch, dst_size,
            dst_format, debug_d3dx_color_key(color_key), palette);

    init_argb_conversion_info(src_format, dst_format, &conv_info);

    if (color_key)
        init_argb_conversion_info(src_format, ck_format, &ck_conv_info);

    color.rgb_range = get_range_for_component_type(src_format->rgb_type);
    color.a_range = get_range_for_component_type(src_format->a_type);
    for (z = 0; z < src_size->depth; z += 2)
    {
        BYTE *dst_slice_ptr = dst + (z >> 1) * dst_slice_pitch;
        const BYTE *src_slice_ptr = src + src_slice_pitch * z;

        for (y = 0; y < src_size->height; y += 2)
        {
            const BYTE *src_row_ptr = src_slice_ptr + src_row_pitch * y;
            BYTE *dst_ptr = dst_slice_ptr + (y >> 1) * dst_row_pitch;

            for (x = 0; x < src_size->width; x += 2)
            {
                const BYTE *src_ptr = src_row_ptr + x * src_format->bytes_per_pixel;

                memset(&color.value, 0, sizeof(color.value));
                for (i = 0; i < (src_size->depth > 1 ? 2 : 1); ++i)
                {
                    const BYTE *ptr = src_ptr + i * src_slice_pitch;

                    adv_pixel_format_to_d3dx_color(src_format, ptr, palette, &tmp);
                    if (color_key)
                        check_color_key(&tmp, color_key, ck_format);
                    vec4_add(&color.value, &tmp.value);

                    adv_pixel_format_to_d3dx_color(src_format, ptr + src_format->bytes_per_pixel, palette, &tmp);
                    if (color_key)
                        check_color_key(&tmp, color_key, ck_format);
                    vec4_add(&color.value, &tmp.value);

                    ptr += src_row_pitch;
                    adv_pixel_format_to_d3dx_color(src_format, ptr, palette, &tmp);
                    if (color_key)
                        check_color_key(&tmp, color_key, ck_format);
                    vec4_add(&color.value, &tmp.value);

                    adv_pixel_format_to_d3dx_color(src_format, ptr + src_format->bytes_per_pixel, palette, &tmp);
                    if (color_key)
                        check_color_key(&tmp, color_key, ck_format);
                    vec4_add(&color.value, &tmp.value);
                }

                vec4_scale(&color.value, src_size->depth > 1 ? 0.125f : 0.25f);
                adv_pixel_format_from_d3dx_color(dst_format, &color, dst_ptr);
                dst_ptr += dst_format->bytes_per_pixel;
            }
        }
    }
}

static HRESULT d3dx_pixels_decompress(struct d3dx_pixels *pixels, const struct pixel_format_desc *desc,
        BOOL is_dst, void **out_memory, uint32_t *out_row_pitch, uint32_t *out_slice_pitch,
        const struct pixel_format_desc **out_desc)
{
    uint32_t uncompressed_slice_pitch, uncompressed_row_pitch, block_buf_row_pitch, block_width_mask, block_height_mask;
    void (*decompress_bcn_block)(const void *src, void *dst, int dst_row_pitch);
    const struct pixel_format_desc *uncompressed_desc = NULL;
    const struct volume *size = &pixels->size;
    BYTE *uncompressed_mem;
    uint8_t block_buf[64];
    unsigned int x, y, z;
    RECT aligned_rect;

    switch (desc->format)
    {
        case D3DX_PIXEL_FORMAT_DXT1_UNORM:
            uncompressed_desc = adv_pixel_get_d3dx_pixel_format_info(D3DX_PIXEL_FORMAT_R8G8B8A8_UNORM);
            decompress_bcn_block = bcdec_bc1;
            break;

        case D3DX_PIXEL_FORMAT_DXT2_UNORM:
        case D3DX_PIXEL_FORMAT_DXT3_UNORM:
            uncompressed_desc = adv_pixel_get_d3dx_pixel_format_info(D3DX_PIXEL_FORMAT_R8G8B8A8_UNORM);
            decompress_bcn_block = bcdec_bc2;
            break;

        case D3DX_PIXEL_FORMAT_DXT4_UNORM:
        case D3DX_PIXEL_FORMAT_DXT5_UNORM:
            uncompressed_desc = adv_pixel_get_d3dx_pixel_format_info(D3DX_PIXEL_FORMAT_R8G8B8A8_UNORM);
            decompress_bcn_block = bcdec_bc3;
            break;

        case D3DX_PIXEL_FORMAT_BC4_UNORM:
        case D3DX_PIXEL_FORMAT_BC4_SNORM:
            if (desc->rgb_type == CTYPE_UNORM)
                uncompressed_desc = adv_pixel_get_d3dx_pixel_format_info(D3DX_PIXEL_FORMAT_R8_UNORM);
            else
                uncompressed_desc = adv_pixel_get_d3dx_pixel_format_info(D3DX_PIXEL_FORMAT_R8_SNORM);
            decompress_bcn_block = bcdec_bc4;
            break;

        case D3DX_PIXEL_FORMAT_BC5_UNORM:
        case D3DX_PIXEL_FORMAT_BC5_SNORM:
            if (desc->rgb_type == CTYPE_UNORM)
                uncompressed_desc = adv_pixel_get_d3dx_pixel_format_info(D3DX_PIXEL_FORMAT_R8G8_UNORM);
            else
                uncompressed_desc = adv_pixel_get_d3dx_pixel_format_info(D3DX_PIXEL_FORMAT_R8G8_SNORM);
            decompress_bcn_block = bcdec_bc5;
            break;


        default:
            FIXME("Unexpected compressed texture format %u.\n", desc->format);
            return E_NOTIMPL;
    }

    block_width_mask = desc->block_width - 1;
    block_height_mask = desc->block_height - 1;
    block_buf_row_pitch = desc->block_width * uncompressed_desc->bytes_per_pixel;
    uncompressed_row_pitch = size->width * uncompressed_desc->bytes_per_pixel;
    uncompressed_slice_pitch = uncompressed_row_pitch * size->height;
    if (!(uncompressed_mem = malloc(size->depth * uncompressed_slice_pitch)))
        return E_OUTOFMEMORY;

    /*
     * For compressed destination pixels, width/height will represent
     * the entire set of compressed blocks our destination rectangle touches.
     * If we're only updating a sub-area of any blocks, we need to decompress
     * the pixels outside of the sub-area.
     */
    if (is_dst)
    {
        SetRect(&aligned_rect, 0, 0, size->width, size->height);

        /*
         * If our destination covers the entire set of blocks, no
         * decompression needs to be done, just return the allocated memory.
         */
        if (EqualRect(&aligned_rect, &pixels->unaligned_rect))
            goto exit;
    }
    /*
     * For compressed source pixels, width/height will represent the size of
     * the unaligned rectangle. I.e, if we have an 8x8 source with an
     * unaligned rect of (2,2)-(6,6) our width/height will be 4.
     */
    else
    {
        SetRect(&aligned_rect, 0, 0, (pixels->unaligned_rect.right + block_width_mask) & ~block_width_mask,
                (pixels->unaligned_rect.bottom + block_height_mask) & ~block_height_mask);
    }

    TRACE("Decompressing pixels.\n");
    for (z = 0; z < size->depth; ++z)
    {
        const uint8_t *src_slice = &((const uint8_t *)pixels->data)[z * pixels->slice_pitch];
        uint8_t *dst_slice = &uncompressed_mem[z * uncompressed_slice_pitch];

        for (y = 0; y < aligned_rect.bottom; y += desc->block_height)
        {
            const uint8_t *src_ptr = &src_slice[(y / desc->block_height) * pixels->row_pitch];

            for (x = 0; x < aligned_rect.right; x += desc->block_width)
            {
                struct volume dst_block_size;
                RECT src_rect, dst_rect;
                uint8_t *dst_ptr;

                SetRect(&src_rect, x, y, x + desc->block_width, y + desc->block_height);
                IntersectRect(&src_rect, &src_rect, &pixels->unaligned_rect);
                dst_rect = src_rect;
                OffsetRect(&dst_rect, -pixels->unaligned_rect.left, -pixels->unaligned_rect.top);

                set_volume_struct(&dst_block_size, dst_rect.right - dst_rect.left, dst_rect.bottom - dst_rect.top, 1);
                dst_ptr = &dst_slice[(dst_rect.top * uncompressed_row_pitch)];
                dst_ptr += dst_rect.left * uncompressed_desc->bytes_per_pixel;

                if (dst_block_size.width != desc->block_width || dst_block_size.height != desc->block_height)
                {
                    if (!is_dst)
                    {
                        unsigned int block_buf_offset;

                        decompress_bcn_block(src_ptr, block_buf, block_buf_row_pitch);
                        block_buf_offset = (src_rect.top - y) * block_buf_row_pitch;
                        block_buf_offset += uncompressed_desc->bytes_per_pixel * (src_rect.left - x);
                        copy_pixels(&block_buf[block_buf_offset], block_buf_row_pitch, 0, dst_ptr,
                                uncompressed_row_pitch, 0, &dst_block_size, uncompressed_desc);
                    }
                    /*
                     * If this is the destination, we can just copy the whole
                     * block. It will be partially overwritten later.
                     */
                    else
                    {
                        dst_ptr = &dst_slice[y * uncompressed_row_pitch + x * uncompressed_desc->bytes_per_pixel];
                        decompress_bcn_block(src_ptr, dst_ptr, uncompressed_row_pitch);
                    }

                }
                /* Full block copy. */
                else if (!is_dst)
                {
                    decompress_bcn_block(src_ptr, dst_ptr, uncompressed_row_pitch);
                }
                src_ptr += desc->block_byte_count;
            }
        }
    }

exit:
    *out_memory = uncompressed_mem;
    *out_row_pitch = uncompressed_row_pitch;
    *out_slice_pitch = uncompressed_slice_pitch;
    *out_desc = uncompressed_desc;

    return S_OK;
}

static HRESULT d3dx_pixels_unpack_index(struct d3dx_pixels *pixels, const struct pixel_format_desc *desc,
        void **out_memory, uint32_t *out_row_pitch, uint32_t *out_slice_pitch, const struct pixel_format_desc **out_desc)
{
    uint32_t x, y, z, unpacked_slice_pitch, unpacked_row_pitch;
    const struct pixel_format_desc *unpacked_desc = NULL;
    const struct volume *size = &pixels->size;
    uint8_t *unpacked_mem;
    uint8_t mask, shift;

    switch (desc->format)
    {
        case D3DX_PIXEL_FORMAT_P1_UINT:
        case D3DX_PIXEL_FORMAT_P2_UINT:
        case D3DX_PIXEL_FORMAT_P4_UINT:
            unpacked_desc = adv_pixel_get_d3dx_pixel_format_info(D3DX_PIXEL_FORMAT_P8_UINT);
            break;

        default:
            FIXME("Unexpected format %u.\n", desc->format);
            return E_NOTIMPL;
    }

    unpacked_row_pitch = size->width * unpacked_desc->bytes_per_pixel;
    unpacked_slice_pitch = unpacked_row_pitch * size->height;
    if (!(unpacked_mem = malloc(size->depth * unpacked_slice_pitch)))
        return E_OUTOFMEMORY;

    shift = 8 / desc->block_width;
    mask = (1u << shift) - 1;

    TRACE("Unpacking pixels.\n");
    for (z = 0; z < size->depth; ++z)
    {
        const uint8_t *slice_data = (const uint8_t *)pixels->data + (pixels->slice_pitch * z);

        for (y = 0; y < size->height; ++y)
        {
            uint8_t *ptr = &unpacked_mem[(z * unpacked_slice_pitch) + (y * unpacked_row_pitch)];
            const uint8_t *row_data = slice_data + (pixels->row_pitch * y);

            for (x = 0; x < size->width; x += desc->block_width)
            {
                const uint8_t packed_data = *row_data;
                unsigned int i;

                for (i = 0; i < desc->block_width; ++i)
                {
                    const uint8_t cur_shift = ((desc->block_width - 1) - i) * shift;

                    if (x + i >= size->width)
                        break;
                    ptr[i] = (packed_data >> cur_shift) & mask;
                }
                ptr += unpacked_desc->bytes_per_pixel * desc->block_width;
                row_data++;
            }
        }
    }

    *out_memory = unpacked_mem;
    *out_row_pitch = unpacked_row_pitch;
    *out_slice_pitch = unpacked_slice_pitch;
    *out_desc = unpacked_desc;

    return S_OK;
}

static void d3dx_compress_block_inner(enum d3dx_pixel_format_id fmt, uint8_t *block_buf, void *dst_buf)
{
    switch (fmt)
    {
        case D3DX_PIXEL_FORMAT_DXT1_UNORM:
            stb_compress_dxt_block(dst_buf, block_buf, FALSE, 0);
            break;

        case D3DX_PIXEL_FORMAT_DXT2_UNORM:
        case D3DX_PIXEL_FORMAT_DXT3_UNORM:
        {
            uint8_t *dst_data_offset = dst_buf;
            unsigned int y;

            /* STB doesn't do DXT2/DXT3, we'll do the alpha part ourselves. */
            for (y = 0; y < 4; ++y)
            {
                uint8_t *tmp_row = &block_buf[y * 4 * 4];

                dst_data_offset[0]  = (tmp_row[7] & 0xf0);
                dst_data_offset[0] |= (tmp_row[3] >> 4);
                dst_data_offset[1]  = (tmp_row[15] & 0xf0);
                dst_data_offset[1] |= (tmp_row[11] >> 4);

                /*
                 * Set all alpha values to 0xff so they aren't considered during
                 * compression. This modifies the source data being passed in.
                 */
                tmp_row[3] = tmp_row[7] = tmp_row[11] = tmp_row[15] = 0xff;
                dst_data_offset += 2;
            }
            stb_compress_dxt_block(dst_data_offset, block_buf, FALSE, 0);
            break;
        }

        case D3DX_PIXEL_FORMAT_DXT4_UNORM:
        case D3DX_PIXEL_FORMAT_DXT5_UNORM:
            stb_compress_dxt_block(dst_buf, block_buf, TRUE, 0);
            break;

        case D3DX_PIXEL_FORMAT_BC4_UNORM:
        case D3DX_PIXEL_FORMAT_BC4_SNORM:
            stb_compress_bc4_block(dst_buf, block_buf);
            break;

        case D3DX_PIXEL_FORMAT_BC5_UNORM:
        case D3DX_PIXEL_FORMAT_BC5_SNORM:
            stb_compress_bc5_block(dst_buf, block_buf);
            break;

        default:
            assert(0);
            break;
    }
}

static void d3dx_compress_block(enum d3dx_pixel_format_id fmt, uint8_t *block, void *dst)
{
    uint8_t bytes[16];
    d3dx_compress_block_inner(fmt,block,bytes);
    unsigned alpha = (fmt == D3DX_PIXEL_FORMAT_DXT2_UNORM || fmt == D3DX_PIXEL_FORMAT_DXT3_UNORM ||
        fmt == D3DX_PIXEL_FORMAT_DXT4_UNORM || fmt == D3DX_PIXEL_FORMAT_DXT5_UNORM) ? 8 : 0;
    if (alpha) memmove(dst,bytes,alpha);
    pixel_check();
    unsigned size=(fmt==D3DX_PIXEL_FORMAT_DXT1_UNORM || fmt==D3DX_PIXEL_FORMAT_BC4_UNORM || fmt==D3DX_PIXEL_FORMAT_BC4_SNORM) ? 8 : 16;
    memmove((BYTE *)dst+alpha,bytes+alpha,size-alpha);
}

/*
 * Source data passed into this function is potentially modified (currently
 * only in the case of DXT2/DXT3). As of now we only pass temporary buffers
 * into this function, but this should be taken to account if used elsewhere
 * outside of adv_pixel_d3dx_load_pixels_from_pixels() in the future.
 */
static HRESULT d3dx_pixels_compress(struct d3dx_pixels *src_pixels,
        const struct pixel_format_desc *src_desc, struct d3dx_pixels *dst_pixels,
        const struct pixel_format_desc *dst_desc)
{
    unsigned int x, y, z, block_buf_row_pitch;
    uint8_t block_buf[64];

    switch (dst_desc->format)
    {
        case D3DX_PIXEL_FORMAT_DXT1_UNORM:
        case D3DX_PIXEL_FORMAT_DXT2_UNORM:
        case D3DX_PIXEL_FORMAT_DXT3_UNORM:
        case D3DX_PIXEL_FORMAT_DXT4_UNORM:
        case D3DX_PIXEL_FORMAT_DXT5_UNORM:
            assert(src_desc->format == D3DX_PIXEL_FORMAT_R8G8B8A8_UNORM);
            break;

        case D3DX_PIXEL_FORMAT_BC4_UNORM:
            assert(src_desc->format == D3DX_PIXEL_FORMAT_R8_UNORM);
            break;

        case D3DX_PIXEL_FORMAT_BC4_SNORM:
            assert(src_desc->format == D3DX_PIXEL_FORMAT_R8_SNORM);
            break;

        case D3DX_PIXEL_FORMAT_BC5_UNORM:
            assert(src_desc->format == D3DX_PIXEL_FORMAT_R8G8_UNORM);
            break;

        case D3DX_PIXEL_FORMAT_BC5_SNORM:
            assert(src_desc->format == D3DX_PIXEL_FORMAT_R8G8_SNORM);
            break;

        default:
            FIXME("Unexpected compressed texture format %u.\n", dst_desc->format);
            return E_NOTIMPL;
    }

    TRACE("Compressing pixels.\n");
    block_buf_row_pitch = src_desc->bytes_per_pixel * dst_desc->block_width;
    for (z = 0; z < src_pixels->size.depth; ++z)
    {
        const uint8_t *src_slice = &((const uint8_t *)src_pixels->data)[z * src_pixels->slice_pitch];
        uint8_t *dst_slice = &((uint8_t *)dst_pixels->data)[z * dst_pixels->slice_pitch];

        for (y = 0; y < src_pixels->size.height; y += dst_desc->block_height)
        {
            const unsigned int tmp_src_height = min(dst_desc->block_height, src_pixels->size.height - y);
            uint8_t *dst_ptr = &dst_slice[(y / dst_desc->block_height) * dst_pixels->row_pitch];
            const uint8_t *src_ptr = &src_slice[y * src_pixels->row_pitch];

            for (x = 0; x < src_pixels->size.width; x += dst_desc->block_width)
            {
                const unsigned int tmp_src_width = min(dst_desc->block_width, src_pixels->size.width - x);
                struct volume block_buf_size = { tmp_src_width, tmp_src_height, 1 };

                if (tmp_src_width != dst_desc->block_width || tmp_src_height != dst_desc->block_height)
                    memset(block_buf, 0, sizeof(block_buf));
                copy_pixels(src_ptr, src_pixels->row_pitch, src_pixels->slice_pitch, block_buf, block_buf_row_pitch, 0,
                        &block_buf_size, src_desc);
                d3dx_compress_block(dst_desc->format, block_buf, dst_ptr);
                src_ptr += (src_desc->bytes_per_pixel * dst_desc->block_width);
                dst_ptr += dst_desc->block_byte_count;
            }
        }
    }

    return S_OK;
}

HRESULT adv_pixel_d3dx_pixels_init(const void *data, uint32_t row_pitch, uint32_t slice_pitch,
        const PALETTEENTRY *palette, enum d3dx_pixel_format_id format, uint32_t left, uint32_t top, uint32_t right,
        uint32_t bottom, uint32_t front, uint32_t back, struct d3dx_pixels *pixels)
{
    const struct pixel_format_desc *fmt_desc = adv_pixel_get_d3dx_pixel_format_info(format);
    const BYTE *ptr = data;
    RECT unaligned_rect;

    memset(pixels, 0, sizeof(*pixels));
    if (is_unknown_format(fmt_desc))
    {
        FIXME("Unsupported format %#x.\n", format);
        return E_NOTIMPL;
    }

    ptr += front * slice_pitch;
    ptr += (top / fmt_desc->block_height) * row_pitch;
    ptr += (left / fmt_desc->block_width) * fmt_desc->block_byte_count;

    if (is_compressed_format(fmt_desc))
    {
        uint32_t left_aligned, top_aligned;

        top_aligned = top & ~(fmt_desc->block_height - 1);
        left_aligned = left & ~(fmt_desc->block_width - 1);
        SetRect(&unaligned_rect, left, top, right, bottom);
        OffsetRect(&unaligned_rect, -left_aligned, -top_aligned);
    }
    else
    {
        SetRect(&unaligned_rect, 0, 0, (right - left), (bottom - top));
    }

    if (!slice_pitch)
        slice_pitch = row_pitch * (bottom - top);
    set_d3dx_pixels(pixels, ptr, row_pitch, slice_pitch, palette, (right - left), (bottom - top), (back - front),
            &unaligned_rect);

    return S_OK;
}

HRESULT adv_pixel_d3dx_load_pixels_from_pixels(struct d3dx_pixels *dst_pixels,
       const struct pixel_format_desc *dst_desc, struct d3dx_pixels *src_pixels,
       const struct pixel_format_desc *src_desc, uint32_t filter_flags, uint32_t color_key)
{
    struct volume src_size, dst_size, dst_size_aligned;
    const struct d3dx_color_key *d3dx_ck = NULL;
    struct d3dx_color_key d3dx_color_key;
    HRESULT hr = S_OK;

    TRACE("dst_pixels %s, dst_desc %p, src_pixels %s, src_desc %p, filter_flags %#x, color_key %#x.\n",
            debug_d3dx_pixels(dst_pixels), dst_desc, debug_d3dx_pixels(src_pixels), src_desc,
            filter_flags, color_key);

    if (is_compressed_format(src_desc))
        set_volume_struct(&src_size, (src_pixels->unaligned_rect.right - src_pixels->unaligned_rect.left),
                (src_pixels->unaligned_rect.bottom - src_pixels->unaligned_rect.top), src_pixels->size.depth);
    else
        src_size = src_pixels->size;

    dst_size_aligned = dst_pixels->size;
    if (is_compressed_format(dst_desc))
        set_volume_struct(&dst_size, (dst_pixels->unaligned_rect.right - dst_pixels->unaligned_rect.left),
                (dst_pixels->unaligned_rect.bottom - dst_pixels->unaligned_rect.top), dst_pixels->size.depth);
    else
        dst_size = dst_size_aligned;

    /* Everything matches, simply copy the pixels. */
    if (src_desc->format == dst_desc->format
            && (dst_size.width == src_size.width && !(dst_size.width % dst_desc->block_width))
            && (dst_size.height == src_size.height && !(dst_size.height % dst_desc->block_height))
            && (dst_size.depth == src_size.depth)
            && color_key == 0
            && !(src_pixels->unaligned_rect.left & (src_desc->block_width - 1))
            && !(src_pixels->unaligned_rect.top & (src_desc->block_height - 1))
            && !(dst_pixels->unaligned_rect.left & (dst_desc->block_width - 1))
            && !(dst_pixels->unaligned_rect.top & (dst_desc->block_height - 1)))
    {
        TRACE("Simple copy.\n");
        copy_pixels(src_pixels->data, src_pixels->row_pitch, src_pixels->slice_pitch, (void *)dst_pixels->data,
                dst_pixels->row_pitch, dst_pixels->slice_pitch, &src_size, src_desc);
        return S_OK;
    }

    /* Stretching or format conversion. */
    if (!is_conversion_from_supported(src_desc)
            || !is_conversion_to_supported(dst_desc))
    {
        FIXME("Unsupported format conversion %#x -> %#x.\n", src_desc->format, dst_desc->format);
        return E_NOTIMPL;
    }

    /*
     * Check to see if we potentially need to change to linear or triangle
     * filtering.
     */
    if ((filter_flags & 0xf) == D3DX_FILTER_BOX)
    {
        if (d3dx_size_is_half(&src_size, &dst_size))
            filter_flags = !d3dx_size_is_pow2(&src_size) ? ((filter_flags & ~0xf) | D3DX_FILTER_LINEAR) : filter_flags;
        else
            filter_flags = (filter_flags & ~0xf) | D3DX_FILTER_TRIANGLE;
    }

    if (is_index_format(src_desc) && (src_desc->block_width > 1))
    {
        uint32_t unpacked_row_pitch, unpacked_slice_pitch;
        const struct pixel_format_desc *unpacked_desc;
        void *unpacked_mem = NULL;

        hr = d3dx_pixels_unpack_index(src_pixels, src_desc, &unpacked_mem, &unpacked_row_pitch,
                &unpacked_slice_pitch, &unpacked_desc);
        if (SUCCEEDED(hr))
        {
            struct d3dx_pixels unpacked_pixels;

            adv_pixel_d3dx_pixels_init(unpacked_mem, unpacked_row_pitch, unpacked_slice_pitch, src_pixels->palette,
                    unpacked_desc->format, 0, 0, src_pixels->size.width, src_pixels->size.height,
                    0, src_pixels->size.depth, &unpacked_pixels);

            hr = adv_pixel_d3dx_load_pixels_from_pixels(dst_pixels, dst_desc, &unpacked_pixels, unpacked_desc,
                    filter_flags, color_key);
        }
        free(unpacked_mem);
        goto exit;
    }

    /*
     * If the source is a compressed image, we need to decompress it first
     * before doing any modifications.
     */
    if (is_compressed_format(src_desc))
    {
        uint32_t uncompressed_row_pitch, uncompressed_slice_pitch;
        const struct pixel_format_desc *uncompressed_desc;
        void *uncompressed_mem = NULL;

        hr = d3dx_pixels_decompress(src_pixels, src_desc, FALSE, &uncompressed_mem, &uncompressed_row_pitch,
                &uncompressed_slice_pitch, &uncompressed_desc);
        if (SUCCEEDED(hr))
        {
            struct d3dx_pixels uncompressed_pixels;

            adv_pixel_d3dx_pixels_init(uncompressed_mem, uncompressed_row_pitch, uncompressed_slice_pitch, NULL,
                    uncompressed_desc->format, 0, 0, src_pixels->size.width, src_pixels->size.height,
                    0, src_pixels->size.depth, &uncompressed_pixels);

            if (color_key)
            {
                TRACE("Clearing color key value on compressed source pixels.\n");
                color_key = 0;
            }
            hr = adv_pixel_d3dx_load_pixels_from_pixels(dst_pixels, dst_desc, &uncompressed_pixels, uncompressed_desc,
                    filter_flags, color_key);
        }
        free(uncompressed_mem);
        goto exit;
    }

    /* Same as the above, need to decompress the destination prior to modifying. */
    if (is_compressed_format(dst_desc))
    {
        uint32_t uncompressed_row_pitch, uncompressed_slice_pitch;
        const struct pixel_format_desc *uncompressed_desc;
        struct d3dx_pixels uncompressed_pixels;
        void *uncompressed_mem = NULL;

        hr = d3dx_pixels_decompress(dst_pixels, dst_desc, TRUE, &uncompressed_mem, &uncompressed_row_pitch,
                &uncompressed_slice_pitch, &uncompressed_desc);
        if (FAILED(hr))
            goto exit;

        adv_pixel_d3dx_pixels_init(uncompressed_mem, uncompressed_row_pitch, uncompressed_slice_pitch, NULL,
                uncompressed_desc->format, dst_pixels->unaligned_rect.left, dst_pixels->unaligned_rect.top,
                dst_pixels->unaligned_rect.right, dst_pixels->unaligned_rect.bottom, 0, dst_pixels->size.depth,
                &uncompressed_pixels);

        hr = adv_pixel_d3dx_load_pixels_from_pixels(&uncompressed_pixels, uncompressed_desc, src_pixels, src_desc, filter_flags,
                color_key);
        if (SUCCEEDED(hr))
        {
            adv_pixel_d3dx_pixels_init(uncompressed_mem, uncompressed_row_pitch, uncompressed_slice_pitch, NULL,
                    uncompressed_desc->format, 0, 0, dst_size_aligned.width, dst_size_aligned.height, 0,
                    dst_pixels->size.depth, &uncompressed_pixels);

            hr = d3dx_pixels_compress(&uncompressed_pixels, uncompressed_desc, dst_pixels, dst_desc);
            if (FAILED(hr))
                WARN("Failed to compress pixels, hr %#lx.\n", hr);
        }
        free(uncompressed_mem);
        goto exit;
    }

    if (color_key)
    {
        d3dx_init_color_key(src_desc, color_key, &d3dx_color_key);
        d3dx_ck = &d3dx_color_key;
    }

    if ((filter_flags & 0xf) == D3DX_FILTER_NONE)
    {
        convert_argb_pixels(src_pixels->data, src_pixels->row_pitch, src_pixels->slice_pitch, &src_size, src_desc,
                (BYTE *)dst_pixels->data, dst_pixels->row_pitch, dst_pixels->slice_pitch, &dst_size, dst_desc,
                d3dx_ck, src_pixels->palette);
    }
    else if ((filter_flags & 0xf) == D3DX_FILTER_BOX)
    {
        box_filter_argb_pixels(src_pixels->data, src_pixels->row_pitch, src_pixels->slice_pitch, &src_size,
                src_desc, (BYTE *)dst_pixels->data, dst_pixels->row_pitch, dst_pixels->slice_pitch, &dst_size,
                dst_desc, d3dx_ck, src_pixels->palette);
    }
    else /* if ((filter & 0xf) == D3DX_FILTER_POINT) */
    {
        if ((filter_flags & 0xf) != D3DX_FILTER_POINT)
            FIXME("Unhandled filter %#x.\n", filter_flags);

        /* Always apply a point filter until D3DX_FILTER_LINEAR,
         * D3DX_FILTER_TRIANGLE and D3DX_FILTER_BOX are implemented. */
        point_filter_argb_pixels(src_pixels->data, src_pixels->row_pitch, src_pixels->slice_pitch, &src_size,
                src_desc, (BYTE *)dst_pixels->data, dst_pixels->row_pitch, dst_pixels->slice_pitch, &dst_size,
                dst_desc, d3dx_ck, src_pixels->palette);
    }

exit:
    if (FAILED(hr))
        WARN("Failed to load pixels, hr %#lx.\n", hr);
    return hr;
}

uint64_t adv_pixel_convert(struct d3dx_pixels *dst, const struct pixel_format_desc *df,
    struct d3dx_pixels *src, const struct pixel_format_desc *sf, uint32_t filter, uint32_t key, uint32_t controls)
{
    uint32_t saved=pixel_save_status();
    pixel_controls=controls; pixel_flags=0; pixel_trap=0;
    memset(pixel_allocations,0,sizeof(pixel_allocations));
    if (setjmp(pixel_jump))
    {
        for (unsigned i=0;i<2;++i) if (pixel_allocations[i]) pixel_free(pixel_allocations[i]);
        pixel_restore_status(saved); return (uint64_t)pixel_trap<<32;
    }
    HRESULT result=adv_pixel_d3dx_load_pixels_from_pixels(dst,df,src,sf,filter,key);
    pixel_restore_status(saved); return (uint32_t)result;
}

/* Build: i686-w64-mingw32-gcc -mcrtdll=msvcrt-os -O2 -masm=intel -fno-toplevel-reorder -fno-omit-frame-pointer -fno-if-conversion -fno-if-conversion2 -mincoming-stack-boundary=2 -mpreferred-stack-boundary=6 threaded_x87_blocks.c -o threaded_x87_blocks.exe -lddraw */
#define COBJMACROS
#include <stdint.h>
#include <stdio.h>
#include <windows.h>
#include <ddraw.h>

enum { WIDTH = 1280, HEIGHT = 128, CHANNELS = 4, TILE = 8, TEAM = 8 };
enum { PITCH = WIDTH * CHANNELS, COLUMNS = WIDTH / TILE, ROWS = HEIGHT / TILE };

typedef struct { float x, y; } Point;
typedef struct {
    int width, height, channels, stride;
    const unsigned char *source;
    unsigned char *dest;
    const Point (*grid)[COLUMNS + 1];
} Frame;

/* Keep the conservative envelope of wrapped neighbors in the source allocation. */
static unsigned char source_storage[3][HEIGHT][PITCH], dest[HEIGHT][PITCH];
static unsigned char (*const source)[PITCH] = source_storage[1];
static unsigned char band[HEIGHT - 2][PITCH];
static Point grid[ROWS + 1][COLUMNS + 1];
int use_sse2 = 1;
static const double unit = 65536.0;

/* The variadic fork calls a cdecl worker with the supplied frame argument. */
static void (__cdecl *fork_team)(int, int, void (__cdecl *)(const Frame *), ...);
static void (__cdecl *static_rows)(int, int, int, int, int *, int *);
static void (__cdecl *static_end)(void);
static int (__cdecl *team_size)(void);
static int (__cdecl *thread_number)(void);
extern double block_floor(double);

__asm__(
    "  .text\n"
    "_block_trunc:\n"
    "  push ebp\n"
    "  mov ebp, esp\n"
    "  sub esp, 16\n"
    "  and esp, -8\n"
    "  cmp dword ptr [_use_sse2], 0\n"
    "  je .L_chop_x87\n"
    "  fstp qword ptr [esp]\n"
    "  cvttsd2si eax, qword ptr [esp]\n"
    "  leave\n"
    "  ret\n"
    ".L_chop_x87:\n"
    "  fnstcw [esp+8]\n"
    "  movzx eax, word ptr [esp+8]\n"
    "  or eax, 0xc00\n"
    "  mov [esp+10], ax\n"
    "  fldcw [esp+10]\n"
    "  fistp dword ptr [esp]\n"
    "  fldcw [esp+8]\n"
    "  mov eax, [esp]\n"
    "  leave\n"
    "  ret\n"
    ".globl _block_floor\n"
    "_block_floor:\n"
    "  sub esp, 8\n"
    "  cmp dword ptr [_use_sse2], 0\n"
    "  je .L_floor_x87\n"
    "  stmxcsr [esp+4]\n"
    "  mov eax, [esp+4]\n"
    "  and eax, 0x1f80\n"
    "  cmp eax, 0x1f80\n"
    "  jne .L_floor_x87\n"
    "  fnstcw [esp]\n"
    "  movzx eax, word ptr [esp]\n"
    "  and eax, 0x3f\n"
    "  cmp eax, 0x3f\n"
    "  jne .L_floor_x87\n"
    "  movsd xmm0, qword ptr [esp+12]\n"
    "  cvttsd2si eax, xmm0\n"
    "  cvtsi2sd xmm1, eax\n"
    "  ucomisd xmm0, xmm1\n"
    "  jae .L_floor_result\n"
    "  dec eax\n"
    ".L_floor_result:\n"
    "  mov [esp+4], eax\n"
    "  fild dword ptr [esp+4]\n"
    "  add esp, 8\n"
    "  ret\n"
    /* Coordinates are finite and within the signed dword range. */
    ".L_floor_x87:\n"
    "  fnstcw [esp]\n"
    "  movzx eax, word ptr [esp]\n"
    "  and eax, 0xf3ff\n"
    "  or eax, 0x400\n"
    "  mov [esp+2], ax\n"
    "  fldcw [esp+2]\n"
    "  fld qword ptr [esp+12]\n"
    "  fistp dword ptr [esp+4]\n"
    "  fldcw [esp]\n"
    "  fild dword ptr [esp+4]\n"
    "  add esp, 8\n"
    "  ret\n"
    "_block_trunc_end:\n"
);

static inline int trunc_coordinate(double coordinate)
{
    int result;
    __asm__ volatile("fld %[x]\n\tcall _block_trunc"
                     : "=a"(result) : [x] "m"(coordinate) : "cc", "memory", "st");
    return result;
}

static inline unsigned fraction(double coordinate)
{
    double remainder = coordinate - block_floor(coordinate);
    unsigned short original, chop;
    unsigned result;
    __asm__ volatile(
        "fld %[remainder]\n\t"
        "fmul %[unit]\n\t"
        "fnstcw %[original]\n\t"
        "movzx eax, %[original]\n\t"
        "or eax, 0xc00\n\t"
        "mov %[chop], ax\n\t"
        "fldcw %[chop]\n\t"
        "fistp %[result]\n\t"
        "fldcw %[original]"
        : [original] "=&m"(original), [chop] "=&m"(chop), [result] "=m"(result)
        : [remainder] "m"(remainder), [unit] "m"(unit)
        : "eax", "cc", "st");
    return result;
}

static inline void blend(unsigned char *dest, const unsigned char *source,
                         int across, int down, unsigned x, unsigned y, int channels)
{
    int diagonal = across + down;
    unsigned xy = x * y >> 16;
    /* Four byte loads feed the three difference terms of bilinear interpolation.
       Within a texel, the channel index advances each of the five streams. */
    __asm__ volatile(
        "xor ecx, ecx\n"
        "1:\n\t"
        "mov esi, %[source]\n\t"
        "add esi, ecx\n\t"
        "mov edi, %[across]\n\t"
        "movzx ebx, byte ptr [esi+edi]\n\t"
        "mov edi, %[down]\n\t"
        "movzx edx, byte ptr [esi+edi]\n\t"
        "movzx eax, byte ptr [esi]\n\t"
        "sub ebx, eax\n\t"
        "sub edx, eax\n\t"
        "mov edi, %[diagonal]\n\t"
        "movzx edi, byte ptr [esi+edi]\n\t"
        "sub edi, eax\n\t"
        "sub edi, ebx\n\t"
        "sub edi, edx\n\t"
        "imul ebx, %[x]\n\t"
        "imul edx, %[y]\n\t"
        "imul edi, %[xy]\n\t"
        "add ebx, edx\n\t"
        "add ebx, edi\n\t"
        "shr ebx, 16\n\t"
        "add al, bl\n\t"
        "mov edi, %[dest]\n\t"
        "mov byte ptr [edi+ecx], al\n\t"
        "inc ecx\n\t"
        "cmp ecx, %[channels]\n\t"
        "jl 1b"
        :
        : [source] "m"(source), [across] "m"(across), [down] "m"(down),
          [diagonal] "m"(diagonal), [x] "m"(x), [y] "m"(y), [xy] "m"(xy),
          [dest] "m"(dest), [channels] "m"(channels)
        : "eax", "ebx", "ecx", "edx", "esi", "edi", "cc", "memory");
}

static __attribute__((noinline, noclone, force_align_arg_pointer))
void warp_rows(const Frame *frame, int first, int last)
{
    int width = frame->width, height = frame->height;
    int channels = frame->channels, stride = frame->stride;
    for (int ty = first; ty <= last; ty++) {
        const Point *top = frame->grid[ty], *bottom = frame->grid[ty+1];
        unsigned char *tile = frame->dest + ty * TILE * stride;
        for (int tx = 0; tx < COLUMNS; tx++, tile += TILE * channels) {
            double dx = ((double)top[tx+1].x - top[tx].x) * (1.0 / TILE);
            double dy = ((double)bottom[tx].y - top[tx].y) * (1.0 / TILE);
            double y = top[tx].y;
            unsigned char *row = tile;
            for (int iy = 0; iy < TILE; iy++, row += stride, y += dy) {
                double x = top[tx].x;
                unsigned char *pixel = row;
                for (int ix = 0; ix < TILE; ix++, pixel += channels, x += dx) {
                    int sx = trunc_coordinate(x);
                    int sy = trunc_coordinate(y);
                    sx = (sx % width + width) % width;
                    sy = (sy % height + height) % height;
                    int across = (unsigned)(sx + 1) < (unsigned)width
                        ? channels : channels - width * channels;
                    int down = (unsigned)(sy + 1) < (unsigned)height
                        ? stride : stride - height * stride;
                    const unsigned char *sample = frame->source + sy * stride + sx * channels;
                    unsigned wx = fraction(x), wy = fraction(y);
                    blend(pixel, sample, across, down, wx, wy, channels);
                }
            }
        }
    }
}
__asm__("_warp_rows_end:");

static void __cdecl warp_team(const Frame *frame)
{
    int first, last;
    unsigned short original, binary64 = 0x27f;
    static_rows(0, ROWS - 1, 1, 1, &first, &last);
    __asm__ volatile("fnstcw %0\n\tfldcw %1" : "=m"(original) : "m"(binary64));
    warp_rows(frame, first, last);
    __asm__ volatile("fldcw %0" : : "m"(original));
    static_end();
}

static __attribute__((noinline, noclone)) void blend_band(
    unsigned char *output, const unsigned char *input, int bytes)
{
    blend(output, input, CHANNELS, PITCH, 16384, 32768, bytes);
}
__asm__("_blend_band_end:");

/* A band of already prepared samples can be split at any byte. Two peers
   first reach the same byte loop while there is local work to price. */
static void __cdecl price_band(const Frame *frame)
{
    (void)frame;
    int id = thread_number();
    if (id < 2)
        blend_band(dest[id * HEIGHT / 2], source[id * HEIGHT / 2], (HEIGHT / 2 - 2) * PITCH);
}

static int prepare_independent_band(const Frame *frame)
{
    IDirectDraw *dd;
    IDirectDrawSurface *primary;
    DDSURFACEDESC desc = {0};
    HWND window = CreateWindowExA(0, "STATIC", "Interpolation", WS_VISIBLE,
                                  0, 0, 64, 64, NULL, NULL, GetModuleHandleA(NULL), NULL);
    if (!window) return 10;
    if (FAILED(DirectDrawCreate(NULL, &dd, NULL))) return 11;
    if (FAILED(IDirectDraw_SetCooperativeLevel(dd, window, DDSCL_NORMAL))) return 12;
    desc.dwSize = sizeof(desc);
    desc.dwFlags = DDSD_CAPS;
    desc.ddsCaps.dwCaps = DDSCAPS_PRIMARYSURFACE;
    if (FAILED(IDirectDraw_CreateSurface(dd, &desc, &primary, NULL))) return 13;
    desc.dwSize = sizeof(desc);
    if (FAILED(IDirectDrawSurface_Lock(primary, NULL, &desc, DDLOCK_WAIT, NULL))) return 14;
    *(uint32_t *)desc.lpSurface = 0;
    if (FAILED(IDirectDrawSurface_Unlock(primary, NULL))) return 15;
    fork_team(1, 1, warp_team, frame);
    for (int i = 0; i < 4; i++) {
        fork_team(1, 1, price_band, frame);
        blend_band(band[0], source[0], sizeof(band));
    }
    IDirectDrawSurface_Release(primary);
    IDirectDraw_Release(dd);
    DestroyWindow(window);
    return 0;
}

static uint8_t reference(int y, int x, int c)
{
    int y1 = (y + 1) % HEIGHT, x1 = (x + 1) % WIDTH;
    unsigned a = source[HEIGHT - 1 - y][x * CHANNELS + c];
    unsigned b = source[HEIGHT - 1 - y][x1 * CHANNELS + c];
    unsigned d = source[HEIGHT - 1 - y1][x * CHANNELS + c];
    unsigned e = source[HEIGHT - 1 - y1][x1 * CHANNELS + c];
    /* The mesh adds (1/4, 1/2) to every destination coordinate. */
    return (uint8_t)((3 * a + b + 3 * d + e) / 8);
}

static int observed_team;
static void __cdecl count_team(const Frame *frame)
{
    (void)frame;
    InterlockedExchange((LONG *)&observed_team, team_size());
}

int main(void)
{
    unsigned iterations, warmup = 0;
    FILE *input = fopen("scenario-input.txt", "r");
    if (!input || fscanf(input, "%u %*u %u", &iterations, &warmup) < 1 || !iterations) return 2;
    fclose(input);

    HMODULE dll = LoadLibraryA("vcomp.dll");
    if (!dll) return 3;
    fork_team = (void *)GetProcAddress(dll, "_vcomp_fork");
    static_rows = (void *)GetProcAddress(dll, "_vcomp_for_static_simple_init");
    static_end = (void *)GetProcAddress(dll, "_vcomp_for_static_end");
    team_size = (void *)GetProcAddress(dll, "omp_get_num_threads");
    thread_number = (void *)GetProcAddress(dll, "omp_get_thread_num");
    if (!fork_team || !static_rows || !static_end || !team_size || !thread_number) return 4;
    fork_team(1, 1, count_team, NULL);
    if (observed_team != TEAM) return 5;

    for (int y = 0; y < HEIGHT; y++)
        for (int b = 0; b < PITCH; b++)
            source[y][b] = (uint8_t)(y * 11 + b * 7 + (b >> 3));
    for (int y = 0; y <= ROWS; y++)
        for (int x = 0; x <= COLUMNS; x++)
            grid[y][x] = (Point){ x * TILE + 0.25f, y * TILE + 0.5f };
    Frame frame = { WIDTH, HEIGHT, CHANNELS, -PITCH, source[HEIGHT - 1],
                    dest[HEIGHT - 1], grid };
    int status = prepare_independent_band(&frame);
    if (status) return status;
    for (unsigned i = 0; i < iterations; i++) {
        if (warmup && i == warmup)
            __asm__ volatile("int 0x2d" : : "a"(0x4849544f), "c"(0) : "memory");
        fork_team(1, 1, warp_team, &frame);
        blend_band(band[0], source[0], sizeof(band));
    }
    if (warmup)
        __asm__ volatile("int 0x2d" : : "a"(0x4849544f), "c"(1) : "memory");

    for (int y = 0; y < HEIGHT; y++)
        for (int x = 0; x < WIDTH; x++)
            for (int c = 0; c < CHANNELS; c++)
                if (dest[HEIGHT - 1 - y][x * CHANNELS + c] != reference(y, x, c))
                    return 7;
    for (int y = 0; y < HEIGHT - 2; y++)
        for (int x = 0; x < PITCH; x++) {
            int across = x + CHANNELS;
            int row = y + across / PITCH, column = across % PITCH;
            unsigned sum = 3 * source[y][x] + source[row][column]
                         + 3 * source[y + 1][x] + source[row + 1][column];
            if (band[y][x] != sum / 8) return 8;
        }
    FILE *output = fopen("threaded-x87-blocks.bin", "wb");
    if (!output) return 9;
    size_t written = fwrite(dest, 1, sizeof(dest), output);
    return fclose(output) == 0 && written == sizeof(dest) ? 0 : 10;
}

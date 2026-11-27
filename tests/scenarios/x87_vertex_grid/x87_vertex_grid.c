/* Build: i686-w64-mingw32-gcc -mcrtdll=msvcrt-os -O2 -masm=intel x87_vertex_grid.c -o x87_vertex_grid.exe */
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

enum { COLS = 64, ROWS = 2, CALLS = 180, STREAMS = 8 };
typedef struct { uint8_t sample[16]; } Corners;
typedef struct { uint32_t stream[STREAMS][ROWS][COLS]; } Grid;
typedef void Kernel(Grid *, Corners, unsigned);
extern Kernel grid_one, grid_two, grid_chop, grid_wide;
extern Kernel grid_down, grid_up, grid_call, grid_single, grid_heap;

__asm__(
    /* EBP retains the arguments. ESP holds the frame and, in the stack
       variants, the eight output streams. */
    ".macro convert mode, width, saved, temp, reordered\n"
    "  .if \\reordered\n"
    "    movzx eax, word ptr [esp+\\saved]\n"
    "    and eax, 0xf3ff\n"
    "    or eax, \\mode\n"
    "    mov word ptr [esp+\\temp], ax\n"
    "  .else\n"
    "    movzx eax, word ptr [esp+\\saved]\n"
    "    or eax, \\mode\n"
    "    mov dword ptr [esp+\\temp], eax\n"
    "  .endif\n"
    "  fldcw word ptr [esp+\\temp]\n"
    "  fistp \\width ptr [esp+\\temp]\n"
    "  .if \\reordered\n"
    "    fldcw word ptr [esp+\\saved]\n"
    "  .endif\n"
    "  movzx eax, byte ptr [esp+\\temp]\n"
    ".endm\n"

    "grid_convert:\n"
    "  sub esp, 16\n"
    "  fnstcw word ptr [esp+2]\n"
    "  convert 0xc00, dword, 2, 8, 1\n"
    "  add esp, 16\n"
    "  ret\n"
    "grid_convert_end:\n"

    ".macro grid name, mode, width, saved, temp, reordered=0, helper=0, single=0, heap=0\n"
    "  .globl _\\name\n"
    "_\\name:\n"
    "  push ebp\n"
    "  mov ebp, esp\n"
    "  push ebx\n"
    "  push esi\n"
    "  push edi\n"
    "  sub esp, 0x1400\n"
    "  mov ebx, dword ptr [ebp+28]\n"
    "  mov eax, dword ptr [ebp+8]\n"
    "  mov dword ptr [esp+12], eax\n"
    "  mov dword ptr [esp+4], 2\n"
    /* Binary64 scale, divisor and start, followed by two binary32 steps. */
    "  mov dword ptr [esp+0x100], 0\n"
    "  mov dword ptr [esp+0x104], 0x3ff00000\n"
    "  mov dword ptr [esp+0x108], 0\n"
    "  mov dword ptr [esp+0x10c], 0x40200000\n"
    "  mov dword ptr [esp+0x110], 0\n"
    "  mov dword ptr [esp+0x114], 0xc0404000\n"
    "  mov dword ptr [esp+0x118], 0x3c800000\n"
    "  mov dword ptr [esp+0x11c], 0x3f000000\n"
    "  .if \\single\n"
    "    fnstcw word ptr [esp+0x1f0]\n"
    "    movzx eax, word ptr [esp+0x1f0]\n"
    "    and eax, 0xfcff\n"
    "    mov word ptr [esp+0x1f2], ax\n"
    "    fldcw word ptr [esp+0x1f2]\n"
    "  .endif\n"
    "  .irp k,0,1,2,3,4,5,6,7\n"
    "    movzx eax, byte ptr [ebp+12+\\k]\n"
    "    movzx edx, byte ptr [ebp+20+\\k]\n"
    "    sub edx, eax\n"
    "    mov dword ptr [esp+0x18], edx\n"
    "    fild dword ptr [esp+0x18]\n"
    "    fstp qword ptr [esp+0x20+8*\\k]\n"
    "    mov dword ptr [esp+0x18], eax\n"
    "    fild dword ptr [esp+0x18]\n"
    "    fstp dword ptr [esp+0x60+4*\\k]\n"
    "  .endr\n"
    "  xor edi, edi\n"
    "  xor esi, esi\n"
    "  fldz\n"
    "  fstp dword ptr [esp+0x10]\n"
    "  fld dword ptr [esp+0x118]\n"
    "  fld dword ptr [esp+0x11c]\n"
    "  fld1\n"
    "  fld1\n"
    "  .if \\heap\n"
    "    mov ebp, dword ptr [esp+12]\n"
    "  .endif\n"
    "  .globl _\\name\\()_row\n"
    "_\\name\\()_row:\n"
    "  fld dword ptr [esp+0x10]\n"
    "  fmul qword ptr [esp+0x100]\n"
    "  fdiv qword ptr [esp+0x108]\n"
    "  fstp dword ptr [esp+0x14]\n"
    "  fld dword ptr [esp+0x14]\n"
    "  .irp k,0,1,2,3,4,5,6,7\n"
    "    fld st(0)\n"
    "    fmul dword ptr [esp+0x60+4*\\k]\n"
    "    fadd qword ptr [esp+0x110]\n"
    "    fstp dword ptr [esp+0x80+4*\\k]\n"
    "    fld st(0)\n"
    "    fmul qword ptr [esp+0x20+8*\\k]\n"
    "    fadd qword ptr [esp+0x110]\n"
    "    fstp dword ptr [esp+0xa0+4*\\k]\n"
    "    fld dword ptr [esp+0xa0+4*\\k]\n"
    "    fsub dword ptr [esp+0x80+4*\\k]\n"
    "    fstp qword ptr [esp+0xc0+8*\\k]\n"
    "  .endr\n"
    "  fldz\n"
    "  xor edx, edx\n"
    "  mov ecx, edi\n"
    "  shl ecx, 8\n"
    "  .globl _\\name\\()_col\n"
    "_\\name\\()_col:\n"
    "  fld st(0)\n"
    "  .if \\heap\n"
    "    fstp dword ptr [ebp+ecx]\n"
    "    fld st(1)\n"
    "    fstp dword ptr [ebp+ecx+0x200]\n"
    "  .else\n"
    "    fstp dword ptr [esp+ecx+0x400]\n"
    "    fld st(1)\n"
    "    fstp dword ptr [esp+ecx+0x600]\n"
    "  .endif\n"
    "  .irp k,0,1,2,3,4,5\n"
    "    fld qword ptr [esp+0xc0+8*\\k]\n"
    "    fmul st, st(1)\n"
    "    .if \\helper\n"
    "      fadd dword ptr [esp+0x80+4*\\k]\n"
    "      call grid_convert\n"
    "    .else\n"
    "      .if \\reordered\n"
    "        fadd dword ptr [esp+0x80+4*\\k]\n"
    "      .endif\n"
    "      fnstcw word ptr [esp+\\saved]\n"
    "      .if !\\reordered\n"
    "        fadd dword ptr [esp+0x80+4*\\k]\n"
    "      .endif\n"
    "      convert \\mode, \\width, \\saved, \\temp, \\reordered\n"
    "    .endif\n"
    "    .if \\heap\n"
    "      mov byte ptr [ebp+ecx+0x400+0x200*\\k], al\n"
    "    .else\n"
    "      mov byte ptr [esp+ecx+0x800+0x200*\\k], al\n"
    "    .endif\n"
    "    .if !\\helper && !\\reordered\n"
    "      fldcw word ptr [esp+\\saved]\n"
    "    .endif\n"
    "  .endr\n"
    "  fst dword ptr [esp+0x200+edx*4]\n"
    "  fld dword ptr [esp+0x200+edx*4]\n"
    "  fsub dword ptr [esp+0x200+edx*4]\n"
    "  faddp\n"
    "  fadd st, st(5)\n"
    "  fstp dword ptr [esp+0x1e0]\n"
    "  fld dword ptr [esp+0x1e0]\n"
    "  add ecx, 4\n"
    "  inc edx\n"
    "  cmp edx, ebx\n"
    "  jl _\\name\\()_col\n"
    "_\\name\\()_col_end:\n"
    "  fstp st(0)\n"
    "  fstp st(0)\n"
    "  fld dword ptr [esp+0x10]\n"
    "  fst dword ptr [esp+esi+0x300]\n"
    "  fsub dword ptr [esp+esi+0x300]\n"
    "  fadd dword ptr [esp+0x10]\n"
    "  fadd st, st(3)\n"
    "  fstp dword ptr [esp+0x10]\n"
    "  add esi, 0x44\n"
    "  inc edi\n"
    "  cmp edi, dword ptr [esp+4]\n"
    "  jl _\\name\\()_row\n"
    "_\\name\\()_row_end:\n"
    "  fucompp\n"
    "  fnstsw ax\n"
    "  test ah, 0x44\n"
    "  jnp .Lordered\\@\n"
    "  ud2\n"
    ".Lordered\\@:\n"
    "  fstp st(0)\n"
    "  fstp st(0)\n"
    "  .if \\single\n"
    "    fldcw word ptr [esp+0x1f0]\n"
    "  .endif\n"
    "  .if !\\heap\n"
    "    lea esi, [esp+0x400]\n"
    "    mov edi, dword ptr [esp+12]\n"
    "    mov ecx, 0x400\n"
    "    rep movsd\n"
    "  .endif\n"
    "  add esp, 0x1400\n"
    "  pop edi\n"
    "  pop esi\n"
    "  pop ebx\n"
    "  pop ebp\n"
    "  ret\n"
    "_\\name\\()_end:\n"
    ".endm\n"
    "grid grid_one, 0xc00, dword, 0x1c0, 0x1c4\n"
    "grid grid_two, 0xc00, dword, 0x1c0, 0x1c4\n"
    "grid grid_chop, 0xc00, dword, 0x1c0, 0x1c4\n"
    "grid grid_wide, 0xc00, dword, 0x1c0, 0x1c4\n"
    "grid grid_down, 0x400, word, 0x1ca, 0x1d0, 1\n"
    "grid grid_up, 0x800, qword, 0x1bc, 0x1b0, 1\n"
    "grid grid_call, 0xc00, dword, 0, 0, 0, 1\n"
    "grid grid_single, 0xc00, dword, 0x1c8, 0x1d0, 1, 0, 1\n"
    "grid grid_heap, 0xc00, dword, 0x1c0, 0x1c4, 0, 0, 0, 1\n"
);

static uint32_t float_bits(float value)
{
    uint32_t bits;
    memcpy(&bits, &value, sizeof(bits));
    return bits;
}

static int expected(Corners c, unsigned k, unsigned row, unsigned col, unsigned rc)
{
    /* Exact numerator in units of 1/1024. The chosen operands and all
       intermediate values fit within 24 significant binary digits. */
    int a = c.sample[k], b = c.sample[k + 8], x = (int)col;
    int numerator = -33280 + (int)row * (64 * a + x * (b - 2 * a));
    int q = numerator / 1024, rem = numerator % 1024;
    return q - (rc == 1 && rem < 0) + (rc == 2 && rem > 0);
}

static int verify(FILE *out, const Grid *grid, Corners c, unsigned cols, unsigned rc)
{
    for (unsigned k = 0; k < STREAMS; k++)
        for (unsigned r = 0; r < ROWS; r++)
            for (unsigned x = 0; x < cols; x++) {
                unsigned n = k < 2 ? 4 : 1;
                uint32_t got = 0;
                memcpy(&got, &grid->stream[k][r][x], n);
                uint32_t want = k == 0 ? float_bits((float)x / 64)
                              : k == 1 ? float_bits((float)r / 16)
                              : (uint8_t)expected(c, k - 2, r, x, rc);
                if (got != want ||
                    fwrite(&got, n, 1, out) != 1)
                    return 0;
            }
    return 1;
}

int main(void)
{
    static const struct { Kernel *run; unsigned cols, rc; } variants[] = {
        {grid_one, 1, 3}, {grid_two, 2, 3}, {grid_chop, 17, 3}, {grid_wide, 64, 3},
        {grid_down, 17, 1}, {grid_up, 17, 2}, {grid_call, 17, 3},
        {grid_single, 17, 3}, {grid_heap, 17, 3},
    };
    unsigned iterations, selected = 0, warmup = 0;
    FILE *input = fopen("scenario-input.txt", "r");
    if (!input || fscanf(input, "%u %u %u", &iterations, &selected, &warmup) < 1 ||
        !iterations || selected > sizeof(variants) / sizeof(variants[0])) return 2;
    fclose(input);
    FILE *out = fopen("x87-vertex-grid.bin", "wb");
    Grid *grid = malloc(sizeof(*grid));
    if (!out || !grid) return 3;
    const uint16_t cw = 0x027f;
    __asm__ volatile("fldcw %0" : : "m"(cw));
    for (unsigned v = 0; v < sizeof(variants) / sizeof(variants[0]); v++) {
        if (selected && selected != v + 1) continue;
        Corners c;
        for (unsigned frame = 0; frame < iterations; frame++) {
            if (warmup && frame == warmup)
                __asm__ volatile("int 0x2d" : : "a"(0x4849544f), "c"(2 * v) : "memory");
            for (unsigned call = 0; call < CALLS; call++) {
                unsigned seed = call + v;
                for (unsigned i = 0; i < sizeof(c.sample); i++)
                    c.sample[i] = (uint8_t)(seed * 3 + i * 29);
                variants[v].run(grid, c, variants[v].cols);
            }
        }
        if (warmup)
            __asm__ volatile("int 0x2d" : : "a"(0x4849544f), "c"(2 * v + 1) : "memory");
        if (!verify(out, grid, c, variants[v].cols, variants[v].rc)) return 4 + v;
    }
    free(grid);
    return fclose(out) == 0 ? 0 : 20;
}

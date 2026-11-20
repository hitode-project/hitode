/* Build: i686-w64-mingw32-gcc -mcrtdll=msvcrt-os -O2 generated_code_rounds.c -o generated_code_rounds.exe */
/* Each round writes FUNCTIONS distinct straight-line functions into one
 * executable buffer and calls each once, so every round replaces the whole
 * decoded code set. Memory kept per decoded unit shows in the peak.
 * scenario-input.txt holds the number of rounds; generated-code-rounds.bin
 * holds the sum of all results. Exit status: 0 when the sum matches. */
#include <windows.h>
#include <stdio.h>
#include <string.h>

enum { FUNCTIONS = 20000, STRIDE = 64 };

static unsigned char *emit(unsigned char *at, unsigned value) {
    unsigned k;
    *at++ = 0xb8; /* mov eax, value */
    memcpy(at, &value, 4);
    at += 4;
    for (k = 0; k < 6; ++k) {
        unsigned step = value * 2654435761u + k;
        *at++ = 0x35; /* xor eax, step */
        memcpy(at, &step, 4);
        at += 4;
        *at++ = 0xc1; /* rol eax, 3 */
        *at++ = 0xc0;
        *at++ = 0x03;
    }
    *at++ = 0xc3; /* ret */
    return at;
}

static unsigned expected(unsigned value) {
    unsigned eax = value, k;
    for (k = 0; k < 6; ++k) {
        eax ^= value * 2654435761u + k;
        eax = eax << 3 | eax >> 29;
    }
    return eax;
}

int main(void) {
    unsigned rounds = 2, round, i, sum = 0, want = 0;
    unsigned char *code = VirtualAlloc(NULL, FUNCTIONS * STRIDE, MEM_COMMIT | MEM_RESERVE,
                                       PAGE_EXECUTE_READWRITE);
    FILE *input = fopen("scenario-input.txt", "r"), *output;
    if (!code) return 3;
    if (input) {
        if (fscanf(input, "%u", &rounds) != 1) return 4;
        fclose(input);
    }
    for (round = 0; round < rounds; ++round) {
        for (i = 0; i < FUNCTIONS; ++i) emit(code + i * STRIDE, round * FUNCTIONS + i);
        FlushInstructionCache(GetCurrentProcess(), code, FUNCTIONS * STRIDE);
        for (i = 0; i < FUNCTIONS; ++i) {
            sum += ((unsigned (*)(void))(code + i * STRIDE))();
            want += expected(round * FUNCTIONS + i);
        }
    }
    output = fopen("generated-code-rounds.bin", "wb");
    if (!output || fwrite(&sum, 4, 1, output) != 1) return 5;
    fclose(output);
    return sum == want ? 0 : 6;
}

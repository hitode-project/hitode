/* Build: i686-w64-mingw32-gcc -mcrtdll=msvcrt-os -O2 -masm=intel virtual_call_loop.c -o virtual_call_loop.exe */
/* A hot loop calls a virtual method, `mov eax,[esi]; mov ecx,esi;
 * call [eax+4]`, whose return value leaves EAX pointing at the stack. At the
 * loop head, [eax+4] holds a stack address, so a call target guessed from
 * the head's registers would name the stack. Ordinary stack stores must
 * stay off the checked path in the steady state.
 * scenario-input.txt holds the iteration count; virtual-call-loop.bin holds
 * the count the method observed. Exit status: 0 when it matches. */
#include <windows.h>
#include <stdio.h>

typedef struct Object {
    void **vtable;
    unsigned calls;
} Object;

void method(void);
unsigned spin(Object *object, unsigned iterations);

__asm__(
    ".globl _method\n"
    "_method:\n"
    "  inc dword ptr [ecx+4]\n"
    "  mov eax, esp\n"
    "  ret\n"
    ".globl _spin\n"
    "_spin:\n"
    "  push ebx\n"
    "  push esi\n"
    "  push edi\n"
    "  mov esi, [esp+16]\n"
    "  mov edi, [esp+20]\n"
    "  push esp\n"
    "1:\n"
    "  mov eax, [esi]\n"
    "  mov ecx, esi\n"
    "  call [eax+4]\n"
    "  dec edi\n"
    "  jnz 1b\n"
    "  add esp, 4\n"
    "  mov eax, [esi+4]\n"
    "  pop edi\n"
    "  pop esi\n"
    "  pop ebx\n"
    "  ret\n");

int main(void) {
    static void *vtable[2] = {0, (void *)method};
    Object object = {vtable, 0};
    unsigned iterations = 100000, calls;
    FILE *input = fopen("scenario-input.txt", "r"), *output;
    if (input) {
        if (fscanf(input, "%u", &iterations) != 1) return 3;
        fclose(input);
    }
    calls = spin(&object, iterations);
    output = fopen("virtual-call-loop.bin", "wb");
    if (!output || fwrite(&calls, 4, 1, output) != 1) return 4;
    fclose(output);
    return calls == iterations ? 0 : 5;
}

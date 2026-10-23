/* Wine loads host exports from a generated version.dll, including its IAT.
 * Build: i686-w64-mingw32-gcc -O1 -mcrtdll=msvcrt-os -o builtin.exe builtin.c */
#include <windows.h>
#include <stdio.h>
#include <string.h>

static int iat_calls, entry_calls;
static int WINAPI iat_hook(const char *text)
{
    ++iat_calls;
    return !strcmp(text, "wine") ? 31 : 0;
}
static int WINAPI entry_hook(const char *text)
{
    ++entry_calls;
    return !strcmp(text, "wine") ? 51 : 0;
}

static ULONG_PTR *find_slot(HMODULE module)
{
    BYTE *base = (BYTE *)module;
    IMAGE_DOS_HEADER *dos = (void *)base;
    IMAGE_NT_HEADERS *nt = (void *)(base + dos->e_lfanew);
    IMAGE_IMPORT_DESCRIPTOR *desc = (void *)(base + nt->OptionalHeader.DataDirectory[IMAGE_DIRECTORY_ENTRY_IMPORT].VirtualAddress);
    for (; desc->Name; ++desc)
    {
        IMAGE_THUNK_DATA *names = (void *)(base + desc->OriginalFirstThunk);
        ULONG_PTR *slots = (void *)(base + desc->FirstThunk);
        if (lstrcmpiA((char *)base + desc->Name, "kernel32.dll")) continue;
        for (; names->u1.AddressOfData; ++names, ++slots)
        {
            IMAGE_IMPORT_BY_NAME *name;
            if (IMAGE_SNAP_BY_ORDINAL(names->u1.Ordinal)) continue;
            name = (void *)(base + names->u1.AddressOfData);
            if (!strcmp((char *)name->Name, "lstrlenA")) return slots;
        }
    }
    return NULL;
}

int main(void)
{
    /* Occupy the generated image's preferred base to exercise Wine rebasing. */
    void *reserved = VirtualAlloc((void *)0x10000000, 0x10000, MEM_RESERVE, PAGE_NOACCESS);
    HMODULE module = LoadLibraryA("version.dll");
    DWORD (WINAPI *compute)(DWORD, DWORD, DWORD, DWORD);
    int (WINAPI *imported)(const char *);
    FARPROC ordinal;
    ULONG_PTR *slot;
    ULONG_PTR target;
    DWORD protect, ignored;
    BYTE saved[5], jump[5] = {0xe9};
    int names, same, rebased, host = 0, original = 0, patched = 0, entry = 0;
    if (!module) return 2;
    compute = (void *)GetProcAddress(module, "HostCompute");
    imported = (void *)GetProcAddress(module, "HostImport");
    ordinal = GetProcAddress(module, (const char *)7);
    names = compute && imported;
    same = ordinal && ordinal == (FARPROC)compute && GetProcAddress(module, (const char *)11) == (FARPROC)imported;
    rebased = (ULONG_PTR)module != 0x10000000;
    slot = find_slot(module);
    if (!names || !same || !slot) return 3;
    host = compute(1, 2, 3, 4);
    original = imported("wine");
    target = *slot;
    if (!VirtualProtect(slot, sizeof(*slot), PAGE_READWRITE, &protect)) return 4;
    *slot = (ULONG_PTR)iat_hook;
    patched = imported("wine");
    *slot = target;
    VirtualProtect(slot, sizeof(*slot), protect, &ignored);
    if (!VirtualProtect((void *)target, sizeof(saved), PAGE_EXECUTE_READWRITE, &protect)) return 5;
    memcpy(saved, (void *)target, sizeof(saved));
    *(DWORD *)(jump + 1) = (DWORD)((BYTE *)entry_hook - (BYTE *)target - 5);
    memcpy((void *)target, jump, sizeof(jump));
    FlushInstructionCache(GetCurrentProcess(), (void *)target, sizeof(jump));
    entry = imported("wine");
    memcpy((void *)target, saved, sizeof(saved));
    FlushInstructionCache(GetCurrentProcess(), (void *)target, sizeof(saved));
    VirtualProtect((void *)target, sizeof(saved), protect, &ignored);
    FreeLibrary(module);
    if (reserved) VirtualFree(reserved, 0, MEM_RELEASE);
    printf("names=%d ordinal=%d host=%d import=%d iat=%d entry=%d rebased=%d\n",
           names, same, host, original, patched, entry, rebased);
    return names && same && host == 1234 && original == 14 && patched == 41
                   && entry == 61 && iat_calls == 1 && entry_calls == 1 && rebased ? 0 : 1;
}

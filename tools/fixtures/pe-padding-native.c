/* Authored PE32 DLLs and bounded mapping probes; no host COM, device or game. */
#define WIN32_LEAN_AND_MEAN
#include <windows.h>

static DWORD sequence;
static void emit(const char *name, DWORD vs, DWORD raw, DWORD next, DWORD size, DWORD loaded, DWORD error) {
    char text[512]; DWORD written;
    int length = wsprintfA(text, "{\"seq\":%u,\"op\":\"load\",\"case\":\"%s\",\"vs\":%u,\"raw\":%u,\"next\":%u,\"size\":%u,\"loaded\":%u,\"error\":%u}\n", ++sequence, name, vs, raw, next, size, loaded, error);
    if (!WriteFile(GetStdHandle(STD_OUTPUT_HANDLE), text, length, &written, NULL) || written != length) ExitProcess(90);
}
static void probe(const char *name, BYTE *base, DWORD offset) {
    MEMORY_BASIC_INFORMATION information; DWORD value = 0, executed = 0, written;
    SIZE_T read = 0; BYTE code[6]; char text[512];
    DWORD queried = VirtualQuery(base + offset, &information, sizeof(information));
    BOOL readable = queried && information.AllocationBase == base && information.State == MEM_COMMIT &&
        ReadProcessMemory(GetCurrentProcess(), base + offset, &value, sizeof(value), &read) && read == sizeof(value);
    if (!queried) { information.State = 0; information.Protect = 0; }
    if (queried && information.AllocationBase == base && information.State == MEM_COMMIT &&
        (information.Protect & (PAGE_EXECUTE | PAGE_EXECUTE_READ | PAGE_EXECUTE_READWRITE | PAGE_EXECUTE_WRITECOPY)) &&
        ReadProcessMemory(GetCurrentProcess(), base + offset, code, sizeof(code), &read) && read == sizeof(code) &&
        code[0] == 0xb8 && code[1] == 0x44 && code[2] == 0x33 && code[3] == 0x22 && code[4] == 0x11 && code[5] == 0xc3) {
        typedef DWORD (__stdcall *Code)(void);
        executed = ((Code)(base + offset))();
    }
    int length = wsprintfA(text, "{\"seq\":%u,\"op\":\"probe\",\"case\":\"%s\",\"offset\":%u,\"state\":%u,\"protect\":%u,\"read\":%u,\"value\":%u,\"executed\":%u}\n", ++sequence, name, offset, information.State, information.Protect, readable, value, executed);
    if (!WriteFile(GetStdHandle(STD_OUTPUT_HANDLE), text, length, &written, NULL) || written != length) ExitProcess(91);
}
static void test(DWORD index, const char *name, DWORD vs, DWORD raw, DWORD next, DWORD image_size) {
    DWORD file_size = 0x200 + raw + (next ? 0x200 : 0);
    BYTE *file = HeapAlloc(GetProcessHeap(), HEAP_ZERO_MEMORY, file_size);
    if (!file) ExitProcess(92);
    IMAGE_DOS_HEADER *dos = (IMAGE_DOS_HEADER *)file;
    IMAGE_NT_HEADERS32 *nt = (IMAGE_NT_HEADERS32 *)(file + 0x80);
    dos->e_magic = IMAGE_DOS_SIGNATURE; dos->e_lfanew = 0x80;
    nt->Signature = IMAGE_NT_SIGNATURE;
    nt->FileHeader.Machine = IMAGE_FILE_MACHINE_I386;
    nt->FileHeader.NumberOfSections = (WORD)(next ? 2 : 1);
    nt->FileHeader.SizeOfOptionalHeader = sizeof(IMAGE_OPTIONAL_HEADER32);
    nt->FileHeader.Characteristics = IMAGE_FILE_EXECUTABLE_IMAGE | IMAGE_FILE_32BIT_MACHINE | IMAGE_FILE_DLL | IMAGE_FILE_RELOCS_STRIPPED;
    nt->OptionalHeader.Magic = IMAGE_NT_OPTIONAL_HDR32_MAGIC;
    nt->OptionalHeader.ImageBase = 0x32000000 + index * 0x100000;
    nt->OptionalHeader.SectionAlignment = 0x1000; nt->OptionalHeader.FileAlignment = 0x200;
    nt->OptionalHeader.MajorOperatingSystemVersion = 6; nt->OptionalHeader.MajorSubsystemVersion = 6;
    nt->OptionalHeader.SizeOfImage = image_size; nt->OptionalHeader.SizeOfHeaders = 0x200;
    nt->OptionalHeader.Subsystem = IMAGE_SUBSYSTEM_WINDOWS_CUI;
    nt->OptionalHeader.SizeOfStackReserve = 0x100000; nt->OptionalHeader.SizeOfStackCommit = 0x1000;
    nt->OptionalHeader.SizeOfHeapReserve = 0x100000; nt->OptionalHeader.SizeOfHeapCommit = 0x1000;
    nt->OptionalHeader.NumberOfRvaAndSizes = IMAGE_NUMBEROF_DIRECTORY_ENTRIES;
    IMAGE_SECTION_HEADER *sections = IMAGE_FIRST_SECTION(nt);
    sections[0].Name[0] = '.'; sections[0].Name[1] = 't';
    sections[0].Misc.VirtualSize = vs; sections[0].VirtualAddress = 0x1000;
    sections[0].SizeOfRawData = raw; sections[0].PointerToRawData = 0x200;
    sections[0].Characteristics = IMAGE_SCN_CNT_CODE | IMAGE_SCN_MEM_EXECUTE | IMAGE_SCN_MEM_READ;
    for (DWORD at = 0; at < raw; ++at) file[0x200 + at] = (BYTE)(at < 0x1000 ? 0xcd : 0xab);
    for (DWORD at = 0x180; at < raw; at += 0x1000) {
        if (at + 6 <= raw) {
            BYTE *code = file + 0x200 + at;
            code[0] = 0xb8; code[1] = 0x44; code[2] = 0x33; code[3] = 0x22; code[4] = 0x11; code[5] = 0xc3;
        }
    }
    if (next) {
        sections[1].Name[0] = '.'; sections[1].Name[1] = 'd';
        sections[1].Misc.VirtualSize = 0x100; sections[1].VirtualAddress = next;
        sections[1].SizeOfRawData = 0x200; sections[1].PointerToRawData = 0x200 + raw;
        sections[1].Characteristics = IMAGE_SCN_CNT_INITIALIZED_DATA | IMAGE_SCN_MEM_READ;
        for (DWORD at = 0; at < 0x200; ++at) file[0x200 + raw + at] = 0x5a;
    }
    HANDLE output = CreateFileA(name, GENERIC_WRITE, 0, NULL, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, NULL);
    DWORD written;
    if (output == INVALID_HANDLE_VALUE || !WriteFile(output, file, file_size, &written, NULL) || written != file_size) ExitProcess(93);
    CloseHandle(output); HeapFree(GetProcessHeap(), 0, file);
    char path[MAX_PATH];
    if (!GetFullPathNameA(name, sizeof(path), path, NULL)) ExitProcess(94);
    SetLastError(0); HMODULE module = LoadLibraryA(path); DWORD error = GetLastError();
    emit(name, vs, raw, next, image_size, module != NULL, error);
    if (module) {
        DWORD offsets[] = { 0x1000, 0x1100, 0x1101, 0x1180, 0x1200, 0x1ff0, 0x2000, 0x2180, 0x2ff0, 0x3000 };
        for (DWORD at = 0; at < sizeof(offsets) / sizeof(offsets[0]); ++at) probe(name, (BYTE *)module, offsets[at]);
        if (next) probe(name, (BYTE *)module, next);
        if (!FreeLibrary(module)) ExitProcess(95);
    }
}
void mainCRTStartup(void) {
    test(0, "same-page.dll", 0x101, 0x200, 0x2000, 0x3000);
    test(1, "cross-gap.dll", 0x101, 0x2000, 0x4000, 0x5000);
    test(2, "cross-adjacent.dll", 0x101, 0x2000, 0x2000, 0x3000);
    test(3, "zero-vsize.dll", 0, 0x2000, 0x3000, 0x4000);
    test(4, "zero-fill.dll", 0x1800, 0x200, 0x3000, 0x4000);
    test(5, "raw-outside-image.dll", 0x101, 0x2000, 0, 0x2000);
    test(6, "virtual-outside-image.dll", 0x1800, 0x200, 0, 0x2000);
    emit("complete", 0, 0, 0, 0, 1, 0);
    ExitProcess(0);
}

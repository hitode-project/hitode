/* Authored x86 startup contracts. No CRT startup, games, windows or sound. */
#include <windows.h>
static DWORD words[2048], used;
static void record(DWORD word) { if (used < 2048) words[used++] = word; else ExitProcess(90); }
static void dump(void) {
    DWORD written; HANDLE file = CreateFileA("startup-contract.bin", GENERIC_WRITE, 0, 0, CREATE_ALWAYS, 0, 0);
    if (file == INVALID_HANDLE_VALUE || !WriteFile(file, words, used * 4, &written, 0)) ExitProcess(91);
    CloseHandle(file);
}
typedef unsigned (__cdecl *Control)(unsigned, unsigned);
typedef int (__cdecl *Secure)(unsigned *, unsigned, unsigned);
typedef int (__cdecl *Separate)(unsigned, unsigned, unsigned *, unsigned *);
typedef void (__cdecl *Invalid)(const WCHAR *, const WCHAR *, const WCHAR *, unsigned, ULONG_PTR);
typedef Invalid (__cdecl *SetInvalid)(Invalid);
static DWORD invalid_calls;
static void __cdecl invalid(const WCHAR *expression, const WCHAR *function, const WCHAR *file, unsigned line, ULONG_PTR reserved) {
    record(!expression && !function && !file && line == 0 && reserved == 0); ++invalid_calls; Sleep(0);
}
static void reset(WORD cw, DWORD mx) { __asm__ volatile("fldcw %0\n\tldmxcsr %1" : : "m"(cw), "m"(mx)); }
static void floating(void) {
    WORD cw; DWORD mx; __asm__ volatile("fnstcw %0\n\tstmxcsr %1" : "=m"(cw), "=m"(mx)); record(cw); record(mx);
}
static void full_w(const WCHAR *path) {
    WCHAR out[512], *part = (WCHAR *)1; DWORD result;
    record(GetFullPathNameW(path, 0, 0, 0));
    SetLastError(0x1234); result = GetFullPathNameW(path, 512, out, &part);
    record(result); record(GetLastError());
    if (result) { record(part ? (DWORD)(part - out) : 0xffffffff); for (DWORD i = 0; i <= result; ++i) record(out[i]); }
}
static void full_a(const char *path) {
    char out[512], *part = (char *)1; DWORD result;
    record(GetFullPathNameA(path, 0, 0, 0));
    SetLastError(0x1234); result = GetFullPathNameA(path, 512, out, &part);
    record(result); record(GetLastError());
    if (result) { record(part ? (DWORD)(part - out) : 0xffffffff); for (DWORD i = 0; i <= result; ++i) record((BYTE)out[i]); }
}
static void temporary(void) {
    WCHAR wide[512], windows[512]; char ansi[512];
    record(SetEnvironmentVariableA("TMP", "C:\\startup-tmp"));
    record(SetEnvironmentVariableW(L"TEMP", L"C:\\startup-temp"));
    record(SetEnvironmentVariableW(L"USERPROFILE", L"C:\\startup-profile"));
    record(GetTempPathW(512, wide)); record(wide[3]=='s' && wide[11]=='t' && wide[13]=='p' && wide[14]=='\\' && wide[15]==0);
    record(GetTempPathA(512, ansi)); record(ansi[14]=='\\' && ansi[15]==0);
    record(GetTempPathW(0,0)); record(GetTempPathA(0,0));
    record(SetEnvironmentVariableW(L"TMP", L""));
    wide[0]=0x7777; wide[1]=0x8888; SetLastError(0x1234); record(GetTempPathW(512, wide)); record(wide[0]); record(wide[1]); record(GetLastError());
    record(SetEnvironmentVariableA("TMP", 0));
    record(GetTempPathW(512, wide)); record(wide[14]=='p' && wide[15]=='\\' && wide[16]==0);
    record(SetEnvironmentVariableA("TEMP", 0));
    record(GetTempPathW(512, wide)); record(wide[17]=='e' && wide[18]=='\\' && wide[19]==0);
    record(SetEnvironmentVariableW(L"USERPROFILE", 0));
    DWORD wn=GetWindowsDirectoryW(windows,512), n=GetTempPathW(512,wide);
    record(n==wn+1); DWORD equal=1; for (DWORD i=0;i<wn;++i) if (wide[i]!=windows[i]) equal=0;
    record(equal && wide[wn]=='\\' && wide[wn+1]==0);
    record(SetEnvironmentVariableW(L"TMP", L"one\\..\\two"));
    n=GetTempPathW(512,wide); WCHAR absolute[512]; DWORD an=GetFullPathNameW(L"one\\..\\two\\",512,absolute,0);
    equal=n==an; for (DWORD i=0;i<n && i<an;++i) if (wide[i]!=absolute[i]) equal=0; record(equal);
    record(wide[0]=='o' && wide[3]=='\\' && wide[4]=='.');
    record(SetEnvironmentVariableW(L"TMP",0));
}
static void lifecycle(void) {
    const WCHAR *source=L"startup_ソ\\source_ソ.dat", *copy=L"startup_ソ\\copy_ソ.dat", *target=L"startup_ソ\\moved_ソ.dat";
    record(CreateDirectoryW(L"startup_ソ",0)); DWORD written=0, value=0x1234abcd;
    HANDLE file=CreateFileW(source,GENERIC_WRITE,0,0,CREATE_NEW,0,0);
    record(file!=INVALID_HANDLE_VALUE); record(WriteFile(file,&value,4,&written,0) && written==4); record(CloseHandle(file));
    record(CopyFileW(source,copy,TRUE)); record(CopyFileW(source,copy,TRUE)); record(GetLastError());
    record(MoveFileW(copy,target)); record(GetFileAttributesW(copy)==INVALID_FILE_ATTRIBUTES);
    record(RemoveDirectoryW(L"startup_ソ")); record(GetLastError());
    file=CreateFileW(target,GENERIC_READ,FILE_SHARE_READ,0,OPEN_EXISTING,0,0); value=0;
    record(file!=INVALID_HANDLE_VALUE); record(ReadFile(file,&value,4,&written,0) && written==4 && value==0x1234abcd); record(CloseHandle(file));
    record(DeleteFileW(source)); record(DeleteFileW(target)); record(RemoveDirectoryW(L"startup_ソ"));
    record(CreateDirectoryA("startup-ascii",0)); record(RemoveDirectoryA("startup-ascii"));
}
void mainCRTStartup(void) {
    full_w(L"c:/Game/one/../日本ソ.txt");
    full_w(L"C:\\Game\\folder\\"); full_w(L"C:\\Game\\."); full_w(L"C:\\Game\\..");
    full_w(L"C:\\Game\\a... "); full_w(L"\\\\server\\share\\one\\..\\name");
    full_w(L"\\\\?\\C:\\one\\..\\name"); full_w(L"");
    full_a("c:/Game/one/../name.txt"); full_a("C:\\Game\\\x83\x5c.txt");
    // One directory owner: A/W change and query the same process cwd.
    record(CreateDirectoryW(L"startup_ソ", 0)); record(SetCurrentDirectoryW(L"startup_ソ"));
    WCHAR wide[512]; char ansi[512]; DWORD wn = GetCurrentDirectoryW(512, wide), an = GetCurrentDirectoryA(512, ansi);
    record(wn != 0 && an != 0); record(GetCurrentDirectoryW(0,0) == wn + 1); record(GetCurrentDirectoryA(0,0) == an + 1);
    WCHAR drive[] = L"C:leaf.dat"; drive[0] = wide[0];
    WCHAR relative[512], *part; DWORD n = GetFullPathNameW(drive, 512, relative, &part);
    record(n == wn + 9); record(part && part[0] == 'l' && part[4] == '.');
    record(SetCurrentDirectoryA("..")); record(RemoveDirectoryW(L"startup_ソ"));
    record(CreateDirectoryA("no_parent\\child", 0)); record(GetLastError());
    temporary(); lifecycle();
    DWORD pm=0, sm=0; SYSTEM_INFO info, native;
    record(GetProcessAffinityMask(GetCurrentProcess(),&pm,&sm)); GetSystemInfo(&info); GetNativeSystemInfo(&native);
    record(pm != 0 && (pm & ~sm) == 0); record(sm == info.dwActiveProcessorMask);
    record(info.dwNumberOfProcessors != 0 && GetCurrentProcessorNumber() < info.dwNumberOfProcessors);
    record(info.dwPageSize == 4096 && info.dwAllocationGranularity == 65536);
    record(native.dwNumberOfProcessors == info.dwNumberOfProcessors);
    record(SetProcessAffinityMask(GetCurrentProcess(),0)); record(GetLastError());
    record(SetThreadAffinityMask(GetCurrentThread(),0)); record(GetLastError());
    record(GetProcessAffinityMask(0,&pm,&sm)); record(GetLastError());
    record(SetThreadAffinityMask(0,1)); record(GetLastError());
    record(SetProcessAffinityMask(0,1)); record(GetLastError());
#ifdef VIRTUAL_PROFILE
    if (pm != 1 || sm != 1 || info.dwNumberOfProcessors != 1 || GetCurrentProcessorNumber() != 0) ExitProcess(93);
    if (!SetProcessAffinityMask(GetCurrentProcess(),1) || SetThreadAffinityMask(GetCurrentThread(),1) != 1) ExitProcess(94);
    if (SetProcessAffinityMask(GetCurrentProcess(),2) || GetLastError() != ERROR_INVALID_PARAMETER) ExitProcess(95);
    HANDLE process,thread;
    if (!DuplicateHandle(GetCurrentProcess(),GetCurrentProcess(),GetCurrentProcess(),&process,0,0,DUPLICATE_SAME_ACCESS) ||
        !DuplicateHandle(GetCurrentProcess(),GetCurrentThread(),GetCurrentProcess(),&thread,0,0,DUPLICATE_SAME_ACCESS)) ExitProcess(96);
    if (!GetProcessAffinityMask(process,&pm,&sm) || !SetProcessAffinityMask(process,1) || SetThreadAffinityMask(thread,1) != 1) ExitProcess(97);
    if (!CloseHandle(process) || !CloseHandle(thread)) ExitProcess(98);
#endif
    // Exercise actual CRT exports, independent of the host compiler's inline helpers.
    HMODULE crt = LoadLibraryA("msvcrt.dll");
    Control fp = (Control)GetProcAddress(crt,"_controlfp"), old = (Control)GetProcAddress(crt,"_control87");
    Secure secure = (Secure)GetProcAddress(crt,"_controlfp_s"); Separate separate = (Separate)GetProcAddress(LoadLibraryA("ucrtbase.dll"),"__control87_2");
    if (!fp || !old || !secure || !separate) ExitProcess(92);
    const DWORD values[] = {0,0x100,0x200,0x300,0x20000,0x10000,0,0x01000000,0x02000000,0x03000000,0x8001f,0,0x80000};
    const DWORD masks[] = {0,0x300,0x300,0x300,0x30000,0x30000,0x30000,0x03000000,0x03000000,0x03000000,0x8001f,0x8001f,0x80000};
    for (DWORD which = 0; which < 2; ++which) for (DWORD i=0;i<13;++i) {
        reset(0x027f,0x1f80); record((which ? old : fp)(values[i],masks[i])); floating();
    }
    reset(0x027f,0x1f80); unsigned current=0x12345678; record(secure(&current,0x200,0x300)); record(current); floating();
    reset(0x027f,0x1f80); record(secure(0,0,0)); floating();
    unsigned x=0,y=0; reset(0x027f,0x1f80); record(separate(0x100,0x300,&x,0)); record(x); floating(); record(fp(0,0));
    record(separate(0x200,0x300,0,&y)); record(y); floating(); record(old(0,0));
#ifdef VIRTUAL_PROFILE
    HMODULE modern = LoadLibraryA("msvcr90.dll");
#else
    HMODULE modern = LoadLibraryA("ucrtbase.dll");
#endif
    SetInvalid set_invalid=(SetInvalid)GetProcAddress(modern,"_set_invalid_parameter_handler");
    secure = (Secure)GetProcAddress(modern,"_controlfp_s");
    if (!set_invalid) ExitProcess(99);
    Invalid (__cdecl *get_invalid)(void)=(Invalid (__cdecl *)(void))GetProcAddress(modern,"_get_invalid_parameter_handler");
    Invalid previous = set_invalid(invalid); record(get_invalid()==invalid); reset(0x067f,0x3f80); current=0x12345678;
    record(secure(&current,0x80000000,0x80000000)); record(current); record(invalid_calls); floating();
    int *(__cdecl *errno_ptr)(void) = (int *(__cdecl *)(void))GetProcAddress(modern,"_errno");
    record(*errno_ptr()); set_invalid(previous);
    int *(__cdecl *legacy_errno)(void) = (int *(__cdecl *)(void))GetProcAddress(crt,"_errno");
    int *modern_errno=errno_ptr(), *old_errno=legacy_errno(); record(modern_errno!=old_errno);
    *old_errno=34; record(*modern_errno==22); record(*legacy_errno()==34);
    reset(0x027f,0x1fbf); record(fp(0x100,0x300)); floating();
    // Seed a legal architectural environment rather than relying on Unicorn's
    // supplementary arithmetic (its FDIV 0/0 reports ZE instead of native IE).
    __asm__ volatile("fninit"); reset(0x027f,0x1f80); DWORD environment[7];
    __asm__ volatile("fnstenv %0" : "=m"(environment));
    environment[1]=(environment[1]&0xffff0000)|0x3801;
    __asm__ volatile("fldenv %0" : : "m"(environment));
    WORD before,after; __asm__ volatile("fnstsw %0" : "=m"(before));
    record(fp(0x100,0x300)); __asm__ volatile("fnstsw %0" : "=m"(after)); record(before); record(after);
    __asm__ volatile("fninit");
    for (DWORD which=0;which<2;++which) {
        Control c=which ? old : fp;
        reset(0x027f,0x1fbf); record(c(0,0)); floating();
        reset(0x027f,0x1fbf); record(c(0,0x30000)); floating();
        reset(0x027f,0x1fbf); record(c(0x80000,0x80000)); floating();
    }
    reset(0x027f,0x1fbf); record(separate(0,0,&x,&y)); floating();
    reset(0x027f,0x1fbf); record(separate(0x100,0x300,0,&y)); floating();
    Control modern_fp=(Control)GetProcAddress(modern,"_controlfp");
    reset(0x027f,0x1fbf); record(modern_fp(0x100,0x300)); floating();
    reset(0x027f,0x1f80); dump(); ExitProcess(0);
}

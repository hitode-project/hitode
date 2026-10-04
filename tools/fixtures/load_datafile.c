#include "win32_min.h"
API HMODULE WINAPI LoadLibraryExA(const char *, HANDLE, DWORD);
API HMODULE WINAPI LoadLibraryExW(const WORD *, HANDLE, DWORD);
API HANDLE WINAPI FindResourceA(HMODULE, const char *, const char *);
API HANDLE WINAPI LoadResource(HMODULE, HANDLE);
API DWORD WINAPI SizeofResource(HMODULE, HANDLE);
API DWORD WINAPI GetModuleFileNameA(HMODULE, char *, DWORD);
API BOOL WINAPI GetModuleHandleExA(DWORD, const char *, HMODULE *);
API DWORD WINAPI GetLastError(void);
API void WINAPI SetLastError(DWORD);
API DWORD WINAPI VirtualQuery(const void *, void *, DWORD);
API BOOL WINAPI VirtualProtect(void *, DWORD, DWORD, DWORD *);
API BOOL WINAPI VirtualFree(void *, DWORD, DWORD);
API HANDLE WINAPI CreateFileA(const char *, DWORD, DWORD, void *, DWORD, DWORD, HANDLE);
API BOOL WINAPI WriteFile(HANDLE, const void *, DWORD, DWORD *, void *);
API DWORD WINAPI SetFilePointer(HANDLE, LONG, LONG *, DWORD);
API BOOL WINAPI SetEndOfFile(HANDLE);
unsigned long __readfsdword(unsigned long);
#pragma intrinsic(__readfsdword)
static DWORD values[256], count, initialized;
static DWORD corners[64], corner_count;
__declspec(dllexport) void __cdecl RecordInit(DWORD value) { (void)value; initialized++; }
static void record(DWORD value) { values[count++]=value; }
static void edge(DWORD value) { corners[corner_count++]=value; }
static void require(BOOL value, UINT code) { if(!value) ExitProcess(code); }
static HANDLE find(HMODULE module, DWORD id) { return FindResourceA(module,(const char *)id,(const char *)10); }
static DWORD state(const void *pointer) { DWORD info[7]; require(VirtualQuery(pointer,info,sizeof info)==sizeof info,101); return info[4]; }
static DWORD in_ldr(DWORD base) {
    DWORD peb=__readfsdword(0x30), ldr=*(DWORD *)(peb+12), head=ldr+12, entry=*(DWORD *)head, remaining=128;
    while(entry!=head && remaining--) { if(*(DWORD *)(entry+24)==base) return 1; entry=*(DWORD *)entry; }
    return 0;
}
static void write_word(HANDLE file,DWORD offset,DWORD value) {
    DWORD written;
    require(SetFilePointer(file,offset,NULL,0)==offset,102);
    require(WriteFile(file,&value,4,&written,NULL)&&written==4,103);
}
static DWORD raw_offset(DWORD base,DWORD rva) {
    DWORD nt=*(DWORD *)(base+60), table=base+nt+24+*(WORD *)(base+nt+20), i;
    for(i=0;i<*(WORD *)(base+nt+6);i++) {
        DWORD section=table+40*i, address=*(DWORD *)(section+12), size=*(DWORD *)(section+16);
        if(rva>=address && rva-address<size) return *(DWORD *)(section+20)+rva-address;
    }
    ExitProcess(104); return 0;
}
void mainCRTStartup(void) {
    HMODULE first, second, image, reused, out=0;
    HANDLE a,b,file;
    DWORD base, info[7], *entry, *data, rva, name_slot, original_name, old, i, original_length;
    char path[256];
    const WORD wide[]={ 'd','a','t','a','f','i','l','e','.','d','l','l',0 };
#ifdef ADV_UNSUPPORTED
    const DWORD flags[]={1,3,0x20,0x22,0x40,0x80,0x1000,0x2000};
    for(i=0;i<sizeof flags/sizeof flags[0];i++) {
        SetLastError(0);
        record(LoadLibraryExA("datafile.dll",0,flags[i])==0);
        record(GetLastError());
    }
    record(initialized); record(GetModuleHandleA("datafile.dll")==0);
#else
    first=LoadLibraryExA("datafile.dll",0,2); require(first!=0,110);
    base=first&~3u; a=find(first,101); require(a!=0,111);
    data=(DWORD *)LoadResource(first,a); require(data!=0,112);
    record(first&3); record(SizeofResource(first,a)); record(data[0]); record(data[1]);
    record(a-base); record((DWORD)data-base); record(a==find(first,101));
    record(VirtualQuery((void *)base,info,sizeof info)); record(info[2]); record(info[5]); record(info[6]);
    record(initialized); record(GetModuleHandleA("datafile.dll")==0); record(GetModuleHandleA("special_dep.dll")==0);
    SetLastError(0); record(GetProcAddress(first,"ReadState")==0); record(GetLastError());
    SetLastError(0); record(GetModuleFileNameA(first,path,sizeof path)); record(GetLastError());
    SetLastError(0); record(GetModuleHandleExA(4,(const char *)base,&out)); record(GetLastError()); record(out==0);
    record(in_ldr(base));
    SetLastError(0); record(VirtualProtect((void *)base,16,4,&old)); record(GetLastError());
    SetLastError(0); record(VirtualFree((void *)base,0,0x8000)); record(GetLastError());
    second=LoadLibraryExW(wide,0,2); require(second!=0,113); record(second!=first); record(second&3);
    b=find(second,101); require(b!=0,114); record(SizeofResource(second,b));
    record(FreeLibrary(first)); record(state((void *)base)); record(*(DWORD *)LoadResource(second,b));
    base=second&~3u; entry=(DWORD *)b; rva=entry[0]; data=(DWORD *)LoadResource(second,b);
    SetLastError(0); file=CreateFileA("datafile.dll",0xc0000000u,7,NULL,2,0,0);
    edge(file==0xffffffffu); edge(GetLastError()); edge(data[0]);
    if(file!=0xffffffffu)CloseHandle(file);
    file=CreateFileA("datafile.dll",0xc0000000u,7,NULL,3,0,0); require(file!=0xffffffffu,115);
    original_length=SetFilePointer(file,0,NULL,2);
    SetFilePointer(file,0,NULL,0); SetLastError(0); edge(SetEndOfFile(file)); edge(GetLastError());
    edge(SetFilePointer(file,0,NULL,1)); edge(SetFilePointer(file,0,NULL,2)==original_length);
    SetFilePointer(file,original_length-1,NULL,0); SetLastError(0); edge(SetEndOfFile(file)); edge(GetLastError());
    edge(SetFilePointer(file,0,NULL,1)==original_length-1); edge(SetFilePointer(file,0,NULL,2)==original_length);
    SetLastError(0); edge(SetEndOfFile(file)); edge(GetLastError());
    SetFilePointer(file,original_length+1,NULL,0); SetLastError(0); edge(SetEndOfFile(file)); edge(GetLastError());
    SetLastError(0); first=LoadLibraryExA("datafile.dll",0,2); edge(first==0); edge(GetLastError()); require(first==0,124);
    CloseHandle(file);
    first=LoadLibraryExA("datafile.dll",0,2); require(first!=0,131);
    file=CreateFileA("datafile.dll",0xc0000000u,7,NULL,3,0,0); require(file!=0xffffffffu,132);
    SetFilePointer(file,original_length,NULL,0); SetLastError(0); edge(SetEndOfFile(file)); edge(GetLastError());
    edge(SetFilePointer(file,0,NULL,1)==original_length); edge(SetFilePointer(file,0,NULL,2)==original_length+1);
    require(FreeLibrary(first),125); SetFilePointer(file,original_length,NULL,0);
    SetLastError(0); edge(SetEndOfFile(file)); edge(GetLastError());
    write_word(file,(DWORD)data-base,0xfeedcafe); record(data[0]);
    write_word(file,b-base+4,4); record(SizeofResource(second,b));
    write_word(file,b-base,rva+4); record(*(DWORD *)LoadResource(second,b));
    write_word(file,b-base,rva); write_word(file,b-base+4,8);
    /* A live directory mutation must affect a fresh lookup, not only Size. */
    i=*(DWORD *)(base+60); name_slot=base+raw_offset(base,*(DWORD *)(base+i+24+96+16));
    name_slot+=( *(DWORD *)(name_slot+20)&0x7fffffff)+16;
    original_name=*(DWORD *)name_slot;
    write_word(file,name_slot-base,102); record(find(second,101)==0); record(find(second,102)==b);
    write_word(file,name_slot-base,original_name);
    /* WRITECOPY can detach one page, and later RO/COPY changes preserve it. */
    SetLastError(0); edge(VirtualProtect(data,4,8,&old)); edge(GetLastError()); edge(old);
    VirtualQuery(data,info,sizeof info); edge(info[2]); edge(info[5]);
    data[0]=0x1111beef; VirtualQuery(data,info,sizeof info); edge(info[5]);
    SetLastError(0); edge(VirtualProtect(data,4,4,&old)); edge(GetLastError());
    require(VirtualProtect(data,4,2,&old),126); edge(old);
    require(VirtualProtect(data,4,8,&old),127); edge(old); edge(data[0]);
    write_word(file,(DWORD)data-base,0xfeedcafe); edge(data[0]);
    CloseHandle(file);
    first=LoadLibraryExA("datafile.dll",0,2); require(first!=0,128);
    edge(*(DWORD *)LoadResource(first,find(first,101))); require(FreeLibrary(first),129);
    /* A failed cross-boundary protection change cannot alter the first page. */
    i=(original_length+4095)&~4095u;
    SetLastError(0); edge(VirtualProtect((void *)(base+i-4096),8192,8,&old)); edge(GetLastError());
    VirtualQuery((void *)(base+i-4096),info,sizeof info); edge(info[5]);
    require(VirtualProtect(data,4,2,&old),130);
    data=(DWORD *)LoadResource(second,b);
    record(data[0]);
    file=CreateFileA("datafile.dll",0x80000000u,0,NULL,3,0,0); require(file!=0xffffffffu,133);
    SetLastError(0); first=LoadLibraryExA("datafile.dll",0,2); edge(first==0); edge(GetLastError()); require(first==0,134);
    CloseHandle(file);
    file=CreateFileA("datafile.dll",0x80000000u,1,NULL,3,0,0); require(file!=0xffffffffu,135);
    SetLastError(0); first=LoadLibraryExA("datafile.dll",0,2); edge(first!=0); require(first!=0,136);
    require(FreeLibrary(first),137); CloseHandle(file);
    file=CreateFileA("datafile.dll",0x10000u,7,NULL,3,0,0); require(file!=0xffffffffu,138);
    SetLastError(0); first=LoadLibraryExA("datafile.dll",0,2); edge(first==0); edge(GetLastError());
    if(first)FreeLibrary(first); CloseHandle(file);
    image=LoadLibraryA("datafile.dll"); require(image!=0,116);
    record(image&3); record(image!=second); record(initialized); record(GetModuleHandleA("datafile.dll")==image);
    record(in_ldr(image)); record(((DWORD (__cdecl *)(void))GetProcAddress(image,"ReadState"))());
    reused=LoadLibraryExA("datafile.dll",0,2); record(reused==image);
    record(FreeLibrary(image)); record(GetModuleHandleA("datafile.dll")==image);
    record(FreeLibrary(second)); record(GetModuleHandleA("datafile.dll")==image); record(initialized);
    record(FreeLibrary(reused)); record(GetModuleHandleA("datafile.dll")==0); record(initialized);
    for(i=0;i<256;i++) { first=LoadLibraryExA("datafile.dll",0,2); require(first!=0,117); require(find(first,101)!=0,118); require(FreeLibrary(first),119); }
    record(initialized);
    SetLastError(0); record(LoadLibraryExA("datafile.dll",1,2)==0); record(GetLastError());
    SetLastError(0); record(LoadLibraryExA("datafile.dll",0,0x80000000u)==0); record(GetLastError());
    SetLastError(0); record(LoadLibraryExA("datafile.dll",0,0x42)==0); record(GetLastError());
    file=CreateFileA("not-pe.bin",0x40000000u,7,NULL,2,0,0); require(file!=0xffffffffu,120); write_word(file,0,0x12345678); CloseHandle(file);
    SetLastError(0); record(LoadLibraryExA("not-pe.bin",0,2)==0); record(GetLastError());
    SetLastError(0); record(LoadLibraryExA("absent-own.dll",0,2)==0); record(GetLastError());
    /* An intentionally forgotten raw owner is checked on runtime Drop. */
    first=LoadLibraryExA("datafile.dll",0,2); require(first!=0,121); record(first&3); record(initialized);
    for(i=0;i<corner_count;i++) record(corners[i]);
#endif
    file=CreateFileA("load-datafile.bin",0x40000000u,7,NULL,2,0,0); require(file!=0xffffffffu,122);
    require(WriteFile(file,values,count*4,&i,NULL)&&i==count*4,123); CloseHandle(file); ExitProcess(0);
}

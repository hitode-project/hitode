/* Authored x86 routine: verify code mutation is visible after native flush. */
#include <windows.h>
typedef LONG(WINAPI *NT_FLUSH)(HANDLE,PVOID,SIZE_T);
typedef LONG(WINAPI *NT_WRITE)(HANDLE,PVOID,PVOID,SIZE_T,PSIZE_T);
static DWORD values[18];
__declspec(naked) DWORD patched(void){ __asm mov eax, 17 __asm ret }
void mainCRTStartup(void){
    HMODULE nt=GetModuleHandleW(L"ntdll.dll"); NT_FLUSH flush=(NT_FLUSH)GetProcAddress(nt,"NtFlushInstructionCache");
    NT_WRITE write=(NT_WRITE)GetProcAddress(nt,"NtWriteVirtualMemory");HANDLE limited=NULL,file;DWORD old,restored,desired=29,written;
    if(!flush || !write)ExitProcess(1);
    SetLastError(0x13572468);values[0]=(DWORD)flush(GetCurrentProcess(),NULL,0);values[1]=GetLastError();
    values[2]=(DWORD)flush((HANDLE)0x12345678,NULL,0);values[3]=(DWORD)flush(GetCurrentProcess(),(PVOID)1,0xffffffff);
    values[4]=DuplicateHandle(GetCurrentProcess(),GetCurrentProcess(),GetCurrentProcess(),&limited,0,FALSE,0);
    values[5]=(DWORD)flush(limited,NULL,0);values[6]=(DWORD)flush(limited,(PVOID)patched,6);values[7]=CloseHandle(limited);
    values[8]=VirtualProtect((PVOID)patched,6,PAGE_EXECUTE_READWRITE,&old);values[9]=patched();
    values[10]=(DWORD)write(GetCurrentProcess(),(BYTE*)patched+1,&desired,4,NULL);
    SetLastError(0x13572468);values[11]=(DWORD)flush(GetCurrentProcess(),(PVOID)patched,6);values[12]=patched();values[13]=GetLastError();
    values[14]=VirtualProtect((PVOID)patched,6,old,&restored);values[15]=restored==PAGE_EXECUTE_READWRITE;
    values[16]=(DWORD)flush((HANDLE)0x12345678,(PVOID)patched,6);values[17]=(DWORD)flush((HANDLE)0x12345678,NULL,6);
    file=CreateFileW(L"nt-flush-cache.bin",GENERIC_WRITE,0,NULL,CREATE_ALWAYS,0,NULL);
    if(file==INVALID_HANDLE_VALUE || !WriteFile(file,values,sizeof(values),&written,NULL) || written!=sizeof(values))ExitProcess(2);
    CloseHandle(file);ExitProcess(0);
}

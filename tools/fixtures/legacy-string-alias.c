/* Authored x86 static/dynamic legacy string alias and PE ownership oracle. */
#include <windows.h>
#undef lstrcpy
__declspec(dllimport) LPSTR WINAPI lstrcpy(LPSTR destination,LPCSTR source);
typedef LPSTR(WINAPI *COPY)(LPSTR,LPCSTR);
static DWORD values[8];
static const char source[]={ (char)0x81,0x40,'x',0 };
void mainCRTStartup(void){
 HMODULE module=GetModuleHandleW(L"kernel32.dll");
 COPY alias=(COPY)GetProcAddress(module,"lstrcpy"),ansi=(COPY)GetProcAddress(module,"lstrcpyA");
 char buffer[8];MEMORY_BASIC_INFORMATION information;HANDLE file;DWORD written;unsigned i;
 values[0]=alias!=NULL;values[1]=alias==ansi;values[2]=(COPY)lstrcpy==alias;
 for(i=0;i<sizeof(buffer);i++)buffer[i]=(char)0xcc;
 SetLastError(0x13572468);values[3]=lstrcpy(buffer,source)==buffer;values[4]=GetLastError();
 values[5]=buffer[0]==source[0]&&buffer[1]==source[1]&&buffer[2]=='x'&&buffer[3]==0&&buffer[4]==(char)0xcc;
 values[6]=VirtualQuery((LPCVOID)alias,&information,sizeof(information))==sizeof(information)&&information.AllocationBase==module;
 values[7]=information.AllocationBase==module&&*(const WORD*)module==0x5a4d;
 file=CreateFileW(L"legacy-string-alias.bin",GENERIC_WRITE,0,NULL,CREATE_ALWAYS,0,NULL);
 if(file==INVALID_HANDLE_VALUE||!WriteFile(file,values,sizeof(values),&written,NULL)||written!=sizeof(values))ExitProcess(2);
 CloseHandle(file);ExitProcess(0);
}

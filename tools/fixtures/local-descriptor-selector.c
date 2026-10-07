/* Authored x86 SLDT register widths, flags and two-byte memory write oracle. */
#include <windows.h>
static DWORD values[8],before,after;
static struct {WORD leading,selector,trailing;} memory={0xabcd,0xffff,0xef01};
__declspec(naked) static void probe(void){
 __asm {
  pushfd
  pop before
  mov edx,13572468h
  mov eax,0aabbccddh
  sldt eax
  mov values[0],eax
  mov eax,0aabbccddh
  sldt ax
  mov values[4],eax
  sldt word ptr memory[2]
  mov values[20],edx
  pushfd
  pop after
  ret
 }
}
void mainCRTStartup(void){
 HANDLE file;DWORD written;
 SetLastError(0x13572468);probe();values[6]=GetLastError();values[2]=before==after;
 values[3]=memory.selector;values[4]=memory.leading==0xabcd&&memory.trailing==0xef01;values[7]=1;
 file=CreateFileW(L"local-descriptor-selector.bin",GENERIC_WRITE,0,NULL,CREATE_ALWAYS,0,NULL);
 if(file==INVALID_HANDLE_VALUE||!WriteFile(file,values,sizeof(values),&written,NULL)||written!=sizeof(values))ExitProcess(2);
 CloseHandle(file);ExitProcess(0);
}

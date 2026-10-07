/* Authored CPL3 CLI/STI exception oracle; no native privileged state is changed.
   Microsoft Learn GetExceptionCode defines EXCEPTION_PRIV_INSTRUCTION. */
#include <windows.h>
static DWORD values[16];
static unsigned active;
static void *fault;
__declspec(naked) static void cli_probe(void){__asm cli __asm ret}
__declspec(naked) static void sti_probe(void){__asm sti __asm ret}
static LONG WINAPI handler(EXCEPTION_POINTERS *event){
 DWORD *out=values+active*8;EXCEPTION_RECORD *record=event->ExceptionRecord;CONTEXT *context=event->ContextRecord;
 if(record->ExceptionCode!=EXCEPTION_PRIV_INSTRUCTION||record->ExceptionAddress!=fault)return EXCEPTION_CONTINUE_SEARCH;
 out[0]=record->ExceptionCode;out[1]=record->ExceptionFlags;out[2]=record->NumberParameters;
 out[3]=record->ExceptionAddress==fault;out[4]=context->Eip==(DWORD)fault;
 out[5]=(context->EFlags&0x3200)==0x200;out[7]++;
 context->Eip++;return EXCEPTION_CONTINUE_EXECUTION;
}
void mainCRTStartup(void){
 void (*probes[2])(void)={cli_probe,sti_probe};PVOID token=AddVectoredExceptionHandler(1,handler);
 HANDLE file;DWORD written;if(!token)ExitProcess(1);
 for(active=0;active<2;active++){
  fault=(void*)probes[active];SetLastError(0x13572468);probes[active]();
  values[active*8+6]=GetLastError();values[active*8+7]=values[active*8+7]==1;
 }
 if(!RemoveVectoredExceptionHandler(token))ExitProcess(3);
 file=CreateFileW(L"user-privileged-instruction.bin",GENERIC_WRITE,0,NULL,CREATE_ALWAYS,0,NULL);
 if(file==INVALID_HANDLE_VALUE||!WriteFile(file,values,sizeof(values),&written,NULL)||written!=sizeof(values))ExitProcess(2);
 CloseHandle(file);ExitProcess(0);
}

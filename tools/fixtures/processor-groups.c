/* Authored x86 affinity oracle: writes scalar contract results to affinity.bin. */
#include <windows.h>
static DWORD values[64];
void mainCRTStartup(void) {
 GROUP_AFFINITY original, current, previous, bad; PROCESSOR_NUMBER cpu;
 HANDLE thread=GetCurrentThread(), file; DWORD n=0,written; unsigned i;
 DWORD size, relation,offset; BYTE *buffer; SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX *record;
 values[n++]=sizeof(original)==12;
 values[n++]=GetThreadGroupAffinity(thread,&original)!=0;
 values[n++]=original.Mask!=0;
 values[n++]=original.Reserved[0]==0&&original.Reserved[1]==0&&original.Reserved[2]==0;
 SetLastError(0x1234); values[n++]=SetThreadGroupAffinity(thread,&original,&previous)!=0;
 values[n++]=GetLastError()==0x1234;
 values[n++]=previous.Mask==original.Mask&&previous.Group==original.Group;
 values[n++]=SetThreadGroupAffinity(thread,&original,NULL)!=0;
 GetCurrentProcessorNumberEx(&cpu);
 values[n++]=cpu.Group==original.Group&&cpu.Reserved==0;
 values[n++]=(original.Mask & ((ULONG_PTR)1<<cpu.Number))!=0;
 bad=original; bad.Mask=0;
 for(i=0;i<sizeof(previous);i++)((BYTE*)&previous)[i]=0xa5;
 values[n++]=SetThreadGroupAffinity(thread,&bad,&previous)!=0;
 values[n++]=GetThreadGroupAffinity(thread,&current)!=0;
 values[n++]=previous.Mask==original.Mask&&current.Mask==original.Mask;
 bad=original;bad.Reserved[0]=1;
 values[n++]=SetThreadGroupAffinity(thread,&bad,NULL)==0;
 values[n++]=GetLastError()==ERROR_INVALID_PARAMETER;
 values[n++]=GetThreadGroupAffinity(NULL,&current)==0;
 values[n++]=GetLastError()==ERROR_INVALID_HANDLE;
 for(relation=0;relation<=4;relation++) {
  if(relation==2)continue;
  size=0;values[n++]=!GetLogicalProcessorInformationEx(relation,NULL,&size)&&GetLastError()==ERROR_INSUFFICIENT_BUFFER&&size>0;
  buffer=HeapAlloc(GetProcessHeap(),0,size);
  values[n++]=GetLogicalProcessorInformationEx(relation,(void*)buffer,&size)!=0;
  values[n]=1;
  for(offset=0;offset<size;offset+=record->Size) {
   record=(void*)(buffer+offset);
   if(record->Relationship!=relation||record->Size!=(relation==4?76:44)){values[n]=0;break;}
  }
  n++;HeapFree(GetProcessHeap(),0,buffer);
 }
 file=CreateFileW(L"affinity.bin",GENERIC_WRITE,0,NULL,CREATE_ALWAYS,0,NULL);
 if(file==INVALID_HANDLE_VALUE||!WriteFile(file,values,n*4,&written,NULL)||written!=n*4)ExitProcess(2);
 CloseHandle(file);ExitProcess(0);
}

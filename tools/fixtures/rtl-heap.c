/* Authored native x86 RTL/Win32 heap owner, return-width and LastError oracle. */
#include <windows.h>
typedef PVOID(WINAPI *ALLOCATE)(PVOID,ULONG,SIZE_T);
typedef ULONG(WINAPI *FREE)(PVOID,ULONG,PVOID);
static DWORD values[24];
static BOOL zero(PVOID pointer,unsigned length){unsigned i;for(i=0;i<length;i++)if(((BYTE*)pointer)[i])return FALSE;return TRUE;}
void mainCRTStartup(void){
 HMODULE nt=GetModuleHandleW(L"ntdll.dll");ALLOCATE allocate=(ALLOCATE)GetProcAddress(nt,"RtlAllocateHeap");FREE release=(FREE)GetProcAddress(nt,"RtlFreeHeap");
 HANDLE current=GetProcessHeap(),custom,file;PVOID pointer;DWORD written;
 values[0]=allocate!=NULL&&release!=NULL;if(!values[0])ExitProcess(1);
 SetLastError(0x13572468);pointer=allocate(current,HEAP_ZERO_MEMORY,19);values[1]=pointer!=NULL;
 if(!pointer)ExitProcess(2);values[2]=((DWORD)pointer&7)==0;values[3]=zero(pointer,19);values[4]=HeapSize(current,0,pointer)==19;values[5]=GetLastError();
 values[6]=HeapFree(current,0,pointer);values[7]=GetLastError();
 custom=HeapCreate(0,0,0);values[8]=custom!=NULL;if(!custom)ExitProcess(3);
 SetLastError(0x13572468);pointer=allocate(custom,HEAP_NO_SERIALIZE|HEAP_ZERO_MEMORY,311);values[9]=pointer!=NULL;
 if(!pointer)ExitProcess(4);values[10]=zero(pointer,311);values[11]=HeapSize(custom,0,pointer)==311;values[12]=release(custom,HEAP_NO_SERIALIZE,pointer);values[13]=GetLastError();
 values[14]=release(custom,0,NULL);values[15]=GetLastError();
 pointer=HeapAlloc(custom,0,33);values[16]=pointer!=NULL;if(!pointer)ExitProcess(5);values[17]=release(custom,0,pointer);
 SetLastError(0x13572468);values[18]=allocate(custom,0,(SIZE_T)-1)==NULL;values[19]=GetLastError();
 pointer=allocate(custom,0,0);values[20]=pointer!=NULL;if(!pointer)ExitProcess(6);values[21]=HeapSize(custom,0,pointer);values[22]=release(custom,0,pointer);values[23]=HeapDestroy(custom);
 file=CreateFileW(L"rtl-heap.bin",GENERIC_WRITE,0,NULL,CREATE_ALWAYS,0,NULL);
 if(file==INVALID_HANDLE_VALUE||!WriteFile(file,values,sizeof(values),&written,NULL)||written!=sizeof(values))ExitProcess(7);CloseHandle(file);ExitProcess(0);
}

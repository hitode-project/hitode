/* Authored personality-layer probe: no sample code, assets or third-party protection logic.
   Usage: personality-N.exe selects contract N (1..9, a..e). Writes scalar DWORDs
   to probe-N.bin. Build and compare with personality_contract.py; no sample inputs. */
#include <windows.h>
#include <intrin.h>
#include "MinHook.h"

static DWORD v[64]; static int n;
static void put(DWORD x){ if(n<64) v[n++]=x; }
static void save(char t){
  char name[]="probe-?.bin"; HANDLE f; DWORD w; name[6]=t;
  f=CreateFileA(name,GENERIC_WRITE,0,NULL,CREATE_ALWAYS,0,NULL);
  if(f==INVALID_HANDLE_VALUE) ExitProcess(90);
  WriteFile(f,v,n*4,&w,NULL); CloseHandle(f);
}
static HMODULE mod_of(void *p){ HMODULE m=0;
  if(!GetModuleHandleExA(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS|GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,(LPCSTR)p,&m)) return 0; return m; }
static DWORD mem_type(void *p){ MEMORY_BASIC_INFORMATION mi; if(!VirtualQuery(p,&mi,sizeof mi)) return 0; return mi.Type; }
static DWORD in_image(HMODULE m, void *p){ IMAGE_NT_HEADERS *nt; if(!m||!p) return 0;
  nt=(IMAGE_NT_HEADERS*)((BYTE*)m+((IMAGE_DOS_HEADER*)m)->e_lfanew);
  return (BYTE*)p>=(BYTE*)m && (BYTE*)p<(BYTE*)m+nt->OptionalHeader.SizeOfImage; }
static DWORD export_names(HMODULE m){ IMAGE_NT_HEADERS *nt; IMAGE_EXPORT_DIRECTORY *e;
  nt=(IMAGE_NT_HEADERS*)((BYTE*)m+((IMAGE_DOS_HEADER*)m)->e_lfanew);
  e=(IMAGE_EXPORT_DIRECTORY*)((BYTE*)m+nt->OptionalHeader.DataDirectory[0].VirtualAddress);
  return e->NumberOfNames; }

/* ---- 1: thread lifetime: 1000 sequential short threads, including the first 200 ---- */
static DWORD WINAPI ret_param(void *p){ return (DWORD)(UINT_PTR)p; }
static void t_threads(void){
  int i, ok=0; DWORD code, err=0;
  for(i=0;i<1000;i++){ HANDLE h=CreateThread(NULL,0,ret_param,(void*)(UINT_PTR)(i+1),0,NULL);
    if(!h){ err=GetLastError(); break; }
    if(WaitForSingleObject(h,5000)!=0 || !GetExitCodeThread(h,&code) || code!=(DWORD)i+1){ CloseHandle(h); err=0xdead; break; }
    CloseHandle(h); ok++; }
  put(ok<200?ok:200); put(ok); put(err);
}

/* ---- 2: NT file hook coverage of Win32 / CRT front ends ---- */
typedef LONG (WINAPI *NTCF)(PHANDLE,ACCESS_MASK,void*,void*,void*,ULONG,ULONG,ULONG,ULONG,void*,ULONG);
typedef LONG (WINAPI *NTOF)(PHANDLE,ACCESS_MASK,void*,void*,ULONG,ULONG);
typedef LONG (WINAPI *NTQA)(void*,void*);
static NTCF oCF; static NTOF oOF; static NTQA oQA; static volatile LONG cCF,cOF,cQA;
static LONG WINAPI hCF(PHANDLE a,ACCESS_MASK b,void*c,void*d,void*e,ULONG f,ULONG g,ULONG h,ULONG i,void*j,ULONG k){ InterlockedIncrement(&cCF); return oCF(a,b,c,d,e,f,g,h,i,j,k); }
static LONG WINAPI hOF(PHANDLE a,ACCESS_MASK b,void*c,void*d,ULONG e,ULONG f){ InterlockedIncrement(&cOF); return oOF(a,b,c,d,e,f); }
static LONG WINAPI hQA(void*a,void*b){ InterlockedIncrement(&cQA); return oQA(a,b); }
static DWORD total(void){ return cCF+cOF+cQA; }
typedef void* (__cdecl *FOPEN)(const char*,const char*); typedef int (__cdecl *FCLOSE)(void*);
static void t_file_hooks(void){
  HMODULE nt=GetModuleHandleA("ntdll.dll"); void *pCF,*pOF,*pQA; DWORD before, ok, w; HANDLE f; WIN32_FIND_DATAA fd; char buf[16];
  HMODULE crt; FOPEN fo; FCLOSE fc; void *fp;
  f=CreateFileA("probe-data.txt",GENERIC_WRITE,0,NULL,CREATE_ALWAYS,0,NULL); WriteFile(f,"0123456789abcdef",16,&w,NULL); CloseHandle(f);
  WritePrivateProfileStringA("s","k","v",".\\probe-data.ini");
  crt=LoadLibraryA("msvcrt.dll"); fo=(FOPEN)GetProcAddress(crt,"fopen"); fc=(FCLOSE)GetProcAddress(crt,"fclose");
  pCF=GetProcAddress(nt,"NtCreateFile"); pOF=GetProcAddress(nt,"NtOpenFile"); pQA=GetProcAddress(nt,"NtQueryAttributesFile");
  put(pCF!=0); put(pOF!=0); put(pQA!=0); put(fo!=0);
  if(MH_Initialize()!=MH_OK) { put(0xbad); return; }
  ok=0;
  if(pCF && MH_CreateHook(pCF,hCF,(void**)&oCF)==MH_OK) ok|=1;
  if(pOF && MH_CreateHook(pOF,hOF,(void**)&oOF)==MH_OK) ok|=2;
  if(pQA && MH_CreateHook(pQA,hQA,(void**)&oQA)==MH_OK) ok|=4;
  put(ok); put(MH_EnableHook(MH_ALL_HOOKS)==MH_OK);
  before=total(); f=CreateFileA("probe-data.txt",GENERIC_READ,FILE_SHARE_READ,NULL,OPEN_EXISTING,0,NULL); put(f!=INVALID_HANDLE_VALUE); put(total()-before); CloseHandle(f);
  before=total(); fp=fo?fo("probe-data.txt","rb"):0; put(fp!=0); put(total()-before); if(fp) fc(fp);
  before=total(); put(GetFileAttributesA("probe-data.txt")!=INVALID_FILE_ATTRIBUTES); put(total()-before);
  before=total(); f=FindFirstFileA("probe-d*.txt",&fd); put(f!=INVALID_HANDLE_VALUE); put(total()-before); if(f!=INVALID_HANDLE_VALUE) FindClose(f);
  before=total(); put(GetPrivateProfileStringA("s","k","",buf,sizeof buf,".\\probe-data.ini")); put(total()-before);
  MH_DisableHook(MH_ALL_HOOKS);
}

/* ---- 3: return-address / code-identity of host-originated frames ---- */
static DWORD th_ret[4];
static DWORD WINAPI show_ret(void *p){ void *r=_ReturnAddress(); NT_TIB *tib=(NT_TIB*)NtCurrentTeb();
  (void)p; th_ret[0]=mod_of(r)!=0; th_ret[1]=mod_of(r)==GetModuleHandleA("kernel32.dll")||mod_of(r)==GetModuleHandleA("ntdll.dll"); th_ret[2]=mem_type(r);
  th_ret[3]=(DWORD)(UINT_PTR)tib->ExceptionList; return 0; }
static void *main_ret;
static void t_identity(void){
  HANDLE h=CreateThread(NULL,0,show_ret,NULL,0,NULL); NT_TIB *tib=(NT_TIB*)NtCurrentTeb();
  WaitForSingleObject(h,5000); CloseHandle(h);
  put(th_ret[0]); put(th_ret[1]); put(th_ret[2]); put(th_ret[3]!=0xffffffff);
  put(mod_of(main_ret)!=0); put(mem_type(main_ret)); put((DWORD)(UINT_PTR)tib->ExceptionList!=0xffffffff);
}

/* ---- 4: export surface of ntdll / kernel32 and forwarder identity ---- */
static const char *nt_names[]={"NtQuerySystemInformation","NtQueryInformationProcess","NtQueryInformationThread","NtSetInformationThread",
 "NtCreateSection","NtMapViewOfSection","NtUnmapViewOfSection","NtProtectVirtualMemory","NtQueryVirtualMemory","NtCreateEvent",
 "NtSetEvent","NtWaitForSingleObject","NtClose","NtCreateFile","NtOpenFile","NtQueryDirectoryFile","NtDeviceIoControlFile",
 "LdrGetDllHandle","LdrLoadDll","LdrGetProcedureAddress","LdrFindEntryForAddress","RtlPcToFileHeader","RtlImageNtHeader",
 "RtlGetVersion","RtlAddVectoredExceptionHandler","RtlInitializeCriticalSection","RtlEnterCriticalSection","RtlLeaveCriticalSection",
 "RtlUserThreadStart","KiUserExceptionDispatcher","RtlCaptureContext","RtlUnwind",
 "NtTestAlert","NtQueueApcThread","NtCreateThreadEx","RtlCreateUserThread","NtSuspendThread","NtResumeThread","NtGetContextThread",
 "RtlNtStatusToDosError","RtlMultiByteToUnicodeN","RtlUnicodeToMultiByteN","LdrRegisterDllNotification","NtYieldExecution",
 "NtOpenProcess","RtlAllocateHeap","RtlFreeHeap","RtlGetLastWin32Error","NtQueryPerformanceCounter","RtlCompareMemory",
 "LdrInitializeThunk","KiUserApcDispatcher","NtContinue","RtlRaiseException","NtAllocateVirtualMemory","RtlExitUserThread"};
static void t_exports(void){
  HMODULE nt=GetModuleHandleA("ntdll.dll"), k32=GetModuleHandleA("kernel32.dll"); int i; DWORD m0=0,m1=0,outside=0,count=0;
  for(i=0;i<(int)(sizeof nt_names/sizeof *nt_names);i++){ void *p=GetProcAddress(nt,nt_names[i]);
    if(p){ count++; if(i<32) m0|=1u<<i; else m1|=1u<<(i-32); if(!in_image(nt,p)) outside++; } }
  put(count); put(m0); put(m1); put(outside); put(export_names(nt)); put(export_names(k32));
  /* kernel32 heap/critical-section exports forward into ntdll on native Windows */
  put(GetProcAddress(k32,"HeapAlloc")==GetProcAddress(nt,"RtlAllocateHeap"));
  put(GetProcAddress(k32,"EnterCriticalSection")==GetProcAddress(nt,"RtlEnterCriticalSection"));
  put(in_image(nt,GetProcAddress(k32,"HeapAlloc")));
}

/* ---- 5: suspended thread initial context ---- */
static void t_context(void){
  CONTEXT c; HANDLE h=CreateThread(NULL,0,ret_param,(void*)0x1234,CREATE_SUSPENDED,NULL); DWORD code=0;
  c.ContextFlags=CONTEXT_FULL; put(GetThreadContext(h,&c));
  put(c.Eip==(DWORD)(UINT_PTR)ret_param); put(mod_of((void*)(UINT_PTR)c.Eip)==GetModuleHandleA("ntdll.dll"));
  put(c.Eax==(DWORD)(UINT_PTR)ret_param); put(c.Ebx==0x1234);
  ResumeThread(h); WaitForSingleObject(h,5000); GetExitCodeThread(h,&code); put(code); CloseHandle(h);

}

/* ---- 6: heap hook coverage: RtlAllocateHeap observed by Win32/CRT allocators ---- */
typedef void* (WINAPI *RAH)(HANDLE,ULONG,SIZE_T); static RAH oRAH; static volatile LONG cRAH;
static void* WINAPI hRAH(HANDLE a,ULONG b,SIZE_T c){ InterlockedIncrement(&cRAH); return oRAH(a,b,c); }
typedef void* (__cdecl *MALLOC)(size_t);
static void t_heap_hooks(void){
  HMODULE nt=GetModuleHandleA("ntdll.dll"); void *p=GetProcAddress(nt,"RtlAllocateHeap"); LONG b; HMODULE crt=LoadLibraryA("msvcrt.dll");
  MALLOC ma=(MALLOC)GetProcAddress(crt,"malloc");
  put(p!=0); if(!p) return;
  if(MH_Initialize()!=MH_OK || MH_CreateHook(p,hRAH,(void**)&oRAH)!=MH_OK || MH_EnableHook(p)!=MH_OK){ put(0xbad); return; }
  b=cRAH; put(HeapAlloc(GetProcessHeap(),0,40)!=0); put(cRAH-b);
  b=cRAH; put(LocalAlloc(LMEM_FIXED,40)!=0); put(cRAH-b);
  b=cRAH; put(GlobalAlloc(GMEM_FIXED,40)!=0); put(cRAH-b);
  b=cRAH; put(ma && ma(40)!=0); put(cRAH-b);
  MH_DisableHook(p);
}

/* ---- 7: loader hook coverage: LdrLoadDll observed by Win32 LoadLibrary front ends ---- */
typedef LONG (WINAPI *LLD)(PWSTR,PULONG,void*,PVOID*); static LLD oLLD; static volatile LONG cLLD; static DWORD ldr_search_flags;
static LONG WINAPI hLLD(PWSTR a,PULONG b,void*c,PVOID*d){ InterlockedIncrement(&cLLD);ldr_search_flags=(ULONG_PTR)a<0x10000?(DWORD)(ULONG_PTR)a:0xffffffff;return oLLD(a,b,c,d); }
static void t_loader_hooks(void){
  void *p=GetProcAddress(GetModuleHandleA("ntdll.dll"),"LdrLoadDll"); LONG b;
  put(p!=0); if(!p) return;
  if(MH_Initialize()!=MH_OK || MH_CreateHook(p,hLLD,(void**)&oLLD)!=MH_OK || MH_EnableHook(p)!=MH_OK){ put(0xbad); return; }
  b=cLLD; put(LoadLibraryA("version.dll")!=0); put(cLLD-b);
  b=cLLD; put(LoadLibraryExW(L"winmm.dll",NULL,0)!=0); put(cLLD-b);
  MH_DisableHook(p);
}

/* ---- 8: ANSI (ACP 936) file names through Win32 and CRT front ends ---- */
static void t_ansi_names(void){
  static const char name[]="\xb2\xe2\xca\xd4.txt"; HANDLE f; DWORD w; HMODULE crt=LoadLibraryA("msvcrt.dll");
  FOPEN fo=(FOPEN)GetProcAddress(crt,"fopen"); FCLOSE fc=(FCLOSE)GetProcAddress(crt,"fclose"); void *fp;
  put(GetACP());
  f=CreateFileA(name,GENERIC_WRITE,0,NULL,CREATE_ALWAYS,0,NULL); put(f!=INVALID_HANDLE_VALUE); WriteFile(f,"x",1,&w,NULL); CloseHandle(f);
  put(GetFileAttributesA(name)!=INVALID_FILE_ATTRIBUTES);
  fp=fo?fo(name,"rb"):0; put(fp!=0); if(fp) fc(fp);
}


/* ---- 9: public virtual-memory fronts preserve the NT boundary ---- */
typedef LONG (WINAPI *NTA)(HANDLE,void**,ULONG,SIZE_T*,ULONG,ULONG);
typedef LONG (WINAPI *NTF)(HANDLE,void**,SIZE_T*,ULONG);
typedef LONG (WINAPI *NTP)(HANDLE,void**,SIZE_T*,ULONG,ULONG*);
typedef LONG (WINAPI *NTQ)(HANDLE,void*,ULONG,void*,SIZE_T,SIZE_T*);
typedef LONG (WINAPI *NTT)(HANDLE,void*,void*,SIZE_T,SIZE_T*);
static NTA oVA; static NTF oVF; static NTP oVP; static NTQ oVQ; static NTT oVR,oVW;
static LONG cVA,cVF,cVP,cVQ,cVR,cVW;
static LONG WINAPI hVA(HANDLE h,void**a,ULONG z,SIZE_T*s,ULONG t,ULONG p){cVA++;return oVA(h,a,z,s,t,p);}
static LONG WINAPI hVF(HANDLE h,void**a,SIZE_T*s,ULONG t){cVF++;return oVF(h,a,s,t);}
static LONG WINAPI hVP(HANDLE h,void**a,SIZE_T*s,ULONG p,ULONG*o){cVP++;return oVP(h,a,s,p,o);}
static LONG WINAPI hVQ(HANDLE h,void*a,ULONG c,void*b,SIZE_T s,SIZE_T*r){cVQ++;return oVQ(h,a,c,b,s,r);}
static LONG WINAPI hVR(HANDLE h,void*a,void*b,SIZE_T s,SIZE_T*r){cVR++;return oVR(h,a,b,s,r);}
static LONG WINAPI hVW(HANDLE h,void*a,void*b,SIZE_T s,SIZE_T*r){cVW++;return oVW(h,a,b,s,r);}
static void t_memory_hooks(void){
 HMODULE nt=GetModuleHandleA("ntdll.dll"); HANDLE self=GetCurrentProcess();
 void *p,*q; LONG b; DWORD input=0x12345678,read=0,old=0; SIZE_T count=0; MEMORY_BASIC_INFORMATION mi;
 int ok=MH_Initialize()==MH_OK;
 ok=ok&&MH_CreateHook(GetProcAddress(nt,"NtAllocateVirtualMemory"),hVA,(void**)&oVA)==MH_OK;
 ok=ok&&MH_CreateHook(GetProcAddress(nt,"NtFreeVirtualMemory"),hVF,(void**)&oVF)==MH_OK;
 ok=ok&&MH_CreateHook(GetProcAddress(nt,"NtProtectVirtualMemory"),hVP,(void**)&oVP)==MH_OK;
 ok=ok&&MH_CreateHook(GetProcAddress(nt,"NtQueryVirtualMemory"),hVQ,(void**)&oVQ)==MH_OK;
 ok=ok&&MH_CreateHook(GetProcAddress(nt,"NtReadVirtualMemory"),hVR,(void**)&oVR)==MH_OK;
 ok=ok&&MH_CreateHook(GetProcAddress(nt,"NtWriteVirtualMemory"),hVW,(void**)&oVW)==MH_OK;
 ok=ok&&MH_EnableHook(MH_ALL_HOOKS)==MH_OK; put(ok); if(!ok)return;
 b=cVA;p=VirtualAlloc(NULL,4096,MEM_RESERVE|MEM_COMMIT,PAGE_READWRITE);put(p!=0);put(cVA-b);
 b=cVA;q=VirtualAllocEx(self,NULL,4096,MEM_RESERVE|MEM_COMMIT,PAGE_READWRITE);put(q!=0);put(cVA-b);
 b=cVW;put(WriteProcessMemory(self,p,&input,4,&count)&&count==4);put(cVW-b);
 b=cVR;put(ReadProcessMemory(self,p,&read,4,&count)&&count==4&&read==input);put(cVR-b);
 b=cVP;put(VirtualProtect(p,4096,PAGE_READONLY,&old)&&old==PAGE_READWRITE);put(cVP-b);
 b=cVP;put(VirtualProtectEx(self,q,4096,PAGE_READONLY,&old)&&old==PAGE_READWRITE);put(cVP-b);
 b=cVQ;put(VirtualQuery(p,&mi,sizeof mi)==sizeof mi&&mi.Protect==PAGE_READONLY);put(cVQ-b);
 b=cVQ;put(VirtualQueryEx(self,q,&mi,sizeof mi)==sizeof mi&&mi.Protect==PAGE_READONLY);put(cVQ-b);
 b=cVF;put(VirtualFree(p,0,MEM_RELEASE));put(cVF-b);
 b=cVF;put(VirtualFreeEx(self,q,0,MEM_RELEASE));put(cVF-b);
 MH_DisableHook(MH_ALL_HOOKS);
}

typedef LONG (WINAPI *NC)(PCONTEXT,BOOLEAN);
typedef VOID (WINAPI *RR)(PEXCEPTION_RECORD);
typedef LONG (WINAPI *NR)(PEXCEPTION_RECORD,PCONTEXT,BOOLEAN);
static NR oNR; static DWORD cNR;
static LONG WINAPI hNR(PEXCEPTION_RECORD r,PCONTEXT c,BOOLEAN f){cNR++;if(r->ExceptionCode==0xe0001234){r->NumberParameters=1;r->ExceptionInformation[0]=0x2468ace0;}return oNR(r,c,f);}
static NC oNC; static RR oRR; static DWORD cVEH,cNC,cRR;
static LONG WINAPI hNC(PCONTEXT c,BOOLEAN a){cNC++;return oNC(c,a);}
static VOID WINAPI hRR(PEXCEPTION_RECORD r){cRR++;oRR(r);}
static void *oKI; static DWORD cKI,ka,kb,kc,vr,vc;
__declspec(naked) static void hKI(void){ __asm {
 push eax
 mov eax,[esp+4]
 mov ka,eax
 mov eax,[esp+8]
 mov kb,eax
 mov eax,[esp+12]
 mov kc,eax
 pushfd
 inc cKI
 popfd
 pop eax
 jmp dword ptr [oKI]
} }
static LONG WINAPI ownVEH(PEXCEPTION_POINTERS p){
 cVEH++;
 if(p->ExceptionRecord->ExceptionCode==0xe0001234)return p->ExceptionRecord->NumberParameters==1&&p->ExceptionRecord->ExceptionInformation[0]==0x2468ace0?EXCEPTION_CONTINUE_EXECUTION:EXCEPTION_CONTINUE_SEARCH;
 if(p->ExceptionRecord->ExceptionCode!=EXCEPTION_BREAKPOINT)return EXCEPTION_CONTINUE_SEARCH;
 vr=(DWORD)(UINT_PTR)p->ExceptionRecord; vc=(DWORD)(UINT_PTR)p->ContextRecord;
 p->ContextRecord->Eip++; return EXCEPTION_CONTINUE_EXECUTION;
}
static void t_exception_layout(void){
 PVOID veh=AddVectoredExceptionHandler(1,ownVEH); void *ki=GetProcAddress(GetModuleHandleA("ntdll.dll"),"KiUserExceptionDispatcher");
 HMODULE nt=GetModuleHandleA("ntdll.dll");
 int ok=veh&&ki&&MH_Initialize()==MH_OK&&MH_CreateHook(ki,hKI,&oKI)==MH_OK;
 ok=ok&&MH_CreateHook(GetProcAddress(nt,"NtContinue"),hNC,(void**)&oNC)==MH_OK;
 ok=ok&&MH_CreateHook(GetProcAddress(nt,"RtlRaiseException"),hRR,(void**)&oRR)==MH_OK;
 ok=ok&&MH_CreateHook(GetProcAddress(nt,"NtRaiseException"),hNR,(void**)&oNR)==MH_OK;
 ok=ok&&MH_EnableHook(MH_ALL_HOOKS)==MH_OK;
 put(ok);if(!ok)return; __debugbreak();
 put(cKI);put(ka==vr);put(kb==vr);put(kb==vc);put(kc==vc);
 RaiseException(0xe0001234,0,0,NULL);
 put(cVEH);put(cNC);put(cRR);put(cKI);put(cNR);
 MH_DisableHook(MH_ALL_HOOKS);RemoveVectoredExceptionHandler(veh);
}


/* ---- 11: loader lifetime reaches the shared native unload boundary ---- */
typedef LONG (WINAPI *LUD)(HMODULE);
static LUD oLUD; static LONG cLUD;
static LONG WINAPI hLUD(HMODULE module){cLUD++; return oLUD(module);}
static void t_unload_hook(void){
 HMODULE module=LoadLibraryA("version.dll");
 void *entry=GetProcAddress(GetModuleHandleA("ntdll.dll"),"LdrUnloadDll");
 int ok=module&&entry&&MH_Initialize()==MH_OK&&MH_CreateHook(entry,hLUD,(void**)&oLUD)==MH_OK&&MH_EnableHook(entry)==MH_OK;
 put(ok);if(!ok)return;
 put(FreeLibrary(module)!=0);put(cLUD);
 MH_DisableHook(entry);
}

/* ---- 12: altered executable loading and module lookup/refcount boundaries ---- */
typedef struct {USHORT Length,MaximumLength;PWSTR Buffer;} LDR_NAME;
typedef LONG (WINAPI *LGH)(PWSTR,PULONG,LDR_NAME*,PVOID*);
typedef LONG (WINAPI *LGHE)(ULONG,PWSTR,PULONG,LDR_NAME*,PVOID*);
typedef LONG (WINAPI *LAR)(ULONG,PVOID);
typedef PVOID (WINAPI *RPC)(PVOID,PVOID*);
static LGH oLGH;static LGHE oLGHE;static LAR oLAR;static RPC oRPC;
static DWORD cLGH,cLGHE,cLAR,cRPC,last_ref_flags;
static LONG WINAPI hLGH(PWSTR a,PULONG b,LDR_NAME*c,PVOID*d){cLGH++;return oLGH(a,b,c,d);}
static LONG WINAPI hLGHE(ULONG f,PWSTR a,PULONG b,LDR_NAME*c,PVOID*d){cLGHE++;return oLGHE(f,a,b,c,d);}
static LONG WINAPI hLAR(ULONG f,PVOID m){cLAR++;last_ref_flags=f;return oLAR(f,m);}
static PVOID WINAPI hRPC(PVOID p,PVOID*out){cRPC++;return oRPC(p,out);}
static void t_loader_queries(void){
 HMODULE nt=GetModuleHandleA("ntdll.dll"),m,out,exe=GetModuleHandleA(NULL);WCHAR path[MAX_PATH];
 void *load,*get,*get_ex,*retain,*pc;DWORD a,b,c,d,len;LONG status;LDR_NAME name;
 static WCHAR missing[]=L"fixture-never-created-personality.dll";static WCHAR suffix[]=L"\\version.dll";
 int ok=MH_Initialize()==MH_OK;
 len=GetSystemDirectoryW(path,MAX_PATH);if(!len||len+sizeof(suffix)/2>MAX_PATH){put(0);return;}
 for(a=0;a<sizeof(suffix)/2;a++)path[len+a]=suffix[a];
 load=GetProcAddress(nt,"LdrLoadDll");get=GetProcAddress(nt,"LdrGetDllHandle");get_ex=GetProcAddress(nt,"LdrGetDllHandleEx");retain=GetProcAddress(nt,"LdrAddRefDll");pc=GetProcAddress(nt,"RtlPcToFileHeader");
 ok=ok&&MH_CreateHook(load,hLLD,(void**)&oLLD)==MH_OK&&MH_CreateHook(get,hLGH,(void**)&oLGH)==MH_OK&&MH_CreateHook(get_ex,hLGHE,(void**)&oLGHE)==MH_OK&&MH_CreateHook(retain,hLAR,(void**)&oLAR)==MH_OK&&MH_CreateHook(pc,hRPC,(void**)&oRPC)==MH_OK&&MH_EnableHook(MH_ALL_HOOKS)==MH_OK;
 put(ok);if(!ok)return;
 a=cLLD;m=LoadLibraryExW(path,NULL,LOAD_WITH_ALTERED_SEARCH_PATH);put(m!=NULL);put(cLLD-a);put(ldr_search_flags==9);if(!m)return;
 a=cLGH;b=cLGHE;put(GetModuleHandleA("version.dll")==m);put(cLGH-a);put(cLGHE-b);
 a=cLGH;b=cLGHE;put(GetModuleHandleW(path)==m);put(cLGH-a);put(cLGHE-b);
 a=cLGH;b=cLGHE;c=cLAR;put(GetModuleHandleExA(2,"version.dll",&out)&&out==m);put(cLGH>a&&cLGHE>b);put(cLAR==c);
 a=cLGH;b=cLGHE;c=cLAR;put(GetModuleHandleExW(0,path,&out)&&out==m);put(cLGH>a&&cLGHE>b);put(cLAR>c&&last_ref_flags==0);FreeLibrary(out);
 a=cLGH;b=cRPC;c=cLAR;put(GetModuleHandleExW(6,(LPCWSTR)((BYTE*)m+1024),&out)&&out==m);put(cRPC>b&&cLGH==a);put(cLAR==c);
 b=cRPC;c=cLAR;put(GetModuleHandleExA(4,(LPCSTR)((BYTE*)m+1024),&out)&&out==m);put(cRPC>b);put(cLAR>c&&last_ref_flags==0);FreeLibrary(out);
 a=cLGH;b=cLGHE;c=cLAR;put(GetModuleHandleExW(1,path,&out)&&out==m);put(cLGH>a&&cLGHE>b);put(cLAR>c&&last_ref_flags==1);
 a=cLGH;b=cLGHE;c=cLAR;d=cRPC;put(GetModuleHandleExW(0,NULL,&out)&&out==exe);put(cLGH==a&&cLGHE==b&&cLAR==c&&cRPC==d);
 a=cLGH;b=cLGHE;put(GetModuleHandleW(missing)==NULL&&GetLastError()==ERROR_MOD_NOT_FOUND);put(cLGH>a&&cLGHE>b);
 name.Length=sizeof(missing)-2;name.MaximumLength=sizeof(missing);name.Buffer=missing;out=m;SetLastError(0x13572468);
 status=oLGHE(1,NULL,NULL,&name,(PVOID*)&out);put(status);put(out==m);put(GetLastError()==0x13572468);
 status=oLGHE(3,NULL,NULL,&name,(PVOID*)&out);put(status);put(out==m);
 name.Length=(USHORT)(lstrlenW(path)*2);name.MaximumLength=name.Length+2;name.Buffer=path;out=NULL;
 status=oLLD((PWSTR)9,NULL,&name,(PVOID*)&out);put(status);put(out==m);FreeLibrary(out);
 MH_DisableHook(get);a=cLGH;b=cLGHE;put(GetModuleHandleA("version.dll")==m);put(cLGH==a&&cLGHE>b);
 MH_DisableHook(MH_ALL_HOOKS);FreeLibrary(m);
}

/* ---- 13: one mutant/semaphore owner for native and Win32 calls ---- */
typedef LONG (WINAPI *NCM)(PHANDLE,ULONG,PVOID,BYTE);
typedef LONG (WINAPI *NRM)(HANDLE,PLONG);
typedef LONG (WINAPI *NCS)(PHANDLE,ULONG,PVOID,LONG,LONG);
typedef LONG (WINAPI *NRS)(HANDLE,LONG,PLONG);
static void t_owned_sync(void){
 HMODULE nt=GetModuleHandleW(L"ntdll.dll");
 NCM cm=(NCM)GetProcAddress(nt,"NtCreateMutant");NRM rm=(NRM)GetProcAddress(nt,"NtReleaseMutant");
 NCS cs=(NCS)GetProcAddress(nt,"NtCreateSemaphore");NRS rs=(NRS)GetProcAddress(nt,"NtReleaseSemaphore");
 HANDLE mutant,semaphore,output;LONG previous;
 if(!cm||!rm||!cs||!rs){put(0xbad);return;}
 put(cm(&mutant,0x1f0001,NULL,1));put(WaitForSingleObject(mutant,0));
 previous=0x12345678;put(rm(mutant,&previous));put(previous);
 previous=0x12345678;put(rm(mutant,&previous));put(previous);
 previous=0x12345678;put(rm(mutant,&previous));put(previous);
 put(ReleaseMutex(mutant));put(GetLastError());
 put(cs(&semaphore,0x1f0003,NULL,1,2));
 previous=0x12345678;put(rs(semaphore,0,&previous));put(previous);
 previous=0x12345678;put(rs(semaphore,2,&previous));put(previous);
 previous=0x12345678;put(rs(semaphore,1,&previous));put(previous);
 previous=0x12345678;put(ReleaseSemaphore(semaphore,-1,&previous));put(GetLastError());put(previous);
 output=(HANDLE)0x12345678;put(cs(&output,0x1f0003,NULL,-1,2));put((DWORD)(ULONG_PTR)output);
 output=(HANDLE)0x12345678;put(cs(&output,0x1f0003,NULL,0,0));put((DWORD)(ULONG_PTR)output);
 put(rs(semaphore,1,(LONG*)1));put(WaitForSingleObject(semaphore,0));put(WaitForSingleObject(semaphore,0));put(WaitForSingleObject(semaphore,0));
 put(ReleaseMutex((HANDLE)0x12345678));put(GetLastError());
 CloseHandle(mutant);CloseHandle(semaphore);
}

/* ---- 14: fixed path-mutation topology, fallback and deletion lifetime ---- */
typedef struct {LONG Status;ULONG_PTR Information;} MUT_IO;
typedef LONG (WINAPI *M_CLOSE)(HANDLE);
typedef LONG (WINAPI *M_SET)(HANDLE,MUT_IO*,PVOID,ULONG,ULONG);
typedef LONG (WINAPI *M_OPEN)(PHANDLE,ULONG,PVOID,MUT_IO*,ULONG,ULONG);
typedef LONG (WINAPI *M_CREATE)(PHANDLE,ULONG,PVOID,MUT_IO*,PVOID,ULONG,ULONG,ULONG,ULONG,PVOID,ULONG);
static M_CLOSE m_close;static M_SET m_set;static M_OPEN m_open;static M_CREATE m_create;
static DWORD m_active,m_inject,m_status,m_forced,m_closes,m_opens,m_creates,m_od,m_os,m_oo,m_cd,m_cs,m_cp,m_co,m_ca,m_sets,m_class[2],m_flags[2];
static LONG WINAPI mutation_close(HANDLE h){LONG status=m_close(h);if(m_active){m_closes++;if(m_active==1&&status>=0){m_forced++;return (LONG)0xc0000022;}}return status;}
static LONG WINAPI mutation_open(PHANDLE h,ULONG d,PVOID a,MUT_IO*i,ULONG s,ULONG o){if(m_active){m_opens++;m_od=d;m_os=s;m_oo=o;}return m_open(h,d,a,i,s,o);}
static LONG WINAPI mutation_create(PHANDLE h,ULONG d,PVOID a,MUT_IO*i,PVOID z,ULONG attrs,ULONG s,ULONG p,ULONG o,PVOID e,ULONG n){if(m_active){m_creates++;m_cd=d;m_cs=s;m_cp=p;m_co=o;m_ca=attrs;}return m_create(h,d,a,i,z,attrs,s,p,o,e,n);}
static LONG WINAPI mutation_set(HANDLE h,MUT_IO*i,PVOID data,ULONG length,ULONG cls){
 if(m_active){
  if(m_sets<2){m_class[m_sets]=cls;m_flags[m_sets]=(cls==13||cls==10)&&length?*(BYTE*)data:length>=4?*(DWORD*)data:0;}m_sets++;
  if(m_inject&&cls==64){i->Status=(LONG)m_status;i->Information=0;return (LONG)m_status;}
 }
 return m_set(h,i,data,length,cls);
}
static void mutation_reset(void){m_forced=m_closes=m_opens=m_creates=m_od=m_os=m_oo=m_cd=m_cs=m_cp=m_co=m_ca=m_sets=0;m_class[0]=m_class[1]=m_flags[0]=m_flags[1]=0;}
static void mutation_file(LPCWSTR name){HANDLE h=CreateFileW(name,GENERIC_WRITE,0,NULL,CREATE_NEW,FILE_ATTRIBUTE_NORMAL,NULL);DWORD written;if(h==INVALID_HANDLE_VALUE||!WriteFile(h,"authored",8,&written,NULL)||written!=8||!CloseHandle(h))ExitProcess(92);}
static void mutation_operation(DWORD operation){
 WCHAR source[]=L"fixture-op-0-src.txt",target[]=L"fixture-op-0-dst.txt";DWORD result,error,attrs;BOOL open_ok,set_ok,state_ok;
 source[11]=target[11]=(WCHAR)(L'0'+operation);
 if(operation<2)mutation_file(source);else if(operation==3&&!CreateDirectoryW(source,NULL))ExitProcess(93);
 mutation_reset();m_active=1;SetLastError(0x13572468);
 result=operation==0?DeleteFileW(source):operation==1?MoveFileW(source,target):operation==2?CreateDirectoryW(source,NULL):RemoveDirectoryW(source);
 error=GetLastError();m_active=0;attrs=GetFileAttributesW(source);
 open_ok=operation==2?m_opens==0&&m_creates==1&&m_cd==0x100001&&m_cs==3&&m_cp==2&&m_co==0x204021&&m_ca==0x80:m_opens==1&&m_creates==0&&m_od==(operation==0?0x10080:0x110080)&&m_os==7&&m_oo==(operation==0?0x204040:operation==1?0x204020:0x204021);
 set_ok=operation==2?m_sets==0:operation==1?m_sets==1&&m_class[0]==10&&m_flags[0]==0:m_sets==1&&m_class[0]==64&&m_flags[0]==3;
 state_ok=operation==2?attrs!=INVALID_FILE_ATTRIBUTES&&(attrs&FILE_ATTRIBUTE_DIRECTORY)!=0:attrs==INVALID_FILE_ATTRIBUTES;
 state_ok=state_ok&&((GetFileAttributesW(target)!=INVALID_FILE_ATTRIBUTES)==(operation==1));
 put(result!=0);put(error==0x13572468);put(open_ok);put(set_ok);put(m_closes==1&&m_forced==1);put(state_ok);
}
static void mutation_fallback(DWORD index,DWORD status){
 WCHAR name[]=L"fixture-fallback-A.txt";DWORD result,error,attrs;BOOL success=(LONG)status>=0,blocked=status==0xc0000121;
 name[17]=(WCHAR)(L'A'+index);mutation_file(name);mutation_reset();m_status=status;m_inject=1;m_active=3;SetLastError(0x13572468);
 result=DeleteFileW(name);error=GetLastError();m_active=m_inject=0;attrs=GetFileAttributesW(name);
 put((result!=0)==!blocked&&error==(blocked?5:0x13572468)&&m_sets==(success||blocked?1:2)&&m_class[0]==64&&m_flags[0]==3&&(success||blocked||(m_class[1]==13&&m_flags[1]==1))&&m_closes==1&&((attrs!=INVALID_FILE_ATTRIBUTES)==(success||blocked)));
}
static void mutation_snapshot(LPCWSTR name,HANDLE remaining,DWORD expected_error){
 DWORD attrs,error,opened,open_error,read=0;HANDLE h;BYTE bytes[8];BOOL readable=FALSE;
 attrs=GetFileAttributesW(name);error=GetLastError();h=CreateFileW(name,GENERIC_READ,7,NULL,OPEN_EXISTING,FILE_ATTRIBUTE_NORMAL,NULL);opened=h!=INVALID_HANDLE_VALUE;open_error=GetLastError();if(opened)CloseHandle(h);
 if(remaining){SetFilePointer(remaining,0,NULL,FILE_BEGIN);readable=ReadFile(remaining,bytes,8,&read,NULL);}
 put(attrs==INVALID_FILE_ATTRIBUTES&&error==expected_error&&!opened&&open_error==expected_error);
 put(remaining?readable&&read==8:!readable&&read==0);
}
static void mutation_lifetime(DWORD classic){
 WCHAR name[]=L"fixture-life-0.txt";HANDLE original,independent,duplicate,deletion;MUT_IO io;BYTE disposition=1;DWORD result,error;
 name[13]=(WCHAR)(L'0'+classic);mutation_file(name);
 original=CreateFileW(name,GENERIC_READ|DELETE,7,NULL,OPEN_EXISTING,FILE_ATTRIBUTE_NORMAL,NULL);independent=CreateFileW(name,GENERIC_READ|DELETE,7,NULL,OPEN_EXISTING,FILE_ATTRIBUTE_NORMAL,NULL);
 if(original==INVALID_HANDLE_VALUE||independent==INVALID_HANDLE_VALUE||!DuplicateHandle(GetCurrentProcess(),original,GetCurrentProcess(),&duplicate,0,FALSE,DUPLICATE_SAME_ACCESS))ExitProcess(94);
 if(classic){deletion=CreateFileW(name,DELETE,7,NULL,OPEN_EXISTING,FILE_ATTRIBUTE_NORMAL,NULL);if(deletion==INVALID_HANDLE_VALUE)ExitProcess(95);SetLastError(0x13572468);result=m_set(deletion,&io,&disposition,1,13);error=GetLastError();CloseHandle(deletion);}
 else{SetLastError(0x13572468);result=DeleteFileW(name);error=GetLastError();}
 put((classic?result==0:result!=0)&&error==0x13572468);
 mutation_snapshot(name,original,classic?5:2);CloseHandle(independent);mutation_snapshot(name,original,classic?5:2);CloseHandle(original);mutation_snapshot(name,duplicate,classic?5:2);CloseHandle(duplicate);mutation_snapshot(name,NULL,2);
}
static void mutation_selection(DWORD operation){
 WCHAR source[]=L"fixture-select-0-src.txt",target[]=L"fixture-select-0-dst.txt";DWORD result,error,attrs;BOOL changed;
 source[15]=target[15]=(WCHAR)(L'0'+operation);
 if(operation<2)mutation_file(source);else if(operation==2&&!CreateDirectoryW(source,NULL))ExitProcess(96);
 mutation_reset();m_active=1;SetLastError(0x13572468);
 result=operation==0?DeleteFileW(source):operation==1?MoveFileW(source,target):operation==2?RemoveDirectoryW(source):CreateDirectoryW(source,NULL);
 error=GetLastError();m_active=0;attrs=GetFileAttributesW(source);
 changed=operation==3?attrs!=INVALID_FILE_ATTRIBUTES&&(attrs&FILE_ATTRIBUTE_DIRECTORY):attrs==INVALID_FILE_ATTRIBUTES;
 put(result!=0&&error==0x13572468&&m_opens==0&&m_creates==0&&m_closes==1&&m_forced==1&&changed&&((GetFileAttributesW(target)!=INVALID_FILE_ATTRIBUTES)==(operation==1)));
}
static void t_mutation_contract(void){
 HMODULE nt=GetModuleHandleW(L"ntdll.dll");DWORD i;
 static DWORD statuses[]={0xc0000003,0xc000000d,0xc00000bb,0xc0000022,0xc0000121,0xc0000043,0,0x40000000,0x80000002,0x80000005,0xc0000001,0xc0000008,0xc0000005};
 int ok=MH_Initialize()==MH_OK;
 ok=ok&&MH_CreateHook(GetProcAddress(nt,"NtClose"),mutation_close,(PVOID*)&m_close)==MH_OK&&MH_CreateHook(GetProcAddress(nt,"NtSetInformationFile"),mutation_set,(PVOID*)&m_set)==MH_OK&&MH_CreateHook(GetProcAddress(nt,"NtOpenFile"),mutation_open,(PVOID*)&m_open)==MH_OK&&MH_CreateHook(GetProcAddress(nt,"NtCreateFile"),mutation_create,(PVOID*)&m_create)==MH_OK&&MH_EnableHook(MH_ALL_HOOKS)==MH_OK;
 put(ok);if(!ok)return;for(i=0;i<4;i++)mutation_operation(i);for(i=0;i<sizeof(statuses)/sizeof(statuses[0]);i++)mutation_fallback(i,statuses[i]);mutation_lifetime(0);mutation_lifetime(1);
 /* A hook on the other entry must not change the frontend's fixed topology. */
 MH_DisableHook(GetProcAddress(nt,"NtOpenFile"));for(i=0;i<3;i++)mutation_selection(i);
 MH_EnableHook(GetProcAddress(nt,"NtOpenFile"));MH_DisableHook(GetProcAddress(nt,"NtCreateFile"));mutation_selection(3);MH_DisableHook(MH_ALL_HOOKS);
}


void mainCRTStartup(void){
  char cl[MAX_PATH]; char t; int len=(int)GetModuleFileNameA(NULL,cl,MAX_PATH);
  main_ret=_ReturnAddress();
  t=len>5?cl[len-5]:'0'; /* probe-N.exe */
  switch(t){case '1':t_threads();break;case '2':t_file_hooks();break;case '3':t_identity();break;
    case '4':t_exports();break;case '5':t_context();break;case '6':t_heap_hooks();break;case '7':t_loader_hooks();break;case '8':t_ansi_names();break;case '9':t_memory_hooks();break;case 'a':t_exception_layout();break;case 'b':t_unload_hook();break;case 'c':t_loader_queries();break;case 'd':t_owned_sync();break;case 'e':t_mutation_contract();break;default:ExitProcess(91);}
  save(t); ExitProcess(0);
}

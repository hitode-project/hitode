/* Authored x86 OpenMP single-team oracle; writes eight boolean invariants. */
#include <windows.h>
typedef void (__cdecl *FORK)(int,int,void*,...);
typedef void (__cdecl *RANGE)(unsigned,unsigned,int,int,unsigned*,unsigned*);
typedef void (__cdecl *END)(void);
typedef void (__cdecl *THREADS)(int);
static FORK fork_region;
static RANGE static_range;
static END static_end;
static DWORD values[8],visits,nested;
static void __cdecl leaf(DWORD *count) {(*count)++;}
static void __cdecl region(DWORD *sum,DWORD *first,DWORD *last) {
 unsigned begin,end,i;
 visits++;
 static_range(*first,*last,3,TRUE,&begin,&end);
 for(i=begin;i<=end;i+=3)*sum+=i;
 static_end();fork_region(FALSE,1,leaf,&nested);
}
void mainCRTStartup(void) {
 HMODULE module=LoadLibraryW(L"vcomp140.dll");HANDLE file;DWORD written,sum=0,first=5,last=17;unsigned begin,end;
 THREADS threads=(THREADS)GetProcAddress(module,"omp_set_num_threads");
 fork_region=(FORK)GetProcAddress(module,"_vcomp_fork");
 static_range=(RANGE)GetProcAddress(module,"_vcomp_for_static_simple_init");
 static_end=(END)GetProcAddress(module,"_vcomp_for_static_end");
 if(!fork_region||!static_range||!static_end)ExitProcess(2);
 if(threads)threads(1);
 values[0]=module!=NULL;
 fork_region(TRUE,3,region,&sum,&first,&last);
 values[1]=sum==55;values[2]=visits==1;values[3]=nested==1;
 static_range(19,1,3,FALSE,&begin,&end);values[4]=begin==19&&end==1;
 static_range(7,7,1,TRUE,&begin,&end);values[5]=begin==7&&end==7;
 static_range(0xfffffffd,0xffffffff,1,TRUE,&begin,&end);values[6]=begin==0xfffffffd&&end==0xffffffff;
 values[7]=first==5&&last==17;
 file=CreateFileW(L"vcomp.bin",GENERIC_WRITE,0,NULL,CREATE_ALWAYS,0,NULL);
 if(file==INVALID_HANDLE_VALUE||!WriteFile(file,values,sizeof(values),&written,NULL)||written!=sizeof(values))ExitProcess(3);
 CloseHandle(file);ExitProcess(0);
}

#define WIN32_LEAN_AND_MEAN
#include <windows.h>
typedef struct { void *first,*last,*end; } Table;
typedef int (__cdecl *Callback)(void);
typedef int (__cdecl *Init)(Table*);
typedef int (__cdecl *Add)(Table*,Callback);
typedef unsigned long long (__cdecl *Parse)(const WCHAR*,WCHAR**,int);
typedef int* (__cdecl *Errno)(void);
static DWORD out[1024], count;
static Table table;
static Add add;
static void rec(DWORD value){out[count++]=value;}
static int __cdecl a(void){rec(1);Sleep(0);return 0;}
static int __cdecl b(void){rec(2);return 9;}
static int __cdecl d(void){rec(4);return 0;}
static int __cdecl c(void){rec(3);rec(add(&table,d));return 0;}
void mainCRTStartup(void){
    HMODULE u=LoadLibraryA("ucrtbase.dll");
    HMODULE runtime=u,convert=u;
#ifdef APISETS
    runtime=LoadLibraryA("api-ms-win-crt-runtime-l1-1-0.dll");
    convert=LoadLibraryA("api-ms-win-crt-convert-l1-1-0.dll");
#endif
    Init init=(Init)GetProcAddress(runtime,"_initialize_onexit_table");
    Init execute=(Init)GetProcAddress(runtime,"_execute_onexit_table");
    Parse parse=(Parse)GetProcAddress(convert,"wcstoull");
    Errno error=(Errno)GetProcAddress(u,"_errno");
    DWORD wrote,i; HANDLE file;
    const WCHAR *texts[]={L"",L"18446744073709551615!",L"18446744073709551616!",L"  -1x",L"  +0x1fZ",L"0xG",L"09",L" +xyz",L"z",L"010",L"-18446744073709551616"};
    int bases[]={10,10,10,0,0,0,0,10,36,0,10};
    add=(Add)GetProcAddress(runtime,"_register_onexit_function");
    if(!init||!execute||!parse||!error||!add)ExitProcess(90);
    rec(init(0));rec(execute(0));rec(add(0,a));
    rec(init(&table));rec(add(&table,a));rec(add(&table,b));rec(add(&table,c));
    rec(init(&table));rec(execute(&table));
    rec(init(&table));rec(add(&table,0));rec(execute(&table));
    rec(init(&table));for(i=0;i<65;i++)rec(add(&table,a));rec(execute(&table));
    for(i=0;i<sizeof(bases)/sizeof(bases[0]);i++){
        WCHAR *end=0; unsigned long long result;
        *error()=777;result=parse(texts[i],&end,bases[i]);
        rec((DWORD)result);rec((DWORD)(result>>32));rec((DWORD)(end-texts[i]));rec(*error());
    }
    rec(0xcab1e123);
    file=CreateFileA("crt-exit-convert.bin",GENERIC_WRITE,0,0,CREATE_ALWAYS,0,0);
    if(file==INVALID_HANDLE_VALUE||!WriteFile(file,out,count*4,&wrote,0)||wrote!=count*4)ExitProcess(91);
    CloseHandle(file);ExitProcess(0);
}

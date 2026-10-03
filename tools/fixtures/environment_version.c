/* Process-local environment and relative OS-version ABI oracle. */
#include "win32_min.h"
typedef unsigned long long U64;
API BOOL WINAPI SetEnvironmentVariableA(const char *,const char *);
API BOOL WINAPI SetEnvironmentVariableW(const WORD *,const WORD *);
API DWORD WINAPI GetEnvironmentVariableA(const char *,char *,DWORD);
API DWORD WINAPI GetEnvironmentVariableW(const WORD *,WORD *,DWORD);
API DWORD WINAPI ExpandEnvironmentStringsA(const char *,char *,DWORD);
API DWORD WINAPI ExpandEnvironmentStringsW(const WORD *,WORD *,DWORD);
API char *WINAPI GetEnvironmentStringsA(void);
API WORD *WINAPI GetEnvironmentStringsW(void);
API BOOL WINAPI FreeEnvironmentStringsA(char *);
API BOOL WINAPI FreeEnvironmentStringsW(WORD *);
API BOOL WINAPI GetVersionExA(void *);
API BOOL WINAPI GetVersionExW(void *);
API BOOL WINAPI VerifyVersionInfoA(void *,DWORD,U64);
API BOOL WINAPI VerifyVersionInfoW(void *,DWORD,U64);
API U64 WINAPI VerSetConditionMask(U64,DWORD,BYTE);
API void WINAPI SetLastError(DWORD);
API DWORD WINAPI GetLastError(void);
API HANDLE WINAPI CreateFileA(const char *,DWORD,DWORD,void *,DWORD,DWORD,HANDLE);
API BOOL WINAPI WriteFile(HANDLE,const void *,DWORD,DWORD *,void *);
static DWORD results[2048],count;
static void emit(DWORD v) {results[count++]=v;}
static void zero(void *p,DWORD n) {BYTE *b=p;while(n--) *b++=0;}
static void text(const char *s) {while(*s) emit((BYTE)*s++);emit(0);}
static void wide(const WORD *s) {while(*s) emit(*s++);emit(0);}
static int eq(const char *a,const char *b) {while(*a&&*a==*b){++a;++b;}return *a==*b;}
static int weq(const WORD *a,const WORD *b) {while(*a&&*a==*b){++a;++b;}return *a==*b;}
static int has(char *p,const char *needle) {while(*p){if(eq(p,needle)) return 1;while(*p++){};}return 0;}
static int whas(WORD *p,const WORD *needle) {while(*p){if(weq(p,needle)) return 1;while(*p++){};}return 0;}
static void check_version(DWORD *v,int w,DWORD mask,U64 condition) {
    SetLastError(0x1234);
    emit(w?VerifyVersionInfoW(v,mask,condition):VerifyVersionInfoA(v,mask,condition));
    emit(GetLastError());
}
void mainCRTStartup(void) {
    char buffer[512],*snapshot; WORD wbuffer[512],*wsnapshot;
    DWORD va[39],vw[71],v[71],bit,op,i,j,size,written; U64 condition;
    HANDLE file;
    emit(SetEnvironmentVariableA("ADV_FIXTURE_ENV_83F2","Alpha"));
    emit(GetEnvironmentVariableA("adv_fixture_env_83f2",NULL,0));
    emit(GetEnvironmentVariableW((const WORD *)L"ADV_FIXTURE_ENV_83F2",wbuffer,512));wide(wbuffer);
    snapshot=GetEnvironmentStringsA(); wsnapshot=GetEnvironmentStringsW();
    emit(snapshot!=NULL);emit(wsnapshot!=NULL);
    emit(has(snapshot,"ADV_FIXTURE_ENV_83F2=Alpha"));
    emit(whas(wsnapshot,(const WORD *)L"ADV_FIXTURE_ENV_83F2=Alpha"));
    emit(SetEnvironmentVariableW((const WORD *)L"ADV_FIXTURE_ENV_83F2",(const WORD *)L"Beta"));
    emit(GetEnvironmentVariableA("ADV_FIXTURE_ENV_83F2",buffer,512));text(buffer);
    emit(has(snapshot,"ADV_FIXTURE_ENV_83F2=Alpha"));
    emit(whas(wsnapshot,(const WORD *)L"ADV_FIXTURE_ENV_83F2=Alpha"));
    emit(FreeEnvironmentStringsA(snapshot));emit(FreeEnvironmentStringsW(wsnapshot));
    emit(SetEnvironmentVariableA("ADV_FIXTURE_ENV_83F2","%ADV_FIXTURE_OTHER_83F2%"));
    emit(SetEnvironmentVariableA("ADV_FIXTURE_OTHER_83F2","unexpanded"));
    emit(ExpandEnvironmentStringsA("a%ADV_FIXTURE_ENV_83F2%b%MISSING_83F2%c%%",NULL,0));
    emit(ExpandEnvironmentStringsA("a%ADV_FIXTURE_ENV_83F2%b%MISSING_83F2%c%%",buffer,512));text(buffer);
    emit(ExpandEnvironmentStringsW((const WORD *)L"%adv_fixture_other_83f2%/tail%",wbuffer,512));wide(wbuffer);
    emit(SetEnvironmentVariableA("ADV_FIXTURE_ENV_83F2",""));SetLastError(0x1234);
    buffer[0]='!';emit(GetEnvironmentVariableA("ADV_FIXTURE_ENV_83F2",buffer,512));emit(GetLastError());emit(buffer[0]);
    emit(SetEnvironmentVariableA("ADV_FIXTURE_ENV_83F2",NULL));SetLastError(0x1234);
    emit(GetEnvironmentVariableA("ADV_FIXTURE_ENV_83F2",buffer,512));emit(GetLastError());
    emit(SetEnvironmentVariableA("ADV_FIXTURE_ENV_83F2",NULL));
    emit(SetEnvironmentVariableA("ADV_FIXTURE_OTHER_83F2",NULL));
    emit(SetEnvironmentVariableW((const WORD *)L"ADV_FIXTURE_ENV_83F2",(const WORD *)L"\x6f22\x5b57"));
    emit(GetEnvironmentVariableW((const WORD *)L"adv_fixture_env_83f2",wbuffer,512));wide(wbuffer);
    emit(SetEnvironmentVariableW((const WORD *)L"ADV_FIXTURE_ENV_83F2",NULL));
    SetLastError(0x1234);emit(SetEnvironmentVariableA("a=b","x"));emit(GetLastError());
    SetLastError(0x1234);emit(SetEnvironmentVariableA("","x"));emit(GetLastError());
    for(i=0;i<7;++i){buffer[0]='!';SetLastError(0x1234);emit(ExpandEnvironmentStringsA("abc",buffer,i));emit(GetLastError());}
    zero(va,sizeof(va));zero(vw,sizeof(vw)); va[0]=156;vw[0]=284;
    emit(GetVersionExA(va));emit(GetVersionExW(vw));
    emit(va[1]==vw[1]&&va[2]==vw[2]&&va[3]==vw[3]&&va[4]==vw[4]);
    for(bit=1;bit<=128;bit<<=1) for(op=0;op<9;++op) {
        condition=VerSetConditionMask(0,bit,(BYTE)op); emit((DWORD)condition);emit((DWORD)(condition>>32));
    }
    for(bit=0;bit<4;++bit) {condition=VerSetConditionMask(0x1122334455667788ULL,bit,3);emit((DWORD)condition);emit((DWORD)(condition>>32));}
    for(i=0;i<2;++i) {
        DWORD *source=i?vw:va;size=i?284:156;
        for(bit=1;bit<=128;bit<<=1) for(op=1;op<=7;++op) {
            for(j=0;j<size/4;++j)v[j]=source[j];
            check_version(v,i,bit,VerSetConditionMask(0,bit,(BYTE)op));
        }
        for(j=0;j<size/4;++j)v[j]=source[j];
        condition=0;condition=VerSetConditionMask(condition,2,3);condition=VerSetConditionMask(condition,1,3);
        condition=VerSetConditionMask(condition,32,3);condition=VerSetConditionMask(condition,16,3);
        check_version(v,i,0x33,condition);
        --v[1];v[2]=999;check_version(v,i,0x33,condition);
        v[1]=source[1];++v[2];check_version(v,i,0x33,condition);
        v[1]=source[1]+1;v[2]=0;check_version(v,i,0x33,condition);
        check_version(v,i,0,condition);check_version(v,i,2,0);
        v[0]=0;check_version(v,i,2,condition);
    }
    file=CreateFileA("environment-version.bin",0x40000000,0,NULL,2,0,0);
    if(file==0xffffffffu||!WriteFile(file,results,count*4,&written,NULL)||written!=count*4)ExitProcess(4);
    CloseHandle(file);ExitProcess(0);
}

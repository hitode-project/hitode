#define WIN32_LEAN_AND_MEAN
#include <windows.h>

static DWORD words[256];
static UINT used;
static char tree[128] = "Software\\CodexPalRegistryFixture-";
static WCHAR name[32], klass[32];
static BYTE data[32];
static DWORD normalized_values[2][5];
static void record(DWORD value) { words[used++] = value; }
static void suffix(DWORD value, UINT at) {
    UINT i; const char *hex = "0123456789abcdef";
    for (i = 0; i < 8; ++i) tree[at + i] = hex[(value >> (28 - 4 * i)) & 15];
}
static void reset(void) {
    UINT i; for (i = 0; i < 32; ++i) { name[i] = klass[i] = 0xcccc; data[i] = 0xcc; }
}
static void info(HKEY key, BOOL wide) {
    DWORD len=32, sub=0, maxsub=0, maxclass=0, values=0, maxname=0, maxdata=0, security=0;
    FILETIME ft={0,0}; reset();
    if (wide) record(RegQueryInfoKeyW(key,klass,&len,NULL,&sub,&maxsub,&maxclass,&values,&maxname,&maxdata,&security,&ft));
    else record(RegQueryInfoKeyA(key,(char*)klass,&len,NULL,&sub,&maxsub,&maxclass,&values,&maxname,&maxdata,&security,&ft));
    record(len); record(sub); record(maxsub); record(maxclass); record(values); record(maxname); record(maxdata);
    record(security >= 20); record(ft.dwLowDateTime != 0 || ft.dwHighDateTime != 0);
}
void mainCRTStartup(void) {
    HKEY key=NULL, a=NULL, b=NULL, replacement=NULL, query_only=NULL;
    DWORD disposition=0, size, type, count, class_count, status, written, details[7];
    FILETIME ft;
    HANDLE file;
    UINT i, slot;
    /* The fixture owns only this freshly created, volatile HKCU subtree. */
    i=sizeof("Software\\CodexPalRegistryFixture-")-1;
    suffix(GetCurrentProcessId(),i); tree[i+8]='-'; suffix(GetTickCount(),i+9); tree[i+17]=0;
    status=RegCreateKeyExA(HKEY_CURRENT_USER,tree,0,"RootClass",REG_OPTION_VOLATILE,KEY_ALL_ACCESS,NULL,&key,&disposition);
    if (status != 0 || disposition != REG_CREATED_NEW_KEY) ExitProcess(20);
    record(status); record(disposition);
    record(RegCreateKeyExW(key,L"bKey",0,L"TypeB",REG_OPTION_VOLATILE,KEY_ALL_ACCESS,NULL,&b,NULL));
    record(RegCreateKeyExW(key,L"aKEY",0,L"TypeA",REG_OPTION_VOLATILE,KEY_ALL_ACCESS,NULL,&a,NULL));
    record(RegSetValueExW(key,L"foo",0,REG_SZ,(BYTE*)L"old",8));
    record(RegSetValueExW(key,L"FOO",0,REG_SZ,(BYTE*)L"new",8));
    record(RegSetValueExA(key,NULL,0,REG_BINARY,(BYTE*)"ab",2));
    info(key,TRUE); info(key,FALSE);
    /* QueryInfo has distinct ANSI/Unicode short-class behavior. */
    reset(); count=2;
    for(i=0;i<7;++i) details[i]=0xcccccccc;
    ft.dwLowDateTime=ft.dwHighDateTime=0xcccccccc;
    record(RegQueryInfoKeyW(key,klass,&count,NULL,&details[0],&details[1],&details[2],&details[3],&details[4],&details[5],&details[6],&ft));
    record(count); for(i=0;i<7;++i) record(details[i]); record(klass[0]);
    record(ft.dwLowDateTime); record(ft.dwHighDateTime);
    reset(); count=2; size=0xcccccccc;
    record(RegQueryInfoKeyA(key,(char*)klass,&count,NULL,&size,NULL,NULL,NULL,NULL,NULL,NULL,NULL));
    record(count); record(size); record(((BYTE*)klass)[0]); record(((BYTE*)klass)[1]);
    reset(); count=0; size=0xcccccccc;
    record(RegQueryInfoKeyW(key,klass,&count,NULL,&size,NULL,NULL,NULL,NULL,NULL,NULL,NULL));
    record(count); record(size); record(klass[0]);
    reset(); count=0;
    record(RegQueryInfoKeyA(key,(char*)klass,&count,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL)); record(count);
    count=0; record(RegQueryInfoKeyW(key,NULL,&count,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL)); record(count);
    count=0; record(RegQueryInfoKeyA(key,NULL,&count,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL)); record(count);
    count=1; record(RegQueryInfoKeyW(key,NULL,&count,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL)); record(count);
    count=1; record(RegQueryInfoKeyA(key,NULL,&count,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL)); record(count);
    for(i=0;i<2;++i) {
        reset(); count=32; class_count=32; ft.dwLowDateTime=ft.dwHighDateTime=0;
        record(RegEnumKeyExW(key,i,name,&count,NULL,klass,&class_count,&ft));
        record(count); record(class_count); record(name[0]); record(name[1]); record(klass[4]);
        record(ft.dwLowDateTime != 0 || ft.dwHighDateTime != 0); record(name[count]==0 && klass[class_count]==0);
    }
    count=32; record(RegEnumKeyExW(key,2,name,&count,NULL,NULL,NULL,NULL));
    count=0; record(RegEnumKeyExW(key,0,name,&count,NULL,NULL,NULL,NULL));
    reset(); count=32; class_count=2; ft.dwLowDateTime=ft.dwHighDateTime=0;
    record(RegEnumKeyExW(key,0,name,&count,NULL,klass,&class_count,&ft));
    record(count); record(class_count); record(name[0]); record(klass[0]);
    record(ft.dwLowDateTime != 0 || ft.dwHighDateTime != 0);
    for(i=0;i<2;++i) {
        reset(); count=32; size=32; type=0;
        status=RegEnumValueW(key,i,name,&count,NULL,&type,data,&size);
        slot=name[0]==0 ? 0:1;
        normalized_values[slot][0]=status; normalized_values[slot][1]=count; normalized_values[slot][2]=size;
        normalized_values[slot][3]=type; normalized_values[slot][4]=(DWORD)data[0]|((DWORD)data[1]<<8)|((DWORD)data[2]<<16)|((DWORD)data[3]<<24);
        if(slot==1) record(name[0]==L'f' && name[1]==L'o' && name[2]==L'o');
    }
    for(slot=0;slot<2;++slot) for(i=0;i<5;++i) record(normalized_values[slot][i]);
    count=32; size=32; record(RegEnumValueA(key,2,(char*)name,&count,NULL,&type,data,&size));
    record(RegOpenKeyExA(key,NULL,0,KEY_QUERY_VALUE,&query_only));
    count=32; record(RegEnumKeyExW(query_only,0,name,&count,NULL,NULL,NULL,NULL));
    record(RegDeleteValueA(query_only,"foo"));
    record(RegDeleteValueW(key,L"fOo")); record(RegDeleteValueW(key,L"foo"));
    record(RegDeleteKeyA(key,"aKEY"));
    size=0; record(RegQueryValueExW(a,NULL,NULL,NULL,NULL,&size));
    record(RegCreateKeyExA(key,"aKEY",0,NULL,REG_OPTION_VOLATILE,KEY_ALL_ACCESS,NULL,&replacement,&disposition)); record(disposition);
    size=0; record(RegQueryValueExW(a,NULL,NULL,NULL,NULL,&size));
    record(RegCloseKey(a)); a=NULL;
    record(RegCloseKey(query_only)); query_only=NULL;
    record(RegDeleteKeyW(key,L"CertainlyNotAChild"));
    record(RegCloseKey(replacement)); replacement=NULL;
    record(RegCloseKey(b)); b=NULL;
    record(RegDeleteKeyA(key,"aKEY")); record(RegDeleteKeyW(key,L"bKey"));
    record(RegDeleteValueA(key,NULL));
    record(RegCloseKey(key)); key=NULL;
    record(RegDeleteKeyA(HKEY_CURRENT_USER,tree));
    file=CreateFileA("registry-groups.bin",GENERIC_WRITE,0,NULL,CREATE_ALWAYS,FILE_ATTRIBUTE_NORMAL,NULL);
    if(file==INVALID_HANDLE_VALUE || !WriteFile(file,words,used*sizeof(DWORD),&written,NULL) || written!=used*sizeof(DWORD)) ExitProcess(21);
    CloseHandle(file); ExitProcess(0);
}

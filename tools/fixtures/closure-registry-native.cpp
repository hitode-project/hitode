// Authored x86 registry views. Only newly created keys and literal values are read or removed.
#include <windows.h>
typedef LONG (WINAPI *Query)(HKEY,int,void*,DWORD,DWORD*);
typedef LONG (WINAPI *Enumerate)(HKEY,DWORD,int,void*,DWORD,DWORD*);
static void fill(void *p,UINT n,BYTE value){volatile BYTE *b=(volatile BYTE*)p;while(n--)*b++=value;}
static char *text(char *p,const char *s){while(*s)*p++=*s++;return p;}
static char *hex(char *p,DWORD value){for(int shift=28;shift>=0;shift-=4)*p++="0123456789abcdef"[(value>>shift)&15];return p;}
static char *decimal(char *p,DWORD value){char digits[10];int n=0;do{digits[n++]=char('0'+value%10);value/=10;}while(value);while(n)*p++=digits[--n];return p;}
static void report(const char *operation,int kind,DWORD size,LONG status,DWORD needed,BYTE *bytes,bool timestamp){
    char output[2048];char *p=text(output,"registry32-view operation=");p=text(p,operation);p=text(p," class=");p=decimal(p,kind);p=text(p," size=");p=decimal(p,size);p=text(p," status=");p=hex(p,status);p=text(p," needed=");p=decimal(p,needed);p=text(p," fields=");
    DWORD end=size<needed?size:needed;if(end>96)end=96;
    for(DWORD i=timestamp?(end<8?end:8):0;i<end;++i){*p++="0123456789abcdef"[bytes[i]>>4];*p++="0123456789abcdef"[bytes[i]&15];}
    *p++='\n';DWORD written;WriteFile(GetStdHandle(STD_OUTPUT_HANDLE),output,DWORD(p-output),&written,0);
}
static LSTATUS create(HKEY parent,const WCHAR *name,const WCHAR *class_name,HKEY *key){DWORD disposition=0;LSTATUS result=RegCreateKeyExW(parent,name,0,const_cast<LPWSTR>(class_name),0,KEY_ALL_ACCESS,0,key,&disposition);if(result==0&&disposition!=REG_CREATED_NEW_KEY){RegCloseKey(*key);*key=0;return ERROR_ALREADY_EXISTS;}return result;}
extern "C" void mainCRTStartup(void){
    WCHAR path[100];const WCHAR prefix[]=L"Software\\HitodeAuthored32-";int n=0;for(const WCHAR *s=prefix;*s;++s)path[n++]=*s;DWORD id=GetCurrentProcessId();for(int shift=28;shift>=0;shift-=4)path[n++]=L"0123456789abcdef"[(id>>shift)&15];path[n++]=L'-';id=GetTickCount();for(int shift=28;shift>=0;shift-=4)path[n++]=L"0123456789abcdef"[(id>>shift)&15];path[n]=0;
    HKEY parent=0,key=0,child=0;LSTATUS result=create(HKEY_CURRENT_USER,path,L"",&parent);if(result!=0)ExitProcess(result);
    result=create(parent,L"Container",L"OwnClass",&key);if(result!=0)goto cleanup;
    result=create(key,L"Child",L"",&child);if(result!=0)goto cleanup;RegCloseKey(child);child=0;
    {DWORD number=17;result=RegSetValueExW(key,L"Odd",0,REG_DWORD,(const BYTE*)&number,4);if(result!=0)goto cleanup;const WCHAR value[]=L"sample";result=RegSetValueExW(key,L"Text",0,REG_SZ,(const BYTE*)value,sizeof(value));if(result!=0)goto cleanup;}
    {HMODULE module=GetModuleHandleW(L"ntdll.dll");Query query=(Query)GetProcAddress(module,"ZwQueryKey");Enumerate enumerate=(Enumerate)GetProcAddress(module,"ZwEnumerateKey");Enumerate values=(Enumerate)GetProcAddress(module,"ZwEnumerateValueKey");if(!query||!enumerate||!values){result=ERROR_PROC_NOT_FOUND;goto cleanup;}
    __declspec(align(8)) BYTE buffer[512];const int classes[]={0,1,2,4,5,6,99};const DWORD sizes[]={0,8,12,16,20,24,28,32,36,40,44,128};
    for(UINT c=0;c<7;++c)for(UINT i=0;i<12;++i){fill(buffer,sizeof(buffer),0xcd);DWORD needed=0x55555555;LONG status=query(key,classes[c],buffer,sizes[i],&needed);report("query",classes[c],sizes[i],status,needed,buffer,classes[c]<=4);}
    for(int c=0;c<3;++c)for(UINT i=0;i<12;++i){fill(buffer,sizeof(buffer),0xcd);DWORD needed=0x55555555;LONG status=enumerate(key,0,c,buffer,sizes[i],&needed);report("key",c,sizes[i],status,needed,buffer,true);}
    for(int c=0;c<5;++c)for(UINT i=0;i<12;++i){fill(buffer,sizeof(buffer),0xcd);DWORD needed=0x55555555;LONG status=values(key,0,c,buffer,sizes[i],&needed);report("value",c,sizes[i],status,needed,buffer,false);}}
cleanup:
    if(child)RegCloseKey(child);
    if(key){RegDeleteKeyW(key,L"Child");RegCloseKey(key);RegDeleteKeyW(parent,L"Container");}
    if(parent){RegCloseKey(parent);RegDeleteKeyW(HKEY_CURRENT_USER,path);}ExitProcess(result);
}

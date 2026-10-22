// Authored metadata only: no registration, imported type libraries or external servers.
#include <windows.h>
#include <oleauto.h>
static const GUID LIBRARY_ID = {0xd4a80001,0x1111,0x2222,{0x80,0,0,0,0,0,0,1}};
static const GUID RECORD_ID = {0xd4a80002,0x1111,0x2222,{0x80,0,0,0,0,0,0,2}};
static const GUID ENUM_ID = {0xd4a80003,0x1111,0x2222,{0x80,0,0,0,0,0,0,3}};
static const GUID ALIAS_ID = {0xd4a80004,0x1111,0x2222,{0x80,0,0,0,0,0,0,4}};
static const GUID MODULE_ID = {0xd4a80005,0x1111,0x2222,{0x80,0,0,0,0,0,0,5}};
static void zero(void *pointer, UINT size) {volatile BYTE *bytes=(volatile BYTE*)pointer;while(size--)*bytes++=0;}
static HRESULT record(ICreateTypeLib2 *library) {
    ICreateTypeInfo *type=0;
    HRESULT result=library->CreateTypeInfo(L"AuthoredRecord",TKIND_RECORD,&type);
    if(FAILED(result))return result;
    result=type->SetGuid(RECORD_ID);
    if(SUCCEEDED(result))result=type->SetAlignment(4);
    const OLECHAR *names[]={L"Count",L"Amount",L"Label"};
    const VARTYPE kinds[]={VT_I4,VT_R8,VT_BSTR};
    const ULONG offsets[]={0,4,12};
    for(UINT index=0;index<3&&SUCCEEDED(result);++index){
        VARDESC variable;zero(&variable,sizeof(variable));variable.memid=100+index;variable.oInst=offsets[index];variable.elemdescVar.tdesc.vt=kinds[index];variable.varkind=VAR_PERINSTANCE;
        result=type->AddVarDesc(index,&variable);
        if(SUCCEEDED(result))result=type->SetVarName(index,const_cast<LPOLESTR>(names[index]));
    }
    if(SUCCEEDED(result))result=type->LayOut();
    type->Release();return result;
}
static HRESULT choice(ICreateTypeLib2 *library) {
    ICreateTypeInfo *type=0;HRESULT result=library->CreateTypeInfo(L"AuthoredChoice",TKIND_ENUM,&type);if(FAILED(result))return result;
    result=type->SetGuid(ENUM_ID);
    const OLECHAR *names[]={L"First",L"Seven"};const LONG values[]={0,7};
    for(UINT index=0;index<2&&SUCCEEDED(result);++index){
        VARIANT value;zero(&value,sizeof(value));V_VT(&value)=VT_I4;V_I4(&value)=values[index];
        VARDESC variable;zero(&variable,sizeof(variable));variable.memid=200+index;variable.lpvarValue=&value;variable.elemdescVar.tdesc.vt=VT_I4;variable.varkind=VAR_CONST;
        result=type->AddVarDesc(index,&variable);if(SUCCEEDED(result))result=type->SetVarName(index,const_cast<LPOLESTR>(names[index]));
    }
    if(SUCCEEDED(result))result=type->LayOut();type->Release();return result;
}
static HRESULT alias(ICreateTypeLib2 *library) {
    ICreateTypeInfo *type=0;HRESULT result=library->CreateTypeInfo(L"AuthoredNumber",TKIND_ALIAS,&type);if(FAILED(result))return result;
    result=type->SetGuid(ALIAS_ID);TYPEDESC description;zero(&description,sizeof(description));description.vt=VT_R8;
    if(SUCCEEDED(result))result=type->SetTypeDescAlias(&description);
    if(SUCCEEDED(result))result=type->LayOut();type->Release();return result;
}
static HRESULT module(ICreateTypeLib2 *library) {
    ICreateTypeInfo *type=0;HRESULT result=library->CreateTypeInfo(L"AuthoredModule",TKIND_MODULE,&type);if(FAILED(result))return result;
    result=type->SetGuid(MODULE_ID);
    ELEMDESC parameters[2];zero(parameters,sizeof(parameters));parameters[0].tdesc.vt=VT_I4;parameters[1].tdesc.vt=VT_R8;parameters[0].paramdesc.wParamFlags=PARAMFLAG_FIN;parameters[1].paramdesc.wParamFlags=PARAMFLAG_FIN;
    FUNCDESC function;zero(&function,sizeof(function));function.memid=300;function.lprgelemdescParam=parameters;function.funckind=FUNC_STATIC;function.invkind=INVOKE_FUNC;function.callconv=CC_STDCALL;function.cParams=2;function.elemdescFunc.tdesc.vt=VT_I4;
    if(SUCCEEDED(result))result=type->AddFuncDesc(0,&function);
    LPOLESTR names[]={const_cast<LPOLESTR>(L"Mix"),const_cast<LPOLESTR>(L"Count"),const_cast<LPOLESTR>(L"Amount")};
    if(SUCCEEDED(result))result=type->SetFuncAndParamNames(0,names,3);
    if(SUCCEEDED(result))result=type->DefineFuncAsDllEntry(0,const_cast<LPOLESTR>(L"authored.dll"),const_cast<LPOLESTR>(L"AuthoredMix"));
    if(SUCCEEDED(result))result=type->LayOut();type->Release();return result;
}
extern "C" void mainCRTStartup(void) {
    HRESULT result=CoInitializeEx(0,COINIT_APARTMENTTHREADED);ICreateTypeLib2 *library=0;
    if(SUCCEEDED(result))result=CreateTypeLib2(SYS_WIN32,L"authored-closure.tlb",&library);
    if(SUCCEEDED(result))result=library->SetName(const_cast<LPOLESTR>(L"AuthoredLibrary"));
    if(SUCCEEDED(result))result=library->SetGuid(LIBRARY_ID);
    if(SUCCEEDED(result))result=library->SetVersion(1,2);
    if(SUCCEEDED(result))result=library->SetLcid(0x409);
    if(SUCCEEDED(result))result=record(library);
    if(SUCCEEDED(result))result=choice(library);
    if(SUCCEEDED(result))result=alias(library);
    if(SUCCEEDED(result))result=module(library);
    if(SUCCEEDED(result))result=library->SaveAllChanges();
    if(library)library->Release();CoUninitialize();ExitProcess(static_cast<UINT>(result));
}

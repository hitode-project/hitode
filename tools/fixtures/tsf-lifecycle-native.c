/*
 * Authored SDK-only x86 TSF lifecycle probe using an STA COM apartment.
 * Checks cover manager activation, document/context creation, identity, references, and retired-owner calls.
 * Inputs are SDK interfaces and synthetic argument combinations; no window, text store, or IME setting is changed.
 * Run from the repository root: ./tools/fixtures/build-tsf-lifecycle-native.ps1
 * Build requires LLVM, the Windows SDK, and MSVC libraries; -OutputDirectory selects the output directory.
 * Outputs: tsf-lifecycle-native.log and tsf-lifecycle-native.stderr.log beside the executable.
 * Each stdout row contains a label and four hexadecimal words: result, value1, value2, and caught SEH code.
 * Caught exceptions are distinct from returned HRESULTs.
 */
#define COBJMACROS
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <objbase.h>
#include <msctf.h>

static void record(const char *name, DWORD result, DWORD value1, DWORD value2, DWORD exception) {
    char line[256]; DWORD count = 0, written = 0;
    DWORD values[4] = { result, value1, value2, exception };
    while (*name && count < 200) line[count++] = *name++;
    for (DWORD index = 0; index < 4; index++) {
        line[count++] = ' ';
        for (DWORD shift = 32; shift; shift -= 4)
            line[count++] = "0123456789abcdef"[(values[index] >> (shift - 4)) & 15];
    }
    line[count++] = '\r'; line[count++] = '\n';
    WriteFile(GetStdHandle(STD_OUTPUT_HANDLE), line, count, &written, NULL);
}

static HRESULT activate(ITfThreadMgr *manager, TfClientId *tid, DWORD *exception) {
    HRESULT result = E_UNEXPECTED;
    *exception = 0;
    __try { result = ITfThreadMgr_Activate(manager, tid); }
    __except(EXCEPTION_EXECUTE_HANDLER) { *exception = GetExceptionCode(); }
    return result;
}

static HRESULT create_document(ITfThreadMgr *manager, ITfDocumentMgr **document, DWORD *exception) {
    HRESULT result = E_UNEXPECTED;
    *exception = 0;
    __try { result = ITfThreadMgr_CreateDocumentMgr(manager, document); }
    __except(EXCEPTION_EXECUTE_HANDLER) { *exception = GetExceptionCode(); }
    return result;
}

static void context_probe(const char *name, ITfDocumentMgr *document, TfClientId tid,
                          BOOL context_output, BOOL cookie_output) {
    ITfContext *context = context_output ? (ITfContext *)(ULONG_PTR)0x12345678 : NULL;
    TfEditCookie cookie = 0xa5a5a5a5;
    HRESULT result = E_UNEXPECTED;
    DWORD exception = 0;
    __try {
        result = ITfDocumentMgr_CreateContext(document, tid, 0, NULL,
            context_output ? &context : NULL, cookie_output ? &cookie : NULL);
    }
    __except(EXCEPTION_EXECUTE_HANDLER) { exception = GetExceptionCode(); }
    record(name, (DWORD)result, (DWORD)(ULONG_PTR)context, cookie, exception);
    if (context && context != (ITfContext *)(ULONG_PTR)0x12345678) {
        ITfDocumentMgr *owner = NULL;
        HRESULT owner_result = ITfContext_GetDocumentMgr(context, &owner);
        record("Context GetDocumentMgr before any Push", (DWORD)owner_result,
               (DWORD)(ULONG_PTR)owner, owner == document, 0);
        if (owner) ITfDocumentMgr_Release(owner);
        record("Release context", ITfContext_Release(context), 0, 0, 0);
    }
}

static void top_base(const char *name, ITfDocumentMgr *document, BOOL top) {
    ITfContext *context = NULL;
    HRESULT result = top ? ITfDocumentMgr_GetTop(document, &context)
                         : ITfDocumentMgr_GetBase(document, &context);
    record(name, (DWORD)result, (DWORD)(ULONG_PTR)context, 0, 0);
    if (context) ITfContext_Release(context);
}

static void deactivate(const char *name, ITfThreadMgr *manager, DWORD *active) {
    HRESULT result = ITfThreadMgr_Deactivate(manager);
    if (SUCCEEDED(result) && *active) --*active;
    record(name, (DWORD)result, *active, 0, 0);
}

static void cleanup_activation(const char *name, ITfThreadMgr *manager, DWORD *active) {
    while (*active) {
        DWORD previous = *active;
        deactivate(name, manager, active);
        if (*active == previous) break; /* Always reach Release even after native failure. */
    }
}

static void held_context_lifetime(ITfThreadMgr *manager, TfClientId tid) {
    ITfDocumentMgr *document = NULL, *owner = NULL;
    ITfContext *context = NULL;
    IUnknown *identity = NULL;
    TfEditCookie cookie = 0xa5a5a5a5;
    DWORD exception = 0;
    HRESULT result = create_document(manager, &document, &exception);
    record("Lifetime CreateDocumentMgr", (DWORD)result, (DWORD)(ULONG_PTR)document, 0, exception);
    if (!document) return;
    result = ITfDocumentMgr_CreateContext(document, tid, 0, NULL, &context, &cookie);
    record("Lifetime CreateContext", (DWORD)result, (DWORD)(ULONG_PTR)context, cookie, 0);
    record("Lifetime Release doc with external context", ITfDocumentMgr_Release(document), 0, 0, 0);
    document = NULL;
    if (!context) return;
    result = E_UNEXPECTED;
    __try { result = ITfContext_GetDocumentMgr(context, &owner); }
    __except(EXCEPTION_EXECUTE_HANDLER) { exception = GetExceptionCode(); }
    record("Held context GetDocumentMgr after doc Release", (DWORD)result,
           (DWORD)(ULONG_PTR)owner, 0, exception);
    if (owner) ITfDocumentMgr_Release(owner);
    result = ITfContext_QueryInterface(context, &IID_IUnknown, (void **)&identity);
    record("Held context QI after doc Release", (DWORD)result, (DWORD)(ULONG_PTR)identity, 0, 0);
    if (identity) IUnknown_Release(identity);
    record("Lifetime Release external context", ITfContext_Release(context), 0, 0, 0);
}

void mainCRTStartup(void) {
    ITfThreadMgr *manager_a = NULL, *manager_b = NULL;
    ITfDocumentMgr *before = NULL, *after = NULL;
    IUnknown *identity_a = NULL, *identity_b = NULL;
    TfClientId tid1 = 0xa5a5a5a5, tid2 = 0xa5a5a5a5, tid_b = 0xa5a5a5a5;
    DWORD exception = 0, active_a = 0, active_b = 0, exit_code = 0;
    HRESULT result = CoInitializeEx(NULL, COINIT_APARTMENTTHREADED);
    record("CoInitialize STA", (DWORD)result, 0, 0, 0);
    if (FAILED(result)) ExitProcess(1);
    result = CoCreateInstance(&CLSID_TF_ThreadMgr, NULL, CLSCTX_INPROC_SERVER,
                             &IID_ITfThreadMgr, (void **)&manager_a);
    record("CoCreate A", (DWORD)result, (DWORD)(ULONG_PTR)manager_a, 0, 0);
    if (FAILED(result) || !manager_a) { exit_code = 2; goto cleanup; }
    result = CoCreateInstance(&CLSID_TF_ThreadMgr, NULL, CLSCTX_INPROC_SERVER,
                             &IID_ITfThreadMgr, (void **)&manager_b);
    record("CoCreate B", (DWORD)result, (DWORD)(ULONG_PTR)manager_b, manager_a == manager_b, 0);
    if (FAILED(result) || !manager_b) { exit_code = 3; goto cleanup; }
    result = ITfThreadMgr_QueryInterface(manager_a, &IID_IUnknown, (void **)&identity_a);
    record("QI identity A", (DWORD)result, (DWORD)(ULONG_PTR)identity_a, 0, 0);
    result = ITfThreadMgr_QueryInterface(manager_b, &IID_IUnknown, (void **)&identity_b);
    record("QI identity B", (DWORD)result, (DWORD)(ULONG_PTR)identity_b,
           identity_a && identity_a == identity_b, 0);
    if (identity_a) { IUnknown_Release(identity_a); identity_a = NULL; }
    if (identity_b) { IUnknown_Release(identity_b); identity_b = NULL; }
    deactivate("Deactivate inactive", manager_a, &active_a);
    result = activate(manager_a, NULL, &exception);
    if (SUCCEEDED(result)) ++active_a;
    record("Activate NULL output", (DWORD)result, active_a, 0, exception);
    result = create_document(manager_a, NULL, &exception);
    record("CreateDocumentMgr NULL output", (DWORD)result, 0, 0, exception);
    result = create_document(manager_a, &before, &exception);
    record("CreateDocumentMgr before Activate", (DWORD)result, (DWORD)(ULONG_PTR)before, 0, exception);
    if (before) {
        top_base("GetTop immediately empty before Activate", before, TRUE);
        top_base("GetBase immediately empty before Activate", before, FALSE);
        context_probe("CreateContext before Activate tid0", before, 0, TRUE, TRUE);
        top_base("GetTop before Activate", before, TRUE);
        top_base("GetBase before Activate", before, FALSE);
    }
    result = activate(manager_a, &tid1, &exception);
    if (SUCCEEDED(result)) ++active_a;
    record("Activate A first", (DWORD)result, tid1, active_a, exception);
    result = activate(manager_a, &tid2, &exception);
    if (SUCCEEDED(result)) ++active_a;
    record("Activate A repeated", (DWORD)result, tid2, tid1 == tid2, exception);
    result = activate(manager_b, &tid_b, &exception);
    if (SUCCEEDED(result)) ++active_b;
    record("Activate B", (DWORD)result, tid_b, tid1 == tid_b, exception);
    result = create_document(manager_a, &after, &exception);
    record("CreateDocumentMgr after Activate", (DWORD)result, (DWORD)(ULONG_PTR)after, 0, exception);
    if (after) {
        top_base("GetTop immediately empty after Activate", after, TRUE);
        top_base("GetBase immediately empty after Activate", after, FALSE);
        context_probe("CreateContext active NULL textstore", after, tid1, TRUE, TRUE);
        top_base("GetTop after CreateContext", after, TRUE);
        top_base("GetBase after CreateContext", after, FALSE);
        context_probe("CreateContext NULL context output", after, tid1, FALSE, TRUE);
        context_probe("CreateContext NULL cookie output", after, tid1, TRUE, FALSE);
    }
    held_context_lifetime(manager_a, tid1);
    deactivate("Deactivate A first", manager_a, &active_a);
    deactivate("Deactivate A second", manager_a, &active_a);
    deactivate("Deactivate B", manager_b, &active_b);
    deactivate("Deactivate underflow A", manager_a, &active_a);
    if (after) context_probe("CreateContext after Deactivate", after, tid1, TRUE, TRUE);
    cleanup_activation("Cleanup Deactivate A", manager_a, &active_a);
    cleanup_activation("Cleanup Deactivate B", manager_b, &active_b);
    record("Release manager A", ITfThreadMgr_Release(manager_a), 0, 0, 0); manager_a = NULL;
    record("Release manager B", ITfThreadMgr_Release(manager_b), 0, 0, 0); manager_b = NULL;
    if (after) {
        top_base("GetTop after manager Release", after, TRUE);
        top_base("GetBase after manager Release", after, FALSE);
        context_probe("CreateContext after manager Release", after, tid1, TRUE, TRUE);
    }
cleanup:
    if (identity_a) IUnknown_Release(identity_a);
    if (identity_b) IUnknown_Release(identity_b);
    if (after) record("Release document after", ITfDocumentMgr_Release(after), 0, 0, 0);
    if (before) record("Release document before", ITfDocumentMgr_Release(before), 0, 0, 0);
    if (manager_a) {
        cleanup_activation("Cleanup Deactivate A", manager_a, &active_a);
        ITfThreadMgr_Release(manager_a);
    }
    if (manager_b) {
        cleanup_activation("Cleanup Deactivate B", manager_b, &active_b);
        ITfThreadMgr_Release(manager_b);
    }
    CoUninitialize();
    record("CoUninitialize complete", 0, 0, 0, 0);
    ExitProcess(exit_code);
}

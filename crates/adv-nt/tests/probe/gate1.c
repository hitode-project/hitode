/* The gate-1 probe: a loader TLS callback, TlsAlloc, LoadLibrary, waits
 * that time out and that are satisfied, and a fault delivered as an
 * exception to a vectored handler, which skips the faulting store, and a
 * call that needs more than 1 MiB of the 2 MiB stack the image asks for. It
 * exits with 0 only when every step did what Windows does.
 * Build: i686-w64-mingw32-gcc -O1 -o gate1.exe gate1.c */
#include <windows.h>
#include <stdio.h>

static volatile LONG tls_callbacks, faults;

static void NTAPI tls_callback(PVOID module, DWORD reason, PVOID reserved)
{
    InterlockedIncrement(&tls_callbacks);
}

__attribute__((section(".CRT$XLB"), used)) static PIMAGE_TLS_CALLBACK tls_cb = tls_callback;

static LONG CALLBACK handler(EXCEPTION_POINTERS *info)
{
    if (info->ExceptionRecord->ExceptionCode != EXCEPTION_ACCESS_VIOLATION)
        return EXCEPTION_CONTINUE_SEARCH;
    InterlockedIncrement(&faults);
    info->ContextRecord->Eip += 10; /* movl $1, 0 */
    return EXCEPTION_CONTINUE_EXECUTION;
}

static __attribute__((noinline)) int deep(void)
{
    volatile char buffer[0x180000];
    buffer[0] = 1;
    return buffer[0];
}

int main(void)
{
    DWORD slot = TlsAlloc();
    HMODULE module;
    FARPROC proc;
    HANDLE event;
    DWORD timed_out, signaled;
    int deep_calls;

    TlsSetValue(slot, (void *)0x1234);
    module = LoadLibraryA("version.dll");
    proc = module ? GetProcAddress(module, "GetFileVersionInfoSizeA") : NULL;
    Sleep(10);
    event = CreateEventA(NULL, TRUE, FALSE, NULL);
    timed_out = WaitForSingleObject(event, 20);
    SetEvent(event);
    signaled = WaitForSingleObject(event, INFINITE);
    CloseHandle(event);
    AddVectoredExceptionHandler(1, handler);
    __asm__ volatile(".byte 0xc7,0x05,0,0,0,0,1,0,0,0");
    deep_calls = deep();
    printf("tls=%p callbacks=%ld module=%p proc=%p waits=%lu,%lu faults=%ld deep=%d\n",
           TlsGetValue(slot), tls_callbacks, module, proc, timed_out, signaled, faults, deep_calls);
    return TlsGetValue(slot) == (void *)0x1234 && tls_callbacks > 0 && proc
                   && timed_out == WAIT_TIMEOUT && signaled == WAIT_OBJECT_0 && faults == 1
                   && deep_calls == 1
               ? 0
               : 1;
}

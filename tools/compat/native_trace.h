/* Shared x86 collector/launcher protocol. All paths are provided by the caller. */
#ifndef ADV_NATIVE_TRACE_H
#define ADV_NATIVE_TRACE_H
#include <windows.h>
#define CONFIG_MAGIC 0x41505431
typedef struct {
    DWORD magic, max_calls, seconds, main_tid;
    WCHAR output[1024];
    char include[2048], exclude[2048];
} TraceConfig;
#endif

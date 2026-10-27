# Win32 exceptions

ID prefix: EXC

This spec covers structured exception dispatch in the Win32 personality: SEH search, saved flags, DllMain failures and the unhandled-exception path. `adv-win32` implements it, and native fixtures in `tools/fixtures/` compare it with Windows. [README.md](README.md) explains the format.

## Structured exceptions

### EXC-SEH-2 Saved flags

**Rule**: Fault contexts MUST set RF; breakpoint traps MUST NOT add RF. Dispatch MUST clear active DF while preserving saved DF, and restoration MUST honor the modified context.
**Verification**: Test

### EXC-SEH-3 RF consumption

**Rule**: Restored RF MUST be consumed when the next decoded instruction starts. Budget exhaustion before instruction start MUST preserve RF, and RF alone MUST NOT generate a single-step exception.
**Verification**: Review — no personality test covers RF consumption together with an exhausted execution budget.

### EXC-SEH-4 DllMain exception failure

**Rule**: Dynamic DllMain exceptions MUST search VEH and inner SEH before unwinding through live `RtlUnwind` to the loader continuation. Failure MUST roll back dependencies and release the loader lock. Win32 MUST return NULL and 1114, while NT MUST return `STATUS_DLL_INIT_FAILED`.
**Verification**: Test

### EXC-SEH-5 DllMain detach and retry

**Rule**: An escaping DllMain exception MUST NOT send PROCESS_DETACH to that DLL; an explicit FALSE return MUST send it. Any nonzero BOOL MUST mean success, and reloading after failure MUST initialize again.
**Verification**: Test

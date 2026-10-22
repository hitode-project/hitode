# Win32 exceptions

ID prefix: EXC

This spec covers structured exception dispatch in the Win32 personality: SEH search, saved flags, DllMain failures and the unhandled-exception path. `adv-win32` implements it, and native fixtures in `tools/fixtures/` compare it with Windows. [README.md](README.md) explains the format.

## Structured exceptions

### EXC-SEH-1 SEH search bounds

**Rule**: SEH MUST follow `Next` links, including descending addresses. Each search MUST sample TEB stack bounds after VEH and before its first stack handler. Invalid frames MUST set `EXCEPTION_STACK_INVALID` and enter second-chance dispatch.
**Verification**: Review — the native exception fixture exercises these cases, but no annotated test checks per-search sampling and nested searches.

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

### EXC-SEH-6 RTL startup filter

**Rule**: The RTL unhandled filter MUST have state independent of the Win32 filter and preserve LastError on registration. Valid SEH exhaustion MUST invoke it and honor its context changes. Invalid chains and explicit second chances MUST NOT invoke it.
**Verification**: Test

### EXC-SEH-7 Second chance

**Rule**: First-chance exhaustion MUST enter live `NtRaiseException(FALSE)`. Without a debugger, second chance MUST terminate with the exception status without repeating VEH, SEH, or top-level filters.
**Verification**: Test

## CRT handlers

### EXC-CRT-1 Handler ownership

**Rule**: Terminate, unexpected and SE translator handlers MUST use runtime/thread state, while purecall handlers MUST use runtime-global state. Setters MUST return the previous handler. Aliases MUST share state and preserve errno and LastError.
**Verification**: Test

### EXC-CRT-2 Termination paths

**Rule**: Returning or recursively entered terminate handlers MUST end through abort. A returning unexpected handler MUST enter terminate. VC14 `__std_terminate` MUST use UCRT's terminate handler, and a handler's explicit process exit MUST retain its code.
**Verification**: Test

### EXC-CRT-3 CRT SEH scopes

**Rule**: CRT EH2/EH3/EH4 filters MUST run with the establisher's frame and exception pointers. Accepted scopes MUST run live global unwinding, then inner finally blocks in order, before entering their handler. Local unwind helpers MUST publish outer scope state before cleanup and stop at the requested state.
**Verification**: Test

### EXC-CRT-4 EH4 security callbacks

**Rule**: EH4 MUST decode the scope table with the supplied cookie. Enabled GS/EH checks MUST call the supplied security callback with the decoded value in ECX before searching scopes. The callback's terminating failure MUST prevent filter execution.
**Verification**: Test

## C++ exception metadata

### EXC-CPP-1 Catch search

**Rule**: MSVC x86 frame handlers MUST search guest FuncInfo try blocks and match ThrowInfo catchable types. Value, reference, base-class and ellipsis catches MUST receive the matching guest object. Rethrow MUST retain the active exception and search enclosing scopes.
**Verification**: Test

### EXC-CPP-2 Cleanup ordering

**Rule**: A selected C++ catch MUST call live global unwinding before local cleanup and catch execution. Cleanup MUST publish its outer state before calling guest destructors. Cross-frame throws MUST run inner-frame cleanup before the selected frame's catch.
**Verification**: Test

### EXC-CPP-3 Exception lifetime

**Rule**: A completed catch MUST destroy its exception object exactly once after the final active owner releases it. Rethrow MUST preserve the object through enclosing catches. Catch continuation MUST retain checked machine ancestry.
**Verification**: Test

### EXC-CPP-4 Uncaught throws

**Rule**: An uncaught CRT-generated C++ exception MUST enter its runtime's terminate handler. A C++ throw from a terminate handler MUST abort. Guest callbacks that explicitly exit MUST preserve their exit code.
**Verification**: Test

### EXC-CPP-5 SE translator

**Rule**: Asynchronous C++ frame handlers MUST invoke their thread's SE translator for Windows exceptions with the original code and exception pointers. A translated C++ throw MUST enter ordinary C++ catch search. Returning translators MUST continue Windows handler search.
**Verification**: Test

### EXC-CPP-6 Processing throw count

**Rule**: Uncaught queries MUST count active C++ local-unwind scopes in the current runtime/thread. Raw Windows delivery MUST NOT itself create C++ processing counts. Completed local cleanup MUST release its count before the catch body executes.
**Verification**: Test

### EXC-CPP-7 Nonlocal jumps

**Rule**: Longjmp MUST unwind intervening registrations through live RtlUnwind before local cleanup. Compiler C++ callbacks MUST interpret the saved FuncInfo and target state. Jump restoration MUST validate continuation ancestry and return one when the requested value is zero.
**Verification**: Test

### EXC-CPP-8 Compiler entry helpers

**Rule**: The x86 EH prolog MUST install the EAX-supplied handler and initial state minus one, preserve caller EBP, and resume with checked machine ancestry. The exception-registration buffer query MUST return sixteen bytes. The NLG return boundary MUST preserve incoming registers, flags and the caller's stack.
**Verification**: Test

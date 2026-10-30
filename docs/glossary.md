# Glossary

These terms have one meaning across the docs, specs, code comments and reports. The Chinese column gives the term used when talking with users in Chinese. [ARCHITECTURE.md](../ARCHITECTURE.md) shows where each part lives.

## Roles and layers

| Term | 中文 | Meaning |
|---|---|---|
| guest | guest（客体） | The original Windows program: the game EXE, its DLLs and the code they load. Not "client" or "emulated program". |
| host | 宿主 | The machine and operating system that run Hitode, and the native code that runs on them. |
| personality | 人格层 | The Windows identity the guest sees: exports, version facts and API behavior. The profile is Windows 7 SP1. |
| HLE | HLE | A Win32 API service that the host implements, covering the personality and the API front ends. |
| API front end / shell | API 前端／宿主壳 | Respectively: the entry point of an API such as DSound, and the platform window with its input and output. Do not write "front end" alone for any of them. |
| overlay input region | 覆盖层输入区域 | A rectangle the launcher overlay claims for input. A press inside a region, or any press while the overlay is modal, goes to the webview. |

## Execution

| Term | 中文 | Meaning |
|---|---|---|
| Tier 0 | Tier 0 | The predecoding interpreter. It runs cold code and instructions the translator does not support, and it is the reference for differential runs. |
| predecode window | 预解码窗口 | A bounded run of instructions with a boundary index that Tier 0 caches to reuse local branches, calls and returns. |
| a64 | a64 | The translation engine for every AArch64 host. It keeps guest registers in fixed host registers, accesses memory directly through a guarded window and translates in one pass without an IR. |
| region / entry / link | 区域／入口／链接 | A region is a bounded unit of guest code. An entry is where the dispatcher enters a region. A link is a direct transfer between regions that does not return to the Rust dispatch loop. |
| execution form | 执行形式 | The path that executes an instruction: translated unit code, an operation helper, an interpret call, or interpreter execution including first visits. Interpreted REP elements are counted separately; native ones count as translated work. |
| execution contract | 执行契约 | The state, memory access, exit and recovery rules that Tier 0 and the AArch64 translator share. `CpuState` is the authoritative state. |
| code cache | 代码缓存 | Translated code retained in one process. |
| soft page | 软页 | A host page protected more strictly than the 4 KiB permission table says, for mixed permissions, translated code, shared audio memory or first file access. Accesses fault and are emulated, and hot access sites are translated again as checked accesses. |
| biased ESP / EBP | 偏置 ESP／EBP | In a64, x23 and x24 hold the window base plus ESP and EBP. The window is 4 GiB aligned, so the low 32 bits are the guest value. |
| carry sense | 进位感 | How a64 maps the ARM C flag to x86 CF: add sense means C equals CF, subtract sense means C is the inverse of CF. Block boundaries use subtract sense. |
| direct body | 直接 body | A native body that generated code and the interpreter call as an AAPCS64 leaf function, without the `call` stub or guest state synchronization; see A64-CALL-2. |
| direct host call | 直接宿主调用 | A recognized API thunk that an a64 unit calls through the `call` stub, running the ordinary handler with full guest state and continuing in the unit when it returns; see A64-CTL-12. |
| shadow return stack | 影子返回栈 | The a64 stack of `{host continuation, guest return EIP}` pairs for each guest thread or fiber. A guest call uses BL, and a checked return uses RET so the hardware predicts it. |
| TXM | TXM | The iOS mechanism, from iOS 26 on A15 and later, in which hardware supervises executable pages. A third-party app can execute only memory that a debugger has allowed. |

## Testing and measurement

| Term | 中文 | Meaning |
|---|---|---|
| differential run (verify) | 差分（verify） | Comparing the observable results of two implementations on the same input, stating what is covered and skipped. CPU verify usually uses Tier 0 as the reference. |
| strict | strict | API strict stops at an unimplemented diagnostic stub. `ADV_JIT_VERIFY=strict` is a separate CPU check policy. Always write the full name. |
| smoke | 冒烟 | A short start of one game per engine. It passes when frames are produced and no new structural gap appears; it does not show that the title is playable. |
| full run | 全量 | The whole workspace test suite or the full sample matrix. It runs when batching changes or updating the compatibility baseline, not for each change. |
| headed / headless | 有头／无头 | A headed run shows a window and is judged by the user. A headless run has no window; agents use short headless A/B runs to check mechanisms, and headless throughput says nothing about the headed experience. |
| cold start / warm start | 冷启动／热启动 | A cold start has no warm profile and no code cache. A warm start reuses one of them and states which. Neither implies clearing the operating system file cache. |
| feature / comparison / diagnostic switch | 功能／对照／诊断开关 | A feature switch changes what the program can do. A comparison switch selects an old or new implementation to verify a migration. A diagnostic switch only observes or checks. |

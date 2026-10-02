MinHook v1.3.4, upstream https://github.com/TsudaKageyu/minhook

Fixed commit: `c3fcafdc10146beb5919319d0683e44e3c30d537`.
Source files were fetched verbatim from raw.githubusercontent.com at that commit.
Original byte hashes are recorded in SHA256SUMS; `.gitattributes` preserves those
bytes across checkouts. The fixture build verifies these hashes before compiling.
Only x86 build inputs are vendored; no upstream code is modified.
MinHook and its HDE disassembler are BSD-2-Clause; see LICENSE.txt and source notices.
The repository-owned `../../patch_headers` provides x86 ABI declarations to build
without an installed Visual C++ CRT. `../../patch_crt.c` supplies memcpy/memset.
This dependency is fixture-only and is never linked into the runtime.

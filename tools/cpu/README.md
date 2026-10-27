# Hardware integer vectors

`import_sst386.py` pins [SingleStepTests/80386](https://github.com/SingleStepTests/80386)
at commit `459d49fbe6280e9ed46fee887b58dacd9cb880ab`. Source data are public domain;
the source license is fetched alongside them. The tool uses the published
[MOO format](https://github.com/dbalsom/moo/blob/c438962d2b30856817d8e59bd5f0f4c628596ca8/doc/moo_format_v1.md),
checks downloaded Git blob identities, retains sparse register deltas and
common/per-case undefined masks, and applies the pinned upstream revocation
list. It does not execute downloaded code. Truncated data, unknown semantic
chunks and count mismatches fail conversion. Generator metadata and bus cycles
are not execution inputs.

No corpus, compiled probe or converted JSON belongs in Git. Everything defaults
to the ignored `target/cpu-reference` directory. `serde_json` is a CPU **dev**
dependency used by the example runner, not by the production executor.

## Reproduce

Run from the repository root, with Python 3 and the Rust toolchain:

```powershell
python tools/cpu/import_sst386.py
cargo run -p adv-cpu --release --example sst386 -- target/cpu-reference/sst386/vectors.jsonl 97
python tools/cpu/import_sst386.py --suite extended --out target/cpu-reference/sst386-extra
cargo run -p adv-cpu --release --example sst386 -- target/cpu-reference/sst386-extra/vectors.jsonl 97
python -m unittest discover -s tools/cpu -p 'test_*.py'
cargo test -p adv-cpu --release --example sst386 --test word_moves
& tools/cpu/test-mov16-native.ps1
```

The optional integer `97` also exercises every 97th selected vector through a
compiled region. The report distinguishes wholly native cases from cases that
use the interpreter fallback. Omitting it executes every selected vector only
through the production interpreter. This is correctness validation, not a speed
benchmark. Extra opcode files can be explicitly selected with `--files`.

The native probe needs the repository's existing LLVM and Windows SDK x86 setup.
It starts one owned hidden WoW64 process with a five-second parent deadline.
There is no game, graphics, audio or DAC execution.

## What the adapter establishes

The pinned upstream corpus is **16-bit real mode**, whereas adv-cpu implements a
32-bit flat process. The runner admits only a stated register integer/flag
instruction set with no memory/stack/control-transfer effects. iced-x86 decodes
the original instruction in 16-bit mode, re-encodes the same instruction and
operands for 32-bit mode, then decodes again and checks code/operand identity.
This exercises 8-, 16- and 32-bit arithmetic while preserving full input GPRs.
An irrelevant segment prefix may be removed only for this memory-free subset.

Instruction bytes must agree with the source RAM. The source next IP must
include exactly one instruction plus its HLT sentinel. Execution stops before
an added HLT: the translated next PC is checked separately. All eight final
GPRs are compared, including registers omitted from the sparse final delta;
the CPU's full code page must remain unchanged. Architectural arithmetic and
direction flags are compared using source masks plus iced undefined-flag
metadata. Actual shift counts refine the OF/CF mask; count zero checks preserved
flags. Only the architecturally undefined BSF/BSR destination bits are masked
when its source is zero.

Memory operands, real-mode exceptions, segment behavior, control flow, bus
cycles and instructions outside the admitted set are **excluded**, not passed.
This is meaningful independent hardware coverage; it is not the full integer
acceptance required by architecture step 4, nor proof of real-mode execution.

## 2026-10-04 results and correction

| Dataset | Input vectors | Selected / passed | Excluded | JIT sampled native / fallback |
|---|---:|---:|---:|---:|
| Core | 85,500 | 85,500 | 0 | 372 / 510 |
| Extended | 215,000 | 51,230 | 163,770 | 111 / 418 |

Extended exclusions: 157,847 memory/non-GPR forms, 5,922 real-mode exceptions,
and **one explicitly identified 386EX division quirk**. Total selected hardware
vectors: **136,730**, with zero unresolved failures in this admitted subset.
Four converter tests, four adapter tests, three precise word-move/divide tests,
25 existing interpreter tests and four existing scalar tests also passed.
Clippy for the new runner/test passed with warnings denied.

The first extended run exposed 1,215 rejected word-to-word `MOVZX/MOVSX` cases.
The interpreter had rejected equal source/destination widths. Current WoW64
raw encodings `66 0F B7/BF` were independently checked in 256 register/memory
combinations: they copy the low word, preserve the destination's high word and
leave flags unchanged. The interpreter now admits them through its existing
transactional move operation. Cross-page faults preserve all pre-instruction
state before retry. These forms may still use the interpreter from a JIT region;
the test does not count that as native instruction coverage.

The remaining initial failure was `F6.7 #516`, hash
`c6d731127bec724d40594edc28a6ad8fdf14acbc`, documented in upstream
[issue 3](https://github.com/SingleStepTests/80386/issues/3).
Its `AX=-25487 / CL=71` yields a quotient below -128. The 386EX record succeeds
with a different result; current WoW64 raises `0xc0000095` for integer overflow.
The adapter excludes this exact hash **and verifies its input/output identity**.
A separate CPU test requires precise #DE with no state commit. No production
division semantics were weakened, and this exclusion is reported in the totals.
The WoW64 status-code observation does not establish the runtime's full Windows
SEH status translation for all divide faults.

Converted JSONL SHA-256:

- Core: `4fe17a787899560d2790f1b4e57f5e9858ae9536a2f2a322ca3ca75ee409f943`
- Extended: `998cc4398231ea6c90f0685f07456c7b42f22aa4af54ce02c7f9588768f558a8`

The matching manifests and result reports are in each local dataset directory.
Further work is the address/segment adapter and exception corpus coverage; a
protected-mode reference suite generated by another emulator is supplementary,
not a replacement for the independently generated hardware vectors.

# Execution acceptance

`acceptance.py` judges captured `adv-cpu` test logs: totals, crashed test
processes (a crashed binary prints no `test result` line, so pass counts alone
can look clean), `tier verification:` lines and failure names. With
`--strict`, names in `strict_baseline.txt` are expected: strict verification
replays native entries through the interpreter, so tests asserting native
shape, link reuse or interrupt timing fail by design. Anything else is new.
It accepts files or directories, including `run-cpu-tests.ps1` output, where
a device exit status of 128 or more counts as a crash.

`arm64-check.sh` runs the CPU suites once on an AArch64 Linux or macOS host and
judges the log plainly and with `--strict`. It detaches the checkout, so use a
clean clone. Logs stay in ignored `target/arm64-acceptance`.

```sh
tools/cpu/arm64-check.sh feat/some-branch
python3 tools/cpu/acceptance.py --strict target/arm64-acceptance/<sha>/tests.log
python3 -m unittest discover -s tools/cpu -p 'test_acceptance.py'
```

Update the baseline only with names that fail by design, in the same commit
that explains why.

# AArch64 encoder vectors

`a64_vectors.py` keeps `crates/adv-cpu-a64/testdata/encode.tsv`: one
`<word>\t<assembly>` line per host instruction, encoded by NDK clang
(`--target=aarch64-linux-android`). The encoder test looks every expected word
up by its assembly text and fails on vectors no encoder case covers, so no word
is typed by hand. These are host encodings, not guest code.

```powershell
python tools/cpu/a64_vectors.py --add "ldr x0, [x1]"   # append, then re-encode all
python tools/cpu/a64_vectors.py --check                # stale words fail
python -m unittest discover -s tools/cpu -p 'test_a64*.py'
cargo test -p adv-cpu-a64 --release
```

The crate's tests also run translated code under Unicorn arm64, so encoder and
translator work needs no device.

# Running the CPU tests on AArch64

The facade exposes the a64 engine, and its integration tests run on every
supported host. Run the full suite on a device with
`tools/android/run-cpu-tests.ps1 -AllTests -NoFailFast` and judge each log with
`acceptance.py --strict` against the usual baseline.

# Writing specs

Each file in `docs/spec/` states the conditions one subsystem must meet. A spec holds only rules that a test, a check or a review can refute. Rationale lives in the `//!` docs of the implementing modules, and trade-offs, process, evidence and plans in commit messages, pull requests and issues. `tools/spec/check_spec.py` checks this directory and runs from `tools/check.py`.

## Entries

Each rule is a level-three heading followed by fields:

```markdown
### A64-MEM-5 Re-execute after a fault

**Rule**: One to three sentences, each stating one requirement.
**Applies to**: AArch64 (omit when the rule applies everywhere)
**Verification**: Test
**Basis**: A64-MEM-1 or an external reference (optional)
```

A table or a list may follow the fields as part of the rule. Three kinds of notes have fixed meanings:

> Warning: breaking this leaks private data, breaks the law or loses user data.

> Caution: breaking this breaks a rule in this or another spec.

> Note: background, not a rule.

## IDs

- An ID is `PREFIX-GROUP-N`, for example `A64-MEM-5`. Each file declares every prefix it uses on a separate `ID prefix:` line. Each prefix belongs to one file. Groups use uppercase letters and digits.
- IDs are never reused. To withdraw a rule, delete its text and move its ID to the `Withdrawn:` line at the end of the file.
- Code and tests cite rules by ID, not by section. A test cites the rules it can refute with a `// spec: ID` line before the function; one line may list several IDs.

## Verification

| Method | Meaning | What the checker requires |
|---|---|---|
| Test | An automated test can refute the rule | At least one `spec:` marker cites it |
| Check | A compile-time assertion, `tools/check.py` or the build configuration enforces it | `Check — ` followed by what enforces it |
| Review | Only a person can verify it | `Review — ` followed by why no test can |

## Scenarios

A scenario pins observable behavior of a whole build. It is a directory `tests/scenarios/NAME/` with guest C sources and a `scenario.json`:

| Field | Meaning |
|---|---|
| `spec` | The rules it pins. Its claims cover only these IDs. |
| `exe`, `args`, `seconds` | The guest program `adv-run` starts, extra `adv-run` options and the time limit. |
| `outputs` | Files the guest writes. Their bytes are compared between builds. |
| `frame` | When true, the last presented frame's hash is compared too. |
| `steady` | `iterations` (two counts written to `scenario-input.txt`), report `counters` whose growth per iteration is compared, and optional `min` per-iteration floors. On AArch64, `retired` counts all retired guest instructions, `owner_retired` counts owner work including its share of split jobs, `worker_retired` counts worker work, `split_retired` counts guest instructions retired in split jobs, and `whole_worker_jobs` counts completed whole worker jobs. |
| `steady.kernel_work` | Optional emitted host-word estimate for fixture kernels with fixed trip counts. It combines JIT map word counts and resident entry counts outside execution. Use `--sync-compilation` to make the form selection independent of background compilation progress. |
| `levels` | Report counters compared by their final value, such as `peak_heap_bytes`, `live_heap_bytes` or `peak_rss`. |
| `min` | Optional minimum values that named final report counters MUST reach on the change. |
| `gpu` | When true, the scenario pins behavior of the target GPUs. CI runners lack them, so CI skips it by name; run it with `--gpu` on a target device when a change touches the GPU path. |

A `kernel_work` layout supplies `calls` per iteration, `rows` per call, and pairs of `[symbol, columns]` in `kernels`.
The executable marks the row and column heads, their fall-throughs, and the function end with `_row`, `_col`, `_col_end`, `_row_end`, and `_end` suffixes.
Optional `callees` entries give the kernel symbol, callee symbol, and calls per cell.
The final run's generated words price both endpoints of the steady interval.
The estimate prices resident arrivals from the first taken backedge, or from entry when Tier-1 arrivals show the first pass bypassed Tier-1.
Separate report counters retain helper, interpreter and native exit work.

The guest judges its own result: exit status 0 means the pinned behavior holds. `tools/scenarios/run.py gate` judges a change by a base build. In CI, the base is the latest cached master build, and CI runs that commit's copy of the judge. Every master commit since that build passed the same gate:

- Every scenario of the base MUST pass on the change, with byte-identical outputs.
- An output may differ, or a base scenario may fail, only when the change adds or edits a scenario that fails on the base, passes on the change and shares a spec ID with it. The base's `spec` list decides that ID.
- Steady counter growth per iteration and level counters MUST NOT rise beyond 5 percent of the clamped non-negative base value, and `min` floors MUST hold, whatever the change claims.
- A new or edited scenario MUST pass twice on the change with identical outputs, stable `steady` and `levels` counters, and satisfied `min` floors, and every ID it names MUST exist.
- Scenarios that check `steady`, `levels` or `min` counters run one at a time so concurrent scenarios do not perturb each other.

To change pinned behavior, edit the rule and the scenario together, so the edited scenario fails on the base. To widen what a rule's claims cover, first add the ID to scenarios in a change that keeps behavior.

## Language

- Requirement keywords follow RFC 2119 and RFC 8174: **MUST**, **MUST NOT**, **SHOULD** (exceptions need a reason) and **MAY**. Only the uppercase forms carry meaning, so the lowercase forms of must, should and shall are not used. Each Rule contains at least one keyword.
- One requirement per sentence. A sentence has at most 25 words, not counting code in backticks.
- State conditions that hold. Leave out status, dates, history and plans: the words currently, planned, not yet, no longer and TODO do not appear. Write an unsupported capability as MUST NOT or "is not supported".
- Do not use the abbreviations e.g., i.e. or etc.
- Name code by symbol or module path. Do not cite line numbers or link to source files.
- Terms follow [glossary.md](../glossary.md). Write guest, not client or emulated program.

## Length

A file has at most 1000 lines. Split a longer file by subsystem instead of compressing fields.

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

- An ID is `PREFIX-GROUP-N`, for example `A64-MEM-5`. Each file declares its prefix on an `ID prefix:` line. Groups use uppercase letters and digits.
- IDs are never reused. To withdraw a rule, delete its text and move its ID to the `Withdrawn:` line at the end of the file.
- Code and tests cite rules by ID, not by section. A test cites the rules it can refute with a `// spec: ID` line before the function; one line may list several IDs.

## Verification

| Method | Meaning | What the checker requires |
|---|---|---|
| Test | An automated test can refute the rule | At least one `spec:` marker cites it |
| Check | A compile-time assertion, `tools/check.py` or the build configuration enforces it | `Check — ` followed by what enforces it |
| Review | Only a person can verify it | `Review — ` followed by why no test can |

## Language

- Requirement keywords follow RFC 2119 and RFC 8174: **MUST**, **MUST NOT**, **SHOULD** (exceptions need a reason) and **MAY**. Only the uppercase forms carry meaning, so the lowercase forms of must, should and shall are not used. Each Rule contains at least one keyword.
- One requirement per sentence. A sentence has at most 25 words, not counting code in backticks.
- State conditions that hold. Leave out status, dates, history and plans: the words currently, planned, not yet, no longer and TODO do not appear. Write an unsupported capability as MUST NOT or "is not supported".
- Do not use the abbreviations e.g., i.e. or etc.
- Name code by symbol or module path. Do not cite line numbers or link to source files.
- Terms follow [glossary.md](../glossary.md). Write guest, not client or emulated program.

## Length

A file has at most 400 lines. Split a longer file by subsystem instead of compressing fields.

# Contributing

Thank you for helping. If anything here is unclear, open an issue and ask.

## Getting started

You need Rust stable, Python 3, and CMake for the differential tests. [docs/building.md](docs/building.md) covers video, the launcher and each platform.

```sh
python3 tools/check.py --install-hook   # checks formatting, privacy, layering and specs before each commit
python3 tools/check.py --fix            # applies formatting and regenerates generated tables
python3 tools/check.py --ci             # the checks plus CI's Clippy gates; run before pushing
cargo test -p adv-cpu                   # test the crates you changed
cargo run -p adv-run --release -- --audio path/to/game.exe   # the code page is detected; --codepage 932|936 overrides
```

Bring your own games and keep them anywhere outside the repository. Tools find them through the `ADV_SAMPLES` environment variable.

## Making a change

- Keep a change small and about one thing. Open an issue first for a large change, a new dependency, a new crate or a change to the layering.
- Format with `cargo fmt`. Files under `data/abi/` are generated; do not format them by hand.
- Stage files by name. When the pre-commit check stops a commit, fix what it reports (`check.py --fix` handles formatting and generated tables); never bypass it with `--no-verify`.
- Write commit messages in English as `type(scope): summary`, with a body that says why.
- Name branches `<type>/<topic>`, with type one of `feat`, `fix`, `perf`, `docs`, `tools` and `debug`.

### Several tasks at once

- Give each task its own worktree and branch, and each worktree its own Cargo target directory.
- Leave uncommitted changes you did not make alone: no `stash`, `checkout` or `reset` over them.
- Do not run `cargo clean` or remove folders under `target/` that someone else made.
- After merging, delete your branch, worktree and stashes.

## Verifying a change

- Run `python3 tools/check.py --ci` before pushing code; it is the lint CI requires, and passing it locally saves a round trip.
- Test the paths your change affects. Reuse a passing result when neither the code nor the conditions changed.
- The full workspace suite (`cargo test-full`) and the full sample matrix are for batched changes and baseline updates.
- Headless checks are yours to run. The felt experience in a window, such as stutter, audio and input response, is confirmed by a person.
- Report compatibility by stage, separately for each engine and entry path: S1 starts and logs its first gap, S2 the title screen responds, S3 sound and basic play work, S4 a chapter finishes. A smoke run that draws frames proves only S1.
- Widen an API's implementation only on evidence of a real call or an observable difference. A static import, a DLL name or an optional probe is not evidence.
- Back a speed claim with a short A/B on a fixed binary, configuration, code page and input, and state those conditions. Headless throughput is not the felt experience.
- Say in the pull request how you verified the change and what still needs a person.

## Switches

A merged change is on by default; there is no off-by-default feature, experiment or option. Comparison, diagnostic and fixture switches are the exceptions, as [docs/configuration.md](docs/configuration.md) describes. Turning a change on by default needs benefit data and no growth in the known-failure list. If a change should not be on, remove it, or say what evidence is missing.

## Documentation

- Do not add documents, reports or plans. Measurements belong in commit messages, pull requests and your local `target/` folder.
- Docs describe what is and why. When code changes something a doc describes, change that doc in the same commit, and delete what is out of date.
- A commit that changes a rule updates its entry in [docs/spec/](docs/spec/README.md); tests cite the rules they check with `// spec: ID`.
- New terms go in [docs/glossary.md](docs/glossary.md), and new switches in [docs/configuration.md](docs/configuration.md).

## Using AI agents

You are welcome to work with AI agents. Code and messages sent from your account are yours, so write your own replies in issues and reviews. [AGENTS.md](AGENTS.md) is for the agents, and most agent tools read it on their own.

## Content boundaries

- Any system behavior a game needs to run normally may be implemented, including disc drives and CD audio, following the levels in [PERS-SCOPE-1](docs/spec/personality.md).
- Never commit game assets, screenshots, traces or dumps, guest code bytes, disassembly or guest instruction addresses, or a game's shader bytecode and effect files.
- Never commit or provide patches or tools that break DRM or unpack protected programs. Research that needs it stays private and local.
- Protected programs (Themida, VMProtect and similar) work only through faithful Windows and CPU behavior. Do not special-case a protector or skip or fake its integrity, anti-debugging or license checks.
- Unpacked code, memory images and protector analysis stay local. A program that needs faked hardware, a clock off real time, or more than its time box is unsupported.
- No local paths, user names or personal email addresses. `tools/privacy/scan.py` checks for them; put extra terms such as game names in the uncommitted `.privacy-terms` file.

## License

Contributions are accepted under [GPL-3.0-or-later](COPYING).

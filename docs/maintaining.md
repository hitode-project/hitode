# Maintaining

This page is for maintainers and the agents working for them. It adds to [CONTRIBUTING.md](../CONTRIBUTING.md) and does not relax it. Grants such as pushing without asking live in each maintainer's local agent configuration, never in the repository.

## Workspace

- The main checkout only integrates. Nobody develops there or leaves uncommitted changes in it.
- Task worktrees sit beside the repository in `adv-host-wt/<task>`, or in the agent tool's default worktree folder. Keep a tool's forced branch prefix, such as `codex/`.

## Integrating a branch

1. Commit in the worktree.
2. Run `git merge master` there.
3. Test the affected crates, and run `cargo check -p adv-run --features adv-shell/gpu,adv-shell/media` with the FFmpeg variables from [building.md](building.md).
4. In the main checkout, run `git merge --ff-only <branch>`.

When someone else's uncommitted files block the fast-forward, stop and say so instead of moving them. Merge shared fixes both ways as soon as they pass: finished stages of a branch into master, and fixes on master into open branches.

## Pull requests

Agents working for a maintainer open pull requests and merge their own once CI is green. Changes to rules, the layering or a public interface wait for a maintainer's review.

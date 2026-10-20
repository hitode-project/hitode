# Maintaining

This page is for maintainers and the agents working for them. It adds to [CONTRIBUTING.md](../CONTRIBUTING.md) and does not relax it. Whether an agent works for a maintainer is a local setting, never part of the repository.

## Who counts as working for a maintainer

An agent works for a maintainer when `git config --get hitode.maintainer` prints `true` in its clone. The maintainer sets this once per clone; it is local and never committed. Other grants still come from the maintainer's own words or local agent configuration.

## Workspace

- The main checkout tracks master and only fast-forwards with `git pull --ff-only`. Nobody develops there or leaves uncommitted changes in it.
- Task worktrees sit beside the repository in `adv-host-wt/<task>`, or in the agent tool's default worktree folder. Keep a tool's forced branch prefix, such as `codex/`.

## Integrating a branch

Master takes changes only through pull requests, which must pass the checks its branch protection lists.

1. Commit in the worktree and bring in the latest master with `git merge origin/master`.
2. Run `python3 tools/check.py --ci` and test the affected crates.
3. Push the branch, open a pull request with `gh pr create`, and turn on auto-merge with `gh pr merge --auto --merge`.
4. When CI fails, fix it on the same branch. After the merge, delete the branch and worktree, and fast-forward the main checkout.

Merge master into open branches as soon as fixes there pass, so long-running branches do not drift.

## Pull requests

Agents working for a maintainer open pull requests and turn on auto-merge, so their own merge once CI is green. Changes to rules, the layering or a public interface wait for a maintainer's review, unless that maintainer asked for the change.

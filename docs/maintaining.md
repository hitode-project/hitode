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

## Releasing

1. Merge a version pull request that bumps the workspace, launcher and Apple project versions. Tag its merge commit `vX.Y.Z` and push the tag.
2. [The release workflow](../.github/workflows/release.yml) builds the macOS DMG, the unsigned iOS IPA and the Android APK on GitHub's machines, embedding the Wine build that [wine.json](../tools/release/wine.json) pins. A maintainer approves the `release` environment before the APK is signed.
3. The workflow leaves a draft release with the license texts, the Wine source and `SHA256SUMS.txt`. Test its files on devices, write the Changes lines and publish it.

A tag with a suffix, such as `v0.2.3-ci.1`, runs the same workflow as a trial; delete its tag and draft afterwards. A new Wine build goes into a new `wine-dist-*` prerelease and into `wine.json`.

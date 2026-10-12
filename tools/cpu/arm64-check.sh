#!/usr/bin/env bash
# AArch64 Linux or macOS acceptance for adv-cpu without hosted CI.
#
# Usage: tools/cpu/arm64-check.sh [REF] [OUT_DIR]
#   REF      ref to test (default: HEAD); origin/REF wins when it exists.
#   OUT_DIR  where logs go (default: target/arm64-acceptance/<short sha>).
# Runs the CPU suites once on the a64 engine and judges the log with
# acceptance.py, plainly and against the strict baseline. Prints one line per
# judgement and exits non-zero if either does not accept. The checkout is detached;
# run it in a clean clone, not in a working tree with uncommitted changes.
set -u
cd "$(git rev-parse --show-toplevel)"
# The checkout below replaces these files, so run from a private copy that
# also keeps the judge and baseline of the invoking revision.
if [ -z "${ARM64_CHECK_TOOLS:-}" ]; then
  ARM64_CHECK_TOOLS=$(mktemp -d)
  cp tools/cpu/arm64-check.sh tools/cpu/acceptance.py tools/cpu/strict_baseline.txt "$ARM64_CHECK_TOOLS"/
  export ARM64_CHECK_TOOLS
  exec bash "$ARM64_CHECK_TOOLS/arm64-check.sh" "$@"
fi
REF=${1:-HEAD}
# Prefer the freshly fetched remote branch over a possibly stale local one.
git fetch -q origin || true
if git rev-parse -q --verify "origin/$REF^{commit}" >/dev/null; then
  REF="origin/$REF"
fi
git checkout -q --detach "$REF" || exit 2
OUT=${2:-target/arm64-acceptance/$(git rev-parse --short HEAD)}
mkdir -p "$OUT"
{ git rev-parse HEAD; rustc -V; uname -sm; echo "page size: $(getconf PAGESIZE)"; } > "$OUT/environment.txt"
TOOL=$ARM64_CHECK_TOOLS/acceptance.py
status=0
cargo test --release --locked -p adv-cpu -p adv-cpu-core -p adv-cpu-a64 --no-fail-fast --lib --tests > "$OUT/tests.log" 2>&1
judge() {
  name=$1; shift
  if python3 "$TOOL" "$@" "$OUT/tests.log" > "$OUT/$name.summary"; then :; else status=1; fi
  echo "$name: $(paste -sd' ' - < "$OUT/$name.summary")"
}
judge plain
judge strict --strict
echo "logs: $OUT"
exit $status

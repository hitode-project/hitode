#!/usr/bin/env bash
# ARM64 Linux acceptance for adv-cpu without hosted CI.
#
# Usage: tools/cpu/arm64-check.sh [REF] [OUT_DIR]
#   REF      ref to test (default: HEAD); origin/REF wins when it exists.
#   OUT_DIR  where logs go (default: target/arm64-acceptance/<short sha>).
# Runs plain and ADV_JIT_VERIFY=strict
# suites, then judges each log with acceptance.py. Prints one line per suite
# and exits non-zero if any suite is not accepted. The checkout is detached;
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
run() {
  name=$1 strict=$2; shift 2
  env "$@" cargo test --release --locked -p adv-cpu -p adv-cpu-core -p adv-cpu-x64 --no-fail-fast --lib --tests > "$OUT/$name.log" 2>&1
  if python3 "$TOOL" $strict "$OUT/$name.log" > "$OUT/$name.summary"; then :; else status=1; fi
  echo "$name: $(paste -sd' ' "$OUT/$name.summary")"
}
run tests ""
run strict --strict ADV_JIT_VERIFY=strict
echo "logs: $OUT"
exit $status

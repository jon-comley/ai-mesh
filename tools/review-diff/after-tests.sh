#!/usr/bin/env bash
# Run the tests; if every one passes on a clean, committed tree, review the
# commits since the last review up to that one (docs/review-diff.md).
# Usage: tools/review-diff/after-tests.sh <test command...>
#   e.g. ~/ai-mesh/tools/review-diff/after-tests.sh make test
set -uo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
if [ $# -eq 0 ]; then echo "usage: after-tests.sh <test command...>"; exit 2; fi
clean() { [ -z "$(git status --porcelain --untracked-files=no)" ]; }
start=$(git rev-parse HEAD) || exit 2
was_clean=0; clean && was_clean=1
"$@"; rc=$?
if [ $rc -ne 0 ]; then
  echo "== tests failed (exit $rc): no review"
  exit $rc
fi
if [ $was_clean = 0 ] || ! clean || [ "$(git rev-parse HEAD)" != "$start" ]; then
  echo "== tests passed, but not on a clean commit: no review (commit, then run them again)"
  exit 0
fi
PY=$(command -v python3 || command -v python)
"$PY" "$HERE/review_diff.py" run --upto "$start" || echo "== the review could not run; the tests still passed"
exit 0

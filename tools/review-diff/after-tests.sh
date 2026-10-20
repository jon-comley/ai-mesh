#!/usr/bin/env bash
# Run the tests; if every one passes on a clean, committed tree, move the
# tests-passed branch to that commit, for lenovo1's bridge to review the commits
# since the last review (docs/review-diff.md). Nothing leaves this machine.
# Usage: tools/review-diff/after-tests.sh <test command...>
#   e.g. ~/ai-mesh/tools/review-diff/after-tests.sh make test
set -uo pipefail
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
if [ "$(git branch --show-current)" = tests-passed ]; then
  echo "== tests passed, but on the tests-passed branch itself: no review"
  exit 0
fi
# lenovo1's bridge picks this up and reviews the commits since the last one
git branch -f tests-passed "$start" && echo "== tests passed: tests-passed is now ${start:0:7}, for review"
exit 0

# Run the tests; if every one passes on a clean, committed tree, move the
# tests-passed branch to that commit, for lenovo1's bridge to review the commits
# since the last review (docs/review-diff.md). Nothing leaves this machine.
# Usage: tools\review-diff\after-tests.ps1 <test command...>
#   e.g. C:\src\ai-mesh\tools\review-diff\after-tests.ps1 npm test
if ($args.Count -eq 0) { Write-Output 'usage: after-tests.ps1 <test command...>'; exit 2 }
function Clean { -not (git status --porcelain --untracked-files=no) }
$start = (git rev-parse HEAD).Trim()
if ($LASTEXITCODE -ne 0) { exit 2 }
$wasClean = Clean
$cmd, $rest = $args
& $cmd @rest
$rc = $LASTEXITCODE
if ($rc -ne 0) { Write-Output "== tests failed (exit $rc): no review"; exit $rc }
if (-not $wasClean -or -not (Clean) -or (git rev-parse HEAD).Trim() -ne $start) {
  Write-Output '== tests passed, but not on a clean commit: no review (commit, then run them again)'
  exit 0
}
if ((git branch --show-current).Trim() -eq 'tests-passed') {
  Write-Output '== tests passed, but on the tests-passed branch itself: no review'
  exit 0
}
# lenovo1's bridge picks this up and reviews the commits since the last one
git branch -f tests-passed $start
if ($LASTEXITCODE -eq 0) { Write-Output "== tests passed: tests-passed is now $($start.Substring(0,7)), for review" }
exit 0

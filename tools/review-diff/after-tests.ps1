# Run the tests; if every one passes on a clean, committed tree, review the
# commits since the last review up to that one (docs/review-diff.md).
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
$py = if (Get-Command py -ErrorAction SilentlyContinue) { 'py' } else { 'python' }
& $py (Join-Path $PSScriptRoot 'review_diff.py') run --upto $start
if ($LASTEXITCODE -ne 0) { Write-Output '== the review could not run; the tests still passed' }
exit 0

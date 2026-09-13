# Run a REAPER bench across the models compared in docs/model-selection.md.
#
#   .\run-reaper-bench.ps1 -Mode tools    # native tool calling (reaper-bench-tools.ps1)
#   .\run-reaper-bench.ps1 -Mode prompt   # the format ai-mesh uses today (reaper-bench.ps1)
#
# A file, not a piped-in script: `powershell -Command -` reads stdin line by
# line, so a multi-line loop fed over ssh silently runs nothing.
param([ValidateSet('tools','prompt')][string]$Mode = 'tools')

Set-Location $PSScriptRoot
$bench = $(if ($Mode -eq 'tools') { '.\reaper-bench-tools.ps1' } else { '.\reaper-bench.ps1' })

$runs = @(
  @{ f='qwen2.5-7b-instruct-q4_k_m-00001-of-00002.gguf'; nt=$false },
  @{ f='Qwen3-8B-Q4_K_M.gguf'; nt=$true },
  @{ f='Llama-xLAM-2-8B-fc-r-Q4_K_M.gguf'; nt=$false },
  @{ f='watt-tool-8B-Q4_K_M.gguf'; nt=$false },
  @{ f='Hammer2.1-7b-Q4_K_M.gguf'; nt=$false },
  @{ f='qwen2.5-14b-instruct-q4_k_m-00001-of-00003.gguf'; nt=$false }
)

foreach ($r in $runs) {
  if ($r.nt) { & $bench -ModelFile $r.f -NoThink } else { & $bench -ModelFile $r.f }
}
Write-Host "ALL-DONE ($Mode)"

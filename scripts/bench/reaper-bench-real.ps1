# The intent bench on what ai-mesh REALLY sends: the system prompts and the
# full tool set exported from coordinator/src/intent.rs, device and sensor
# lines in build_device_context / build_sensor_context's exact format, and
# cases that cover more than REAPER + lights.
#
# Export the inputs on a dev box, copy them next to this script, then run on
# beelink1:
#   AI_MESH_BENCH_OUT=intent-bench.json cargo test -p coordinator dump_intent_bench_inputs -- --ignored
#   .\reaper-bench-real.ps1 -ModelFile qwen2.5-7b-instruct-q4_k_m-00001-of-00002.gguf -Mode native
#   .\reaper-bench-real.ps1 -ModelFile ... -Mode prompt
#
# Scoring follows what the coordinator does with a reply, not whether it
# parses: the right tools called, real targets, and plain text (no calls) for
# a state question. Native mode reads structured tool_calls first and falls
# back to the text, exactly as intent.rs does.
#
# ASCII only: Windows PowerShell 5.1 reads BOM-less files as ANSI.
param(
  [string]$ModelFile,
  [ValidateSet('native','prompt')][string]$Mode = 'native',
  [string]$Inputs = (Join-Path $PSScriptRoot 'intent-bench.json'),
  [int]$Port = 8093,
  [int]$Ctx = 8192,
  [switch]$NoThink
)

$Model = Join-Path "C:\Users\jonno\.ai-mesh\models" $ModelFile
$bin   = "C:\Users\jonno\AppData\Local\Programs\llama.cpp\llama-server.exe"
# Read as UTF-8 explicitly: the prompts contain em dashes.
$data  = [IO.File]::ReadAllText($Inputs, [Text.Encoding]::UTF8) | ConvertFrom-Json
$toolsJson = ($data.tools | ConvertTo-Json -Depth 30 -Compress)
$system = $(if ($Mode -eq 'native') { $data.native_system } else { $data.prompt_system })
$knownTools = @($data.tool_names)

$srvArgs = @('--model',$Model,'--host','127.0.0.1','--port',"$Port",'--ctx-size',"$Ctx",'--n-gpu-layers','99')
$p = Start-Process -FilePath $bin -ArgumentList $srvArgs -RedirectStandardError "$env:TEMP\reaper-bench-real-$Port.err.log" -RedirectStandardOutput "$env:TEMP\reaper-bench-real-$Port.out.log" -PassThru -WindowStyle Hidden
$ok=$false
for ($i=0; $i -lt 180; $i++) {
  Start-Sleep 1
  if ($p.HasExited) { Write-Host "!!! exited early (code $($p.ExitCode))"; break }
  try { $h = Invoke-RestMethod "http://127.0.0.1:$Port/health" -TimeoutSec 2; if ($h.status -eq 'ok') { $ok=$true; break } } catch {}
}
if (-not $ok) { Write-Host "=== $ModelFile : FAILED TO LOAD ==="; exit 1 }
Write-Host "=== $ModelFile [$Mode] loaded in ${i}s ==="

# Warm-up: one throwaway request with the same system prompt (and tools), so the first
# timed case doesn't pay llama-server's first-request cost (2-3 s on beelink1,
# 2026-09-13). The coordinator sends the same kind of request when a model
# becomes Ready, so this matches what a real first command sees.
$wsys = $(if ($NoThink) { $system + "`n`n/no_think" } else { $system })
$wmsgs = ConvertTo-Json -Depth 5 -Compress -InputObject @(@{role='system';content=$wsys}, @{role='user';content='Warm-up request: reply with the single word OK.'})
$wbody = '{"model":"bench","messages":' + $wmsgs + ',"max_tokens":8,"temperature":0,"stream":false' + $(if ($Mode -eq 'native') { ',"tools":' + $toolsJson } else { '' }) + '}'
$ww = [Diagnostics.Stopwatch]::StartNew()
try { $null = Invoke-RestMethod "http://127.0.0.1:$Port/v1/chat/completions" -Method Post -Body ([Text.Encoding]::UTF8.GetBytes($wbody)) -ContentType 'application/json; charset=utf-8' -TimeoutSec 180 } catch { Write-Host "  (warm-up failed: $($_.Exception.Message))" }
Write-Host ("  (warm-up {0:N1}s)" -f $ww.Elapsed.TotalSeconds)

# build_device_context + build_sensor_context format, "\n\n"-terminated.
$context = @"
Known devices:
  - Studio Ceiling [Studio]  (online, off)
  - Studio Lamp [Studio]  (online, on, 60% brightness)
  - Kitchen Pendant [Kitchen]  (online, on, 100% brightness, 2700 K)
  - Kitchen Spots [Kitchen]  (online, off)
  - Desk Strip [Office]  (online, off)
  - Office Lamp [Office]  (online, on, 40% brightness)
Known groups (control all members at once): Kitchen Group
Available scenes: Relax, Focus

Known sensors:
  - Bedroom Sensor [Bedroom]: 19.5$([char]0x00B0)C, 52% RH, battery 88%
  - Office Sensor [Office]: 22.1$([char]0x00B0)C, 45% RH, motion detected

"@
$targets = @('Studio Ceiling','Studio Lamp','Kitchen Pendant','Kitchen Spots','Desk Strip','Office Lamp','Kitchen Group','Studio','Kitchen','Office')

# want: tool names that must appear (any order). text: $true means the right
# answer is plain text with no tool calls.
$cases = @(
  @{ name='simple lights';  q='turn the studio lights off';                                   want=@('light_command') },
  @{ name='transport';      q='start recording';                                              want=@('reaper_transport') },
  @{ name='multi-step';     q='add a guitar track, arm it, and set the tempo to 96';          want=@('reaper_add_track','reaper_set_tempo') },
  @{ name='cross-domain';   q='dim the studio lamp to 20% and stop playback';                 want=@('light_command','reaper_transport') },
  @{ name='colour';         q='make the kitchen lights blue';                                 want=@('light_command') },
  @{ name='state question'; q="what's on?";                                                   text=$true },
  @{ name='light + climate';q='turn off the office lamp and tell me the bedroom temperature'; want=@('light_command','get_climate') }
)

function Get-TextCalls([string]$txt) {
  # A PowerShell cut of try_parse_tool_calls: strip fences, read JSON values,
  # keep objects that have "tool" and object "args".
  $clean = ($txt -replace '```json',' ' -replace '```',' ').Trim()
  $out = @()
  try { $v = $clean | ConvertFrom-Json } catch { return $out }
  foreach ($el in @($v)) { if ($el.tool -and $el.args) { $out += [pscustomobject]@{ tool=$el.tool; args=$el.args } } }
  return $out
}

$pass = 0
foreach ($c in $cases) {
  $userText = $context + $c.q
  if ($NoThink) { $sys = $system + "`n`n/no_think" } else { $sys = $system }
  $msgs = ConvertTo-Json -Depth 5 -Compress -InputObject @(@{role='system';content=$sys}, @{role='user';content=$userText})
  $body = '{"model":"bench","messages":' + $msgs + ',"max_tokens":400,"temperature":0.4,"repeat_penalty":1.1,"stream":false'
  if ($Mode -eq 'native') { $body += ',"tools":' + $toolsJson }
  $body += '}'

  $sw=[Diagnostics.Stopwatch]::StartNew()
  try {
    $r = Invoke-RestMethod "http://127.0.0.1:$Port/v1/chat/completions" -Method Post -Body ([Text.Encoding]::UTF8.GetBytes($body)) -ContentType 'application/json; charset=utf-8' -TimeoutSec 180
  } catch { Write-Host "  [$($c.name)] REQUEST FAILED: $($_.Exception.Message)"; continue }
  $sw.Stop()
  $msg = $r.choices[0].message
  $content = "$($msg.content)" -replace '(?s)<think>.*?</think>',''
  $content = $content.Trim()

  $calls = @()
  $source = 'none'
  foreach ($tc in @($msg.tool_calls | Where-Object { $_ })) {
    try { $a = $tc.function.arguments | ConvertFrom-Json } catch { $a = $null }
    if ($a -ne $null) { $calls += [pscustomobject]@{ tool=$tc.function.name; args=$a }; $source = 'structured' }
  }
  if ($calls.Count -eq 0) { $calls = @(Get-TextCalls $content); if ($calls.Count) { $source = 'text' } }

  $notes = @()
  if ($c.text) {
    $ok = ($calls.Count -eq 0) -and ($content.Length -gt 0)
    if (-not $ok) { $notes += 'expected a plain-text answer' }
  } else {
    $names = @($calls | ForEach-Object { $_.tool })
    $missing = @($c.want | Where-Object { $names -notcontains $_ })
    $unknown = @($names | Where-Object { $knownTools -notcontains $_ })
    $ok = ($missing.Count -eq 0) -and ($unknown.Count -eq 0)
    if ($missing.Count) { $notes += "missing: $($missing -join ', ')" }
    if ($unknown.Count) { $notes += "unknown tool: $($unknown -join ', ')" }
    foreach ($call in $calls) {
      if ($call.tool -eq 'light_command' -and $call.args.target -and ($targets -notcontains $call.args.target)) { $ok = $false; $notes += "invented target '$($call.args.target)'" }
    }
  }
  if ($ok) { $pass++ }
  $t = $r.timings
  Write-Host ("  [{0}] {1}  {2:N1}s  decode {3:N1} t/s  calls={4} ({5})" -f $c.name, $(if ($ok) {'PASS'} else {'FAIL'}), $sw.Elapsed.TotalSeconds, $t.predicted_per_second, $calls.Count, $source)
  foreach ($call in $calls) { Write-Host ("      -> {0} {1}" -f $call.tool, ($call.args | ConvertTo-Json -Compress -Depth 10)) }
  if ($calls.Count -eq 0 -and $content) { Write-Host ("      text: " + ($content -replace "`r?`n"," ")) }
  foreach ($n in $notes) { Write-Host "      !! $n" }
}
Write-Host "  SCORE $pass/$($cases.Count)"

Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
Wait-Process -Id $p.Id -Timeout 30 -ErrorAction SilentlyContinue
Write-Host ""

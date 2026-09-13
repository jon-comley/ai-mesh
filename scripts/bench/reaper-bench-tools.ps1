# Same bench as reaper-bench.ps1, but with NATIVE tool calling: the tools go in
# the request's `tools` field and llama-server (with --jinja) renders them with
# the model's own chat template, then returns structured `tool_calls`.
#
# reaper-bench.ps1 is the prompt-described format ai-mesh uses today and stays
# the reference - this file changes nothing in ai-mesh. Same four cases, same
# devices, same tools, same sampling, so the two outputs compare directly.
#
# Run on beelink1:  .\reaper-bench-tools.ps1 -ModelFile Hammer2.1-7b-Q4_K_M.gguf
# Qwen3 needs -NoThink, exactly as in the prompt-mode bench.
#
# ASCII only: Windows PowerShell 5.1 reads BOM-less files as ANSI.
param([string]$ModelFile, [int]$Port = 8092, [int]$Ctx = 8192, [switch]$NoThink)

$Model = Join-Path "C:\Users\jonno\.ai-mesh\models" $ModelFile
$bin   = "C:\Users\jonno\AppData\Local\Programs\llama.cpp\llama-server.exe"
$log   = "$env:TEMP\reaper-bench-tools.err.log"
if (Test-Path $log) { Remove-Item $log -Force }

$srvArgs = @('--model',$Model,'--host','127.0.0.1','--port',"$Port",'--ctx-size',"$Ctx",'--n-gpu-layers','99','--jinja')
$p = Start-Process -FilePath $bin -ArgumentList $srvArgs -RedirectStandardError $log -RedirectStandardOutput "$env:TEMP\reaper-bench-tools.out.log" -PassThru -WindowStyle Hidden

$ok=$false
for ($i=0; $i -lt 180; $i++) {
  Start-Sleep 1
  try { $h = Invoke-RestMethod "http://127.0.0.1:$Port/health" -TimeoutSec 2; if ($h.status -eq 'ok') { $ok=$true; break } } catch {}
  if ($p.HasExited) { Write-Host "!!! exited early (code $($p.ExitCode))"; break }
}
if (-not $ok) { Write-Host "=== $ModelFile : FAILED TO LOAD after ${i}s ==="; Get-Content $log -Tail 5; exit 1 }
Write-Host "=== $ModelFile loaded in ${i}s (native tools, --jinja) ==="

# The prompt-mode system prompt minus the parts native tools replace: the JSON
# reply format and the tool list. The target rules and devices are unchanged.
$system = @'
You are a helpful smart home assistant embedded in ai-mesh. You have direct control of and live state for all listed devices.

Rules:
- The "target" field must be an exact device or group name from the known list. Never invent a target name.
- For compound requests (e.g. requests spanning lighting + REAPER), call one tool per command.
- Only call tools when the user is explicitly asking you to CHANGE or CONTROL something.

Known devices: Studio Ceiling [Studio], Studio Lamp [Studio], Kitchen Group [Kitchen], Desk Strip [Office]
'@

# The same twelve tools as reaper-bench.ps1, as JSON Schema. Types follow that
# file's shorthand exactly (arm is a boolean, light value is a string) so the
# only difference between the benches is how the tools reach the model.
$toolsJson = @'
[
 {"type":"function","function":{"name":"reaper_transport","description":"Control REAPER transport","parameters":{"type":"object","properties":{"action":{"type":"string","enum":["play","stop","pause","record","rewind"]}},"required":["action"]}}},
 {"type":"function","function":{"name":"reaper_add_track","description":"Add a track to the REAPER project","parameters":{"type":"object","properties":{"name":{"type":"string"},"arm":{"type":"boolean"}},"required":["name"]}}},
 {"type":"function","function":{"name":"reaper_remove_track","description":"Remove a track by index","parameters":{"type":"object","properties":{"index":{"type":"integer"}},"required":["index"]}}},
 {"type":"function","function":{"name":"reaper_remove_all_tracks","description":"Remove every track","parameters":{"type":"object","properties":{}}}},
 {"type":"function","function":{"name":"reaper_set_tempo","description":"Set the project tempo","parameters":{"type":"object","properties":{"bpm":{"type":"number"}},"required":["bpm"]}}},
 {"type":"function","function":{"name":"reaper_add_fx","description":"Add an FX to a track","parameters":{"type":"object","properties":{"track":{"type":"integer"},"fx":{"type":"string"}},"required":["track","fx"]}}},
 {"type":"function","function":{"name":"reaper_list_fx","description":"List FX on a track","parameters":{"type":"object","properties":{"track":{"type":"integer"}},"required":["track"]}}},
 {"type":"function","function":{"name":"reaper_list_fx_params","description":"List an FX's parameters","parameters":{"type":"object","properties":{"track":{"type":"integer"},"fx":{"type":"integer"}},"required":["track","fx"]}}},
 {"type":"function","function":{"name":"reaper_get_project","description":"Get the REAPER project state","parameters":{"type":"object","properties":{}}}},
 {"type":"function","function":{"name":"reaper_action","description":"Run a named REAPER action","parameters":{"type":"object","properties":{"action":{"type":"string"}},"required":["action"]}}},
 {"type":"function","function":{"name":"reaper_script","description":"Run a Lua script in REAPER","parameters":{"type":"object","properties":{"script":{"type":"string"}},"required":["script"]}}},
 {"type":"function","function":{"name":"light_command","description":"Control a light","parameters":{"type":"object","properties":{"target":{"type":"string"},"action":{"type":"string","enum":["on","off","brightness","colour"]},"value":{"type":"string"}},"required":["target","action"]}}}
]
'@

$knownTools   = @('reaper_transport','reaper_add_track','reaper_remove_track','reaper_remove_all_tracks','reaper_set_tempo','reaper_add_fx','reaper_list_fx','reaper_list_fx_params','reaper_get_project','reaper_action','reaper_script','light_command')
$knownTargets = @('Studio Ceiling','Studio Lamp','Kitchen Group','Desk Strip')

$cases = @(
  @{ name='simple lights';    q='turn the studio lights off' },
  @{ name='simple transport'; q='start recording' },
  @{ name='multi-step reaper'; q='add a guitar track, arm it, and set the tempo to 96' },
  @{ name='cross-domain';     q='dim the studio lamp to 20% and stop playback' }
)


# Warm-up: one throwaway request with the same system prompt and tools, so the first
# timed case doesn't pay llama-server's first-request cost (2-3 s on beelink1,
# 2026-09-13). The coordinator sends the same kind of request when a model
# becomes Ready, so this matches what a real first command sees.
$wmsgs = ConvertTo-Json -Depth 5 -Compress -InputObject @(@{role='system';content=$system}, @{role='user';content='Warm-up request: reply with the single word OK.'})
$wbody = '{"model":"bench","messages":' + $wmsgs + ',"tools":' + $toolsJson + ',"max_tokens":8,"temperature":0,"stream":false}'
$ww = [Diagnostics.Stopwatch]::StartNew()
try { $null = Invoke-RestMethod "http://127.0.0.1:$Port/v1/chat/completions" -Method Post -Body ([Text.Encoding]::UTF8.GetBytes($wbody)) -ContentType 'application/json' -TimeoutSec 180 } catch {}
Write-Host ("  (warm-up {0:N1}s)" -f $ww.Elapsed.TotalSeconds)

foreach ($c in $cases) {
  $userText = $(if ($NoThink) { $c.q + ' /no_think' } else { $c.q })
  # -InputObject, not a pipe: a pipe unrolls arrays in PowerShell 5.1.
  $msgs = ConvertTo-Json -Depth 5 -Compress -InputObject @(@{role='system';content=$system}, @{role='user';content=$userText})
  $body = '{"model":"bench","messages":' + $msgs + ',"tools":' + $toolsJson + ',"tool_choice":"auto","max_tokens":300,"temperature":0.1,"stream":false}'

  $sw=[Diagnostics.Stopwatch]::StartNew()
  try {
    $r = Invoke-RestMethod "http://127.0.0.1:$Port/v1/chat/completions" -Method Post -Body ([Text.Encoding]::UTF8.GetBytes($body)) -ContentType 'application/json' -TimeoutSec 180
  } catch {
    Write-Host "  [$($c.name)] REQUEST FAILED: $($_.Exception.Message)"
    continue
  }
  $sw.Stop()
  $msg = $r.choices[0].message
  $t = $r.timings

  # A pass needs structured tool_calls: every name a real tool, every
  # arguments string valid JSON. Text that merely looks like JSON is a fail.
  $calls = @($msg.tool_calls | Where-Object { $_ })
  $valid = $calls.Count -gt 0
  $shown = @()
  $notes = @()
  foreach ($tc in $calls) {
    $name = $tc.function.name
    if ($knownTools -notcontains $name) { $valid = $false; $notes += "unknown tool '$name'" }
    try {
      $a = $tc.function.arguments | ConvertFrom-Json
      if ($name -eq 'light_command' -and $knownTargets -notcontains $a.target) { $notes += "invented target '$($a.target)'" }
      if ($name -eq 'reaper_add_track' -and $null -ne $a.arm -and -not ($a.arm -is [bool])) { $notes += "arm not boolean ($($a.arm.GetType().Name))" }
    } catch { $valid = $false; $notes += "bad arguments JSON for $name" }
    $shown += "$name $($tc.function.arguments)"
  }
  $content = "$($msg.content)".Trim()
  if ($calls.Count -eq 0) { $notes += "no tool_calls" + $(if ($content) { " (text: " + ($content -replace "`r?`n"," ") + ")" } else { "" }) }

  Write-Host ("  [{0}] {1:N1}s  prefill {2:N0} t/s  decode {3:N1} t/s  tool_calls={4}  valid={5}" -f $c.name, $sw.Elapsed.TotalSeconds, $t.prompt_per_second, $t.predicted_per_second, $calls.Count, $valid)
  foreach ($s in $shown) { Write-Host "      -> $s" }
  foreach ($n in $notes) { Write-Host "      !! $n" }
}

Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
# Wait for it to go: the next run's health check must not reach this server.
Wait-Process -Id $p.Id -Timeout 30 -ErrorAction SilentlyContinue
Write-Host ""

# Bench a model on REALISTIC command generation for REAPER + lights.
# Measures prefill/decode AND whether the tool call is valid — speed is useless
# if the JSON is wrong. Mirrors coordinator/src/intent.rs build_system_prompt.
param([string]$ModelFile, [int]$Port = 8092, [int]$Ctx = 8192, [switch]$NoThink)

$Model = Join-Path "C:\Users\jonno\.ai-mesh\models" $ModelFile
$bin   = "C:\Users\jonno\AppData\Local\Programs\llama.cpp\llama-server.exe"
$log   = "$env:TEMP\reaper-bench.err.log"
if (Test-Path $log) { Remove-Item $log -Force }

$args = @('--model',$Model,'--host','127.0.0.1','--port',"$Port",'--ctx-size',"$Ctx",'--n-gpu-layers','99')
$p = Start-Process -FilePath $bin -ArgumentList $args -RedirectStandardError $log -RedirectStandardOutput "$env:TEMP\reaper-bench.out.log" -PassThru -WindowStyle Hidden

$ok=$false
for ($i=0; $i -lt 120; $i++) {
  Start-Sleep 1
  try { $h = Invoke-RestMethod "http://127.0.0.1:$Port/health" -TimeoutSec 2; if ($h.status -eq 'ok') { $ok=$true; break } } catch {}
  if ($p.HasExited) { Write-Host "!!! exited early (code $($p.ExitCode))"; break }
}
if (-not $ok) { Write-Host "=== $ModelFile : FAILED TO LOAD after ${i}s ==="; exit 1 }
Write-Host "=== $ModelFile loaded in ${i}s ==="

$system = @'
You are a helpful smart home assistant embedded in ai-mesh. You have direct control of and live state for all listed devices.

To control one device, reply with ONLY this JSON (no extra text):
{"tool": "<name>", "args": { ... }}

To control multiple devices in one request, reply with ONLY a JSON array (no extra text):
[{"tool": "<name>", "args": { ... }}, {"tool": "<name>", "args": { ... }}]

Rules:
- The "target" field must be an exact device or group name from the known list. Never invent a target name.
- For compound requests (e.g. requests spanning lighting + REAPER), emit one array element per command.
- Only output JSON when the user is explicitly asking you to CHANGE or CONTROL something.

Available tools:
[{"name":"reaper_transport","args":{"action":"play|stop|pause|record|rewind"}},
 {"name":"reaper_add_track","args":{"properties":{"name":"string","arm":"bool"}}},
 {"name":"reaper_remove_track","args":{"index":"int"}},
 {"name":"reaper_remove_all_tracks","args":{}},
 {"name":"reaper_set_tempo","args":{"bpm":"number"}},
 {"name":"reaper_add_fx","args":{"track":"int","fx":"string"}},
 {"name":"reaper_list_fx","args":{"track":"int"}},
 {"name":"reaper_list_fx_params","args":{"track":"int","fx":"int"}},
 {"name":"reaper_get_project","args":{}},
 {"name":"reaper_action","args":{"action":"string"}},
 {"name":"reaper_script","args":{"script":"string"}},
 {"name":"light_command","args":{"target":"string","action":"on|off|brightness|colour","value":"string"}}]

Known devices: Studio Ceiling [Studio], Studio Lamp [Studio], Kitchen Group [Kitchen], Desk Strip [Office]
'@

$cases = @(
  @{ name='simple lights';    q='turn the studio lights off' },
  @{ name='simple transport'; q='start recording' },
  @{ name='multi-step reaper'; q='add a guitar track, arm it, and set the tempo to 96' },
  @{ name='cross-domain';     q='dim the studio lamp to 20% and stop playback' }
)

foreach ($c in $cases) {
  $body = @{
    model='bench'
    messages=@(@{role='system';content=$system}, @{role='user';content=$(if ($NoThink) { $c.q + ' /no_think' } else { $c.q })})
    max_tokens=300; temperature=0.1; stream=$false
  } | ConvertTo-Json -Depth 8
  $sw=[Diagnostics.Stopwatch]::StartNew()
  try { $r = Invoke-RestMethod "http://127.0.0.1:$Port/v1/chat/completions" -Method Post -Body $body -ContentType 'application/json' -TimeoutSec 180 } catch { Write-Host "  $($c.name): REQUEST FAILED"; continue }
  $sw.Stop()
  $txt = $r.choices[0].message.content
  $t = $r.timings
  $valid = $false
  $trimmed = ($txt -replace '(?s)```json','' -replace '(?s)```','').Trim()
  # Empty is not valid. ConvertFrom-Json returns null for "" rather than
  # throwing, which scored Qwen3's silent multi-step answer as a pass.
  $trimmed = ($trimmed -replace '(?s)<think>.*?</think>','').Trim()
  if ($trimmed.Length -gt 0) { try { $null = $trimmed | ConvertFrom-Json; $valid = $true } catch {} }
  Write-Host ("  [{0}] {1:N1}s  prefill {2:N0} t/s  decode {3:N1} t/s  json={4}" -f $c.name, $sw.Elapsed.TotalSeconds, $t.prompt_per_second, $t.predicted_per_second, $valid)
  Write-Host ("      -> " + ($trimmed -replace "`r?`n"," " ) )
}

Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue
# Wait for it to go: the next run's health check must not reach this server.
Wait-Process -Id $p.Id -Timeout 30 -ErrorAction SilentlyContinue
Write-Host ""

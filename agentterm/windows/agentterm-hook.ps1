# Claude's hooks and status line for a pooled worker's terminal on a Windows
# node (plans/agent-terminals.md in ai-mesh). Forwards each to agentterm on
# lenovo1, which keeps what the tile shows. Not yet run: beelink1 has no Claude.
#   agentterm-hook.ps1 <agent> <event>
param([string]$agent, [string]$event)
$inputJson = [Console]::In.ReadToEnd()
$what = if ($event -eq 'statusline') { 'statusline' } else { "hook $event" }
# A missed update must never block the session.
try {
    $inputJson | ssh -o BatchMode=yes -o ConnectTimeout=5 lenovo1 "AGENTTERM_AGENT=$agent /usr/local/bin/agentterm $what" 2>$null
} catch {}
exit 0

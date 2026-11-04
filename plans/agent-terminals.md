# Agent terminals: every worker is a Claude terminal

## Status (2026-11-04)

Agreed with Jon, not started. Core ai-mesh: a new workspace crate, `agentterm`, which
agentwatch's tiles open. It replaces the "Talk to <name>" chat box.

## What Jon gets

1. **Full screen.** A tile opens its agent full screen: a real `claude` terminal in the
   browser (ttyd and tmux on the node that runs it), the same as a local Claude Code
   session.
2. **Opens on a summary, for free.** The terminal prints the `State:` and `NEXT:` lines of
   the agent's status file (else its first line) before Claude starts: no model call, so
   opening costs nothing until Jon types.
3. **Jon types, then leaves.** tmux keeps the session; the agent carries on with the tasks.
4. **The tile shows the result:** `working`, `done` or `needs you`, written by a Claude
   `Stop` hook. Clicking in gives the summary.
5. **Escalate only when needed.** Sessions start on Haiku and step up to Sonnet or Opus
   only when the task needs it, saying so in one line with the reason.
6. **Costs.** Each tile shows today's and this month's spend; each session shows its
   running cost in the status line; a notification fires when a session or a day passes
   its limit (defaults GBP 2 a session, GBP 10 a day, set per node).

## Access

A terminal is a shell on its node, so ttyd listens on Tailscale only, behind a login
(`~/.config/agentterm/login.env` on lenovo1, never in a repo). Open the page at
`http://100.82.51.25/` from a device on Tailscale (Windows needs its own Tailscale, not
WSL's). The browser asks for the login once, on the terminal's own address
(`:7681/?arg=desk`): it will not ask inside the page's frame.

## Workers on mac1

mac1 has no tmux or Rust, so the tmux session stays on lenovo1 and holds an `ssh -t`
to mac1, where `claude` runs in the worker's own directory with its env and brief.
`~/.ai-mesh/bin/agentterm-hook` on mac1 forwards the hooks and status line to lenovo1.
The orchestrator's ticks pause while its terminal is in use: opening it waits for a
running tick, a prompt during a tick is refused, and agentwatch gives the ticks back
after 30 minutes with nothing to do or when the terminal closes. Reviews reads only.

## Where a session runs

The coordinator already places models and inference by headroom. Agent sessions join it:
1. Each node advertises what it can host: `claude` logged in, which repo clones it has,
   what it can reach (the work laptop, the site PCs, the phones), and its current load.
2. A session goes to the least-loaded node that can do its job. Pins override: work
   tasks to the work laptop, gated runs to mac1.
3. The tile shows the node a session is on.

Built 4 Nov in `agentterm/src/node.rs`, without the coordinator: `[[node]]` entries in
lenovo1's `agents.toml` list each machine's `can` (`claude`, a worker's slug for its clone,
`phones`, `work`); a worker may set `needs` and `pin`. A session already open keeps its
node. lenovo1 has the Desk and Standards clones, mac1 none yet, and beelink1 nothing until
Jon adds Claude there (the Windows scripts are written and not yet run). The tiles refresh
every 10 seconds while the page is in view: no AI, a file read and one ssh.

## Work laptop (built 4 Nov)

Nothing of Claude is on the laptop.
- You talk tasks through in its tile (Haiku, lenovo1); it queues them with
  `agentterm queue add work-laptop ~/repos/<repo> "<task>"`.
- The dispatcher (no AI) shows **wl on/off** and, when the laptop is on and its `~/repos`
  is mounted on lenovo1 (sshfs, `agentterm-wl-repos.service`), runs the next task on
  lenovo1 in tmux: Claude edits through the mount and runs commands with `wl`.
- `wl` (`agentterm/lenovo1/wl`) refuses push, gh, sudo, installs and other machines, makes
  pushes fail even from scripts, and commits as JPComley. Refused work ends NEEDS YOU.
- Nothing about the work goes in a repo.
- The brief shows the item in hand: STATUS.md's `Ticket: <repo>/<file>.md` lines link to the
  ticket rendered from the mount (titled by its first heading), `Kanban: https://...` to the board.

## Order

1. `agentterm` on lenovo1 with the Desk and Standards tiles (local `claude`, their chat
   clones), status hook and costs.
2. mac1's orchestrator and Reviews tiles (claude on mac1, tmux on lenovo1).
3. Placement: nodes listed with what they can do; the least busy able node takes a new
   session (done, beelink1 waiting on Claude).
4. The work-laptop worker: queue and dispatcher (done).

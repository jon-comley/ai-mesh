# Agent terminals: every worker is a Claude terminal

## Status (2026-11-04)

Agreed with Jon, not started. Core ai-mesh: a new workspace crate, `agentterm`, which
agentwatch's tiles open. It replaces the "Talk to <name>" chat box.

## What Jon gets

1. **Full screen.** A tile opens its agent full screen: a real `claude` terminal in the
   browser (ttyd and tmux on the node that runs it), the same as a local Claude Code
   session.
2. **Opens on a summary.** Haiku reads the agent's status file and says where it is up to.
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

## Where a session runs

The coordinator already places models and inference by headroom. Agent sessions join it:
1. Each node advertises what it can host: `claude` logged in, which repo clones it has,
   what it can reach (the work laptop, the site PCs, the phones), and its current load.
2. A session goes to the least-loaded node that can do its job. Pins override: work
   tasks to the work laptop, gated runs to mac1.
3. The tile shows the node a session is on.

## Work-laptop agent

A worker on the work laptop, run there, because some work (its GitHub account, the
site PCs) is only reachable from it. Its tile shows `off` when the laptop is off.
- It may edit, build and commit locally on its own.
- It never pushes, opens a PR or installs anything without Jon's yes.
- Installing it on the laptop needs Jon's yes first (his rule one).
- Nothing about the work goes in this public repo: the laptop's brief and status live
  on the laptop.

## Order

1. `agentterm` on lenovo1 with the Desk and Standards tiles (local `claude`, their chat
   clones), status hook and costs.
2. mac1's orchestrator and Reviews tiles (tmux on mac1 over ssh).
3. Placement: nodes advertise what they can host; the coordinator picks the node.
4. The work-laptop worker, on Jon's yes.

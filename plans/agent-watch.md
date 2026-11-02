# Agent watch: seeing what the scheduled agents are doing

## Status (2026-11-02)

**Plan.** Nothing built. First agent to watch: Standards (`jon-comley/standards`, repo not yet
created). Second proposed below: Desk.

## Goal

Jon runs scheduled Claude agents (cloud routines) that each live in their own repo and
write there. He wants one place to see, for every agent: did it run, what did it do, what
does it need from him. More agents will follow, so adding one must be a config line, not
code.

## Where it runs

**lenovo1, on its own, at `http://lenovo1/`.** A standalone `agentwatch` binary with its own
systemd unit serves the page and the API itself on port 80 (`AmbientCapabilities=CAP_NET_BIND_SERVICE`,
port 80 free on lenovo1, checked 2 Nov). That is a shorter URL than the dashboard's
`http://pi1:9001`. It does not go through the coordinator or the node agent, so it keeps
working when pi1 drops off its wifi.

It keeps `~/.ai-mesh/agents/`: `repos/` (bare clones), `agents.db`, `agents.toml` and the
deploy keys. Nothing is kept on pi1, and pi1 is not involved.

Why lenovo1: it is Linux with systemd, always on and already a mesh node. mac1 is busy
with the gated Maestro runs and reviews. beelink1 is Windows under NSSM, where keys and
paths are awkward, and it carries STT.

## Access

**One read-only deploy key per agent repo**, not an account token. A deploy key opens one
repo, cannot write and does not expire, and the repo's owner adds it under the repo's
Settings, Deploy keys. So no account token is needed, and a leaked key
exposes one repo read-only.

- lenovo1 makes the key: `ssh-keygen -t ed25519 -f ~/.ai-mesh/agents/keys/<agent> -N ""`.
- Jon pastes the `.pub` into `https://github.com/<owner>/<repo>/settings/keys`, read-only.
- `agents.toml` names the key per agent. The clone uses
  `GIT_SSH_COMMAND="ssh -i <key> -o IdentitiesOnly=yes"`, so no `~/.ssh/config` entries are needed.

## The contract every agent repo keeps

Standards already does all of this. A new agent's brief must say the same.

- `agent-brief.md`: the standing brief.
- `agent-log.md`: newest first, one line per run:
  `- **YYYY-MM-DD** <summary>. [Report](reports/YYYY-MM-DD.md)`
- `reports/YYYY-MM-DD.md`: that run's report.
- `HUB.md` (optional): Jon's page. The watcher counts the rows under a `## Needs you`
  heading.

## Config

`~/.ai-mesh/agents/agents.toml` on lenovo1, editable from the page:

```toml
[[agent]]
name = "Standards"
repo = "git@github.com:jon-comley/standards.git"
key = "standards"
every = "7d"        # how often it is meant to run (Standards is weekly)
grace = "6h"        # late after every + grace with no new log line
hub = "HUB.md"
```

## What it works out

Every 15 minutes it runs `git fetch` on each repo. Nothing else talks to GitHub.

- **Last run:** the date on the top log line, plus the time of the commit that added it.
- **State:**
  - `ok`: the newest log line is within `every + grace`.
  - `late`: there is no log line in that window. This is the only sign a cloud run failed,
    because a routine that breaks before it pushes leaves nothing to see.
  - `broken`: commits landed after the last log line but no new line. The run started
    and did not finish.
  - `unreachable`: the fetch failed (key removed, repo renamed).
- **Needs you:** the row count under `## Needs you` in the hub, and the change since the
  last run.
- **Latest report:** its Markdown, rendered on the page.

`agents.db` keeps one row per fetch (agent, time, state, top log line, needs-you count),
so the page can show a run history.

## The page

One card per agent: name, state light, last run ("2 Nov, 06:12"), the top log line,
needs-you count, and links to the latest report (opens in the page) and the hub on GitHub.
Below the cards, a combined run history. Settings: add or remove an agent (writes
`agents.toml`), the ntfy topic.

**ntfy push** when an agent turns `late`, `broken` or `unreachable`, and when needs-you
grows. One push per change, not one per poll.

## Routes

Served by `agentwatch` on lenovo1:

- `GET /`: the page.
- `GET /api/agents`: the cards and recent history.
- `GET /api/agents/{name}/report`: the latest report's Markdown.
- `POST /api/agents/refresh`: fetch now.
- `POST /api/agents`, `DELETE /api/agents/{name}`: edit `agents.toml`.

## Adding an agent

1. Create its repo with the contract files and point the routine at it.
2. On lenovo1, make a deploy key and add it to the repo, read-only.
3. Add the agent in the page's settings.

## A second agent: Desk

**What it does:** takes the paperwork off Jon. Each morning it reads the repos and leaves
one page, `HUB.md`, saying what only Jon can do today, with each item ready to act on, plus
the tidy-up it has already done for him.

- **Reads** (through jon-comley, read-only): guv, admin, dashboard, ai-mesh, ventures and
  `~/repos/HANDOVER.md`'s repo copy if one exists. Never ThingCo or the work laptop.
- **"Only you" (at most five):** things blocked on Jon, from ROADMAP rows, `docs/builds.md`,
  and "waits on Jon" lines. Each comes with what he needs to act at once: the command, the
  direct URL, or the draft message as plain text to paste.
- **"Done for you":** the chores that keep coming back, prepared as patches in its own
  repo (`patches/YYYY-MM-DD-<repo>.patch`), never pushed to the repos themselves:
  - ROADMAP rows whose work has landed, moved to HISTORY.md with the write-up.
  - Tracking lines that git now contradicts (a "not deployed" with a later deploy commit, a
    "waits on" for something already merged), with the corrected line.
  - The handover rewritten to one screen from the current state.

  Jon says "apply the desk patches" in a local session, which applies them with `git am`, runs
  the checks and commits as jon. He reads one diff instead of writing it.
- **"Stale or stuck":** anything waiting more than 7 days, once each, then left alone
  (parked items stay parked).
- **Repo:** `jon-comley/desk`, daily (`every = "1d"`), same contract. Its brief carries
  Jon's writing rules (no em dashes, short, no flannel) so the drafts need no editing.

It cannot see deploy or console state (Cloudflare, Firebase, Stripe), only git. So it
flags "git says X, check it" rather than asserting what is live.

## Build order

1. `agentwatch` crate, pure: parse `agent-log.md`, the hub's needs-you count and
   `agents.toml`; state rules from (top log line, commits since, now). Unit tests on each,
   with Standards' real files as fixtures.
2. `agentwatch` binary: axum server on port 80, bare clones, the 15-minute fetch,
   `agents.db`, ntfy on state change. The page (`static/agents.html`, `agents.js`) compiled
   in with `include_str!`, with vitest tests as for `reviews.js`.
3. `systemd/agentwatch.service` and a `just deploy-agentwatch` recipe that builds and
   installs it on lenovo1.
4. Make the Standards deploy key on lenovo1 and add it to the repo.
5. `docs/agent-watch.md`: setup and the contract, for the next agent's brief to point at.

## Checks before calling it done

- A temporary agent pointed at a scratch repo shows `ok`. It turns `late` when its
  `every` is set to 1 minute, `broken` after a commit without a log line, and
  `unreachable` once its key is removed from the repo. One push each.
- Standards shows its real last run and needs-you count.
- With pi1 powered off, `http://lenovo1/` still loads and still fetches.
- With lenovo1 restarted, the page comes back with its history intact.

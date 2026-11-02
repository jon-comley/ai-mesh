# Agent watch: seeing what the scheduled agents are doing

## Status (2026-11-02)

**Live on lenovo1** (steps 1 to 4): `http://192.168.1.47/` at home, and over Tailscale
`http://lenovo1.tail3c4ee3.ts.net/` (the bare name `lenovo1` only resolves on a
Tailscale client with MagicDNS). Three tiles: Desk, Standards, the mac1 orchestrator.
Deploy keys `agentwatch on lenovo1` (read-write) are on jon-comley/desk and
jon-comley/standards. History rows are written when what a tile shows changes, not on
every fetch. 38 Rust tests, 6 page tests.

**Fourth tile, Reviews** (2 Nov): the nightly code reviews on mac1 are a worker too.
A local worker may have `reports = "<dir>"` instead of a status file: its last run is
its newest report, the tile lists the ten newest, each opens on the page, and what waits
on Jon is each repo's confirmed high and medium findings. No Run now yet (the reviews
start from the coordinator).

The ai-mesh dashboard has a **Workers** sub-tab under Tools that shows this page in a
frame; the data still comes straight from lenovo1.

**Chat, live for Desk and Standards** (2 Nov): "Talk to <name>" at the top of each
worker's page. `claude` on lenovo1 (logged in as Jon) answers in a clone kept for chats
(`~/.ai-mesh/agents/chats/<name>`), resuming one conversation per worker until "New
conversation". Each message carries its brief and what its tile shows. It may edit and
push its own repo, and writes what is agreed to `conversations/<date>.md`, which both
briefs now read first. Run now for
Desk and Standards works (tested 2 Nov, a real Desk run). The reply boxes and quick buttons
were taken out the same evening: the chat is the one way to answer, and "Waiting on you" is a
plain list (the reply route stays in the server, unused).

**Chat on mac1, and memory** (2 Nov): Orchestrator and Reviews chat too, `claude` on mac1
over ssh (`bash -s`, the context and message in quoted heredocs). The orchestrator's chat
works in `~/guv` with its env and brief, refuses while a tick is running, and pauses the
tick (resumed after 30 minutes quiet, or by Jon's own Pause or Resume). Reviews' chat is
read-only except for memory. The shared memory is live: `jon-comley/memory`, 85 facts in
`jon/`, `working/`, `vanround-guv/`, `infra/`, `money/`, `pointers/`, seeded from the
laptop's Claude memory with ThingCo and secrets left out; deploy keys for lenovo1 and mac1;
clones at `~/.ai-mesh/agents/memory` (lenovo1) and `~/.ai-mesh/memory` (mac1). Every chat
starts with `INDEX.md` and `jon/` only and opens other files when the subject comes up;
both cloud routines attach it and both briefs say the same.

**Not yet:** Desk's chat reading the other repos (it sees only its own); the Memory tile;
the orchestrator's scheduled ticks reading memory; step 8, the doc.

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

**One deploy key per agent repo**, not an account token. A deploy key opens one
repo and does not expire, and the repo's owner adds it under the repo's
Settings, Deploy keys. So no account token is needed, and a leaked key
exposes one agent repo, which holds only that agent's documents.

- lenovo1 makes the key: `ssh-keygen -t ed25519 -f ~/.ai-mesh/agents/keys/<agent> -N ""`.
- Jon pastes the `.pub` into `https://github.com/<owner>/<repo>/settings/keys` and ticks "Allow write access" (the page writes replies to the hub).
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
purpose = "Writes the Cyber Essentials, UK GDPR, ISO 27001 and ISO 9001 documents"
repo = "git@github.com:jon-comley/standards.git"
key = "standards"
every = "7d"        # how often it is meant to run (Standards is weekly)
grace = "6h"        # late after every + grace with no new log line
hub = "HUB.md"
routine = "trig_..."   # the cloud routine id, for Run now
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

Two layers.

**Top level: one tile per agent.** The tile says what the agent is: its name and its
`purpose` line from `agents.toml` ("Writes the Cyber Essentials, UK GDPR, ISO 27001 and
ISO 9001 documents"). A state light in the corner (`ok`, `late`, `broken`,
`unreachable`) and a needs-you badge when that count is above zero. Nothing else, so a
row of tiles reads at a glance.

**Click a tile: that agent's detail.**
- What it does, how often, and when it is next due.
- Its last run: when, and the top log line.
- What it is waiting on from Jon: the rows under the hub's `## Needs you`, each linking
  to the hub on GitHub where he replies.
- The latest report, rendered.
- Its run history (one line per run from `agent-log.md`, with its state at the time).
- **Talk to it**, from the tile:
  - **Reply** under each item it is waiting on (Standards' "Needs you", Desk's "Only
    you"): a text box and quick buttons (Approve, Done, No, Later). The reply is written
    into that row's "Your reply" cell in `HUB.md`, exactly as if Jon had edited it on
    GitHub, and the agent acts on it at its next run.
  - **Talk to it**: a chat on the detail page, like talking to someone in the office.
    Jon types, the agent answers there and then, and it can do the work while he
    watches ("write the access control policy now", "why is that on my list?", "drop
    X"). Details under "Chat" below.
- **Run now**: starts the agent's cloud routine immediately. The tile shows `running`
  until its next log line lands, then the new report.
- A back button to the tiles.

**How Run now starts a run.** The routine runs in Anthropic's cloud, not on lenovo1;
lenovo1 only asks for it to start. Two ways, in order of preference:

1. The routine's own API trigger token, if claude.ai offers one per routine (the routine
   carries an `api_token_hint` field; check its page before building). lenovo1 keeps the
   token in `~/.ai-mesh/agents/keys/<agent>.trigger`, mode 600, and POSTs to it.
2. Otherwise, `claude` on lenovo1, logged in once as Jon, asked headless to run the
   routine by its id through its RemoteTrigger tool. This is how runs are started from a
   local session today, so it is known to work.

One run at a time per agent: the button is disabled while one is `running`.

**Chat.** Each agent has one ongoing conversation, kept between visits, run by `claude`
on lenovo1 (logged in once as Jon, his subscription):

- It works in lenovo1's clone of the agent's repo, with the agent's `agent-brief.md` as
  its standing instructions, so it is the same agent as the scheduled run.
- **It starts with what the tile shows.** Each chat opens with the tile's current
  view as context: state, last run and its log line, what it is waiting on from Jon,
  the latest report, and for the orchestrator its last ten status entries. Updated
  again on each message if the tile has changed since (a run finished mid-chat), so
  "what did you just do?" is answered from what Jon can see, without it re-reading
  the repo. Together with the memory index this is the whole starting context.
- One conversation per agent, resumed each time (`claude --resume <id>`, the id kept in
  `agents.db`), streamed to the page as it answers. A "New conversation" button starts
  fresh.
- It may change its own repo while Jon talks to it: write a document, edit the hub,
  then commit as `jon <joncomley@googlemail.com>` and push. Nothing else.
- At the end of each chat (or after 30 minutes idle) it adds a short summary to
  `conversations/<YYYY-MM-DD>.md` in its repo: what Jon said, what it did, what it
  promised. The scheduled run reads that first, so the cloud agent knows what was
  agreed in the office.
- Desk's chat needs to read the other repos too. That is the one place lenovo1 needs
  more than the agent repos: a fine-grained GitHub token, jon-comley, all repos,
  **Contents: read-only**, kept in `~/.ai-mesh/agents/keys/read-all.token` (mode 600),
  never ThingCo (those are on another account anyway).

**Writing to the hub.** Replies and chat need write access to the agent's own repo, so
each agent's deploy key is **read-write on that repo only**. The agent repos hold only
the agents' documents, never code or secrets, and every other repo stays out of
lenovo1's write reach. lenovo1 commits as `jon <joncomley@googlemail.com>`, message
`Hub: reply from Jon`, pulling first and retrying once if the
agent pushed in between. Reply buttons change only that row; chat changes only the agent's own repo.

Desk's patches are not applied from the page: that needs write access to the other
repos, which lenovo1 never gets. They stay "apply the desk patches" in a local session.


Settings (from the top level): add or remove an agent (writes `agents.toml`), the ntfy topic.

**ntfy push** when an agent turns `late`, `broken` or `unreachable`, and when needs-you
grows. One push per change, not one per poll.

## Routes

Served by `agentwatch` on lenovo1:

- `GET /`: the page.
- `GET /api/agents`: the cards and recent history.
- `GET /api/agents/{name}/report`: the latest report's Markdown.
- `POST /api/agents/refresh`: fetch now.
- `POST /api/agents/{name}/run`: Run now.
- `POST /api/agents`, `DELETE /api/agents/{name}`: edit `agents.toml`.

## Adding an agent

1. Create its repo with the contract files and point the routine at it.
2. On lenovo1, make a deploy key and add it to the repo with write access.
3. Add the agent in the page's settings.

## A second agent: Desk

**What it does:** takes the paperwork off Jon. Each morning it reads the repos and leaves
one page, `HUB.md`, saying what only Jon can do today, with each item ready to act on, plus
the tidy-up it has already done for him.

- **Reads** (through jon-comley, read-only): every jon-comley repo, ai-mesh included
  (guv, admin, dashboard, ai-mesh, ventures, standards and any added later). Never ThingCo:
  not the ThingCo repos, `~/repos/thingco` or the work laptop.
- **"Only you" (at most five):** things blocked on Jon, from ROADMAP rows, `docs/builds.md`,
  and "waits on Jon" lines. Each comes with what he needs to act at once: the command, the
  direct URL, or the draft message as plain text to paste.
- **"Done for you":** the chores that keep coming back, prepared as patches in its own
  repo (`patches/YYYY-MM-DD-<repo>.patch`), never pushed to the repos themselves:
  - ROADMAP rows whose work has landed, moved to HISTORY.md with the write-up.
  - Tracking lines that git now contradicts (a "not deployed" with a later deploy commit, a
    "waits on" for something already merged), with the corrected line.
  - The handover rewritten to one screen from the current state, ThingCo left out.

  Jon says "apply the desk patches" in a local session, which applies them with `git am`, runs
  the checks and commits as jon. He reads one diff instead of writing it.
- **"Stale or stuck":** anything waiting more than 7 days, once each, then left alone
  (parked items stay parked).
- **Repo:** `jon-comley/desk`, daily (`every = "1d"`), same contract. Its brief carries
  Jon's writing rules (no em dashes, short, no flannel) so the drafts need no editing.

It cannot see deploy or console state (Cloudflare, Firebase, Stripe), only git. So it
flags "git says X, check it" rather than asserting what is live.

## Workers on our own machines: the mac1 orchestrator

Not every worker is a cloud routine. The guv orchestrator on mac1 (`launchd`
`com.jonno.orchestrator-tick`, a fresh `claude -p` step every 20 minutes, brief
`mac1:~/guv-orchestrator.md`) is one already, and gets a tile like the others.

`agents.toml` gives each worker a `kind`:

```toml
[[agent]]
name = "Orchestrator"
kind = "local"                # "cloud" (default) or "local"
host = "mac1"
purpose = "Runs the gated Maestro tests on guv, fixes failures, commits and pushes"
status = "~/orchestrator-status.md"
pause = "~/orchestrator-paused"
```

- **Tile and state** come from the top entry of its status file, read over ssh from
  lenovo1 (`# STEP <date> (<kind>): <summary>`). `ok` when the top entry is under 30
  minutes old, `late` past that, `paused` while the pause file exists, `broken` when
  the latest tick log in `~/orchestrator-ticks/` ends in an error.
- **Detail** shows the last ten entries, what it is waiting on (its "Waiting on Jon"
  lines), and Pause / Resume buttons (create or delete the pause file).
- **Talk to it** runs `claude` on mac1 itself, in `~/guv` with `~/orchestrator-env.sh`
  and the orchestrator's brief, so it is the same worker with its repo, deploy key and
  devices. While a chat is open the tick is paused (the page creates the pause file and
  removes it when the chat ends or after 30 minutes idle), so the two never work in
  `~/guv` at once. What Jon says that should outlast the chat is written into the status
  file as an instruction, which is where the brief already says his instructions go.
- **Run now** starts one tick at once: `launchctl kickstart gui/<uid>/com.jonno.orchestrator-tick`.
- Its rules stay its own: it never starts a gated run without Jon's yes, and stops one
  only with TERM to run-gated. The page has no "start gated run" button; Jon asks in the
  chat, as he would now.

**Workers handing work to each other.** In Desk's chat, Jon can say "give the guv patch
to the orchestrator": the page writes it into the orchestrator's status file as an
instruction (apply `patches/<file>` from jon-comley/desk, run the checks, commit and
push), and the orchestrator does it at its next tick. That takes "apply the desk
patches" off Jon too, for guv. Only on Jon's say-so, never automatic.

lenovo1 already reaches mac1 over the LAN; the page uses lenovo1's existing ssh key to
mac1, nothing new.

## Memory: shared, by subject, loaded only when needed

Every worker (Desk, Standards, the orchestrator, and each tile's chat) shares one memory,
so what Jon tells one of them is known to all. It lives in its own repo,
`jon-comley/memory`, one folder per subject:

```
INDEX.md            the only file loaded at the start
jon/                how Jon works: writing rules, replies, what to ask first
vanround-guv/       release state, partners, builds, devices
standards/          decisions behind the documents
infra/              machines, accounts, where things run
money/              spend, renewals, what costs money
agents/<name>/      things only one worker needs
```

- **One fact per file**, with a short front matter: `name`, `description` (one line),
  `updated`. Facts link to each other by name.
- **`INDEX.md` is the directory:** one line per file, `path: description`, grouped by
  folder, and nothing else. It is all a worker reads at the start of a run or a chat.
  That keeps the starting context to a few hundred lines however big the memory grows.
- **A subject is opened only when it is in play.** A worker opens a folder's files only
  when the conversation or the job touches that subject ("the Vanround release" opens
  `vanround-guv/`, not `money/`). It never reads the whole memory, and never "just in
  case". `jon/` is the exception: always read, because it is how every reply is written.
- **Writing:** when Jon says something that should outlast the chat (a decision, a
  correction, a preference), the worker writes or updates one file, then rewrites that
  line of `INDEX.md`, in the same commit. Update before adding: no duplicates. A fact
  that turns out wrong is deleted, not left with a note.
- **Never in memory:** secrets, personal data beyond names and roles, ThingCo.
- **Access:** a write deploy key on lenovo1 (chats), the cloud routines attach it with
  push access, and the orchestrator gets its own clone on mac1.
- **On the page:** a Memory tile showing `INDEX.md` by folder; click a file to read or
  edit it.

**Seeding it:** the Vanround/Guv, infra and how-Jon-works memories from this laptop's
Claude memory (`~/.claude/projects/-home-jonno-repos/memory/`), each one checked by hand
for secrets and ThingCo before it is copied. None go across unchecked.

## Build order

1. `agentwatch` crate, pure: parse `agent-log.md`, the hub's needs-you count and
   `agents.toml`; state rules from (top log line, commits since, now). Unit tests on each,
   with Standards' real files as fixtures.
2. `agentwatch` binary: axum server on port 80, bare clones, the 15-minute fetch,
   `agents.db`, ntfy on state change. The page (`static/agents.html`, `agents.js`) compiled
   in with `include_str!`, with vitest tests as for `reviews.js`.
3. `systemd/agentwatch.service` and a `just deploy-agentwatch` recipe that builds and
   installs it on lenovo1. Port 80 is reached as `http://192.168.1.47/` from Windows,
   which has no Tailscale.
4. Make the Standards and Desk deploy keys on lenovo1 and add them to their repos,
   read-write.
5. Local workers: `kind = "local"`, the orchestrator's tile from its status file,
   Pause / Resume, Run now by `launchctl kickstart`, chat on mac1 with the tick paused.
6. Chat: `claude` on lenovo1, the per-agent resume, streaming to the page,
   `conversations/`. Add one step to both briefs: read `conversations/` since the last
   run first and keep what was agreed. (Each brief says to change it only when Jon asks;
   this plan is that ask.)
7. Memory: create `jon-comley/memory`, seed it (checked by hand), add a step to each brief
   and each chat's instructions: read `INDEX.md` and `jon/`, open other folders only
   when the subject comes up, write what should outlast the chat. The Memory tile.
8. `docs/agent-watch.md`: setup and the contract, for the next agent's brief to point at.

## Checks before calling it done

- A temporary agent pointed at a scratch repo shows `ok`. It turns `late` when its
  `every` is set to 1 minute, `broken` after a commit without a log line, and
  `unreachable` once its key is removed from the repo. One push each.
- Standards shows its real last run and needs-you count.
- With pi1 powered off, `http://lenovo1/` still loads and still fetches.
- With lenovo1 restarted, the page comes back with its history intact.

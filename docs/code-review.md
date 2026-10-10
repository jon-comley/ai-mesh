# Code reviews on the mesh

mac1 reviews your GitHub repos on a schedule or whenever you ask, answers
questions about their code, and shares the work out across the mesh's
machines like a team of agents, using local models only. Findings and answers
show on the dashboard's **Reviews** tab; reports are also Markdown files on
mac1, and an ntfy push gives the headline numbers. Nothing leaves the house.

Written by local models: treat every finding as a lead to check, not a verdict.

## Who does what

| | mac1 (runs the reviews) | pi1 (coordinator) | beelink1 and any other LLM node |
|---|---|---|---|
| Does | Schedule, fetch repos, plan chunks, keep the task list, choose which machine takes each task, merge findings, write reports, send the push | Delivers each task to the machine mac1 chose; puts home commands first, pausing review work when it has to; serves the Reviews tab | Answer review and check tasks with whatever model it has loaded |
| Keeps | `~/.ai-mesh/reviews/`: `repos/` (bare clones), `reviews.db`, `reports/<repo>/` | The latest snapshot mac1 sent (memory only) | Nothing |

Routing stays on pi1 because home commands arrive there and only pi1 sees
whether each machine is busy. Everything else about reviews is on mac1.

## Lights first

Each machine runs one model and one request at a time. Before this, a light
command queued behind a long review step on mac1 would have hit the 150 s
inference timeout. Now (`coordinator/src/work_router.rs`):

- **Home commands, voice, art and `/v1`** go to an *idle* machine with a
  control model (largest model first). If none is idle but one is busy only
  with review work, that work is **paused**: cancelled on the machine,
  reported to mac1 as `Preempted`, and put back on mac1's task list. The
  machine then **rests** from review work for 30 s so a back-and-forth voice
  conversation is not interrupted again.
- **Review work** only ever goes to a machine that is idle and not resting.
- A paused task is retried up to 5 times; a failed one up to 3.
- A non-streaming request that times out is now cancelled on the machine too,
  instead of leaving it generating for nobody.

**Which models do what** is a setting (`/api/work/roles`, the Reviews tab's
settings, or `just work-roles`), with `MESH_CONTROL_MODELS` /
`MESH_WORK_MODELS` on the coordinator as the fallback:

- *control* list empty → every model **not** listed only as work;
- *work* list empty → every model.

With both empty — the default — routing behaves exactly as before. Two
sensible setups:

```bash
# Qwen3-Coder passed the home-control bench: both machines do everything.
just work-roles "" ""

# It did not: keep it off the lights; beelink1 handles them during reviews.
just work-roles "qwen2.5:7b,qwen2.5:14b" "qwen3-coder:30b,qwen2.5:7b"
```

## Models and context

| Node | Model | `LLAMA_CTX_SIZE` | Why |
|---|---|---|---|
| mac1 | `qwen3-coder:30b` (Q5_K_M, ~22 GB) | 262144, `q8_0` cache | Feature-sized review tasks: a page, the services it calls and the companion repo code they import |
| beelink1 | `qwen2.5:7b` (4.7 GB) | 32768 | Checks with the whole file in view; the model's native maximum (more needs rope scaling, which hurts short home prompts) |

At 256k with a `q8_0` cache, mac1 uses about 34 GB: under macOS's default GPU
limit (~75% of RAM, ~48 GB) with room for the iOS simulator. Going past that
needs `sudo sysctl iogpu.wired_limit_mb=…`, which the install avoids.

New llama-server settings on the agent (`capabilities/llm/src/llama.rs`),
written to the agent's environment by `install-node-macos.sh` from the node
file:

| Variable | Effect |
|---|---|
| `LLAMA_KV_CACHE_TYPE` | `f16`, `q8_0` or `q4_0` for `--cache-type-k/-v` |
| `LLAMA_PARALLEL` | `--parallel N` (2–8) |
| `LLAMA_KV_UNIFIED` | `true` adds `--kv-unified` |

**Task sizes follow each machine.** Every node now reports its context size
(`NodeCapabilities.llm_ctx_size`, wire v13), and mac1 measures each machine's
prompt-reading speed from finished tasks. A task only goes to a machine whose
context fits it and which can read it in 15 minutes (reviews) or 3 minutes
(checks, which run on the machines that also answer the lights). From 17:00
to 23:00 review tasks are capped at 32k tokens so a pause costs less.

## Before turning it on

These need the real machines; none of them could be checked from the code.

1. **Load the model on mac1** at 256k: `just load-model mac1 qwen3-coder:30b`
   after deploying mac1 with the new node file (`just deploy-node mac1`). The
   GGUF name comes from Unsloth's repo; if the download 404s, check the file
   list there and correct `resolve_gguf`.
2. **Bench it** and record the results in `docs/model-selection.md`:
   - memory in use, and time to read 32k, 100k and 200k tokens;
   - `scripts/bench/reaper_bench_real.py` — can it also do home control? That
     decides the roles above;
   - on beelink1, time to read 4k, 12k and 30k tokens with STT running.
3. **Try the two slots.** An unknown flag stops llama-server starting, so
   `LLAMA_PARALLEL` and `LLAMA_KV_UNIFIED` stay commented out in
   `nodes/mac1.env` until this passes. On mac1 (`ssh mac1`), using the agent's
   own llama-server binary:

   ```bash
   BIN=$(sed -n 's/^LLAMA_SERVER_BIN=//p' ~/ai-mesh/agent.env)
   "$BIN" --version
   "$BIN" --help 2>&1 | grep -E -- '--kv-unified|--parallel'
   ```

   If `--kv-unified` is listed, check it really starts, on a spare port with
   any small model already downloaded (the agent's server on 8080 is not
   touched):

   ```bash
   MODEL=$(find ~/.ai-mesh/models -name '*.gguf' -size -6G | head -1)
   "$BIN" -m "$MODEL" --port 8099 --ctx-size 8192 --parallel 2 --kv-unified > /tmp/kvu.log 2>&1 &
   sleep 30; curl -s localhost:8099/health; echo; kill %1
   grep -iE "error|unknown|invalid" /tmp/kvu.log | head
   ```

   `{"status":"ok"}` with no errors means it is supported: uncomment the two
   lines and `just deploy-node mac1`.
4. **Known bugs.** Add dashboard and guv, then `just review-now dashboard` on a
   range that includes the invoice that bills the cheapest quote option
   (`JobDetailPage.tsx:219` + `jobPricing.ts:125`) and the payment reversal
   that cannot be entered (`InvoiceDetailPage.tsx:236` + guv's `money.ts:38`).
   Both should be in the report with the right file and line.
5. **Lights during a run.** While it runs, say "turn the kitchen lights off".
   It should answer in the usual time; the coordinator log shows either
   beelink1 answering or `pausing review work for a home command`.

## Setting it up

1. **Deploy keys on mac1**, one read-only key per private repo (the same
   choice as `GUV_DEPLOY_KEY` for dashboard CI: one repo, no expiry):

   ```bash
   ssh mac1
   ssh-keygen -t ed25519 -N '' -f ~/.ssh/deploy-dashboard -C 'mac1 reviews: dashboard'
   cat >> ~/.ssh/config <<'CONF'
   Host github-dashboard
       HostName github.com
       User git
       IdentityFile ~/.ssh/deploy-dashboard
       IdentitiesOnly yes
   CONF
   ```

   Add the public key on GitHub (repo → Settings → Deploy keys, **read-only**),
   then use `git@github-dashboard:jon-comley/dashboard.git` as the repo URL.
   Public repos (ai-mesh) can use `https://github.com/…`.
2. **Deploy** mac1 and beelink1 with the updated node files
   (`just deploy-node mac1`, `just deploy-node beelink1`) and the coordinator.
3. **Add repos** on the Reviews tab. For dashboard, add guv too and set
   *Imports from other repos* to `@app/ = guv:src`, so guv code it imports is
   read as context.
4. **Notifications:** paste an ntfy topic URL in the tab's settings. Use a long
   random topic name; pushes carry only counts and repo names, never code.

Only URLs of the form `https://github.com/owner/repo`,
`git@github.com:owner/repo` or `git@github-<alias>:owner/repo` are accepted,
and with `REVIEW_ALLOWED_OWNERS` set (mac1's is `jon-comley`), only those
owners. Clones are bare (no working tree), hooks are off, and repo code is
never run.

## Reviews on demand

Besides the schedule, a review can be started any time — from the tab's
**Review now** picker or `just review-now`:

| Choice | Reviews | Moves the nightly / sweep bookmark? |
|---|---|---|
| New commits | Everything since the last review | Yes |
| The next folder | The next top-level folder in turn | Yes (the sweep's) |
| A folder or file | Everything under that path, at the latest commit | No |
| A branch | What the branch changes since it left the main branch | No |

```bash
just review-now dashboard                     # new commits
just review-now dashboard sweep               # the next folder
just review-now dashboard path:src/services   # one folder or file
just review-now guv branch:feature-x          # a branch against main
```

Runs queue one at a time; the same request twice is queued once.

## Asking questions

Type a question on the tab's **Ask about the code** box, or:

```bash
just ask dashboard "where is the invoice total worked out?"
```

mac1 searches the repo for the question's words (identifiers are split too,
so `parsePrice` also finds *parse* and *price*), ranks the files — rare words
and file names count for more — and shows the best of them, as many as fit,
to the free machine with the largest context. The answer cites
`repo/path:line`, and lists the files it read. If no file mentions any of the
words, it says so without asking a model.

Questions come first: while one is waiting, a running review hands out no new
tasks, so a machine frees up within minutes rather than at the end of the run.
Like review work, a question gives way to home commands and is asked again if
paused. The tab keeps the last 20; one unanswered when mac1 restarts is marked
failed, so ask it again.

Answers come from the files shown, so a question about something spread thinly
across the repo ("how does auth work?") gets a thinner answer than one that
names the thing ("what does `requireRole` check?").

## How a run works

`capabilities/review/src/run.rs`, with the logic in the pure `codereview`
crate:

1. **Fetch** the repo and any companion repos.
2. **Choose files.** Nightly and *Run now*: files changed since the last
   reviewed commit (the last 20 commits on a first run). Weekly sweep: every
   file in the next top-level folder, in turn. Tests, generated and vendored
   code are skipped as targets.
3. **Context:** the files they import, two levels deep, including the
   companion repo's (`@app/…` → guv).
4. **Plan chunks** (`codereview::chunk`): targets that import each other or
   share a folder stay together; targets take at most 70% of a chunk and the
   rest is their imports.
5. **Hand out tasks** (`codereview::assign`): big reviews to the biggest
   context; each check to a *different* model from the one that raised the
   finding whenever such a machine exists, even if it means waiting for it.
6. **Quote check:** a finding whose quoted code is not in the file is dropped
   without asking anyone; one that is found gets its line number corrected.
7. **Check** each finding (`prompts/verify.md`): confirmed, rejected or unsure.
8. **Report:** duplicates merged, rejected ones left out, the rest by
   severity; unchecked and unsure ones in a separate *Not confirmed* section.
   Saved as `reports/<repo>/<date>-run<id>.md` and `latest.md`.

The prompts live in `codereview/prompts/` so they can be tuned without code.

**Schedule:** every minute mac1 checks whether a nightly slot (default 02:15)
or the weekly sweep (default Sunday 03:15) has come round since the last
scheduled run, in local time. A slot missed while mac1 was asleep runs once
when it wakes. A run left half-done by a restart is marked failed and queued
again.

**Findings** have a stable id (repo, path, quoted code), so one dismissed on
the tab stays dismissed when a later run reports it again.

## Wire messages (v13)

| Message | Direction | Purpose |
|---|---|---|
| `WorkInferenceRequest` | mac1 → pi1 | Run a task on the named machine and model |
| `WorkInferenceDone` | pi1 → mac1 | Finished, Preempted, Failed or NoWorker, with the output |
| `RequestWorkers` / `WorkerSnapshot` | mac1 ↔ pi1 | Machines with models: context, roles, busy, resting |
| `ReviewSnapshot` | mac1 → pi1 | The whole review state, for the tab |
| `ReviewCommand` | pi1 → mac1 | Run now (new commits, next folder, a path or a branch), ask a question, edit repos, finding status, settings, fetch report |
| `ReviewReply` | mac1 → pi1 | A report, or why a command failed |

pi1 streams each task from the worker, so the agent's 90 s non-streaming cap
does not apply; it allows 20 minutes for the first token (reading a big
prompt) and 3 minutes between tokens after that.

## Commands

```bash
just review-now dashboard                     # new commits since the last review
just review-now dashboard sweep               # the next folder of the whole repo
just review-now dashboard path:src/services   # one folder or file
just review-now guv branch:feature-x          # what a branch changes
just ask dashboard "where is VAT added?"      # a question, waits for the answer
just work-roles                               # show which models do what
```

## Settings

| Where | Setting | Default |
|---|---|---|
| Reviews tab / `REVIEW_MAX_TOKENS` | Largest review task | 100,000 tokens |
| Reviews tab / `REVIEW_EVENING_MAX_TOKENS` | Largest task 17:00–23:00 | 32,000 tokens |
| Reviews tab | ntfy topic URL | none |
| mac1 env `REVIEW_ALLOWED_OWNERS` | GitHub owners that may be added | any |
| mac1 env `REVIEW_HOME` | Where clones, database and reports live | `~/.ai-mesh/reviews` |
| Coordinator `MESH_CONTROL_MODELS` / `MESH_WORK_MODELS` | Roles fallback | empty |

## Known limits

- **Quality.** A 30B local model misses things a frontier model finds and
  invents others. The quote check and the second-model check reduce this; they
  do not remove it.
- **mac1 off means no reviews.** The tab says mac1 is offline and missed
  slots run when it is back.
- **A paused task starts again from the beginning** unless the two-slot setup
  is on. The try limit, the 30 s rest and smaller evening tasks keep a busy
  evening from stalling a run.
- **Streaming `/v1` requests are not counted as busy** when routing work (only
  non-streaming home commands are). They still pause review work in their way.
- **The schedule is mac1's clock;** pi1 only relays.

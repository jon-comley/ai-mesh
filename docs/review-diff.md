# review-diff: the code review, off the mesh

The home mesh reviews guv and the dashboard once their tests pass
(`docs/code-review.md`). `tools/review-diff/review_diff.py` does the same for a
machine that isn't on the mesh, such as the work laptop: no mac1, no pi1,
nothing sent anywhere except the AI accounts named in its config.

It uses the same prompts (`codereview/prompts/`), the same five kinds of item
(bugs, tests, refactors, improvements, ideas) and the same report: an
explanation and a diff for each item, for Claude Code to fix or drop.

## What a run does

`review_diff.py run --upto <commit>`, normally from `after-tests.sh` once the
tests have passed:

1. **What to review:** the commits since the last review, up to the one that
   passed. A first run goes back along the branch while the range holds
   at most `first_run_commits` (20), so a merge of main doesn't bring in
   hundreds. The bookmark is
   `.git/review-diff/state.json`, so it never leaves the repo and is never
   committed.
2. **The prompt:** each changed file's diff, with each changed function whole
   (`git diff -W`), the lines elsewhere that call the changed functions, and
   the whole file. A big change is split across prompts.
3. **Reviewers take turns:** the first look goes to the next reviewer in the
   config each run, and `reviews_per_run` (default 2) reviewers look at each
   change. One that is out of quota, has no key, or finds the change too big
   for its budget is skipped, and the next one is asked.
4. **Quote check:** an item whose quoted code isn't in the file is dropped.
5. **Check:** each item goes to a *different* reviewer (an API one first),
   with the whole file, and comes back confirmed, rejected or unsure. The
   checker rejects only on evidence. The finder checks its own item only when
   no other reviewer answers.
6. **Diff check:** each item's diff is tried with `git apply --check`.
7. **Report:** `.git/review-diff/reports/<date>-<commit>.md` and `latest.md`,
   with a copy in `.git/review-diff/inbox/` for Claude Code.
8. **Scorecard:** every item raised is noted in `.git/review-diff/ledger.json`.

## Setting it up on the work laptop

1. **Get the code.** ai-mesh is public:
   `git clone https://github.com/jon-comley/ai-mesh`.
2. **Check Python** is 3.9 or newer: `python --version`, or `py --version` on
   Windows. Nothing else needs installing.
3. **Write the config:**
   `python ai-mesh/tools/review-diff/review_diff.py init`. That writes
   `~/.config/review-diff/config.json` with three reviewers: Gemini, OpenAI and
   Copilot. Edit it:
   - `context`: a sentence or two on what the code is (e.g. "STM32 firmware
     for a data logger: FreeRTOS, MQTT, OTA").
   - each reviewer's `model`. `gemini-pro-latest` and `gpt-4.1` are
     placeholders; use the models the work accounts have.
   - `budget_tokens`: the most one prompt may hold for that reviewer.
4. **Keys** go in environment variables, never in the file: `GEMINI_API_KEY`
   and `OPENAI_API_KEY`, or whatever each reviewer's `key_env` names.
5. **Copilot** runs as the `copilot` command-line tool, signed in with the
   work account. It reads the prompt from `context.md` in a scratch folder,
   the way beelink1's morning review does.
6. **Hook it to the tests.** Run the tests through the wrapper:
   `ai-mesh/tools/review-diff/after-tests.sh make test`, or on Windows
   `ai-mesh\tools\review-diff\after-tests.ps1 npm test`. It reviews only when
   every test passed, on a clean committed tree; otherwise it says why not.
7. **Install the Claude Code skill:** copy `tools/review-diff/SKILL.md` to
   `~/.claude/skills/review-inbox/SKILL.md`. Then "go through the review
   inbox" has Claude verify each item, fix or drop it, run the tests, and
   record the outcome.

`review_diff.py run --dry-run` shows what would be sent, how big each prompt
is and who looks first, without sending anything.

## Copilot on another machine

A command reviewer with `"ssh_host"` runs on that Windows machine instead,
for Copilot signed in there (beelink1):

```json
{ "name": "copilot", "type": "command", "ssh_host": "beelink1", "command": ["copilot", "-p", "..."] }
```

The prompt goes over ssh on stdin, lands in `context.md` in a scratch folder
under `%TEMP%`, and the folder is removed after. ssh must log in without a
password (`BatchMode`). What is sent goes to the Copilot account signed in on
that machine.

## Adding the work Claude later

Add an entry to `reviewers`; nothing else changes:

```json
{
  "name": "claude",
  "type": "api",
  "base_url": "https://api.anthropic.com/v1",
  "model": "<the work account's Claude model>",
  "key_env": "ANTHROPIC_API_KEY",
  "budget_tokens": 150000
}
```

Anthropic's API accepts the same OpenAI-style requests at that address. Any
other OpenAI-compatible provider is added the same way. `"disabled": true`
takes a reviewer out without deleting it.

## The scorecard

```bash
python review_diff.py score                 # each reviewer's record
python review_diff.py mark <id> fixed       # or dismissed: Claude Code does this
```

The columns are the same as the Reviews tab at home: raised, made up (quoted
code that isn't in the file), rejected, confirmed, fixed and dismissed, and
the two rates. A finding raised again by the same reviewer counts once.

## What leaves the laptop

Only what goes to the reviewers in the config: the changed files, their
diffs, and the lines that call them. Check that each account is one the work
allows code to go to.

## Tests

`python3 -m unittest discover tools/review-diff` (fake reviewers and a local
stand-in API; no keys needed).

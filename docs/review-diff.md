# review-diff: the code review, off the mesh

The home mesh reviews guv and the dashboard once their tests pass
(`docs/code-review.md`). `tools/review-diff/review_diff.py` does the same for a
machine that isn't on the mesh, such as the work laptop: no mac1, no pi1,
nothing sent anywhere except the AI accounts named in its config.

It uses the same prompts (`codereview/prompts/`), the same five kinds of item
(bugs, tests, refactors, improvements, ideas) and the same report: an
explanation and a diff for each item, for Claude Code to fix or drop.

## What a run does

`review_diff.py run --upto <commit>`, normally from lenovo1's bridge once the
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

## The lenovo1 bridge: work repos, reviewed from home

The work laptop never connects out: the tailnet lets only Jon's own devices
start connections, and `tag:work` can't. So lenovo1, which reaches it, does
the reviewing:

1. **On the work laptop** the tests run through the wrapper,
   `~/ai-mesh/tools/review-diff/after-tests.sh make test`. When every test
   passes on a clean commit it moves the local `tests-passed` branch there.
   Nothing else happens on the laptop.
2. **On lenovo1** `tools/review-diff/bridge.sh` finds each repo under
   `~/repos` on the work laptop (two levels, so `ltf-bench-park/GD` too) with
   a `tests-passed` branch, fetches it into a bare mirror in `~/work-review`,
   and reviews the commits since the last review with `review_diff.py`.
   Mirrors are named after the path: `repos/ltf-bench-park/GD` is
   `ltf-bench-park-GD`.
3. **The report** is copied into that repo's `.git/review-diff/inbox` on the
   work laptop, where "go through the review inbox" has Claude Code verify,
   fix or drop each item (`SKILL.md`, copied to
   `~/.claude/skills/review-inbox/SKILL.md` there).
4. **Outcomes** come back: Claude writes `fixed` or `dismissed` to
   `.git/review-diff/marks/<id>`, and the next pass moves them into lenovo1's
   scorecard.

lenovo1's config (`~/.config/review-diff/config.json`) has Copilot on
beelink1 as its reviewer, with `"whole_files": false`: reviewers get the diff
with each changed function whole and the lines that call them, and a checker
gets 60 lines either side of the item, never a whole file.

`REVIEW_BRIDGE_HOST` (default `work-laptop`), `REVIEW_BRIDGE_ROOTS` (`repos`)
and `REVIEW_BRIDGE_HOME` (`~/work-review`) change where it looks.

## Running it on one machine

`review_diff.py` also runs where the code is, if that machine can reach the
reviewers: `review_diff.py init` writes an example config, keys go in
environment variables (`GEMINI_API_KEY`, `OPENAI_API_KEY`, or each
reviewer's `key_env`), and `review_diff.py run --upto <commit>` reviews.
`--dry-run` shows what would be sent, how big each prompt is and who looks
first, without sending anything.

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
python3 review_diff.py --repo ~/work-review/<name>.git score   # on lenovo1
python3 review_diff.py mark <id> fixed       # or a file in .git/review-diff/marks/<id>
```

The columns are the same as the Reviews tab at home: raised, made up (quoted
code that isn't in the file), rejected, confirmed, fixed and dismissed, and
the two rates. A finding raised again by the same reviewer counts once.

## What leaves the laptop

The tested commits go to lenovo1's mirror. From there, only what goes to the
reviewers in its config: with `"whole_files": false`, the diffs, the lines
that call the changed functions, and the lines around each item for the
check.

## Tests

`python3 -m unittest discover tools/review-diff` (fake reviewers and a local
stand-in API; no keys needed).

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
   `bridge.sh <name>...` reviews only those; `bridge.sh --list` lists them.
   Mirrors are named after the path: `repos/ltf-bench-park/GD` is
   `ltf-bench-park-GD`.
3. **The report** is copied into that repo's `.git/review-diff/inbox` on the
   work laptop, where "go through the review inbox" has Claude Code verify,
   fix or drop each item (`SKILL.md`, copied to
   `~/.claude/skills/review-inbox/SKILL.md` there).
4. **Outcomes** come back: Claude writes `fixed` or `dismissed` to
   `.git/review-diff/marks/<id>`, and the next pass moves them into lenovo1's
   scorecard.

lenovo1's config (`~/.config/review-diff/config.json`) has the free AIs
(Gemini, Mistral, OpenRouter) and Copilot on beelink1 taking turns at
the first look, called straight, not through pi1. Their keys are in
`~/.config/review-diff/keys.env` (mode 600), which the bridge loads. mac1's
coder looks too, and is the config's `"checker"`: it checks every item it
didn't raise before anyone else (`"check_only"` would keep it to checking),
through a tunnel the bridge opens
to mac1's `127.0.0.1:8080` for the run (`REVIEW_CODER_TUNNEL`, empty for
none). `"prompt_tokens"` sizes the prompts; a reviewer whose budget is
smaller is skipped for the first look. Groq isn't in it: its free tier's 6k
tokens a minute holds neither a review nor a check prompt. `"whole_files": false` sends diffs only.

An item whose quote isn't in the file is still judged by another reviewer,
with a note that a misquote alone is no reason to reject, and listed under
*Misquoted* with the verdict. Nothing is removed on a verdict: rejected items
are listed under *Rejected by the checker* with the reason, since a checker
can be wrong both ways, and Claude makes the final call on every item.

`REVIEW_BRIDGE_HOST` (default `work-laptop`), `REVIEW_BRIDGE_ROOTS` (`repos`)
and `REVIEW_BRIDGE_HOME` (`~/work-review`) change where it looks.

## Choosing a repo

```bash
just copilot-review guv                         # Copilot on beelink1 only
just ai-review work:little-theo-firmware        # every reviewer in the config
just copilot-review                             # lists what can be reviewed, asks
just copilot-review guv HEAD                    # up to HEAD, tested or not
```

A name is looked for in `~/repos` here, reviewed up to its `tests-passed`
branch (guv's all-tests pushes it to GitHub), and on the work laptop through
lenovo1's bridge. A name in both places is refused until it's `home:<name>`
or `work:<name>`, so work code never goes to the home reviewers by mistake.
The report is printed at the end. `tools/review-diff/review-repo.sh` does it.

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

The prompt goes over with scp as `context.md` in a scratch folder in the
home folder there, and the folder is removed after. ssh must log in without a
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

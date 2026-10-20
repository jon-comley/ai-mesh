---
name: review-inbox
description: Work through the AI code-review reports waiting for this repo (review_diff.py writes them to .git/review-diff/inbox after the tests pass). Use when asked to go through, triage or act on the review inbox or the reviewers' findings.
---

# Review inbox

`review_diff.py` (ai-mesh `tools/review-diff`) reviews each change that passed
the tests with the AI accounts in its config, and leaves a report in
`.git/review-diff/inbox/` (inside `.git`, so it is never committed). Each
item has a kind (bug, test, refactor, improvement, idea), a location, an id,
an explanation, the quoted code, who found and checked it, and usually a
diff.

The reports are leads, not verdicts. Treat every item as a claim to verify.

## For each report in the inbox, oldest first

1. Read the whole report.
2. For each bug, test, refactor and improvement, in the report's order:
   - **Verify it against the code as it is now.** Read the file and what calls
     it. If the item is wrong, already fixed, or not worth the change, drop it.
   - **To fix it,** use the diff as a starting point, not as gospel. Keep the
     change to what the item needs. For a bug, add a test that fails without
     the fix where the code can be tested.
   - **Run the repo's tests** after each fix, or after a group of small ones.
     Never commit with tests failing.
   - **Record the outcome** so the reviewers' scorecard stays true:
     `python <ai-mesh>/tools/review-diff/review_diff.py mark <id> fixed`
     or `... mark <id> dismissed`.
3. **Ideas** are for the user to choose. Don't build them. List them in your
   summary.
4. Move the report to `.git/review-diff/inbox/done/` when every item in it
   has an outcome.

## What to write back

For each item, in this shape:

- **What it was:** the item's kind, location and one-line claim.
- **What I decided:** fixed or dropped, and why, citing lines.
- **The diff applied**, for a fix, or the reason it doesn't need one.

End with the ideas from the reports, and the scorecard
(`review_diff.py score`) if the user asks how the reviewers are doing.

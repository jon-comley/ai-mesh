#!/usr/bin/env bash
# Records what became of a review finding, on mac1, for the review agent to
# apply within a minute (docs/code-review.md, Findings).
#
# Usage: review-mark <id> fixed|dismissed|open
#
# Installed on mac1 as ~/.local/bin/review-mark. It replaces writing the
# file by hand: on 23 Nov two marks went in with a character missing from
# the id, the agent skips a name that isn't a finding's, and both findings
# stayed open with nothing saying so. So the id must be 16 hex characters and
# appear in a report under ~/reviews before the file is written.
set -euo pipefail

id=${1:-}
status=${2:-}
reviews=${REVIEWS_DIR:-$HOME/reviews}

if [[ ! $id =~ ^[0-9a-f]{16}$ ]]; then
    echo "✗ '$id' is not an id: an id is 16 hex characters, as in the report" >&2
    exit 1
fi
case $status in
    fixed | dismissed | open) ;;
    *)
        echo "✗ status must be fixed, dismissed or open" >&2
        exit 1
        ;;
esac
if ! grep -rqF "(id \`$id\`)" "$reviews/inbox" "$reviews/done" 2>/dev/null; then
    echo "✗ $id is in no report under $reviews/inbox or $reviews/done: copy it from the report again" >&2
    exit 1
fi

mkdir -p "$reviews/marks"
echo "$status" >"$reviews/marks/$id"
echo "✓ $id marked $status"

#!/usr/bin/env bash
# lenovo1: review the work laptop's tested commits without the work laptop
# ever connecting out (docs/review-diff.md, "The lenovo1 bridge").
#
# Every repo under REVIEW_BRIDGE_ROOTS on the work laptop with a tests-passed
# branch (after-tests.sh moves it) is fetched into a bare mirror here, its new
# commits reviewed by review_diff.py with the reviewers in lenovo1's config, and
# the report copied into that repo's .git/review-diff/inbox for Claude Code.
# Claude's outcomes (.git/review-diff/marks/<id>) come back the same way.
# Usage: tools/review-diff/bridge.sh                 every repo with new tested commits
#        tools/review-diff/bridge.sh little-theo-firmware ltf-bench-park-GD
#        tools/review-diff/bridge.sh --list          the repos it can review
#        REVIEW_ONLY=copilot tools/review-diff/bridge.sh st-bsp   one reviewer only
set -uo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
HOST=${REVIEW_BRIDGE_HOST:-work-laptop}
ROOTS=${REVIEW_BRIDGE_ROOTS:-repos}
MIRRORS=${REVIEW_BRIDGE_HOME:-$HOME/work-review}
SSH=(ssh -o BatchMode=yes -o ConnectTimeout=15)
LIST=0
if [ "${1:-}" = --list ]; then LIST=1; shift; fi
WANT=("$@")
mkdir -p "$MIRRORS"

# Repo paths relative to the work laptop's home, one per line
globs=""
for r in $ROOTS; do globs+="$r/*/ $r/*/*/ "; done
repos=$("${SSH[@]}" "$HOST" "cd ~ && for d in $globs; do
  [ -e \"\$d.git\" ] && git -C \"\$d\" rev-parse -q --verify refs/heads/tests-passed >/dev/null && echo \"\${d%/}\"
done; true") || { echo "can't reach $HOST"; exit 1; }

# repos/ltf-bench-park/GD is ltf-bench-park-GD; repos/st-bsp is st-bsp
name_of() { local rel=${1#*/}; echo "${rel//\//-}"; }

if [ $LIST = 1 ]; then
  while read -r path; do [ -n "$path" ] && echo "$(name_of "$path")  ($path)"; done <<< "$repos"
  exit 0
fi
for w in "${WANT[@]}"; do
  found=0
  while read -r path; do [ "$(name_of "$path")" = "$w" ] && found=1; done <<< "$repos"
  if [ $found = 0 ]; then
    echo "no work-laptop repo '$w' with a tests-passed branch; these have one:"
    while read -r path; do [ -n "$path" ] && echo "  $(name_of "$path")"; done <<< "$repos"
    exit 1
  fi
done

# Copilot calls take minutes; a second run started meanwhile leaves it be
exec 9>"$MIRRORS/.lock"
flock -n 9 || { echo "another bridge run is going"; exit 0; }

while read -r path; do
  [ -n "$path" ] || continue
  name=$(name_of "$path")
  if [ ${#WANT[@]} -gt 0 ] && [[ ! " ${WANT[*]} " == *" $name "* ]]; then continue; fi
  m="$MIRRORS/$name.git"
  [ -d "$m" ] || git init -q --bare "$m"
  echo "== $name ($HOST:$path)"
  if ! git -C "$m" fetch -q "$HOST:$path" "+refs/heads/tests-passed:refs/heads/tests-passed"; then
    echo "   fetch failed"; continue
  fi
  # Claude's marks first, so the scorecard is up to date
  mkdir -p "$m/review-diff/marks"
  for id in $("${SSH[@]}" "$HOST" "ls '$path/.git/review-diff/marks' 2>/dev/null"); do
    scp -q -o BatchMode=yes "$HOST:$path/.git/review-diff/marks/$id" "$m/review-diff/marks/" \
      && "${SSH[@]}" "$HOST" "rm -f '$path/.git/review-diff/marks/$id'"
  done
  python3 -u "$HERE/review_diff.py" --repo "$m" run --tested --upto tests-passed \
    ${REVIEW_ONLY:+--only "$REVIEW_ONLY"}
  # Reports waiting here go over; one that can't stays for the next run
  mkdir -p "$m/review-diff/sent"
  for f in "$m/review-diff/inbox"/*.md; do
    [ -f "$f" ] || continue
    "${SSH[@]}" "$HOST" "mkdir -p '$path/.git/review-diff/inbox'" \
      && scp -q -o BatchMode=yes "$f" "$HOST:$path/.git/review-diff/inbox/" \
      && mv "$f" "$m/review-diff/sent/" && echo "   report sent: $(basename "$f")"
  done
done <<< "$repos"

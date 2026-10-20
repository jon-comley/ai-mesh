#!/usr/bin/env bash
# just copilot-review / just ai-review: review a repo's tested commits, at home
# or on the work laptop, chosen by name (docs/review-diff.md, "Choosing a repo").
#
# A name is looked for in ~/repos here (reviewed up to its tests-passed branch,
# which guv's all-tests pushes to GitHub) and on the work laptop through
# lenovo1's bridge. Found in both, it asks for home:<name> or work:<name>
# rather than guess, so work code never goes to the home reviewers by mistake.
# No name: lists what can be reviewed and asks.
#
# Usage: review-repo.sh <reviewer|all> [name] [commit]
#   review-repo.sh copilot guv            Copilot only, up to guv's tests-passed
#   review-repo.sh all work:st-bsp        every reviewer in lenovo1's config
#   review-repo.sh copilot guv HEAD       up to HEAD, tests or not
set -uo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
ONLY=${1:?usage: review-repo.sh <reviewer|all> [name] [commit]}
[ "$ONLY" = all ] && ONLY=""
NAME=${2:-}
UPTO=${3:-}
ROOT=${REVIEW_HOME_ROOT:-$HOME/repos}
BRIDGE=${REVIEW_BRIDGE:-lenovo1}
BRIDGE_SH='~/ai-mesh/tools/review-diff/bridge.sh'

# The commit a home repo's review goes up to: its tests-passed branch, from
# GitHub when it is there, else a local one
tested_ref() {
  local d=$1
  if git -C "$d" rev-parse -q --verify refs/remotes/origin/tests-passed >/dev/null; then
    echo origin/tests-passed
  elif git -C "$d" rev-parse -q --verify refs/heads/tests-passed >/dev/null; then
    echo tests-passed
  fi
}

home_list() {
  for d in "$ROOT"/*/; do
    [ -e "$d.git" ] || continue
    local ref; ref=$(tested_ref "$d")
    [ -n "$ref" ] || continue
    local name; name=$(basename "$d")
    local last waiting
    last=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get("last",""))' \
           "$d.git/review-diff/state.json" 2>/dev/null)
    if [ -n "$last" ] && git -C "$d" merge-base --is-ancestor "$last" "$ref" 2>/dev/null; then
      waiting="$(git -C "$d" rev-list --count "$last..$ref") new commits"
    else
      waiting="first review"
    fi
    echo "home:$name  ($waiting)"
  done
}

work_list() {
  ssh -o BatchMode=yes -o ConnectTimeout=15 "$BRIDGE" "$BRIDGE_SH --list" 2>/dev/null \
    | awk '{print "work:" $1 "  " $2}'
}

if [ -z "$NAME" ]; then
  mapfile -t choices < <(home_list; work_list)
  if [ ${#choices[@]} -eq 0 ]; then
    echo "nothing to review: no repo here or on the work laptop has a tests-passed branch"
    exit 1
  fi
  for i in "${!choices[@]}"; do printf '%2d) %s\n' $((i + 1)) "${choices[$i]}"; done
  read -rp "Review which? " n
  [[ "$n" =~ ^[0-9]+$ ]] && [ "$n" -ge 1 ] && [ "$n" -le ${#choices[@]} ] || { echo "no such choice"; exit 1; }
  NAME=${choices[$((n - 1))]%% *}
fi

case "$NAME" in
  home:*) where=home; NAME=${NAME#home:} ;;
  work:*) where=work; NAME=${NAME#work:} ;;
  *)
    here=0; there=0
    [ -e "$ROOT/$NAME/.git" ] && here=1
    work_list | grep -q "^work:$NAME " && there=1
    if [ $here = 1 ] && [ $there = 1 ]; then
      echo "$NAME is here and on the work laptop: say home:$NAME or work:$NAME"; exit 1
    elif [ $there = 1 ]; then where=work
    elif [ $here = 1 ]; then where=home
    else
      echo "no repo $NAME in $ROOT or on the work laptop with a tests-passed branch"; exit 1
    fi
    ;;
esac

if [ $where = work ]; then
  [ -z "$UPTO" ] || { echo "a work repo is reviewed up to its tests-passed branch only"; exit 1; }
  out=$(ssh -o BatchMode=yes "$BRIDGE" "REVIEW_ONLY='$ONLY' $BRIDGE_SH '$NAME'" | tee /dev/stderr) || exit 1
  # The report as it went to the work laptop's inbox
  report=$(sed -n 's/^report: //p' <<< "$out" | tail -1)
  [ -z "$report" ] || { echo; ssh -o BatchMode=yes "$BRIDGE" "cat '$report'"; }
  exit 0
fi

d="$ROOT/$NAME"
git -C "$d" fetch -q origin tests-passed 2>/dev/null
if [ -z "$UPTO" ]; then
  UPTO=$(tested_ref "$d")
  if [ -z "$UPTO" ]; then
    echo "$NAME has no tests-passed branch yet: pass its tests first, or name a commit (just copilot-review $NAME HEAD)"
    exit 1
  fi
  tested=(--tested)
else
  tested=()
fi
out=$(python3 -u "$HERE/review_diff.py" --repo "$d" run "${tested[@]}" --upto "$UPTO" \
      ${ONLY:+--only "$ONLY"} | tee /dev/stderr) || exit 1
report=$(sed -n 's/^report: //p' <<< "$out" | tail -1)
[ -z "$report" ] || { echo; cat "$report"; }

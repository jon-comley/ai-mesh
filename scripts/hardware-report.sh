#!/bin/bash
# Print hardware details for every node currently registered in the mesh.
# Usage: ./scripts/hardware-report.sh [coordinator-host:port]
#        just hardware-report

set -e

COORDINATOR="${1:-127.0.0.1:9000}"
CLI="cargo run -q -p cli --"

divider() { printf '%.0s─' {1..55}; echo; }
thick()   { printf '%.0s═' {1..55}; echo; }

thick
echo "  ai-mesh Hardware Report"
echo "  Coordinator: $COORDINATOR"
thick
echo ""

NODE_IDS=$($CLI --coordinator "$COORDINATOR" nodes 2>/dev/null \
    | awk -F'|' '{gsub(/ /,"",$2); if($2~/^[0-9a-f-]{36}$/) print $2}')

if [ -z "$NODE_IDS" ]; then
    echo "  No nodes registered. Is the coordinator running at $COORDINATOR?"
    exit 1
fi

COUNT=$(echo "$NODE_IDS" | wc -l | tr -d ' ')
echo "  $COUNT node(s) found"
echo ""

i=1
for id in $NODE_IDS; do
    divider
    printf "  Node %d of %d\n" "$i" "$COUNT"
    divider
    $CLI --coordinator "$COORDINATOR" info "$id"
    echo ""
    i=$((i + 1))
done

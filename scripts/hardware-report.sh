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

# Pre-req: verify coordinator is responding via the CLI.
# A raw TCP check against the LAN IP is unreliable — the Windows portproxy
# accepts the handshake even when nothing is listening behind it.
PORT="${COORDINATOR#*:}"
if ! timeout 3 bash -c "echo > /dev/tcp/127.0.0.1/$PORT" 2>/dev/null; then
    echo "  ERROR: Coordinator not running (checked 127.0.0.1:$PORT)"
    echo ""
    echo "  Start it first:"
    echo "    just run-coordinator   (coordinator only)"
    echo "    just dev               (full cluster)"
    exit 1
fi

fetch_nodes() {
    $CLI --coordinator "$COORDINATOR" nodes 2>/dev/null \
        | awk -F'|' '{gsub(/ /,"",$2); if($2~/^[0-9a-f-]{36}$/) print $2}'
}

# Scan for the full window, printing each node's info as it registers.
SEEN_IDS=""
COUNT=0
SCAN_SECS=20
END=$((SECONDS + SCAN_SECS))

echo "  Scanning for nodes (${SCAN_SECS}s)..."

while [ $SECONDS -lt $END ]; do
    CURRENT_IDS=$(fetch_nodes)

    for id in $CURRENT_IDS; do
        if ! echo "$SEEN_IDS" | grep -qF "$id"; then
            SEEN_IDS="$SEEN_IDS $id"
            COUNT=$((COUNT + 1))
            printf "\r%55s\r" ""   # clear the countdown line
            divider
            printf "  Node registered: %s\n" "$id"
            divider
            # Brief pause so the agent's hardware/capabilities messages
            # arrive before we query — heartbeat registers first, hardware
            # and capabilities follow within ~1s.
            sleep 2
            $CLI --coordinator "$COORDINATOR" info "$id"
            echo ""
        fi
    done

    REMAINING=$((END - SECONDS))
    printf "  Waiting for more nodes... (%ds remaining)\r" "$REMAINING"
    sleep 1
done

printf "\r%55s\r" ""   # clear the countdown line

if [ $COUNT -eq 0 ]; then
    echo "  No nodes registered after ${SCAN_SECS}s. Check: just logs"
    exit 1
fi

thick
printf "  %d node(s) reported.\n" "$COUNT"
thick

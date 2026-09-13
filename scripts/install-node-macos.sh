#!/usr/bin/env bash
# Provision or update an ai-mesh agent on a macOS (Apple Silicon) node.
# Run ON the node, from the synced source at ~/ai-mesh-src — `just deploy-node`
# and `just update-node` do both. Safe to re-run: it rebuilds, reinstalls and
# restarts, and it keeps any credentials `just set-fingerprint` has pushed.
#
#     install-node-macos.sh <role> <features> [coordinator_ip] [ctx_size] [default_model]
#
# **Why the agent is built here.** Cross-compiling a macOS binary from Linux
# needs Apple's SDK (osxcross), which the controller doesn't have. The node has
# Xcode, so a native `cargo build` is the simple, reliable path.
#
# **Why cron, not launchd — found 2026-09-13.** macOS's Local Network privacy
# blocks a LaunchAgent from reaching LAN addresses until someone clicks Allow
# on the Mac itself: the agent logged "No route to host (os error 65)" to the
# coordinator while the same binary started from ssh connected at once. Cron
# jobs are not subject to that prompt (tested: cron reached pi1:9000), so a
# one-minute cron watchdog keeps the agent running and restarts it after a
# reboot, with nobody at the machine. ~/ai-mesh/agentctl.sh starts, stops and
# restarts it; the justfile's macOS branches call that.
#
# **And a loopback relay for the coordinator connection.** Starting from cron
# was not enough on its own: the unsigned agent binary is still refused LAN
# access, while Apple's /usr/bin/python3 is not. So when the node pins a
# coordinator address, the agent dials 127.0.0.1:19000 and scripts/mesh-relay.py
# (run by agentctl.sh with /usr/bin/python3) forwards to the real coordinator.
# TLS still runs agent-to-coordinator; the agent pins the cert fingerprint.
#
# **Nothing here needs sudo.** Rust, llama.cpp and the agent all live under $HOME.
set -euo pipefail

ROLE="${1:-compute}"
FEATURES="${2:-llm}"
COORDINATOR_IP="${3:-}"
CTX_SIZE="${4:-8192}"
DEFAULT_MODEL="${5:-}"

# Matches beelink1's server build, so benches on both nodes compare like for like.
LLAMA_VERSION="b9444"
LABEL="ai-mesh.agent"
BASE="$HOME/ai-mesh"
SRC="$HOME/ai-mesh-src"
ENV_FILE="$BASE/agent.env"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
mkdir -p "$BASE/logs" "$HOME/.ai-mesh/models" "$HOME/Library/LaunchAgents"

echo ">>> Rust toolchain..."
if [ ! -x "$HOME/.cargo/bin/cargo" ]; then
    curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --no-modify-path
fi
# shellcheck disable=SC1091
. "$HOME/.cargo/env"
rustc --version

echo ">>> llama.cpp ${LLAMA_VERSION}..."
LLAMA_DIR="$HOME/.ai-mesh/llama.cpp-${LLAMA_VERSION}"
LLAMA_BIN="$(find "$LLAMA_DIR" -name llama-server -type f 2>/dev/null | head -1 || true)"
if [ -z "$LLAMA_BIN" ]; then
    mkdir -p "$LLAMA_DIR"
    curl -fsSL -o "$LLAMA_DIR/llama.tgz" \
        "https://github.com/ggml-org/llama.cpp/releases/download/${LLAMA_VERSION}/llama-${LLAMA_VERSION}-bin-macos-arm64.tar.gz"
    tar -xzf "$LLAMA_DIR/llama.tgz" -C "$LLAMA_DIR" && rm "$LLAMA_DIR/llama.tgz"
    # A downloaded binary carries the quarantine flag and Gatekeeper would
    # refuse to run it unattended.
    xattr -dr com.apple.quarantine "$LLAMA_DIR" 2>/dev/null || true
    LLAMA_BIN="$(find "$LLAMA_DIR" -name llama-server -type f | head -1)"
fi
"$LLAMA_BIN" --version 2>&1 | head -1

echo ">>> Building agent (features: ${FEATURES})..."
cd "$SRC"
cargo build --release -p agent --features "$FEATURES"

echo ">>> Writing ${ENV_FILE}..."
# Keep credentials pushed by `just set-fingerprint`; rewrite everything else.
KEEP="$(grep -E '^(MESH_TLS_FINGERPRINT|MESH_AUTH_TOKEN|MESH_AUTH_TOKEN_NEXT)=' "$ENV_FILE" 2>/dev/null || true)"
{
    echo "# Managed by scripts/install-node-macos.sh; credentials by just set-fingerprint."
    echo "AGENT_ROLE=${ROLE}"
    echo "LLAMA_MODEL_DIR=$HOME/.ai-mesh/models"
    echo "LLAMA_SERVER_BIN=${LLAMA_BIN}"
    # Apple Silicon memory is unified: all layers on the GPU is the whole point.
    echo "LLAMA_GPU_LAYERS=99"
    echo "LLAMA_CTX_SIZE=${CTX_SIZE}"
    [ -n "$DEFAULT_MODEL" ] && echo "DEFAULT_MODEL=${DEFAULT_MODEL}"
    if [ -n "$COORDINATOR_IP" ]; then
        # The agent dials the loopback relay; RELAY_TARGET is the real coordinator.
        echo "COORDINATOR_IP=127.0.0.1"
        echo "COORDINATOR_PORT=19000"
        echo "RELAY_TARGET=${COORDINATOR_IP}:9000"
    fi
    [ -n "$KEEP" ] && echo "$KEEP"
} > "$ENV_FILE.new"
mv "$ENV_FILE.new" "$ENV_FILE"
chmod 600 "$ENV_FILE"

cat > "$BASE/run-agent.sh" <<'RUN'
#!/usr/bin/env bash
# agentctl.sh starts this, often from cron's bare environment; load the agent's own settings.
set -a
# shellcheck disable=SC1091
. "$HOME/ai-mesh/agent.env"
set +a
export PATH="/usr/bin:/bin:/usr/sbin:/sbin"
exec "$HOME/ai-mesh/agent"
RUN
chmod 755 "$BASE/run-agent.sh"

cat > "$BASE/agentctl.sh" <<'CTL'
#!/usr/bin/env bash
# start | stop | restart | status for the ai-mesh agent on macOS.
set -u
BASE="$HOME/ai-mesh"
running() { pgrep -f "^$BASE/agent\$" >/dev/null; }
relay_running() { pgrep -f "$BASE/mesh-relay.py" >/dev/null; }
start() {
    # The relay first: without it the agent can't reach the coordinator.
    target="$(sed -n 's/^RELAY_TARGET=//p' "$BASE/agent.env" | head -1)"
    if [ -n "$target" ] && ! relay_running; then
        nohup /usr/bin/python3 "$BASE/mesh-relay.py" 127.0.0.1:19000 "$target" >> "$BASE/logs/relay.log" 2>&1 < /dev/null &
        disown 2>/dev/null || true
        sleep 1
    fi
    running && return 0
    nohup /bin/bash "$BASE/run-agent.sh" >> "$BASE/logs/agent.log" 2>&1 < /dev/null &
    disown 2>/dev/null || true
}
stop() {
    pkill -f "^$BASE/agent\$" 2>/dev/null || true
    # The agent owns its llama-server (always port 8080, LLAMA_PORT in llama.rs);
    # don't leave one holding memory. Match the port so a bench server on
    # another port survives an agent restart.
    pkill -f "llama-server .*--port 8080" 2>/dev/null || true
    pkill -f "$BASE/mesh-relay.py" 2>/dev/null || true
    for _ in 1 2 3 4 5 6 7 8 9 10; do running || break; sleep 1; done
}
case "${1:-status}" in
    start) start ;;
    stop) stop ;;
    restart) stop; start ;;
    status) if running; then echo running; else echo stopped; fi
            if relay_running; then echo "relay running"; fi ;;
    *) echo "usage: $0 start|stop|restart|status" >&2; exit 2 ;;
esac
CTL
chmod 755 "$BASE/agentctl.sh"
install -m 755 "$SRC/scripts/mesh-relay.py" "$BASE/mesh-relay.py"

echo ">>> Removing any LaunchAgent from an earlier install (it can't reach the LAN)..."
launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
rm -f "$PLIST"

echo ">>> Installing the cron watchdog..."
WATCH="* * * * * $BASE/agentctl.sh start # ai-mesh-agent-watchdog"
# `|| true` on both: with pipefail, an empty crontab (crontab -l exits 1) or no
# lines left after grep -v would otherwise abort the install here.
( { crontab -l 2>/dev/null || true; } | { grep -v 'ai-mesh-agent-watchdog' || true; }; echo "$WATCH" ) | crontab -

echo ">>> Installing the new binary and restarting..."
"$BASE/agentctl.sh" stop
install -m 755 "$SRC/target/release/agent" "$BASE/agent"
"$BASE/agentctl.sh" start
sleep 3
"$BASE/agentctl.sh" status
echo ">>> Agent installed. Logs: ${BASE}/logs/agent.log"

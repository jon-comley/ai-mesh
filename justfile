coordinator_ip   := "192.168.1.12"
coordinator_port := "9000"

default: build

install-hooks:
    bash scripts/install-hooks.sh

# Dump hardware + capability summary for every registered node.
# Starts the coordinator if not running, resets the registry (removes stale
# entries), starts remote agents, then streams each node as it registers.
# Usage: just hardware-report
hardware-report: update-portproxy
    #!/usr/bin/env bash
    set -e
    COORD="{{coordinator_ip}}:{{coordinator_port}}"
    COORD_PID=""

    # Check 127.0.0.1, not the LAN IP — portproxy accepts TCP even when nothing is behind it.
    if ! timeout 2 bash -c "echo > /dev/tcp/127.0.0.1/{{coordinator_port}}" 2>/dev/null; then
        echo ">>> Coordinator not running — starting in background..."
        MDNS_ADVERTISE_IP={{coordinator_ip}} cargo run -q -p coordinator \
            > /tmp/mesh-coordinator.log 2>&1 &
        COORD_PID=$!
        trap '[ -n "$COORD_PID" ] && kill "$COORD_PID" 2>/dev/null || true' EXIT

        echo ">>> Waiting for coordinator to accept connections..."
        for i in $(seq 1 30); do
            sleep 1
            if timeout 1 bash -c "echo > /dev/tcp/{{coordinator_ip}}/{{coordinator_port}}" 2>/dev/null; then
                echo ">>> Coordinator ready."
                break
            fi
            if [ "$i" -eq 30 ]; then
                echo ">>> ERROR: Coordinator did not start in time. Check: /tmp/mesh-coordinator.log"
                exit 1
            fi
        done
    else
        echo ">>> Coordinator already running at $COORD"
    fi

    cargo run -q -p cli -- reset-registry > /dev/null || true
    just start-agents
    bash scripts/hardware-report.sh "$COORD"

build:
    cargo build

test:
    cargo test

lint:
    cargo fmt --all
    cargo clippy --all-targets --all-features -- -D warnings

run-agent:
    cargo run -p agent

run-coordinator: update-portproxy
    MDNS_ADVERTISE_IP={{coordinator_ip}} cargo run -p coordinator

run-controller:
    AGENT_ROLE=controller cargo run -p agent

reset:
    #!/usr/bin/env bash
    set -e
    pkill -f "target/debug/coordinator" || true
    sleep 0.3
    cargo run -p coordinator > /tmp/mesh-coordinator.log 2>&1 &
    COORD_PID=$!
    trap 'kill $COORD_PID 2>/dev/null || true' EXIT
    sleep 1
    cargo run -p cli -- reset-registry

# ── Local sanity (coordinator + controller only) ──────────────────────────────

sanity:
    #!/usr/bin/env bash
    set -e
    trap 'kill $COORD_PID $AGENT_PID 2>/dev/null || true' EXIT
    echo "=== Starting coordinator ==="
    cargo run -p coordinator > /tmp/mesh-coordinator.log 2>&1 &
    COORD_PID=$!
    sleep 1

    echo "=== Starting controller agent ==="
    AGENT_ROLE=controller cargo run -p agent > /tmp/mesh-agent.log 2>&1 &
    AGENT_PID=$!
    sleep 1

    echo "=== Checking node list ==="
    cargo run -p cli -- nodes || true

    echo "=== Coordinator log tail ==="
    tail -n 20 /tmp/mesh-coordinator.log

    echo "=== Agent log tail ==="
    tail -n 20 /tmp/mesh-agent.log

    echo "=== Sanity check complete ==="

# ── Generic node management ───────────────────────────────────────────────────
# Node config lives in nodes/<name>.env  (NODE_HOST, NODE_USER, NODE_OS, NODE_ROLE)

# First-time provision or re-provision a node.
# Usage: just deploy-node pi1
#        just deploy-node beelink1
deploy-node node:
    #!/usr/bin/env bash
    set -e
    source nodes/{{node}}.env

    case "$NODE_OS" in
      linux)
        echo ">>> Building Linux ARM64 agent..."
        cargo build --release --target aarch64-unknown-linux-gnu -p agent

        echo ">>> Checking Ollama on ${NODE_HOST}..."
        ssh -t ${NODE_USER}@${NODE_HOST} \
            "command -v ollama >/dev/null 2>&1 || (echo 'Installing Ollama...' && curl -fsSL https://ollama.com/install.sh | sh)"

        echo ">>> Uploading agent binary..."
        ssh ${NODE_USER}@${NODE_HOST} "sudo systemctl stop ai-mesh-agent 2>/dev/null || true"
        scp target/aarch64-unknown-linux-gnu/release/agent ${NODE_USER}@${NODE_HOST}:/home/${NODE_USER}/agent

        echo ">>> Uploading install script..."
        scp scripts/install-node-linux.sh ${NODE_USER}@${NODE_HOST}:/tmp/install-node.sh
        ssh -t ${NODE_USER}@${NODE_HOST} \
            "chmod +x /tmp/install-node.sh && sudo /tmp/install-node.sh {{coordinator_ip}} ${NODE_ROLE} ${NODE_USER}"
        ;;

      windows)
        echo ">>> Building Windows x86_64 agent..."
        cargo build --release -p agent --target x86_64-pc-windows-gnu

        WIN_PATH="C:\\Users\\${NODE_USER}\\ai-mesh"
        echo ">>> Creating ${WIN_PATH} on ${NODE_HOST}..."
        ssh ${NODE_USER}@${NODE_HOST} \
            "powershell -Command \"if (-not (Test-Path '${WIN_PATH}')) { New-Item -ItemType Directory -Path '${WIN_PATH}' | Out-Null }\""

        echo ">>> Uploading agent.exe (via temp file to avoid lock)..."
        scp target/x86_64-pc-windows-gnu/release/agent.exe \
            ${NODE_USER}@${NODE_HOST}:"${WIN_PATH}\\agent_next.exe"

        echo ">>> Uploading install script..."
        scp scripts/install-node-windows.ps1 \
            ${NODE_USER}@${NODE_HOST}:"${WIN_PATH}\\install-node-windows.ps1"

        echo ">>> Stopping service, swapping binary, provisioning..."
        ssh ${NODE_USER}@${NODE_HOST} "powershell -ExecutionPolicy Bypass -Command \"\
            sc.exe stop ai-mesh-agent 2>&1 | Out-Null;\
            Start-Sleep 2;\
            Get-Process agent -ErrorAction SilentlyContinue | Stop-Process -Force;\
            Get-Process nssm  -ErrorAction SilentlyContinue | Stop-Process -Force;\
            Start-Sleep 2;\
            Move-Item -Force '${WIN_PATH}\\agent_next.exe' '${WIN_PATH}\\agent.exe';\
            & '${WIN_PATH}\\install-node-windows.ps1' -CoordinatorIp '{{coordinator_ip}}' -Role '${NODE_ROLE}'\
        \""
        ;;

      *)
        echo "Unknown NODE_OS: $NODE_OS (expected linux or windows)"
        exit 1
        ;;
    esac
    echo ">>> Node {{node}} provisioned."

# OTA update: rebuild agent, upload, restart — no reprovisioning.
# Usage: just update-node pi1
update-node node:
    #!/usr/bin/env bash
    set -e
    source nodes/{{node}}.env

    case "$NODE_OS" in
      linux)
        cargo build --release --target aarch64-unknown-linux-gnu -p agent
        echo ">>> Uploading updated agent to ${NODE_HOST}..."
        ssh ${NODE_USER}@${NODE_HOST} "sudo systemctl stop ai-mesh-agent"
        scp target/aarch64-unknown-linux-gnu/release/agent ${NODE_USER}@${NODE_HOST}:/home/${NODE_USER}/agent
        ssh ${NODE_USER}@${NODE_HOST} "sudo systemctl start ai-mesh-agent"
        ;;

      windows)
        cargo build --release -p agent --target x86_64-pc-windows-gnu
        WIN_PATH="C:\\Users\\${NODE_USER}\\ai-mesh"
        echo ">>> Uploading updated agent.exe to ${NODE_HOST}..."
        scp target/x86_64-pc-windows-gnu/release/agent.exe \
            ${NODE_USER}@${NODE_HOST}:"${WIN_PATH}\\agent_next.exe"
        ssh ${NODE_USER}@${NODE_HOST} "powershell -Command \"\
            sc.exe stop ai-mesh-agent 2>&1 | Out-Null;\
            Start-Sleep 2;\
            Get-Process agent -ErrorAction SilentlyContinue | Stop-Process -Force;\
            Get-Process nssm  -ErrorAction SilentlyContinue | Stop-Process -Force;\
            Start-Sleep 2;\
            Move-Item -Force '${WIN_PATH}\\agent_next.exe' '${WIN_PATH}\\agent.exe';\
            sc.exe start ai-mesh-agent 2>&1 | Out-Null;\
            exit 0\
        \""
        ;;
    esac
    echo ">>> Node {{node}} updated."

# Remove the ai-mesh-agent service from a node.
# Usage: just uninstall-node pi1
uninstall-node node:
    #!/usr/bin/env bash
    set -e
    source nodes/{{node}}.env

    case "$NODE_OS" in
      linux)
        echo ">>> Uninstalling ai-mesh-agent on ${NODE_HOST}..."
        scp scripts/uninstall-node-linux.sh ${NODE_USER}@${NODE_HOST}:/tmp/uninstall-node.sh
        ssh -t ${NODE_USER}@${NODE_HOST} \
            "chmod +x /tmp/uninstall-node.sh && sudo /tmp/uninstall-node.sh"
        ;;

      windows)
        WIN_PATH="C:\\Users\\${NODE_USER}\\ai-mesh"
        echo ">>> Uninstalling ai-mesh-agent on ${NODE_HOST}..."
        scp scripts/uninstall-node-windows.ps1 \
            ${NODE_USER}@${NODE_HOST}:"${WIN_PATH}\\uninstall-node-windows.ps1"
        ssh ${NODE_USER}@${NODE_HOST} \
            "powershell -ExecutionPolicy Bypass -Command \"& '${WIN_PATH}\\uninstall-node-windows.ps1'\""
        ;;
    esac
    echo ">>> Node {{node}} uninstalled."

# Tail live logs from a node.
# Usage: just logs-node pi1
logs-node node:
    #!/usr/bin/env bash
    source nodes/{{node}}.env

    case "$NODE_OS" in
      linux)
        ssh ${NODE_USER}@${NODE_HOST} "journalctl -u ai-mesh-agent -f --no-pager"
        ;;
      windows)
        WIN_PATH="C:\\Users\\${NODE_USER}\\ai-mesh"
        ssh ${NODE_USER}@${NODE_HOST} \
            "powershell -Command \"Get-Content '${WIN_PATH}\\logs\\agent.log' -Tail 20 -Wait\""
        ;;
    esac

# Check service state on a node and show the node table.
# Usage: just sanity-node pi1
sanity-node node:
    #!/usr/bin/env bash
    set -e
    source nodes/{{node}}.env

    echo ">>> Checking ai-mesh-agent on ${NODE_HOST} (${NODE_OS})..."
    case "$NODE_OS" in
      linux)
        ssh ${NODE_USER}@${NODE_HOST} \
            "systemctl is-active ai-mesh-agent && echo 'Service: RUNNING' || echo 'Service: NOT RUNNING'"
        ;;
      windows)
        ssh ${NODE_USER}@${NODE_HOST} \
            "powershell -Command \"(Get-Service -Name ai-mesh-agent).Status\""
        ;;
    esac

    echo ">>> Node table:"
    cargo run -p cli -- nodes

# ── Portproxy ─────────────────────────────────────────────────────────────────

# Refresh the Windows portproxy rule to point at the current WSL2 IP.
# WSL2 assigns a new IP on each restart; the portproxy goes stale without this.
# No-op if the rule is already correct.
update-portproxy:
    #!/usr/bin/env bash
    set -e
    WSL_IP=$(ip addr show eth0 | grep 'inet ' | awk '{print $2}' | cut -d/ -f1)
    CURRENT=$(netsh.exe interface portproxy show all | awk '/9000/{print $3}' | head -1)
    if [ "$CURRENT" = "$WSL_IP" ]; then
        echo ">>> Portproxy OK (0.0.0.0:9000 → ${WSL_IP}:9000)"
        exit 0
    fi
    echo ">>> WSL IP changed: ${CURRENT:-none} → ${WSL_IP} — updating (UAC prompt will appear)..."
    powershell.exe -Command "Start-Process powershell -ArgumentList \"-NoProfile -Command netsh interface portproxy delete v4tov4 listenport=9000 listenaddress=0.0.0.0; netsh interface portproxy add v4tov4 listenport=9000 listenaddress=0.0.0.0 connectport=9000 connectaddress=${WSL_IP}\" -Verb RunAs -Wait"
    echo ">>> Portproxy updated: 0.0.0.0:9000 → ${WSL_IP}:9000"

# ── Full cluster operations ───────────────────────────────────────────────────

# Start the ai-mesh-agent service on all remote nodes without touching the local coordinator.
# Safe to call when agents are already running (systemctl start is idempotent).
start-agents:
    #!/usr/bin/env bash
    for f in nodes/*.env; do
        source "$f"
        NODE_NAME=$(basename "$f" .env)
        case "$NODE_OS" in
          linux)
            echo ">>> Starting agent on ${NODE_NAME} (${NODE_HOST})..."
            ssh -o ConnectTimeout=5 ${NODE_USER}@${NODE_HOST} \
                "sudo systemctl start ai-mesh-agent" 2>/dev/null \
                || echo ">>> Warning: could not reach ${NODE_NAME} (offline?)"
            ;;
          windows)
            echo ">>> Starting agent on ${NODE_NAME} (${NODE_HOST})..."
            ssh -o ConnectTimeout=5 ${NODE_USER}@${NODE_HOST} \
                "powershell -Command \"sc.exe start ai-mesh-agent 2>&1 | Out-Null; exit 0\"" 2>/dev/null \
                || echo ">>> Warning: could not reach ${NODE_NAME} (offline?)"
            ;;
        esac
    done

# Start coordinator + controller locally, then start all remote nodes.
# Ctrl+C stops only the local processes; remote services keep running.
dev: update-portproxy
    #!/usr/bin/env bash
    set -e

    echo ">>> Stopping any stale local processes..."
    pkill -f "target/debug/coordinator" || true
    pkill -f "target/debug/agent" || true
    sleep 0.5

    cleanup() {
        echo ""
        echo ">>> Stopping local coordinator and controller..."
        kill $COORD_PID $AGENT_PID 2>/dev/null || true
        echo ">>> Done."
    }
    trap cleanup EXIT

    echo ">>> Stopping all remote compute agents..."
    for f in nodes/*.env; do
        source "$f"
        case "$NODE_OS" in
          linux)
            ssh -o ConnectTimeout=5 ${NODE_USER}@${NODE_HOST} \
                "sudo systemctl stop ai-mesh-agent" 2>/dev/null || true
            ;;
          windows)
            ssh -o ConnectTimeout=5 ${NODE_USER}@${NODE_HOST} \
                "powershell -Command \"\
                    sc.exe stop ai-mesh-agent 2>&1 | Out-Null;\
                    Start-Sleep 1;\
                    Get-Process agent -ErrorAction SilentlyContinue | Stop-Process -Force;\
                    Get-Process nssm  -ErrorAction SilentlyContinue | Stop-Process -Force;\
                    exit 0\"" 2>/dev/null || true
            ;;
        esac
    done

    echo ">>> Starting coordinator (log: /tmp/mesh-coordinator.log)..."
    MDNS_ADVERTISE_IP={{coordinator_ip}} cargo run -p coordinator > /tmp/mesh-coordinator.log 2>&1 &
    COORD_PID=$!
    sleep 1

    cargo run -p cli -- reset-registry || true

    echo ">>> Starting local controller agent (log: /tmp/mesh-agent.log)..."
    AGENT_ROLE=controller cargo run -p agent > /tmp/mesh-agent.log 2>&1 &
    AGENT_PID=$!

    echo ">>> Verifying portproxy (remote nodes connect via {{coordinator_ip}}:{{coordinator_port}})..."
    if timeout 3 bash -c "echo > /dev/tcp/{{coordinator_ip}}/{{coordinator_port}}" 2>/dev/null; then
        echo ">>> Portproxy OK — {{coordinator_ip}}:{{coordinator_port}} is reachable"
    else
        echo ">>> WARNING: {{coordinator_ip}}:{{coordinator_port}} not reachable from WSL"
        echo ">>>   Remote nodes will not be able to connect. Try: just update-portproxy"
    fi

    echo ">>> Starting all remote compute agents..."
    for f in nodes/*.env; do
        source "$f"
        NODE_NAME=$(basename "$f" .env)
        case "$NODE_OS" in
          linux)
            ssh -o ConnectTimeout=5 ${NODE_USER}@${NODE_HOST} \
                "sudo systemctl start ai-mesh-agent" 2>/dev/null \
                || echo ">>> Warning: could not start agent on ${NODE_NAME} (offline?)"
            ;;
          windows)
            ssh -o ConnectTimeout=5 ${NODE_USER}@${NODE_HOST} \
                "powershell -Command \"sc.exe start ai-mesh-agent 2>&1 | Out-Null; exit 0\"" 2>/dev/null \
                || echo ">>> Warning: could not start agent on ${NODE_NAME} (offline?)"
            ;;
        esac
    done

    echo ">>> Waiting for nodes to register..."
    sleep 10

    echo ">>> Live watch (Ctrl+C to stop local coordinator + controller)..."
    cargo run -p cli -- watch

# Full cluster sanity test: coordinator + controller + all remote nodes.
sanity-full: update-portproxy
    #!/usr/bin/env bash
    set -e

    cleanup() {
        kill $COORD_PID $AGENT_PID 2>/dev/null || true
        for f in nodes/*.env; do
            source "$f"
            [ "$NODE_OS" = "linux" ] && \
                ssh -o ConnectTimeout=5 ${NODE_USER}@${NODE_HOST} "sudo systemctl stop ai-mesh-agent" 2>/dev/null || true
        done
    }
    trap cleanup EXIT

    echo ">>> Stopping all remote agents..."
    for f in nodes/*.env; do
        source "$f"
        case "$NODE_OS" in
          linux)
            ssh -o ConnectTimeout=5 ${NODE_USER}@${NODE_HOST} "sudo systemctl stop ai-mesh-agent 2>/dev/null || true" || true
            ;;
          windows)
            WIN_PATH="C:\\Users\\${NODE_USER}\\ai-mesh"
            ssh -o ConnectTimeout=5 ${NODE_USER}@${NODE_HOST} "powershell -Command \"\
                sc.exe stop ai-mesh-agent 2>&1 | Out-Null;\
                Start-Sleep 2;\
                Get-Process agent -ErrorAction SilentlyContinue | Stop-Process -Force;\
                Get-Process nssm  -ErrorAction SilentlyContinue | Stop-Process -Force;\
                exit 0\
            \"" || true
            ;;
        esac
    done

    pkill -f "target/debug/coordinator" || true
    pkill -f "target/debug/agent" || true
    sleep 0.5

    cargo run -p coordinator &
    COORD_PID=$!
    sleep 1

    cargo run -p cli -- reset-registry || true

    AGENT_ROLE=controller cargo run -p agent &
    AGENT_PID=$!
    sleep 1

    echo ">>> Starting all remote agents..."
    for f in nodes/*.env; do
        source "$f"
        NODE_NAME=$(basename "$f" .env)
        case "$NODE_OS" in
          linux)
            echo ">>> Starting agent service on ${NODE_NAME} (${NODE_HOST})..."
            ssh -o ConnectTimeout=5 ${NODE_USER}@${NODE_HOST} "sudo systemctl start ai-mesh-agent" \
                || echo ">>> Warning: could not reach ${NODE_NAME} (offline?)"
            ;;
          windows)
            echo ">>> Starting agent service on ${NODE_NAME} (${NODE_HOST})..."
            ssh -o ConnectTimeout=5 ${NODE_USER}@${NODE_HOST} \
                "powershell -Command \"sc.exe start ai-mesh-agent 2>&1 | Out-Null; exit 0\"" \
                || echo ">>> Warning: could not reach ${NODE_NAME} (offline?)"
            ;;
        esac
    done

    sleep 12

    echo ">>> Node table:"
    cargo run -p cli -- nodes

# Tail all mesh logs simultaneously (local + all remote nodes).
logs:
    #!/usr/bin/env bash
    trap 'kill $(jobs -p) 2>/dev/null; wait' EXIT
    echo ">>> Tailing all mesh logs (Ctrl+C to stop)..."
    tail -f /tmp/mesh-coordinator.log 2>/dev/null | sed 's/^/[coordinator] /' &
    tail -f /tmp/mesh-agent.log       2>/dev/null | sed 's/^/[controller]  /' &
    for f in nodes/*.env; do
        source "$f"
        NODE_NAME=$(basename "$f" .env)
        case "$NODE_OS" in
          linux)
            ssh ${NODE_USER}@${NODE_HOST} \
                "journalctl -u ai-mesh-agent -f --no-pager" 2>/dev/null \
                | sed "s/^/[${NODE_NAME}]  /" &
            ;;
          windows)
            WIN_PATH="C:\\Users\\${NODE_USER}\\ai-mesh"
            ssh ${NODE_USER}@${NODE_HOST} \
                "powershell -Command \"Get-Content '${WIN_PATH}\\logs\\agent.log' -Tail 20 -Wait\"" 2>/dev/null \
                | sed "s/^/[${NODE_NAME}]  /" &
            ;;
        esac
    done
    wait

# Run an end-to-end live inference loop across the whole cluster.
test-inference: update-portproxy
    #!/usr/bin/env bash
    set -e

    cleanup() {
        echo "=== Cleaning up cluster ==="
        kill $COORD_PID $AGENT_PID 2>/dev/null || true
        for f in nodes/*.env; do
            source "$f"
            case "$NODE_OS" in
              linux)
                ssh ${NODE_USER}@${NODE_HOST} "sudo systemctl stop ai-mesh-agent" 2>/dev/null || true
                ;;
              windows)
                ssh ${NODE_USER}@${NODE_HOST} "powershell -Command \"\
                    sc.exe stop ai-mesh-agent 2>&1 | Out-Null;\
                    Start-Sleep 2;\
                    Get-Process agent -ErrorAction SilentlyContinue | Stop-Process -Force;\
                    Get-Process nssm  -ErrorAction SilentlyContinue | Stop-Process -Force;\
                    exit 0\
                \"" 2>/dev/null || true
                ;;
            esac
        done
    }
    trap cleanup EXIT

    echo "=== Step 1: Stopping remote agents and starting cluster fresh ==="
    for f in nodes/*.env; do
        source "$f"
        case "$NODE_OS" in
          linux)
            ssh ${NODE_USER}@${NODE_HOST} "sudo systemctl stop ai-mesh-agent 2>/dev/null || true" || true
            ;;
          windows)
            ssh ${NODE_USER}@${NODE_HOST} "powershell -Command \"\
                sc.exe stop ai-mesh-agent 2>&1 | Out-Null;\
                Start-Sleep 2;\
                Get-Process agent -ErrorAction SilentlyContinue | Stop-Process -Force;\
                Get-Process nssm  -ErrorAction SilentlyContinue | Stop-Process -Force;\
                exit 0\
            \"" || true
            ;;
        esac
    done
    pkill -f "target/debug/coordinator" || true
    pkill -f "target/debug/agent" || true
    sleep 0.5

    cargo run -p coordinator &
    COORD_PID=$!
    sleep 1

    cargo run -p cli -- reset-registry || true

    AGENT_ROLE=controller cargo run -p agent &
    AGENT_PID=$!
    sleep 1

    for f in nodes/*.env; do
        source "$f"
        NODE_NAME=$(basename "$f" .env)
        case "$NODE_OS" in
          linux)
            echo "=== Starting agent service on ${NODE_NAME} (${NODE_HOST}) ==="
            ssh ${NODE_USER}@${NODE_HOST} "sudo systemctl start ai-mesh-agent"
            ;;
          windows)
            ssh ${NODE_USER}@${NODE_HOST} \
                "powershell -Command \"sc.exe start ai-mesh-agent 2>&1 | Out-Null; exit 0\""
            ;;
        esac
    done

    sleep 5

    echo "=== Step 2: Verifying all compute nodes are registered ==="
    cargo run -q -p cli -- nodes

    COMPUTE_NODES=$(cargo run -q -p cli -- nodes | grep -E "Compute")
    COMPUTE_COUNT=$(echo "$COMPUTE_NODES" | grep -c "Compute" || true)
    echo "Found ${COMPUTE_COUNT} compute node(s)"

    echo "=== Step 3: Loading model on all compute nodes ==="
    while IFS= read -r line; do
        NODE_ID=$(echo "$line" | awk -F'|' '{print $2}' | xargs)
        HOSTNAME=$(echo "$line" | awk -F'|' '{print $3}' | xargs)
        if [ -n "$NODE_ID" ]; then
            echo "  Loading qwen2.5:0.5b on ${HOSTNAME} (${NODE_ID})..."
            cargo run -q -p cli -- load "${NODE_ID}" qwen2.5:0.5b 4200
        fi
    done <<< "$COMPUTE_NODES"

    echo "=== Step 4: Waiting for all nodes to reach Ready ==="
    sleep 5
    cargo run -q -p cli -- nodes

    echo "=== Step 5: Firing 4 inference requests (expect load distribution) ==="
    PROMPT='In one sentence, what is the Itchen Bridge toll for?'
    for i in 1 2 3 4; do
        echo "--- Request ${i} ---"
        cargo run -q -p cli -- infer 'qwen2.5:0.5b' "${PROMPT}"
    done

    echo "=== Step 6: Final cluster state ==="
    cargo run -q -p cli -- nodes

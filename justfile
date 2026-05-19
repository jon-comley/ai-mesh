pi_host := "192.168.1.11"
pi_user := "jonno"

beelink_host := "192.168.1.14"
beelink_user := "jonno"
beelink_path := "C:\\Users\\jonno\\ai-mesh"

coordinator_ip := "192.168.1.12"
coordinator_port := "9000"

default: build

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
    cargo run -p coordinator

sanity:
    #!/usr/bin/env bash
    set -e
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

    echo "=== Skipping Phase 6 message injection ==="
    echo "Note: ai-mesh uses a 4-byte little-endian length prefix for all wire messages."
    echo "Raw nc cannot be used. A framed test client will be added in Phase 6 proper."

    echo "=== Coordinator log tail ==="
    tail -n 20 /tmp/mesh-coordinator.log

    echo "=== Agent log tail ==="
    tail -n 20 /tmp/mesh-agent.log

    echo "=== Cleaning up ==="
    kill $COORD_PID $AGENT_PID 2>/dev/null || true
    echo "Sanity check complete."

build-pi:
    cargo build --release --target aarch64-unknown-linux-gnu -p agent

deploy-pi: build-pi
    @echo "=== Checking Ollama installation on target compute node ==="
    ssh -t {{pi_user}}@{{pi_host}} "command -v ollama >/dev/null 2>&1 || (echo 'Ollama missing! Installing natively now...' && curl -fsSL https://ollama.com/install.sh | sh)"

    @echo "=== Ensuring Ollama daemon service is active ==="
    ssh -t {{pi_user}}@{{pi_host}} "sudo systemctl daemon-reload && sudo systemctl enable --now ollama"

    @echo "=== Pre-caching model weights on target hardware ==="
    ssh {{pi_user}}@{{pi_host}} "ollama pull qwen2.5:1.5b"

    @echo "=== Shipping compiled agent binary to remote filesystem ==="
    scp target/aarch64-unknown-linux-gnu/release/agent {{pi_user}}@{{pi_host}}:/home/{{pi_user}}/agent

run-pi:
    ssh {{pi_user}}@{{pi_host}} "COORDINATOR_IP={{coordinator_ip}} AGENT_ROLE=compute /home/{{pi_user}}/agent"

# ============================================================
# Beelink1 (Windows 11) — Windows agent build + provisioning
# ============================================================

# Build Windows agent (.exe) from WSL using MinGW GNU toolchain.
# Prereqs (once):
#   sudo apt install gcc-mingw-w64-x86-64
#   rustup target add x86_64-pc-windows-gnu
build-beelink-exe:
    cargo build --release -p agent --target x86_64-pc-windows-gnu

# First-time provisioning of Beelink1 as a Windows compute node.
deploy-beelink-windows: build-beelink-exe
    @echo ">>> Creating {{beelink_path}} on Beelink (if missing)..."
    ssh {{beelink_user}}@{{beelink_host}} "powershell -Command \"if (-not (Test-Path '{{beelink_path}}')) { New-Item -ItemType Directory -Path '{{beelink_path}}' | Out-Null }\""

    @echo ">>> Uploading Windows agent.exe (via temp file to avoid lock)..."
    scp target/x86_64-pc-windows-gnu/release/agent.exe {{beelink_user}}@{{beelink_host}}:"{{beelink_path}}\\agent_next.exe"

    @echo ">>> Uploading provision script..."
    scp scripts/provision-beelink.ps1 {{beelink_user}}@{{beelink_host}}:"{{beelink_path}}\\provision-beelink.ps1"

    @echo ">>> Stopping service, swapping binary, running provision script..."
    ssh {{beelink_user}}@{{beelink_host}} "powershell -ExecutionPolicy Bypass -Command \"\
        sc.exe stop ai-mesh-agent 2>&1 | Out-Null;\
        Start-Sleep 2;\
        Get-Process agent -ErrorAction SilentlyContinue | Stop-Process -Force;\
        Get-Process nssm -ErrorAction SilentlyContinue | Stop-Process -Force;\
        Start-Sleep 2;\
        Move-Item -Force '{{beelink_path}}\\agent_next.exe' '{{beelink_path}}\\agent.exe';\
        & '{{beelink_path}}\\provision-beelink.ps1' -CoordinatorIp '{{coordinator_ip}}'\
    \""

    @echo ">>> Beelink Windows provisioning complete."

# OTA-style update: rebuild agent.exe, push it, restart service.
update-beelink: build-beelink-exe
    @echo ">>> Uploading updated agent.exe (via temp file to avoid lock)..."
    scp target/x86_64-pc-windows-gnu/release/agent.exe {{beelink_user}}@{{beelink_host}}:"{{beelink_path}}\\agent_next.exe"

    @echo ">>> Stopping service, swapping binary, restarting..."
    ssh {{beelink_user}}@{{beelink_host}} "powershell -Command \"\
        sc.exe stop ai-mesh-agent 2>&1 | Out-Null;\
        Start-Sleep 2;\
        Get-Process agent -ErrorAction SilentlyContinue | Stop-Process -Force;\
        Get-Process nssm -ErrorAction SilentlyContinue | Stop-Process -Force;\
        Start-Sleep 2;\
        Move-Item -Force '{{beelink_path}}\\agent_next.exe' '{{beelink_path}}\\agent.exe';\
        sc.exe start ai-mesh-agent 2>&1 | Out-Null;\
        exit 0\
    \""

    @echo ">>> Beelink agent updated and restarted."

logs-beelink:
    ssh {{beelink_user}}@{{beelink_host}} "powershell -Command \"Get-Content '{{beelink_path}}\\logs\\agent.log' -Tail 100 -Wait\""

# Refresh the Windows portproxy rule to point at the current WSL2 IP.
# WSL2 assigns a new IP on each restart; the portproxy goes stale without this.
# No-op if the rule is already correct — no UAC prompt in that case.
# Automatically runs as a dependency of any recipe that starts the coordinator.
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

# Sanity check: coordinator + controller locally, Beelink via service restart.
# Requires: deploy-beelink-windows already run and ai-mesh-agent service installed.
sanity-beelink: update-portproxy
    #!/usr/bin/env bash
    set -e

    # Stop Beelink service first — prevents stale duplicate entries after registry reset
    ssh {{beelink_user}}@{{beelink_host}} "powershell -Command \"\
        sc.exe stop ai-mesh-agent 2>&1 | Out-Null;\
        Start-Sleep 2;\
        Get-Process agent -ErrorAction SilentlyContinue | Stop-Process -Force;\
        Get-Process nssm -ErrorAction SilentlyContinue | Stop-Process -Force;\
        exit 0\
    \""

    pkill -f "target/debug/coordinator" || true
    pkill -f "target/debug/agent" || true
    sleep 0.5

    cargo run -p coordinator &
    COORD_PID=$!
    sleep 1

    just reset || true

    AGENT_ROLE=controller cargo run -p agent &
    AGENT_PID=$!
    sleep 1

    echo ">>> Starting Beelink agent service..."
    ssh {{beelink_user}}@{{beelink_host}} "sc.exe start ai-mesh-agent 2>&1 | Out-Null; exit 0"
    sleep 12

    echo ">>> Node table:"
    cargo run -p cli -- nodes

    kill $COORD_PID $AGENT_PID 2>/dev/null || true

run-controller:
    AGENT_ROLE=controller cargo run -p agent

reset:
    cargo run -p cli -- reset-registry

sanity-pi:
    @echo "=== Checking Multi-Node Mesh State ==="
    cargo run -p cli -- nodes
    @echo "=== Active Diagnostic Monitoring ==="
    cargo run -p cli -- watch

# Full cluster sanity test (coordinator + controller + Pi)
sanity-all:
    #!/usr/bin/env bash
    set -e

    # Kill any stale processes
    pkill -f "target/debug/coordinator" || true
    pkill -f "target/debug/agent" || true
    sleep 0.5

    # Start coordinator
    cargo run -p coordinator &
    COORD_PID=$!
    sleep 1

    # Reset registry now that coordinator is live
    just reset || true

    # Start controller agent
    AGENT_ROLE=controller cargo run -p agent &
    AGENT_PID=$!

    # Run CLI nodes
    cargo run -p cli -- nodes

    # Cleanup
    kill $COORD_PID $AGENT_PID || true

# Full cluster sanity test (coordinator + controller + Pi + Beelink)
sanity-full: update-portproxy
    #!/usr/bin/env bash
    set -e

    # Stop all remote agents first — prevents them reconnecting to the new coordinator
    # before the registry reset, which would create stale duplicate entries.
    echo ">>> Stopping remote agents..."
    ssh {{pi_user}}@{{pi_host}} "pkill -f agent || true" || true
    ssh {{beelink_user}}@{{beelink_host}} "powershell -Command \"\
        sc.exe stop ai-mesh-agent 2>&1 | Out-Null;\
        Start-Sleep 2;\
        Get-Process agent -ErrorAction SilentlyContinue | Stop-Process -Force;\
        Get-Process nssm -ErrorAction SilentlyContinue | Stop-Process -Force;\
        exit 0\
    \"" || true

    # Kill any stale local processes
    pkill -f "target/debug/coordinator" || true
    pkill -f "target/debug/agent" || true
    sleep 0.5

    # Start coordinator
    cargo run -p coordinator &
    COORD_PID=$!
    sleep 1

    # Reset registry
    just reset || true

    # Start controller agent
    AGENT_ROLE=controller cargo run -p agent &
    AGENT_PID=$!
    sleep 1

    # Start Pi compute agent
    ssh {{pi_user}}@{{pi_host}} "echo Connected to Pi OK"
    ssh {{pi_user}}@{{pi_host}} "COORDINATOR_IP={{coordinator_ip}} AGENT_ROLE=compute /home/{{pi_user}}/agent" &

    # Start Beelink agent service
    echo ">>> Starting Beelink agent service..."
    ssh {{beelink_user}}@{{beelink_host}} "powershell -Command \"sc.exe start ai-mesh-agent 2>&1 | Out-Null; exit 0\""

    sleep 12

    # Validate full cluster
    echo ">>> Node table:"
    cargo run -p cli -- nodes

    # Cleanup
    kill $COORD_PID $AGENT_PID || true
    ssh {{pi_user}}@{{pi_host}} "pkill -f agent" || true

# Run an end-to-end live inference loop across the whole cluster automatically
test-inference: update-portproxy
    #!/usr/bin/env bash
    set -e

    cleanup() {
        echo "=== Cleaning up cluster ==="
        kill $COORD_PID $AGENT_PID 2>/dev/null || true
        ssh {{pi_user}}@{{pi_host}} "pkill -f agent" 2>/dev/null || true
    }
    trap cleanup EXIT

    echo "=== Step 1: Cleaning workspace and starting cluster fresh ==="
    pkill -f "target/debug/coordinator" || true
    pkill -f "target/debug/agent" || true
    sleep 0.5

    cargo run -p coordinator &
    COORD_PID=$!
    sleep 1

    just reset || true

    AGENT_ROLE=controller cargo run -p agent &
    AGENT_PID=$!
    sleep 1

    ssh {{pi_user}}@{{pi_host}} "echo Connected to Pi OK"
    ssh {{pi_user}}@{{pi_host}} "COORDINATOR_IP={{coordinator_ip}} AGENT_ROLE=compute /home/{{pi_user}}/agent" &

    # Give the cluster a moment to stabilize heartbeats and populate the registry
    sleep 3

    echo "=== Step 2: Fetching the dynamic Compute node ID ==="
    NODE_ID=$(cargo run -q -p cli -- nodes | grep -E "Compute" | head -n 1 | awk -F'|' '{print $2}' | xargs)

    if [ -z "$NODE_ID" ]; then
        echo "Error: Could not find an active Compute node in the registry!"
        exit 1
    fi
    echo "Found Compute node ID: ${NODE_ID}"

    echo "=== Step 3: Triggering model load on target node ==="
    cargo run -q -p cli -- load "${NODE_ID}" qwen2.5:0.5b 4200

    echo "=== Step 4: Waiting for model state transition to Ready ==="
    sleep 3

    echo "=== Step 5: Verifying model status in cluster table ==="
    cargo run -q -p cli -- nodes

    echo "=== Step 6: Dispatching load-balanced inference prompt ==="
    cargo run -p cli -- infer 'qwen2.5:0.5b' 'Context: The Itchen Bridge is a high-level bridge in Southampton, England. Why does it have a toll?'

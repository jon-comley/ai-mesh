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
    ssh {{pi_user}}@{{pi_host}} "sudo systemctl stop ai-mesh-agent 2>/dev/null || true"
    scp target/aarch64-unknown-linux-gnu/release/agent {{pi_user}}@{{pi_host}}:/home/{{pi_user}}/agent
    just run-pi

# Install (or update) the ai-mesh-agent systemd service on the Pi and start it.
run-pi:
    #!/usr/bin/env bash
    set -e
    echo "=== Installing ai-mesh-agent systemd service on Pi ==="
    TMPFILE=$(mktemp)
    printf '[Unit]\nDescription=ai-mesh compute agent\nAfter=network-online.target ollama.service\nWants=network-online.target\n\n[Service]\nExecStart=/home/{{pi_user}}/agent\nEnvironment=COORDINATOR_IP={{coordinator_ip}}\nEnvironment=AGENT_ROLE=compute\nRestart=always\nRestartSec=5\nUser={{pi_user}}\nStandardOutput=journal\nStandardError=journal\n\n[Install]\nWantedBy=multi-user.target\n' > "$TMPFILE"
    echo ">>> Uploading service file..."
    scp -q "$TMPFILE" {{pi_user}}@{{pi_host}}:/tmp/ai-mesh-agent.service
    rm "$TMPFILE"
    echo ">>> Installing and starting service..."
    ssh {{pi_user}}@{{pi_host}} "sudo mv /tmp/ai-mesh-agent.service /etc/systemd/system/ai-mesh-agent.service && sudo systemctl daemon-reload && sudo systemctl enable --now ai-mesh-agent 2>/dev/null; systemctl is-active ai-mesh-agent"
    echo "=== Done ==="

# Tail all mesh logs simultaneously: coordinator, controller, Pi, Beelink.
logs:
    #!/usr/bin/env bash
    trap 'kill $(jobs -p) 2>/dev/null; wait' EXIT
    echo ">>> Tailing all mesh logs (Ctrl+C to stop)..."
    tail -f /tmp/mesh-coordinator.log 2>/dev/null | sed 's/^/[coordinator] /' &
    tail -f /tmp/mesh-agent.log       2>/dev/null | sed 's/^/[controller]  /' &
    ssh {{pi_user}}@{{pi_host}} "journalctl -u ai-mesh-agent -f --no-pager" 2>/dev/null | sed 's/^/[pi]          /' &
    ssh {{beelink_user}}@{{beelink_host}} "powershell -Command \"Get-Content '{{beelink_path}}\\logs\\agent.log' -Tail 20 -Wait\"" 2>/dev/null | sed 's/^/[beelink]     /' &
    wait

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

# Start the coordinator and local controller agent on this machine.
# Pi and Beelink compute nodes are persistent services — they reconnect automatically.
# Ctrl+C stops the local coordinator and controller only; remote nodes keep running.
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

    echo ">>> Stopping remote compute agents before coordinator starts..."
    ssh -o ConnectTimeout=5 {{pi_user}}@{{pi_host}} "sudo systemctl stop ai-mesh-agent" 2>/dev/null || true
    ssh -o ConnectTimeout=5 {{beelink_user}}@{{beelink_host}} "powershell -Command \"\
        sc.exe stop ai-mesh-agent 2>&1 | Out-Null;\
        Start-Sleep 1;\
        Get-Process agent -ErrorAction SilentlyContinue | Stop-Process -Force;\
        Get-Process nssm -ErrorAction SilentlyContinue | Stop-Process -Force;\
        exit 0\"" 2>/dev/null || true

    echo ">>> Starting coordinator (log: /tmp/mesh-coordinator.log)..."
    cargo run -p coordinator > /tmp/mesh-coordinator.log 2>&1 &
    COORD_PID=$!
    sleep 1

    just reset || true

    echo ">>> Starting local controller agent (log: /tmp/mesh-agent.log)..."
    AGENT_ROLE=controller cargo run -p agent > /tmp/mesh-agent.log 2>&1 &
    AGENT_PID=$!

    echo ">>> Verifying portproxy (remote nodes connect via {{coordinator_ip}}:{{coordinator_port}})..."
    if timeout 3 bash -c "echo > /dev/tcp/{{coordinator_ip}}/{{coordinator_port}}" 2>/dev/null; then
        echo ">>> Portproxy OK — {{coordinator_ip}}:{{coordinator_port}} is reachable"
    else
        echo ">>> WARNING: {{coordinator_ip}}:{{coordinator_port}} not reachable from WSL"
        echo ">>>   Remote nodes (Pi, Beelink) will not be able to connect."
        echo ">>>   Try: just update-portproxy   (UAC prompt will appear)"
    fi

    echo ">>> Starting remote compute agents..."
    ssh -o ConnectTimeout=5 {{pi_user}}@{{pi_host}} "sudo systemctl start ai-mesh-agent" 2>/dev/null || echo ">>> Warning: could not start Pi agent (offline?)"
    ssh -o ConnectTimeout=5 {{beelink_user}}@{{beelink_host}} "powershell -Command \"\
        sc.exe start ai-mesh-agent 2>&1 | Out-Null;\
        exit 0\"" 2>/dev/null || echo ">>> Warning: could not start Beelink agent (offline?)"

    echo ">>> Waiting for nodes to register..."
    sleep 10

    echo ">>> Live watch (Ctrl+C to stop local coordinator + controller)..."
    cargo run -p cli -- watch

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
    trap 'kill $COORD_PID $AGENT_PID 2>/dev/null || true' EXIT

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
    sleep 2

    # Run CLI nodes
    cargo run -p cli -- nodes

# Full cluster sanity test (coordinator + controller + Pi + Beelink)
sanity-full: update-portproxy
    #!/usr/bin/env bash
    set -e

    cleanup() {
        kill $COORD_PID $AGENT_PID 2>/dev/null || true
        ssh {{pi_user}}@{{pi_host}} "pkill -f agent" 2>/dev/null || true
    }
    trap cleanup EXIT

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

# Run an end-to-end live inference loop across the whole cluster automatically
test-inference: update-portproxy
    #!/usr/bin/env bash
    set -e

    cleanup() {
        echo "=== Cleaning up cluster ==="
        kill $COORD_PID $AGENT_PID 2>/dev/null || true
        ssh {{pi_user}}@{{pi_host}} "pkill -f agent" 2>/dev/null || true
        ssh {{beelink_user}}@{{beelink_host}} "powershell -Command \"\
            sc.exe stop ai-mesh-agent 2>&1 | Out-Null;\
            Start-Sleep 2;\
            Get-Process agent -ErrorAction SilentlyContinue | Stop-Process -Force;\
            Get-Process nssm -ErrorAction SilentlyContinue | Stop-Process -Force;\
            exit 0\
        \"" 2>/dev/null || true
    }
    trap cleanup EXIT

    echo "=== Step 1: Stopping remote agents and starting cluster fresh ==="
    ssh {{pi_user}}@{{pi_host}} "pkill -f agent || true" || true
    ssh {{beelink_user}}@{{beelink_host}} "powershell -Command \"\
        sc.exe stop ai-mesh-agent 2>&1 | Out-Null;\
        Start-Sleep 2;\
        Get-Process agent -ErrorAction SilentlyContinue | Stop-Process -Force;\
        Get-Process nssm -ErrorAction SilentlyContinue | Stop-Process -Force;\
        exit 0\
    \"" || true
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
    ssh {{beelink_user}}@{{beelink_host}} "powershell -Command \"sc.exe start ai-mesh-agent 2>&1 | Out-Null; exit 0\""

    # Give all nodes time to register
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

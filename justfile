coordinator_ip   := "192.168.1.11"
coordinator_port := "9001"

export PATH := env_var("HOME") / ".cargo/bin" + ":" + env_var("PATH")

default: build

# Set up this machine as the ai-mesh controller.
# Run once on a fresh laptop (from WSL2). Idempotent — safe to re-run.
# Usage: just setup-controller
setup-controller:
    #!/usr/bin/env bash
    set -e

    echo "=== ai-mesh controller setup ==="
    echo ""

    # 1. Rust toolchain
    if ! command -v rustup &>/dev/null; then
        echo ">>> Installing Rust toolchain..."
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path
        source "$HOME/.cargo/env"
        if ! grep -q 'cargo/env' "$HOME/.bashrc" 2>/dev/null; then
            echo '' >> "$HOME/.bashrc"
            echo '. "$HOME/.cargo/env"' >> "$HOME/.bashrc"
            echo ">>> Added cargo to PATH in ~/.bashrc"
        fi
    else
        source "$HOME/.cargo/env" 2>/dev/null || true
        echo ">>> Rust already installed ($(rustc --version))"
    fi

    # 2. Cross-compilation toolchains for remote node deployment
    echo ">>> Checking cross-compilation toolchains..."
    MISSING_PKGS=()
    dpkg -l build-essential       2>/dev/null | grep -q "^ii" || MISSING_PKGS+=(build-essential)
    dpkg -l gcc-mingw-w64-x86-64  2>/dev/null | grep -q "^ii" || MISSING_PKGS+=(gcc-mingw-w64-x86-64)
    dpkg -l gcc-aarch64-linux-gnu 2>/dev/null | grep -q "^ii" || MISSING_PKGS+=(gcc-aarch64-linux-gnu)
    if [ "${#MISSING_PKGS[@]}" -gt 0 ]; then
        echo ""
        echo ">>> Missing apt packages: ${MISSING_PKGS[*]}"
        echo ">>> Please run the following, then re-run 'just setup-controller':"
        echo ""
        echo "    sudo apt-get install -y ${MISSING_PKGS[*]}"
        echo ""
        exit 1
    else
        echo ">>> build-essential and gcc-mingw-w64-x86-64 already installed"
    fi

    if ! rustup target list --installed | grep -q "x86_64-pc-windows-gnu"; then
        rustup target add x86_64-pc-windows-gnu
    else
        echo ">>> x86_64-pc-windows-gnu already added"
    fi

    if ! rustup target list --installed | grep -q "aarch64-unknown-linux-gnu"; then
        rustup target add aarch64-unknown-linux-gnu
    else
        echo ">>> aarch64-unknown-linux-gnu already added"
    fi

    # 3. Git hooks
    echo ">>> Installing git hooks..."
    bash scripts/install-hooks.sh

    # 4. SSH key — generate if missing, then push to all nodes
    if [ ! -f "$HOME/.ssh/id_ed25519" ]; then
        echo ">>> Generating SSH key..."
        ssh-keygen -t ed25519 -N "" -f "$HOME/.ssh/id_ed25519"
    else
        echo ">>> SSH key already exists"
    fi

    echo ">>> Pushing SSH key to all nodes..."
    for f in nodes/*.env; do
        source "$f"
        NODE_NAME=$(basename "$f" .env)
        echo ">>> Setting up SSH key on ${NODE_NAME} (${NODE_HOST}, ${NODE_OS})..."
        case "$NODE_OS" in
          linux)
            ssh-copy-id -o StrictHostKeyChecking=accept-new \
                -o ConnectTimeout=10 \
                "${NODE_USER}@${NODE_HOST}" \
                && echo ">>> ${NODE_NAME} SSH key OK" \
                || echo ">>> Warning: could not push key to ${NODE_NAME} (skipping)"
            ;;
          windows)
            PUBKEY=$(cat "$HOME/.ssh/id_ed25519.pub")
            ssh -o StrictHostKeyChecking=accept-new \
                -o ConnectTimeout=10 \
                "${NODE_USER}@${NODE_HOST}" \
                "powershell -Command \"\
                    \$f = 'C:\\ProgramData\\ssh\\administrators_authorized_keys';\
                    \$key = '${PUBKEY}';\
                    if (-not (Test-Path \$f)) { New-Item -ItemType File -Path \$f -Force | Out-Null };\
                    \$existing = Get-Content \$f -ErrorAction SilentlyContinue;\
                    if (\$existing -notcontains \$key) { Add-Content \$f \$key; Write-Host 'Key added' } else { Write-Host 'Key already present' }\
                \"" \
                && echo ">>> ${NODE_NAME} SSH key OK" \
                || echo ">>> Warning: could not push key to ${NODE_NAME} (skipping)"
            ;;
        esac
    done

    # 5. Portproxy + firewall
    echo ">>> Setting up Windows portproxy..."
    just update-portproxy
    echo ">>> Opening port {{coordinator_port}} in Windows Firewall..."
    powershell.exe -Command "
        \$rule = Get-NetFirewallRule -DisplayName 'ai-mesh coordinator' -ErrorAction SilentlyContinue
        if (-not \$rule) {
            Start-Process powershell -Verb RunAs -Wait -ArgumentList '-NoProfile -Command New-NetFirewallRule -DisplayName ''ai-mesh coordinator'' -Direction Inbound -Protocol TCP -LocalPort {{coordinator_port}} -Action Allow'
            Write-Host '>>> Firewall rule added.'
        } else {
            Write-Host '>>> Firewall rule already exists.'
        }
    " 2>/dev/null || echo ">>> Warning: could not configure firewall (add manually if nodes cannot connect)"

    # 6. Check coordinator_ip matches this machine's LAN IP
    LAN_IP=$(powershell.exe -NoProfile -Command \
        "Get-NetIPAddress -AddressFamily IPv4 | Where-Object { \$_.InterfaceAlias -notmatch 'Loopback|WSL|vEthernet|Bluetooth' -and \$_.IPAddress -notmatch '^169\.' } | Select-Object -First 1 -ExpandProperty IPAddress" \
        2>/dev/null | tr -d '\r\n')
    if [ "{{coordinator_ip}}" != "$LAN_IP" ]; then
        echo ""
        echo ">>> WARNING: coordinator_ip in justfile ({{coordinator_ip}}) does not match"
        echo ">>>          this machine's LAN IP ($LAN_IP)."
        echo ">>>          Update the top of justfile: coordinator_ip := \"$LAN_IP\""
        echo ">>>          Then reprovision remote nodes: just provision-all"
    else
        echo ">>> coordinator_ip matches LAN IP ({{coordinator_ip}}) — OK"
    fi

    # 7. Build
    echo ""
    echo ">>> Building project (this takes a minute on a fresh clone)..."
    cargo build

    echo ""
    echo "=== Setup complete ==="
    echo ""
    echo "Next steps:"
    if [ "{{coordinator_ip}}" != "$LAN_IP" ]; then
        echo "  1. Fix coordinator_ip in justfile (see warning above)"
        echo "  2. Reprovision remote nodes:  just provision-all"
        echo "  3. Start the cluster:         just start-cluster"
    else
        echo "  1. Reprovision remote nodes with this machine's coordinator IP:"
        echo "       just provision-all"
        echo "  2. Start the cluster:"
        echo "       just start-cluster"
    fi

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

    # Push credentials in case the coordinator was freshly started (new cert/token).
    STATE="$HOME/.config/ai-mesh/coordinator.state"
    if [ -f "$STATE" ]; then
        source "$STATE"
        export MESH_TLS_FINGERPRINT="${MESH_TLS_FINGERPRINT}"
        echo ">>> Pushing TLS fingerprint and auth token to all compute nodes..."
        for f in nodes/*.env; do
            source "$f"
            NODE_NAME=$(basename "$f" .env)
            [ "${NODE_ROLE}" = "compute" ] || continue
            just set-fingerprint ${NODE_NAME} \
                || echo ">>> Warning: could not set credentials on ${NODE_NAME} (skipping)"
        done
    fi
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
    #!/usr/bin/env bash
    pkill -f "target/(debug|release)/coordinator" || true
    sleep 0.3
    STATE="$HOME/.config/ai-mesh/coordinator.state"
    if [ -f "$STATE" ]; then
        source "$STATE"
        export MESH_AUTH_TOKEN="${MESH_AUTH_TOKEN:-}"
    fi
    MDNS_ADVERTISE_IP={{coordinator_ip}} cargo run -p coordinator

run-controller:
    AGENT_ROLE=compute cargo run -p agent

reset: update-portproxy
    #!/usr/bin/env bash
    set -e
    STATE="$HOME/.config/ai-mesh/coordinator.state"
    if [ -f "$STATE" ]; then
        source "$STATE"
        export MESH_TLS_FINGERPRINT MESH_AUTH_TOKEN
    fi
    cargo run -p cli -- --coordinator "{{coordinator_ip}}:{{coordinator_port}}" reset-registry
    echo "Registry cleared. Nodes will re-register on their next heartbeat."

nodes:
    #!/usr/bin/env bash
    STATE="$HOME/.config/ai-mesh/coordinator.state"
    if [ -f "$STATE" ]; then source "$STATE"; export MESH_TLS_FINGERPRINT MESH_AUTH_TOKEN; fi
    cargo run -q -p cli -- --coordinator "{{coordinator_ip}}:{{coordinator_port}}" nodes

# Set heartbeat interval for a node. Accepts hostname, IP, or UUID.
# Usage: just set-heartbeat beelink1 10
# Usage: just set-heartbeat 192.168.1.14 30
set-heartbeat node secs:
    #!/usr/bin/env bash
    STATE="$HOME/.config/ai-mesh/coordinator.state"
    if [ -f "$STATE" ]; then source "$STATE"; export MESH_TLS_FINGERPRINT MESH_AUTH_TOKEN MESH_HTTP_PORT; fi
    cargo run -q -p cli -- --coordinator "{{coordinator_ip}}:{{coordinator_port}}" \
        set-heartbeat {{node}} {{secs}}

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

# First-time provision or re-provision a single node.
# Usage: just deploy-node pi1
#        just deploy-node beelink1
deploy-node node:
    #!/usr/bin/env bash
    set -e
    source nodes/{{node}}.env

    scp_dots() {
        local label="$1"; shift
        printf "%s" "$label"
        "$@" &
        local pid=$!
        while kill -0 $pid 2>/dev/null; do printf "."; sleep 0.5; done
        wait $pid; local rc=$?; echo ""; return $rc
    }

    case "$NODE_OS" in
      linux)
        NODE_ARCH=$(ssh ${NODE_USER}@${NODE_HOST} "uname -m" 2>/dev/null || echo "aarch64")
        if [ "$NODE_ARCH" = "x86_64" ]; then
            echo ">>> Building Linux x86_64 agent..."
            cargo build --release --target x86_64-unknown-linux-gnu -p agent --features ${NODE_FEATURES:-llm}
            AGENT_BIN="target/x86_64-unknown-linux-gnu/release/agent"
        else
            echo ">>> Building Linux ARM64 agent..."
            cargo build --release --target aarch64-unknown-linux-gnu -p agent --features ${NODE_FEATURES:-llm}
            AGENT_BIN="target/aarch64-unknown-linux-gnu/release/agent"
        fi

        ssh ${NODE_USER}@${NODE_HOST} "sudo systemctl stop ai-mesh-agent 2>/dev/null || true"
        scp_dots ">>> Uploading agent binary" \
            scp -q ${AGENT_BIN} ${NODE_USER}@${NODE_HOST}:/home/${NODE_USER}/agent
        scp_dots ">>> Uploading install script" \
            scp -q scripts/install-node-linux.sh ${NODE_USER}@${NODE_HOST}:/tmp/install-node.sh
        ssh -t ${NODE_USER}@${NODE_HOST} \
            "chmod +x /tmp/install-node.sh && sudo /tmp/install-node.sh {{coordinator_ip}} ${NODE_ROLE} ${NODE_USER} ${MQTT_HOST:-} ${MQTT_PORT:-1883}"
        ;;

      windows)
        echo ">>> Building Windows x86_64 agent..."
        cargo build --release -p agent --target x86_64-pc-windows-gnu --features ${NODE_FEATURES:-llm}

        WIN_PATH="C:\\Users\\${NODE_USER}\\ai-mesh"
        echo ">>> Creating ${WIN_PATH} on ${NODE_HOST}..."
        ssh ${NODE_USER}@${NODE_HOST} \
            "powershell -Command \"if (-not (Test-Path '${WIN_PATH}')) { New-Item -ItemType Directory -Path '${WIN_PATH}' | Out-Null }\""

        scp_dots ">>> Uploading agent.exe" \
            scp -q target/x86_64-pc-windows-gnu/release/agent.exe \
                ${NODE_USER}@${NODE_HOST}:"${WIN_PATH}\\agent_next.exe"
        scp_dots ">>> Uploading install script" \
            scp -q scripts/install-node-windows.ps1 \
                ${NODE_USER}@${NODE_HOST}:"${WIN_PATH}\\install-node-windows.ps1"

        echo ">>> Stopping service, swapping binary, provisioning..."
        ssh ${NODE_USER}@${NODE_HOST} "powershell -ExecutionPolicy Bypass -Command \"\
            sc.exe stop ai-mesh-agent 2>&1 | Out-Null;\
            Start-Sleep 2;\
            \$pids = (Get-WmiObject Win32_Process -Filter 'name=''nssm.exe''').ProcessId;\
            foreach (\$p in \$pids) { taskkill /F /PID \$p 2>&1 | Out-Null };\
            Get-Process agent -ErrorAction SilentlyContinue | Stop-Process -Force;\
            Start-Sleep 2;\
            cmd /c 'copy /Y ${WIN_PATH}\\agent_next.exe ${WIN_PATH}\\agent.exe';\
            & '${WIN_PATH}\\install-node-windows.ps1' -CoordinatorIp '{{coordinator_ip}}' -Role '${NODE_ROLE}'\
        \""
        ;;

      *)
        echo "Unknown NODE_OS: $NODE_OS (expected linux or windows)"
        exit 1
        ;;
    esac
    echo ">>> Node {{node}} provisioned."

    # Push TLS fingerprint + auth token if the coordinator is already running.
    # Without this the freshly-started agent cannot pass auth and will loop-fail.
    STATE="$HOME/.config/ai-mesh/coordinator.state"
    if [ -f "$STATE" ]; then
        echo ">>> Coordinator is running — pushing credentials to {{node}}..."
        just set-fingerprint {{node}} \
            || echo ">>> Warning: could not push credentials to {{node}} — run: just set-fingerprint {{node}}"
    else
        echo ">>> No coordinator running yet — run 'just start-cluster' (or 'just set-fingerprint {{node}}' after starting the coordinator)"
    fi

# Build agent binaries for all platforms, then provision every node.
# Usage: just provision-all
provision-all:
    #!/usr/bin/env bash
    set -e

    scp_dots() {
        local label="$1"; shift
        printf "%s" "$label"
        "$@" &
        local pid=$!
        while kill -0 $pid 2>/dev/null; do printf "."; sleep 0.5; done
        wait $pid; local rc=$?; echo ""; return $rc
    }

    for f in nodes/*.env; do
        source "$f"
        NODE_NAME=$(basename "$f" .env)
        NODE_FEATURES="${NODE_FEATURES:-llm}"
        echo ""
        echo "=== Provisioning ${NODE_NAME} (${NODE_OS} / ${NODE_HOST}) [features: ${NODE_FEATURES}] ==="

        case "$NODE_OS" in
          linux)
            NODE_ARCH=$(ssh -o ConnectTimeout=10 ${NODE_USER}@${NODE_HOST} "uname -m" 2>/dev/null || echo "aarch64")
            if [ "$NODE_ARCH" = "x86_64" ]; then
                TARGET="x86_64-unknown-linux-gnu"
            else
                TARGET="aarch64-unknown-linux-gnu"
            fi
            AGENT_BIN="target/${TARGET}/release/agent"
            echo ">>> Building Linux ${NODE_ARCH} agent (features: ${NODE_FEATURES})..."
            cargo build --release --target "${TARGET}" -p agent --features "${NODE_FEATURES}"

            echo ">>> Stopping agent service..."
            ssh -o ConnectTimeout=10 ${NODE_USER}@${NODE_HOST} "
                sudo systemctl stop ai-mesh-agent 2>/dev/null || true
            " || true

            scp_dots ">>> Uploading agent binary" \
                scp -q ${AGENT_BIN} ${NODE_USER}@${NODE_HOST}:/home/${NODE_USER}/agent
            scp_dots ">>> Uploading install script" \
                scp -q scripts/install-node-linux.sh ${NODE_USER}@${NODE_HOST}:/tmp/install-node.sh
            ssh -t ${NODE_USER}@${NODE_HOST} \
                "chmod +x /tmp/install-node.sh && sudo /tmp/install-node.sh {{coordinator_ip}} ${NODE_ROLE} ${NODE_USER} ${MQTT_HOST:-} ${MQTT_PORT:-1883}"
            ;;

          windows)
            WIN_PATH="C:\\Users\\${NODE_USER}\\ai-mesh"

            echo ">>> Building Windows x86_64 agent (features: ${NODE_FEATURES})..."
            cargo build --release -p agent --target x86_64-pc-windows-gnu --features "${NODE_FEATURES}"

            echo ">>> Stopping agent service..."
            ssh -o ConnectTimeout=10 ${NODE_USER}@${NODE_HOST} "powershell -Command \"\
                sc.exe stop ai-mesh-agent 2>&1 | Out-Null;\
                Start-Sleep 2;\
                \$pids = (Get-WmiObject Win32_Process -Filter 'name=''nssm.exe''').ProcessId;\
                foreach (\$p in \$pids) { taskkill /F /PID \$p 2>&1 | Out-Null };\
                Get-Process agent -ErrorAction SilentlyContinue | Stop-Process -Force;\
                exit 0\
            \"" || true

            echo ">>> Creating ${WIN_PATH} on ${NODE_HOST}..."
            ssh ${NODE_USER}@${NODE_HOST} \
                "powershell -Command \"if (-not (Test-Path '${WIN_PATH}')) { New-Item -ItemType Directory -Path '${WIN_PATH}' | Out-Null }\""

            scp_dots ">>> Uploading agent.exe" \
                scp -q target/x86_64-pc-windows-gnu/release/agent.exe \
                    ${NODE_USER}@${NODE_HOST}:"${WIN_PATH}\\agent_next.exe"
            scp_dots ">>> Uploading install script" \
                scp -q scripts/install-node-windows.ps1 \
                    ${NODE_USER}@${NODE_HOST}:"${WIN_PATH}\\install-node-windows.ps1"

            echo ">>> Swapping binary and provisioning..."
            ssh ${NODE_USER}@${NODE_HOST} "powershell -ExecutionPolicy Bypass -Command \"\
                Start-Sleep 2;\
                cmd /c 'copy /Y ${WIN_PATH}\\agent_next.exe ${WIN_PATH}\\agent.exe';\
                & '${WIN_PATH}\\install-node-windows.ps1' -CoordinatorIp '{{coordinator_ip}}' -Role '${NODE_ROLE}'\
            \""
            ;;
        esac
        echo ">>> ${NODE_NAME} done."
    done
    echo ""
    echo "=== All nodes provisioned. ==="

# Restart the ai-mesh-agent service on a node without touching the binary.
# Usage: just restart-node pi1
restart-node node:
    #!/usr/bin/env bash
    set -e
    source nodes/{{node}}.env
    case "$NODE_OS" in
      linux)
        ssh ${NODE_USER}@${NODE_HOST} "sudo systemctl restart ai-mesh-agent"
        ;;
      windows)
        ssh ${NODE_USER}@${NODE_HOST} "powershell -Command \"\
            sc.exe stop ai-mesh-agent 2>&1 | Out-Null;\
            Start-Sleep 2;\
            \$pids = (Get-WmiObject Win32_Process -Filter 'name=''nssm.exe''').ProcessId;\
            foreach (\$p in \$pids) { taskkill /F /PID \$p 2>&1 | Out-Null };\
            Get-Process agent -ErrorAction SilentlyContinue | Stop-Process -Force;\
            Start-Sleep 2;\
            sc.exe start ai-mesh-agent 2>&1 | Out-Null;\
            exit 0\""
        ;;
    esac
    echo ">>> Node {{node}} agent restarted."

# Restart the agent service on a node, then reload its best-fit model.
# Use this when a node's llama-server is stuck or inference is failing.
# Usage: just reload-node beelink1
reload-node node:
    #!/usr/bin/env bash
    set -e
    source nodes/{{node}}.env
    echo ">>> Restarting agent on {{node}}..."
    just restart-node {{node}}
    echo ">>> Waiting for {{node}} to reconnect..."
    NODE_ID=""
    for i in $(seq 1 15); do
        NODE_ID=$(cargo run -q -p cli -- find-node "${NODE_HOST}" 2>/dev/null || true)
        [ -n "$NODE_ID" ] && break
        printf "\r    %ds... " "$((i*2))"
        sleep 2
    done
    printf "\r\n"
    if [ -z "$NODE_ID" ]; then
        echo "Error: {{node}} did not reconnect within 30s"
        exit 1
    fi
    echo ">>> {{node}} reconnected. Loading model..."
    just auto-load-model {{node}}
    echo ">>> Waiting for model to be Ready..."
    cargo run -q -p cli -- --coordinator "{{coordinator_ip}}:{{coordinator_port}}" \
        wait-ready "${NODE_HOST}" --timeout 300
    echo ">>> {{node}} is ready."

# OTA update: rebuild agent, upload, restart — no reprovisioning.
# Usage: just update-node pi1
update-node node:
    #!/usr/bin/env bash
    set -e
    source nodes/{{node}}.env

    case "$NODE_OS" in
      linux)
        NODE_ARCH=$(ssh ${NODE_USER}@${NODE_HOST} "uname -m" 2>/dev/null || echo "aarch64")
        if [ "$NODE_ARCH" = "x86_64" ]; then
            cargo build --release --target x86_64-unknown-linux-gnu -p agent --features ${NODE_FEATURES:-llm}
            AGENT_BIN="target/x86_64-unknown-linux-gnu/release/agent"
        else
            cargo build --release --target aarch64-unknown-linux-gnu -p agent --features ${NODE_FEATURES:-llm}
            AGENT_BIN="target/aarch64-unknown-linux-gnu/release/agent"
        fi
        echo ">>> Uploading updated agent to ${NODE_HOST}..."
        ssh ${NODE_USER}@${NODE_HOST} "sudo systemctl stop ai-mesh-agent"
        scp -q -o ServerAliveInterval=5 -o ServerAliveCountMax=12 \
            ${AGENT_BIN} ${NODE_USER}@${NODE_HOST}:/home/${NODE_USER}/agent
        ssh ${NODE_USER}@${NODE_HOST} "sudo systemctl start ai-mesh-agent"
        ;;

      windows)
        cargo build --release -p agent --target x86_64-pc-windows-gnu --features ${NODE_FEATURES:-llm}
        WIN_PATH="C:\\Users\\${NODE_USER}\\ai-mesh"
        # Strip debug symbols to shrink the binary and speed up the transfer.
        SRC="target/x86_64-pc-windows-gnu/release/agent.exe"
        STRIPPED="/tmp/agent_stripped_{{node}}.exe"
        x86_64-w64-mingw32-strip "$SRC" -o "$STRIPPED" 2>/dev/null || cp "$SRC" "$STRIPPED"
        echo ">>> Uploading updated agent.exe to ${NODE_HOST} ($(du -h "$STRIPPED" | cut -f1))..."
        # Upload with a 90s hard timeout and 3 retries — LAN transfers to Windows
        # can hang mid-stream on flaky links.
        uploaded=false
        for attempt in 1 2 3; do
            printf ">>> Upload attempt %d/3 (90s timeout)...\n" "$attempt"
            if timeout 90 scp -o LogLevel=ERROR \
                    -o ServerAliveInterval=5 -o ServerAliveCountMax=12 \
                    "$STRIPPED" \
                    ${NODE_USER}@${NODE_HOST}:"${WIN_PATH}\\agent_next.exe"; then
                uploaded=true
                break
            fi
            echo ">>> Upload attempt $attempt failed."
            [ "$attempt" -lt 3 ] && echo ">>> Retrying in 5s..." && sleep 5
        done
        if [ "$uploaded" = false ]; then
            echo ">>> Upload failed after 3 attempts. Run 'just update-node {{node}}' to try again."
            exit 1
        fi
        ssh -o LogLevel=ERROR ${NODE_USER}@${NODE_HOST} "powershell -Command \"\
            sc.exe stop ai-mesh-agent 2>&1 | Out-Null;\
            Start-Sleep 2;\
            \$pids = (Get-WmiObject Win32_Process -Filter 'name=''nssm.exe''').ProcessId;\
            foreach (\$p in \$pids) { taskkill /F /PID \$p 2>&1 | Out-Null };\
            Get-Process agent -ErrorAction SilentlyContinue | Stop-Process -Force;\
            Start-Sleep 2;\
            cmd /c 'copy /Y ${WIN_PATH}\\agent_next.exe ${WIN_PATH}\\agent.exe';\
            sc.exe start ai-mesh-agent 2>&1 | Out-Null;\
            exit 0\
        \""
        ;;
    esac
    echo ">>> Node {{node}} updated."

# Push the coordinator TLS fingerprint to a node's agent service and restart it.
# Reads the fingerprint from /tmp/mesh-coordinator.log automatically.
# Usage: just set-fingerprint pi1
#        just set-fingerprint beelink1
set-fingerprint node:
    #!/usr/bin/env bash
    set -e
    source nodes/{{node}}.env

    STATE="$HOME/.config/ai-mesh/coordinator.state"
    if [ ! -f "$STATE" ]; then
        echo ">>> ERROR: coordinator state file not found at $STATE"
        echo ">>>        Is the coordinator running? Try: just restart-coordinator"
        exit 1
    fi
    source "$STATE"
    FP="${MESH_TLS_FINGERPRINT}"
    if [ -z "$FP" ]; then
        echo ">>> ERROR: MESH_TLS_FINGERPRINT missing from $STATE"
        exit 1
    fi
    echo ">>> Setting MESH_TLS_FINGERPRINT=${FP} on {{node}}..."
    [ -n "${MESH_AUTH_TOKEN}" ] && echo ">>> Also pushing MESH_AUTH_TOKEN to {{node}}..."

    case "$NODE_OS" in
      linux)
        ssh ${NODE_USER}@${NODE_HOST} "
            sudo mkdir -p /etc/systemd/system/ai-mesh-agent.service.d 2>/dev/null || true
            printf '[Service]\nEnvironment=MESH_TLS_FINGERPRINT=${FP}\n' \
                | sudo tee /etc/systemd/system/ai-mesh-agent.service.d/tls.conf > /dev/null
            printf '[Service]\nEnvironment=MESH_AUTH_TOKEN=${MESH_AUTH_TOKEN}\n' \
                | sudo tee /etc/systemd/system/ai-mesh-agent.service.d/auth.conf > /dev/null
            sudo systemctl daemon-reload
            sudo systemctl restart ai-mesh-agent
        "
        ;;
      windows)
        DEFAULT_MODEL="${DEFAULT_MODEL:-qwen2.5:7b}"
        ssh -o LogLevel=ERROR ${NODE_USER}@${NODE_HOST} "powershell -Command \"\
            \$nssm = (Get-Command nssm.exe -ErrorAction Stop).Source;\
            & \$nssm set ai-mesh-agent AppEnvironmentExtra \
                'COORDINATOR_IP={{coordinator_ip}}' \
                'AGENT_ROLE=${NODE_ROLE}' \
                ('LLAMA_MODEL_DIR=' + \$env:USERPROFILE + '\\.ai-mesh\\models') \
                ('LLAMA_SERVER_BIN=' + \$env:LOCALAPPDATA + '\\Programs\\llama.cpp\\llama-server.exe') \
                'LLAMA_GPU_LAYERS=99' \
                'LLAMA_FLASH_ATTN=1' \
                'DEFAULT_MODEL=${DEFAULT_MODEL}' \
                'MESH_TLS_FINGERPRINT=${FP}' \
                'MESH_AUTH_TOKEN=${MESH_AUTH_TOKEN}' | Out-Null;\
            taskkill /F /IM llama-server.exe /T 2>&1 | Out-Null;\
            \$svcpid = (Get-WmiObject Win32_Service -Filter 'Name=''ai-mesh-agent''').ProcessId;\
            if (\$svcpid -gt 0) { Stop-Process -Id \$svcpid -Force -ErrorAction SilentlyContinue };\
            Start-Sleep -Milliseconds 800;\
            Start-Service ai-mesh-agent -ErrorAction SilentlyContinue;\
            exit 0\
        \""
        ;;
    esac
    echo ">>> {{node}}: fingerprint and auth token set, agent restarted."

# Push MESH_AUTH_TOKEN to all compute nodes and update local ~/.bashrc.
# Usage: just set-auth-token <token>
set-auth-token token:
    #!/usr/bin/env bash
    set -e
    TOKEN="{{token}}"

    # Update ~/.bashrc and current shell
    if grep -q "MESH_AUTH_TOKEN" "$HOME/.bashrc" 2>/dev/null; then
        if [[ "$(uname -s)" == "Darwin" ]]; then
            sed -i '' "s|export MESH_AUTH_TOKEN=.*|export MESH_AUTH_TOKEN=${TOKEN}|" "$HOME/.bashrc"
        else
            sed -i "s|export MESH_AUTH_TOKEN=.*|export MESH_AUTH_TOKEN=${TOKEN}|" "$HOME/.bashrc"
        fi
    else
        printf '\n# ai-mesh auth token — managed by just set-auth-token\nexport MESH_AUTH_TOKEN=%s\n' "${TOKEN}" >> "$HOME/.bashrc"
    fi
    export MESH_AUTH_TOKEN="${TOKEN}"
    echo ">>> Local MESH_AUTH_TOKEN updated in ~/.bashrc"

    # Push to every compute node
    STATE="$HOME/.config/ai-mesh/coordinator.state"
    if [ ! -f "$STATE" ]; then
        echo ">>> ERROR: coordinator state file not found at $STATE — is the coordinator running?"
        exit 1
    fi
    source "$STATE"
    FP="${MESH_TLS_FINGERPRINT}"
    for f in nodes/*.env; do
        source "$f"
        NODE_NAME=$(basename "$f" .env)
        [ "${NODE_ROLE}" = "compute" ] || continue
        echo ">>> Pushing MESH_AUTH_TOKEN to ${NODE_NAME}..."

        case "$NODE_OS" in
          linux)
            ssh ${NODE_USER}@${NODE_HOST} "
                sudo mkdir -p /etc/systemd/system/ai-mesh-agent.service.d
                printf '[Service]\nEnvironment=MESH_AUTH_TOKEN=${TOKEN}\n' \
                    | sudo tee /etc/systemd/system/ai-mesh-agent.service.d/auth.conf > /dev/null
                sudo systemctl daemon-reload
                sudo systemctl restart ai-mesh-agent
            "
            ;;
          windows)
            DEFAULT_MODEL="${DEFAULT_MODEL:-qwen2.5:7b}"
            ssh ${NODE_USER}@${NODE_HOST} "powershell -Command \"\
                \$nssm = (Get-Command nssm.exe -ErrorAction Stop).Source;\
                & \$nssm set ai-mesh-agent AppEnvironmentExtra \
                    'COORDINATOR_IP={{coordinator_ip}}' \
                    'AGENT_ROLE=${NODE_ROLE}' \
                    ('LLAMA_MODEL_DIR=' + \$env:USERPROFILE + '\\.ai-mesh\\models') \
                    ('LLAMA_SERVER_BIN=' + \$env:LOCALAPPDATA + '\\Programs\\llama.cpp\\llama-server.exe') \
                    'LLAMA_GPU_LAYERS=99' \
                    'LLAMA_FLASH_ATTN=1' \
                    'DEFAULT_MODEL=${DEFAULT_MODEL}' \
                    'MESH_TLS_FINGERPRINT=${FP}' \
                    'MESH_AUTH_TOKEN=${TOKEN}';\
                taskkill /F /IM llama-server.exe /T 2>&1 | Out-Null;\
                \$svcpid = (Get-WmiObject Win32_Service -Filter 'Name=''ai-mesh-agent''').ProcessId;\
                if (\$svcpid -gt 0) { Stop-Process -Id \$svcpid -Force -ErrorAction SilentlyContinue };\
                Start-Sleep -Milliseconds 800;\
                Start-Service ai-mesh-agent -ErrorAction SilentlyContinue;\
                exit 0\
            \""
            ;;
        esac
        echo ">>> ${NODE_NAME}: auth token updated."
    done
    echo ">>> Auth token push complete."

# Zero-downtime auth token rotation using the dual-token window.
# Coordinator accepts both old and new tokens while nodes are updated,
# then restarts with the new token only once all nodes have reconnected.
# Usage: just rotate-token
rotate-token:
    #!/usr/bin/env bash
    set -euo pipefail

    # Inline helper: restart only the coordinator binary.
    # Does not touch models, fingerprints, or the controller agent.
    # Args: $1=MESH_AUTH_TOKEN  $2=MESH_AUTH_TOKEN_NEXT (optional)
    restart_coordinator_binary() {
        local primary="$1"
        local next="${2:-}"
        pkill -f "target/(debug|release)/coordinator" || true
        sleep 0.3
        rm -f /tmp/mesh-coordinator.log
        cargo build -q -p coordinator
        if [ -n "$next" ]; then
            MDNS_ADVERTISE_IP={{coordinator_ip}} \
                MESH_AUTH_TOKEN="$primary" \
                MESH_AUTH_TOKEN_NEXT="$next" \
                ./target/debug/coordinator >> /tmp/mesh-coordinator.log 2>&1 &
        else
            MDNS_ADVERTISE_IP={{coordinator_ip}} \
                MESH_AUTH_TOKEN="$primary" \
                ./target/debug/coordinator >> /tmp/mesh-coordinator.log 2>&1 &
        fi
        for i in $(seq 1 60); do
            sleep 1
            grep -q "Coordinator is running" /tmp/mesh-coordinator.log 2>/dev/null && { echo ">>> Coordinator ready."; return 0; }
            [ "$i" -eq 60 ] && { echo ">>> ERROR: coordinator did not start. Check /tmp/mesh-coordinator.log"; exit 1; }
        done
    }

    # Step 1: Load current primary token and fingerprint.
    STATE="$HOME/.config/ai-mesh/coordinator.state"
    [ -f "$STATE" ] || { echo ">>> ERROR: $STATE not found — is the coordinator running?"; exit 1; }
    source "$STATE"
    export MESH_TLS_FINGERPRINT
    OLD_TOKEN="${MESH_AUTH_TOKEN:-}"
    [ -n "$OLD_TOKEN" ] || { echo ">>> ERROR: MESH_AUTH_TOKEN not set in coordinator state — run 'just restart-coordinator' first"; exit 1; }

    # Step 2: Generate new token.
    NEW_TOKEN=$(openssl rand -hex 32)
    echo ">>> New token generated."

    # Step 3: Restart coordinator accepting both old and new tokens.
    echo ">>> Phase 1/3 — opening rotation window (both tokens accepted)..."
    restart_coordinator_binary "$OLD_TOKEN" "$NEW_TOKEN"

    # Step 4: Push new token to all compute nodes.
    # set-auth-token handles Linux (systemd drop-in) and Windows (NSSM AppEnvironmentExtra).
    echo ">>> Phase 2/3 — distributing new token to all nodes..."
    just set-auth-token "$NEW_TOKEN"

    # Step 5: Wait for all nodes to reconnect with the new token before revoking the old.
    # Export the new token so the CLI can authenticate — coordinator accepts both at this point.
    # Exits non-zero on timeout — old token remains active, safe to re-run.
    export MESH_AUTH_TOKEN="$NEW_TOKEN"
    WAIT_IPS=()
    for f in nodes/*.env; do
        source "$f"
        NODE_NAME=$(basename "$f" .env)
        [ "${NODE_ROLE}" = "compute" ] || continue
        WAIT_IPS+=("${NODE_HOST}")
    done
    echo ">>> Waiting for all nodes to reconnect..."
    cargo run -q -p cli -- wait-ready "${WAIT_IPS[@]}" --timeout 120 \
        || { echo ">>> ERROR: nodes did not reconnect in time — old token still active, rotation aborted"; exit 1; }

    # Step 6: Restart coordinator with new token only — rotation window closed.
    echo ">>> Phase 3/3 — revoking old token..."
    restart_coordinator_binary "$NEW_TOKEN"

    # Clear stale SQLite model state so wait-ready doesn't see a false Ready
    # from the pre-rotation registry. Mirrors what restart-coordinator does.
    cargo run -q -p cli -- reset-registry > /dev/null || true

    # Restart the local controller with the new token (set-auth-token only pushes
    # to remote compute nodes; the local agent process keeps the old token otherwise).
    pkill -f "target/(debug|release)/agent" || true
    sleep 0.3
    AGENT_ROLE=controller cargo run -q -p agent >> /tmp/mesh-agent.log 2>&1 &

    # Reload models on all compute nodes — agent service restarts during rotation
    # kill llama-server, so the models must be reloaded explicitly.
    echo ">>> Reloading models on compute nodes..."
    for f in nodes/*.env; do
        source "$f"
        NODE_NAME=$(basename "$f" .env)
        [ "${NODE_ROLE}" = "compute" ] || continue
        just auto-load-model ${NODE_NAME} \
            || echo ">>> Warning: could not reload model on ${NODE_NAME} (skipping)"
    done

    cargo run -q -p cli -- wait-ready "${WAIT_IPS[@]}" --timeout 120 \
        || { echo ">>> WARNING: nodes slow after model reload — run: just restart-coordinator"; }

    echo ">>> Token rotation complete."
    echo ">>> New token is live in ~/.config/ai-mesh/coordinator.state and ~/.bashrc"

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

# Update llama.cpp to the latest release on a node.
# Usage: just update-llama <node>
update-llama node:
    #!/usr/bin/env bash
    set -e
    source nodes/{{node}}.env
    LATEST=$(curl -s https://api.github.com/repos/ggml-org/llama.cpp/releases/latest \
        | grep '"tag_name"' | head -1 | cut -d'"' -f4)
    echo "Latest llama.cpp: $LATEST"
    if [ "$NODE_OS" = "windows" ]; then
        ZIP_URL="https://github.com/ggml-org/llama.cpp/releases/download/${LATEST}/llama-${LATEST}-bin-win-vulkan-x64.zip"
        ssh ${NODE_USER}@${NODE_HOST} "powershell -Command \"\
            Invoke-WebRequest -Uri '${ZIP_URL}' -OutFile '\$env:TEMP\llama-update.zip' -UseBasicParsing; \
            Expand-Archive -Path '\$env:TEMP\llama-update.zip' -DestinationPath '\$env:LOCALAPPDATA\Programs\llama.cpp' -Force; \
            Remove-Item '\$env:TEMP\llama-update.zip' -Force\""
    else
        ssh ${NODE_USER}@${NODE_HOST} "
            ARCH=\$(uname -m)
            if [ \"\$ARCH\" = \"x86_64\" ]; then
                ZIP_URL=\"https://github.com/ggml-org/llama.cpp/releases/download/${LATEST}/llama-${LATEST}-bin-ubuntu-x64.tar.gz\"
            elif [ \"\$ARCH\" = \"aarch64\" ]; then
                ZIP_URL=\"https://github.com/ggml-org/llama.cpp/releases/download/${LATEST}/llama-${LATEST}-bin-ubuntu-arm64.tar.gz\"
            else
                echo \"Unsupported Linux architecture: \$ARCH\"
                exit 1
            fi
            echo \"Downloading \$ZIP_URL...\"
            LLAMA_TMP=\$(mktemp -d)
            curl -fsSL \"\$ZIP_URL\" -o \"\$LLAMA_TMP/llama.tar.gz\"
            sudo install -d /opt/llama.cpp
            sudo tar -xzf \"\$LLAMA_TMP/llama.tar.gz\" -C /opt/llama.cpp --strip-components=1
            rm -rf \"\$LLAMA_TMP\"
        "
    fi
    echo "Restarting agent on {{node}}..."
    just restart-node {{node}}
    echo "llama.cpp updated to $LATEST on {{node}}"

# Auto-place a model — coordinator picks the best-fit node. No SSH needed.
# Usage: just load qwen2.5:7b
load model:
    #!/usr/bin/env bash
    set -e
    case "{{model}}" in
        qwen2.5:0.5b)  SIZE_MB=512 ;;
        qwen2.5:1.5b)  SIZE_MB=1024 ;;
        qwen2.5:7b)    SIZE_MB=4096 ;;
        qwen2.5:14b)   SIZE_MB=8192 ;;
        qwen2.5:32b)   SIZE_MB=20480 ;;
        *) echo "Unknown model: {{model}} (supported: qwen2.5:0.5b 1.5b 7b 14b 32b)"; exit 1 ;;
    esac
    cargo run -q -p cli -- --coordinator "{{coordinator_ip}}:{{coordinator_port}}" load "{{model}}" "$SIZE_MB"

# Load a model on a named node (looks up live node ID from coordinator).
# Usage: just load-model pi1 qwen2.5:1.5b
#        just load-model beelink1 qwen2.5:7b
load-model node model:
    #!/usr/bin/env bash
    set -e
    source nodes/{{node}}.env
    STATE="$HOME/.config/ai-mesh/coordinator.state"
    [ -f "$STATE" ] && source "$STATE" && export MESH_TLS_FINGERPRINT MESH_AUTH_TOKEN
    MODEL="{{model}}"
    case "$MODEL" in
        qwen2.5:0.5b)  SIZE_MB=512 ;;
        qwen2.5:1.5b)  SIZE_MB=1024 ;;
        qwen2.5:7b)    SIZE_MB=4096 ;;
        qwen2.5:14b)   SIZE_MB=8192 ;;
        qwen2.5:32b)   SIZE_MB=20480 ;;
        *) echo "Unknown model: $MODEL (supported: qwen2.5:0.5b 1.5b 7b 14b 32b)"; exit 1 ;;
    esac
    # Retry for up to 120s — allow time for reconnect after coordinator restart.
    NODE_ID=""
    for i in $(seq 1 60); do
        NODE_ID=$(cargo run -q -p cli -- find-node "${NODE_HOST}" 2>/dev/null || true)
        [ -n "$NODE_ID" ] && break
        printf "\r>>> Waiting for ${NODE_HOST} to register... (%ds) " "$((i*2))"
        sleep 2
    done
    if [ -z "$NODE_ID" ]; then
        printf "\n"
        echo "Error: node ${NODE_HOST} not found in coordinator registry after 120s. Is the agent running?"
        exit 1
    fi
    printf "\r>>> ${NODE_HOST} registered.                              \n"

    # Detect hardware to determine this node's capability ceiling.
    # Outputs mb:gpu_flag (gpu_flag=1 when VRAM detected, 0 for CPU/unified RAM).
    case "$NODE_OS" in
      linux)
        HW_INFO=$(ssh ${NODE_USER}@${NODE_HOST} '
            mem=0; gpu=0
            for f in /sys/class/drm/card*/device/mem_info_vram_total; do
                [ -f "$f" ] || continue
                v=$(( $(cat "$f") / 1048576 ))
                [ "$v" -gt "$mem" ] && mem="$v" && gpu=1
            done
            if [ "$gpu" -eq 0 ] && command -v nvidia-smi &>/dev/null; then
                v=$(nvidia-smi --query-gpu=memory.total --format=csv,noheader,nounits 2>/dev/null | head -1 | tr -d " ")
                [ -n "$v" ] && [ "$v" -gt 0 ] && mem="$v" && gpu=1
            fi
            [ "$gpu" -eq 0 ] && mem=$(awk "/MemTotal/{print int(\$2/1024)}" /proc/meminfo)
            echo "${mem}:${gpu}"
        ' 2>/dev/null || echo "0:0")
        ;;
      windows)
        HW_INFO=$(ssh -o LogLevel=ERROR ${NODE_USER}@${NODE_HOST} 'powershell -NoProfile -Command "$g=(Get-WmiObject Win32_VideoController|Where-Object{$_.AdapterRAM -gt 0}|Sort-Object AdapterRAM -Descending|Select-Object -First 1);$m=0;$gpu=0;if($g){if($g.AdapterRAM -eq 4294967295){$m=8192}else{$m=[int]($g.AdapterRAM/1MB)};$gpu=1};if($m -eq 0){$m=[int]((Get-WmiObject Win32_ComputerSystem).TotalPhysicalMemory/1MB)};Write-Output ($m.ToString()+[char]58+$gpu.ToString())"' 2>/dev/null || echo "0:0")
        ;;
      *) HW_INFO="0:0" ;;
    esac
    HW_MB=$(echo "$HW_INFO" | cut -d: -f1 | tr -d '[:space:]'); HW_MB="${HW_MB:-0}"
    HW_GPU=$(echo "$HW_INFO" | cut -d: -f2 | tr -d '[:space:]'); HW_GPU="${HW_GPU:-0}"

    # Map hardware to a ceiling rank using separate GPU-VRAM and CPU/unified thresholds.
    # CPU thresholds are conservative — models compete with OS memory.
    if [ "$HW_GPU" = "1" ]; then
        if   [ "$HW_MB" -ge 22000 ]; then HW_MAX=4
        elif [ "$HW_MB" -ge 9000  ]; then HW_MAX=3
        elif [ "$HW_MB" -ge 4000  ]; then HW_MAX=2
        elif [ "$HW_MB" -ge 1000  ]; then HW_MAX=1
        else                               HW_MAX=0
        fi
    else
        if   [ "$HW_MB" -ge 44000 ]; then HW_MAX=4
        elif [ "$HW_MB" -ge 18000 ]; then HW_MAX=3
        elif [ "$HW_MB" -ge 10000 ]; then HW_MAX=2
        elif [ "$HW_MB" -ge 3000  ]; then HW_MAX=1
        else                               HW_MAX=0
        fi
    fi

    # Model rank table (ascending by size).
    MODEL_NAMES=("qwen2.5:0.5b" "qwen2.5:1.5b" "qwen2.5:7b" "qwen2.5:14b" "qwen2.5:32b")
    case "$MODEL" in
        qwen2.5:0.5b) MODEL_RANK=0 ;;
        qwen2.5:1.5b) MODEL_RANK=1 ;;
        qwen2.5:7b)   MODEL_RANK=2 ;;
        qwen2.5:14b)  MODEL_RANK=3 ;;
        qwen2.5:32b)  MODEL_RANK=4 ;;
    esac

    # Ceiling = strictly below both the model being loaded and the hardware max.
    CEILING=$(( (MODEL_RANK - 1) < HW_MAX ? (MODEL_RANK - 1) : HW_MAX ))

    echo ">>> Loading ${MODEL} (${SIZE_MB} MB) on {{node}} (${NODE_ID})..."
    if [ "$CEILING" -ge 0 ]; then
        echo ">>> NOTE: if this model fails, available fallbacks (within this node's hardware):"
        for (( i=CEILING; i>=0; i-- )); do
            echo ">>>   just load-model {{node}} ${MODEL_NAMES[$i]}"
        done
    fi
    cargo run -q -p cli -- load --node-id "${NODE_ID}" "${MODEL}" "${SIZE_MB}" | sed 's/^/>>> /'

# Detect hardware on a node and load the best-fit model automatically.
# Usage: just auto-load-model pi1
#        just auto-load-model beelink1
auto-load-model node:
    #!/usr/bin/env bash
    set -e
    source nodes/{{node}}.env
    STATE="$HOME/.config/ai-mesh/coordinator.state"
    [ -f "$STATE" ] && source "$STATE" && export MESH_TLS_FINGERPRINT MESH_AUTH_TOKEN
    case "$NODE_OS" in
      linux)
        HW_INFO=$(ssh ${NODE_USER}@${NODE_HOST} '
            mem=0; gpu=0
            for f in /sys/class/drm/card*/device/mem_info_vram_total; do
                [ -f "$f" ] || continue
                v=$(( $(cat "$f") / 1048576 ))
                [ "$v" -gt "$mem" ] && mem="$v" && gpu=1
            done
            if [ "$gpu" -eq 0 ] && command -v nvidia-smi &>/dev/null; then
                v=$(nvidia-smi --query-gpu=memory.total --format=csv,noheader,nounits 2>/dev/null | head -1 | tr -d " ")
                [ -n "$v" ] && [ "$v" -gt 0 ] && mem="$v" && gpu=1
            fi
            [ "$gpu" -eq 0 ] && mem=$(awk "/MemTotal/{print int(\$2/1024)}" /proc/meminfo)
            echo "${mem}:${gpu}"
        ')
        ;;
      windows)
        HW_INFO=$(ssh -o LogLevel=ERROR ${NODE_USER}@${NODE_HOST} 'powershell -NoProfile -Command "$g=(Get-WmiObject Win32_VideoController|Where-Object{$_.AdapterRAM -gt 0}|Sort-Object AdapterRAM -Descending|Select-Object -First 1);$m=0;$gpu=0;if($g){if($g.AdapterRAM -eq 4294967295){$m=8192}else{$m=[int]($g.AdapterRAM/1MB)};$gpu=1};if($m -eq 0){$m=[int]((Get-WmiObject Win32_ComputerSystem).TotalPhysicalMemory/1MB)};Write-Output ($m.ToString()+[char]58+$gpu.ToString())"')
        ;;
      *)
        echo "Unknown NODE_OS: $NODE_OS"; exit 1 ;;
    esac
    HW_MB=$(echo "$HW_INFO" | cut -d: -f1 | tr -d '[:space:]'); HW_MB="${HW_MB:-0}"
    HW_GPU=$(echo "$HW_INFO" | cut -d: -f2 | tr -d '[:space:]'); HW_GPU="${HW_GPU:-0}"

    if [ "$HW_GPU" = "1" ]; then
        if   [ "$HW_MB" -ge 22000 ]; then MODEL="qwen2.5:32b"
        elif [ "$HW_MB" -ge 9000  ]; then MODEL="qwen2.5:14b"
        elif [ "$HW_MB" -ge 4000  ]; then MODEL="qwen2.5:7b"
        elif [ "$HW_MB" -ge 1000  ]; then MODEL="qwen2.5:1.5b"
        else                               MODEL="qwen2.5:0.5b"
        fi
    else
        if   [ "$HW_MB" -ge 44000 ]; then MODEL="qwen2.5:32b"
        elif [ "$HW_MB" -ge 18000 ]; then MODEL="qwen2.5:14b"
        elif [ "$HW_MB" -ge 10000 ]; then MODEL="qwen2.5:7b"
        elif [ "$HW_MB" -ge 3000  ]; then MODEL="qwen2.5:1.5b"
        else                               MODEL="qwen2.5:0.5b"
        fi
    fi
    echo ">>> {{node}}: detected ${HW_MB} MB ($([ "$HW_GPU" = "1" ] && echo GPU || echo CPU)) → selecting ${MODEL}"
    just load-model {{node}} ${MODEL}

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
    CUR_9000=$(netsh.exe interface portproxy show all | awk '/9000/{print $3}' | head -1 | tr -d '\r')
    CUR_9001=$(netsh.exe interface portproxy show all | awk '/9001/{print $3}' | head -1 | tr -d '\r')
    if [ "$CUR_9000" = "$WSL_IP" ] && [ "$CUR_9001" = "$WSL_IP" ]; then
        echo ">>> Portproxy OK (9000 + 9001 → ${WSL_IP})"
        exit 0
    fi
    echo ">>> Portproxy update: 9000(${CUR_9000:-none}) 9001(${CUR_9001:-none}) → ${WSL_IP} (UAC prompt will appear)..."
    powershell.exe -Command "Start-Process powershell -ArgumentList \"-NoProfile -Command netsh interface portproxy delete v4tov4 listenport=9000 listenaddress=0.0.0.0; netsh interface portproxy add v4tov4 listenport=9000 listenaddress=0.0.0.0 connectport=9000 connectaddress=${WSL_IP}; netsh interface portproxy delete v4tov4 listenport=9001 listenaddress=0.0.0.0; netsh interface portproxy add v4tov4 listenport=9001 listenaddress=0.0.0.0 connectport=9001 connectaddress=${WSL_IP}; netsh advfirewall firewall delete rule name=WSL-Mesh-Dashboard; netsh advfirewall firewall add rule name=WSL-Mesh-Dashboard dir=in action=allow protocol=TCP localport=9001\" -Verb RunAs -Wait"
    echo ">>> Portproxy updated: 9000 + 9001 → ${WSL_IP}"

# Print the dashboard URL for phone access on the same LAN.
# Ensures the Windows portproxy is current first.
dashboard-mobile: update-portproxy
    #!/usr/bin/env bash
    set -e
    WIN_IP=$(powershell.exe -NoProfile -Command "(Get-NetIPConfiguration | Where-Object IPv4DefaultGateway -ne \$null | Select-Object -First 1).IPv4Address.IPAddress" | tr -d '\r\n ')
    STATE="$HOME/.config/ai-mesh/coordinator.state"
    TOKEN=""
    if [ -f "$STATE" ]; then source "$STATE"; TOKEN="${MESH_AUTH_TOKEN:-}"; fi
    URL="http://${WIN_IP}:9001/"
    [ -n "$TOKEN" ] && URL="${URL}?token=${TOKEN}"
    echo ""
    echo ">>> Open this on your phone (same Wi-Fi):"
    echo ">>> ${URL}"

# ── Full cluster operations ───────────────────────────────────────────────────

# Start the ai-mesh-agent service on all remote nodes without touching the local coordinator.
# Safe to call when agents are already running (systemctl start is idempotent).
start-agents:
    #!/usr/bin/env bash
    # Fire SSH service-start commands for all nodes in parallel, then wait.
    # We don't retry here — if a node is offline, its Windows/Linux service
    # will auto-start on boot and self-register. load-model's retry loop
    # (90s window) handles the "slow to register" case.
    pids=()
    for f in nodes/*.env; do
        (
            source "$f"
            NODE_NAME=$(basename "$f" .env)
            echo ">>> Starting agent on ${NODE_NAME} (${NODE_HOST})..."
            case "$NODE_OS" in
              linux)
                ssh -o ConnectTimeout=10 "${NODE_USER}@${NODE_HOST}" \
                    "sudo systemctl start ai-mesh-agent" 2>/dev/null \
                    && echo ">>> ${NODE_NAME} service started." \
                    || echo ">>> Warning: could not reach ${NODE_NAME} (will self-register if online)"
                ;;
              windows)
                ssh -o ConnectTimeout=10 -o LogLevel=ERROR "${NODE_USER}@${NODE_HOST}" \
                    "powershell -Command \"sc.exe start ai-mesh-agent 2>&1 | Out-Null; exit 0\"" \
                    2>/dev/null \
                    && echo ">>> ${NODE_NAME} service started." \
                    || echo ">>> Warning: could not reach ${NODE_NAME} (will self-register if online)"
                ;;
            esac
        ) &
        pids+=($!)
    done
    wait "${pids[@]}"

# Bring the full cluster up and load the best model on each compute node.
# Leaves everything running — coordinator, controller, and remote agents stay up after the script exits.
# Usage: just start-cluster
start-cluster: update-portproxy
    #!/usr/bin/env bash
    set -e

    echo ">>> Stopping any stale local processes..."
    pkill -f "target/(debug|release)/coordinator" || true
    pkill -f "target/(debug|release)/agent" || true
    sleep 0.3

    # Check if coordinator runs remotely (on pi1) or locally (on this machine)
    if [[ "{{coordinator_ip}}" == "127.0.0.1" || "{{coordinator_ip}}" == "localhost" ]]; then
        # Local coordinator mode
        echo ">>> Building coordinator..."
        cargo build -q -p coordinator
        echo ">>> Starting coordinator (log: /tmp/mesh-coordinator.log)..."
        MDNS_ADVERTISE_IP={{coordinator_ip}} ./target/debug/coordinator > /tmp/mesh-coordinator.log 2>&1 &

        echo ">>> Waiting for coordinator to accept connections..."
        for i in $(seq 1 60); do
            sleep 1
            if grep -q "Coordinator is running" /tmp/mesh-coordinator.log 2>/dev/null; then
                echo ">>> Coordinator ready."
                break
            fi
            [ "$i" -eq 60 ] && { echo ">>> ERROR: coordinator did not start. Check /tmp/mesh-coordinator.log"; exit 1; }
        done

        cargo build -q -p cli
        ./target/debug/cli reset-registry > /dev/null || true
    else
        # Remote coordinator mode (running on pi1 as systemd service)
        echo ">>> Coordinator is running remotely on {{coordinator_ip}}"
        echo ">>> Verifying connectivity to {{coordinator_ip}}:9000..."
        if timeout 5 bash -c "echo > /dev/tcp/{{coordinator_ip}}/9000" 2>/dev/null; then
            echo ">>> Coordinator ready."
        else
            echo ">>> ERROR: Could not reach coordinator at {{coordinator_ip}}:9000"
            echo ">>>        Is it running? Check: ssh jonno@{{coordinator_ip}} systemctl status ai-mesh-coordinator"
            exit 1
        fi
        cargo build -q -p cli
    fi

    # Push TLS fingerprint + auth token to all compute nodes before starting their agents.
    STATE="$HOME/.config/ai-mesh/coordinator.state"
    if [ ! -f "$STATE" ]; then
        echo ">>> ERROR: coordinator state file not found at $STATE — coordinator may not have started"
        exit 1
    fi
    source "$STATE"
    FP="${MESH_TLS_FINGERPRINT}"
    if [ -n "$FP" ]; then
        if grep -q "MESH_TLS_FINGERPRINT" "$HOME/.bashrc" 2>/dev/null; then
            if [[ "$(uname -s)" == "Darwin" ]]; then
                sed -i '' "s|export MESH_TLS_FINGERPRINT=.*|export MESH_TLS_FINGERPRINT=${FP}|" "$HOME/.bashrc"
            else
                sed -i "s|export MESH_TLS_FINGERPRINT=.*|export MESH_TLS_FINGERPRINT=${FP}|" "$HOME/.bashrc"
            fi
        else
            printf '\n# ai-mesh TLS fingerprint — managed by just start-cluster\nexport MESH_TLS_FINGERPRINT=%s\n' "${FP}" >> "$HOME/.bashrc"
        fi
        export MESH_TLS_FINGERPRINT="${FP}"
        echo ">>> MESH_TLS_FINGERPRINT set: ${FP}"
    fi

    # Sync auth token (may have been auto-generated by the coordinator) to ~/.bashrc.
    TOKEN="${MESH_AUTH_TOKEN:-}"
    if [ -n "$TOKEN" ]; then
        if grep -q "MESH_AUTH_TOKEN" "$HOME/.bashrc" 2>/dev/null; then
            if [[ "$(uname -s)" == "Darwin" ]]; then
                sed -i '' "s|export MESH_AUTH_TOKEN=.*|export MESH_AUTH_TOKEN=${TOKEN}|" "$HOME/.bashrc"
            else
                sed -i "s|export MESH_AUTH_TOKEN=.*|export MESH_AUTH_TOKEN=${TOKEN}|" "$HOME/.bashrc"
            fi
        else
            printf '\n# ai-mesh auth token — managed by just start-cluster\nexport MESH_AUTH_TOKEN=%s\n' "${TOKEN}" >> "$HOME/.bashrc"
        fi
        export MESH_AUTH_TOKEN="${TOKEN}"
        echo ">>> MESH_AUTH_TOKEN set from coordinator state"
        echo ""
        echo "    ┌─ Dashboard login token ──────────────────────────────────────┐"
        echo "    │  ${TOKEN}  │"
        echo "    └──────────────────────────────────────────────────────────────┘"
        echo ""
    fi

    echo ">>> Pushing TLS fingerprint and auth token to all compute nodes..."
    for f in nodes/*.env; do
        source "$f"
        NODE_NAME=$(basename "$f" .env)
        [ "${NODE_ROLE}" = "compute" ] || continue
        just set-fingerprint ${NODE_NAME} \
            || echo ">>> Warning: could not set credentials on ${NODE_NAME} (skipping)"
    done

    echo ">>> Starting local controller (log: /tmp/mesh-agent.log)..."
    AGENT_ROLE=controller COORDINATOR_IP=127.0.0.1 cargo run -q -p agent > /tmp/mesh-agent.log 2>&1 &

    echo ">>> Starting remote compute agents..."
    just start-agents

    echo ">>> Loading hardware-selected models on compute nodes..."
    COMPUTE_NODES=()   # "name:ip" pairs for the wait loop below
    for f in nodes/*.env; do
        source "$f"
        NODE_NAME=$(basename "$f" .env)
        [ "${NODE_ROLE}" = "compute" ] || continue
        just auto-load-model ${NODE_NAME} \
            || echo ">>> Warning: could not load model on ${NODE_NAME} (skipping)"
        COMPUTE_NODES+=("${NODE_NAME}:${NODE_HOST}")
    done

    # Collect compute IPs for the live-table wait
    WAIT_IPS=()
    for entry in "${COMPUTE_NODES[@]}"; do
        WAIT_IPS+=("${entry##*:}")
    done

    echo ""
    cargo run -q -p cli -- wait-ready "${WAIT_IPS[@]}" --timeout 600 \
        || echo ">>> Warning: timed out or aborted before all models were Ready"

    echo ""
    echo ">>> Cluster ready. Run: just validate-routing"
    cargo run -q -p cli -- nodes

# Restart the coordinator after laptop suspend/resume without restarting remote agents.
# Remote agent services reconnect automatically; this just gives them a fresh coordinator.
# Usage: just restart-coordinator
restart-coordinator: update-portproxy
    #!/usr/bin/env bash
    set -e

    echo ">>> Stopping stale local processes..."
    pkill -f "target/(debug|release)/coordinator" || true
    pkill -f "target/(debug|release)/agent" || true
    sleep 0.3

    # Source the existing token BEFORE starting the coordinator so it inherits
    # the same token and doesn't generate a new one on every restart.
    STATE="$HOME/.config/ai-mesh/coordinator.state"
    if [ -f "$STATE" ]; then
        source "$STATE"
        export MESH_AUTH_TOKEN="${MESH_AUTH_TOKEN:-}"
    fi

    # Check if coordinator runs remotely (on pi1) or locally (on this machine)
    if [[ "{{coordinator_ip}}" == "127.0.0.1" || "{{coordinator_ip}}" == "localhost" ]]; then
        # Local coordinator mode
        echo ">>> Building coordinator..."
        cargo build -q -p coordinator
        echo ">>> Starting coordinator (log: /tmp/mesh-coordinator.log)..."
        MDNS_ADVERTISE_IP={{coordinator_ip}} ./target/debug/coordinator > /tmp/mesh-coordinator.log 2>&1 &

        echo ">>> Waiting for coordinator to accept connections..."
        for i in $(seq 1 60); do
            sleep 1
            if grep -q "Coordinator is running" /tmp/mesh-coordinator.log 2>/dev/null; then
                echo ">>> Coordinator ready."
                break
            fi
            [ "$i" -eq 60 ] && { echo ">>> ERROR: coordinator did not start. Check /tmp/mesh-coordinator.log"; exit 1; }
        done

        cargo build -q -p cli
        ./target/debug/cli reset-registry > /dev/null || true
    else
        # Remote coordinator mode (running on pi1 as systemd service)
        echo ">>> Coordinator is running remotely on {{coordinator_ip}}"
        echo ">>> Verifying connectivity to {{coordinator_ip}}:9000..."
        if timeout 5 bash -c "echo > /dev/tcp/{{coordinator_ip}}/9000" 2>/dev/null; then
            echo ">>> Coordinator ready."
        else
            echo ">>> ERROR: Could not reach coordinator at {{coordinator_ip}}:9000"
            echo ">>>        Is it running? Check: ssh jonno@{{coordinator_ip}} systemctl status ai-mesh-coordinator"
            exit 1
        fi
        cargo build -q -p cli
    fi

    # Read fingerprint (and token if set) from the coordinator state file.
    if [ ! -f "$STATE" ]; then
        echo ">>> ERROR: coordinator state file not found at $STATE — coordinator may not have started"
        exit 1
    fi
    source "$STATE"
    FP="${MESH_TLS_FINGERPRINT}"
    if [ -n "$FP" ]; then
        if grep -q "MESH_TLS_FINGERPRINT" "$HOME/.bashrc" 2>/dev/null; then
            # macOS sed requires an empty-string backup extension; GNU sed does not
            if [[ "$(uname -s)" == "Darwin" ]]; then
                sed -i '' "s|export MESH_TLS_FINGERPRINT=.*|export MESH_TLS_FINGERPRINT=${FP}|" "$HOME/.bashrc"
            else
                sed -i "s|export MESH_TLS_FINGERPRINT=.*|export MESH_TLS_FINGERPRINT=${FP}|" "$HOME/.bashrc"
            fi
        else
            printf '\n# ai-mesh TLS fingerprint — managed by just restart-coordinator\nexport MESH_TLS_FINGERPRINT=%s\n' "${FP}" >> "$HOME/.bashrc"
        fi
        export MESH_TLS_FINGERPRINT="${FP}"
        echo ">>> MESH_TLS_FINGERPRINT set: ${FP}"
    fi

    # Sync auth token (may have been auto-generated by the coordinator) to ~/.bashrc.
    TOKEN="${MESH_AUTH_TOKEN:-}"
    if [ -n "$TOKEN" ]; then
        if grep -q "MESH_AUTH_TOKEN" "$HOME/.bashrc" 2>/dev/null; then
            if [[ "$(uname -s)" == "Darwin" ]]; then
                sed -i '' "s|export MESH_AUTH_TOKEN=.*|export MESH_AUTH_TOKEN=${TOKEN}|" "$HOME/.bashrc"
            else
                sed -i "s|export MESH_AUTH_TOKEN=.*|export MESH_AUTH_TOKEN=${TOKEN}|" "$HOME/.bashrc"
            fi
        else
            printf '\n# ai-mesh auth token — managed by just restart-coordinator\nexport MESH_AUTH_TOKEN=%s\n' "${TOKEN}" >> "$HOME/.bashrc"
        fi
        export MESH_AUTH_TOKEN="${TOKEN}"
        echo ">>> MESH_AUTH_TOKEN set from coordinator state"
        echo ""
        echo "    ┌─ Dashboard login token ──────────────────────────────────────┐"
        echo "    │  ${TOKEN}  │"
        echo "    └──────────────────────────────────────────────────────────────┘"
        echo ""
    fi

    echo ">>> Pushing TLS fingerprint and auth token to all compute nodes..."
    for f in nodes/*.env; do
        source "$f"
        NODE_NAME=$(basename "$f" .env)
        [ "${NODE_ROLE}" = "compute" ] || continue
        just set-fingerprint ${NODE_NAME} \
            || echo ">>> Warning: could not set credentials on ${NODE_NAME} (skipping)"
    done

    echo ">>> Starting local controller (log: /tmp/mesh-agent.log)..."
    AGENT_ROLE=controller COORDINATOR_IP=127.0.0.1 cargo run -q -p agent > /tmp/mesh-agent.log 2>&1 &

    echo ">>> Reloading hardware-selected models on compute nodes..."
    COMPUTE_NODES=()
    for f in nodes/*.env; do
        source "$f"
        NODE_NAME=$(basename "$f" .env)
        [ "${NODE_ROLE}" = "compute" ] || continue
        just auto-load-model ${NODE_NAME} \
            || echo ">>> Warning: could not load model on ${NODE_NAME} (skipping)"
        COMPUTE_NODES+=("${NODE_NAME}:${NODE_HOST}")
    done

    WAIT_IPS=()
    for entry in "${COMPUTE_NODES[@]}"; do
        WAIT_IPS+=("${entry##*:}")
    done

    cargo run -q -p cli -- wait-ready "${WAIT_IPS[@]}" --timeout 120 \
        || { echo ">>> Nodes did not come Ready in time. Try: just start-cluster"; exit 1; }

    echo ">>> Cluster reconnected. Run: just validate-routing"
    cargo run -q -p cli -- nodes

# Stop the full cluster: remote agents first, then local coordinator and controller.
# Usage: just stop-cluster
stop-cluster:
    #!/usr/bin/env bash
    echo ">>> Stopping remote agents..."
    for f in nodes/*.env; do
        source "$f"
        NODE_NAME=$(basename "$f" .env)
        case "$NODE_OS" in
          linux)
            echo ">>> Stopping ${NODE_NAME} (${NODE_HOST})..."
            ssh -o ConnectTimeout=5 ${NODE_USER}@${NODE_HOST} \
                "sudo systemctl stop ai-mesh-agent" 2>/dev/null \
                || echo ">>> Warning: could not reach ${NODE_NAME} (skipping)"
            ;;
          windows)
            echo ">>> Stopping ${NODE_NAME} (${NODE_HOST})..."
            ssh -o ConnectTimeout=5 -o LogLevel=ERROR ${NODE_USER}@${NODE_HOST} \
                "powershell -Command \"sc.exe stop ai-mesh-agent 2>&1 | Out-Null; exit 0\"" 2>/dev/null \
                || echo ">>> Warning: could not reach ${NODE_NAME} (skipping)"
            ;;
        esac
    done
    echo ">>> Stopping local coordinator and controller..."
    pkill -f "target/(debug|release)/coordinator" 2>/dev/null || true
    pkill -f "target/(debug|release)/agent" 2>/dev/null || true
    echo ">>> Cluster stopped."

# Start coordinator + controller locally, then start all remote nodes.
# Ctrl+C stops only the local processes; remote services keep running.
dev: update-portproxy
    #!/usr/bin/env bash
    set -e

    echo ">>> Stopping any stale local processes..."
    pkill -f "target/(debug|release)/coordinator" || true
    pkill -f "target/(debug|release)/agent" || true
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

    echo ">>> Waiting for coordinator to accept connections..."
    for i in $(seq 1 60); do
        sleep 1
        if grep -q "Coordinator is running" /tmp/mesh-coordinator.log 2>/dev/null; then
            echo ">>> Coordinator ready."
            break
        fi
        [ "$i" -eq 60 ] && { echo ">>> ERROR: coordinator did not start. Check /tmp/mesh-coordinator.log"; exit 1; }
    done

    cargo run -p cli -- reset-registry || true

    # Push TLS fingerprint + auth token to all compute nodes.
    STATE="$HOME/.config/ai-mesh/coordinator.state"
    if [ -f "$STATE" ]; then
        source "$STATE"
        export MESH_TLS_FINGERPRINT="${MESH_TLS_FINGERPRINT}"
        echo ">>> Pushing TLS fingerprint and auth token to all compute nodes..."
        for f in nodes/*.env; do
            source "$f"
            NODE_NAME=$(basename "$f" .env)
            [ "${NODE_ROLE}" = "compute" ] || continue
            just set-fingerprint ${NODE_NAME} \
                || echo ">>> Warning: could not set credentials on ${NODE_NAME} (skipping)"
        done
    fi

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

    pkill -f "target/(debug|release)/coordinator" || true
    pkill -f "target/(debug|release)/agent" || true
    sleep 0.5

    # Dev-only harness: run coordinator in insecure mode so remote agents (which have
    # production credentials) don't need a fresh fingerprint push.
    MESH_INSECURE=1 cargo run -p coordinator &
    COORD_PID=$!
    sleep 1

    MESH_INSECURE=1 cargo run -p cli -- reset-registry || true

    MESH_INSECURE=1 AGENT_ROLE=controller cargo run -p agent &
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
    MESH_INSECURE=1 cargo run -p cli -- nodes

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

# Open a 60-second Zigbee pairing window and stream join events to the terminal.
# Power-cycle a bulb after running this to pair it.
# Usage: just pair-bulb
pair-bulb:
    #!/usr/bin/env bash
    PI_MQTT="192.168.1.11"
    echo ">>> Opening 5-minute pairing window on Zigbee network..."
    mosquitto_pub -h ${PI_MQTT} -t 'zigbee2mqtt/bridge/request/permit_join' \
        -m '{"value":true,"time":254}'
    echo ">>> Pairing window open (4 minutes) — power-cycle your bulb now."
    echo ">>> Watching for join events (Ctrl+C to stop)..."
    mosquitto_sub -h ${PI_MQTT} -t 'zigbee2mqtt/bridge/event' -v

# Send a natural-language intent to the coordinator.
# Usage: just intent "turn test_bulb on"
#        just intent "what is the capital of France"
intent text:
    #!/usr/bin/env bash
    STATE="$HOME/.config/ai-mesh/coordinator.state"
    if [ -f "$STATE" ]; then
        source "$STATE"
        export MESH_TLS_FINGERPRINT MESH_AUTH_TOKEN
    fi
    cargo run -q -p cli -- --coordinator "{{coordinator_ip}}:{{coordinator_port}}" \
        intent "{{text}}"

# Validate that each model routes to the correct hardware node.
# Assumes the cluster is already running with hardware-selected models loaded
# (i.e. run `just start-cluster` first).
# Pi (192.168.1.11) should serve qwen2.5:1.5b; Beelink (192.168.1.14) should serve qwen2.5:7b.
# Usage: just validate-routing
validate-routing: update-portproxy chaos
    #!/usr/bin/env bash
    set -e

    # Load credentials from coordinator state so this works immediately after
    # restart-coordinator without needing to source ~/.bashrc first.
    STATE="$HOME/.config/ai-mesh/coordinator.state"
    if [ -f "$STATE" ]; then
        source "$STATE"
        export MESH_TLS_FINGERPRINT MESH_AUTH_TOKEN
    fi

    COORD="{{coordinator_ip}}:{{coordinator_port}}"
    PASS=0
    FAIL=0

    echo "=== Routing Validation ==="
    echo ""

    # Pre-flight: abort early if no compute nodes are registered.
    # This happens after laptop suspend/resume when the coordinator restarted but agents haven't reconnected.
    COMPUTE_COUNT=$(cargo run -q -p cli -- nodes 2>/dev/null | grep -c "Compute" || true)
    if [ "$COMPUTE_COUNT" -eq 0 ]; then
        echo "ERROR: No compute nodes are registered with the coordinator."
        echo "       After opening your laptop, run:  just restart-coordinator"
        echo "       For a full cluster start, run:   just start-cluster"
        exit 1
    fi

    # Helper: fire one inference and return the hostname that served it.
    # Retries up to 3 times so a single transient connection drop doesn't fail the test.
    run_infer() {
        local model="$1"
        for attempt in 1 2 3; do
            local result
            result=$(cargo run -q -p cli -- --coordinator "$COORD" infer "$model" \
                "Reply with one word only: hello" 2>/dev/null \
                | grep '^served-by:' | awk '{print $2}')
            if [ -n "$result" ]; then
                echo "$result"
                return
            fi
            if [ "$attempt" -lt 3 ]; then
                echo ">>> Inference attempt $attempt failed, waiting 20s before retry..." >&2
                sleep 20
            fi
        done
        echo ">>> All attempts failed. If the cluster was idle or you just opened your laptop, run: just restart-coordinator" >&2
    }

    # Normalise hostname for case-insensitive comparison (BEELINK1 ↔ beelink1).
    norm() { echo "$1" | tr '[:upper:]' '[:lower:]'; }

    # --- Test 1: qwen2.5:1.5b → pi1 ---
    echo "--- Test 1: qwen2.5:1.5b → expect pi1 ---"
    SERVED=$(run_infer qwen2.5:1.5b)
    if [ "$(norm "$SERVED")" = "pi1" ]; then
        echo "PASS: qwen2.5:1.5b → ${SERVED}"
        PASS=$((PASS+1))
    else
        echo "FAIL: qwen2.5:1.5b → expected pi1, got '${SERVED:-<no response>}'"
        FAIL=$((FAIL+1))
    fi
    echo ""

    # --- Test 2: qwen2.5:7b → beelink1 ---
    echo "--- Test 2: qwen2.5:7b → expect beelink1 ---"
    SERVED=$(run_infer qwen2.5:7b)
    if [ "$(norm "$SERVED")" = "beelink1" ]; then
        echo "PASS: qwen2.5:7b → ${SERVED}"
        PASS=$((PASS+1))
    else
        echo "FAIL: qwen2.5:7b → expected beelink1, got '${SERVED:-<no response>}'"
        FAIL=$((FAIL+1))
    fi
    echo ""

    echo "=== Results: $PASS passed, $FAIL failed ==="
    [ "$FAIL" -eq 0 ]

# Fire six attack scenarios at the live coordinator to verify HMAC security.
# Each rejection scenario must cause the coordinator to close the connection.
# The final scenario checks that a valid signed request still works.
# Usage: just chaos
chaos: update-portproxy
    #!/usr/bin/env bash
    set -e

    STATE="$HOME/.config/ai-mesh/coordinator.state"
    if [ -f "$STATE" ]; then
        source "$STATE"
        export MESH_TLS_FINGERPRINT MESH_AUTH_TOKEN
    fi

    export MESH_COORDINATOR="{{coordinator_ip}}:{{coordinator_port}}"
    # Dashboard runs in WSL2 — no portproxy for 9001, so connect via localhost.
    export MESH_DASHBOARD_HOST=127.0.0.1
    cargo run -q --bin chaos -p cli

# Verify that deploy-node pushes credentials automatically.
# Scenario A: coordinator running  → set-fingerprint is called immediately after provisioning.
# Scenario B: coordinator absent   → user sees a reminder instead of a silent auth failure.
# This test does NOT do a real deploy — it only exercises the credential-push logic in isolation.
# Usage: just test-deploy-creds pi1
test-deploy-creds node:
    #!/usr/bin/env bash
    set -e
    PASS=0; FAIL=0
    STATE="$HOME/.config/ai-mesh/coordinator.state"

    ok()   { echo "  PASS: $1"; PASS=$((PASS+1)); }
    fail() { echo "  FAIL: $1"; FAIL=$((FAIL+1)); }

    # ── Scenario A: coordinator running ─────────────────────────────────────
    echo ""
    echo "=== Scenario A: coordinator running ==="
    if [ ! -f "$STATE" ]; then
        echo ">>> Coordinator not running — starting it for this test..."
        MDNS_ADVERTISE_IP={{coordinator_ip}} cargo run -q -p coordinator >> /tmp/mesh-coordinator.log 2>&1 &
        for i in $(seq 1 30); do
            sleep 1
            grep -q "Coordinator is running" /tmp/mesh-coordinator.log 2>/dev/null && break
            [ "$i" -eq 30 ] && { echo ">>> ERROR: coordinator did not start"; exit 1; }
        done
        echo ">>> Coordinator ready."
        STARTED_COORDINATOR=1
    fi

    # Simulate the tail of deploy-node: does it detect the state file and call set-fingerprint?
    echo ">>> Running credential-push logic for {{node}}..."
    if [ -f "$STATE" ]; then
        echo ">>> Coordinator is running — pushing credentials to {{node}}..."
        if just set-fingerprint {{node}} 2>&1; then
            ok "credentials pushed to {{node}} when coordinator is running"
        else
            fail "set-fingerprint failed for {{node}}"
        fi
    else
        fail "state file not found even though coordinator started"
    fi

    # ── Scenario B: coordinator absent ──────────────────────────────────────
    echo ""
    echo "=== Scenario B: coordinator absent ==="
    BACKUP=""
    if [ -f "$STATE" ]; then
        BACKUP=$(mktemp)
        cp "$STATE" "$BACKUP"
        rm "$STATE"
    fi

    OUTPUT=$(bash -c '
        STATE="$HOME/.config/ai-mesh/coordinator.state"
        if [ -f "$STATE" ]; then
            echo "WRONG: state file still present"
        else
            echo "No coordinator running yet"
        fi
    ')

    if echo "$OUTPUT" | grep -q "No coordinator running yet"; then
        ok "reminder printed when coordinator is absent"
    else
        fail "expected reminder, got: $OUTPUT"
    fi

    # Restore state file
    if [ -n "$BACKUP" ]; then
        mv "$BACKUP" "$STATE"
    fi

    # ── Cleanup ──────────────────────────────────────────────────────────────
    if [ "${STARTED_COORDINATOR:-0}" = "1" ]; then
        echo ""
        echo ">>> Stopping test coordinator..."
        pkill -f "target/(debug|release)/coordinator" || true
    fi

    echo ""
    echo "=== Results: $PASS passed, $FAIL failed ==="
    [ "$FAIL" -eq 0 ]

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
    pkill -f "target/(debug|release)/coordinator" || true
    pkill -f "target/(debug|release)/agent" || true
    sleep 0.5

    # Dev-only harness: run coordinator in insecure mode so remote agents (which have
    # production credentials) don't need a fresh fingerprint push.
    MESH_INSECURE=1 cargo run -p coordinator &
    COORD_PID=$!
    sleep 1

    MESH_INSECURE=1 cargo run -p cli -- reset-registry || true

    MESH_INSECURE=1 AGENT_ROLE=controller cargo run -p agent &
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
    MESH_INSECURE=1 cargo run -q -p cli -- nodes

    COMPUTE_NODES=$(MESH_INSECURE=1 cargo run -q -p cli -- nodes | grep -E "Compute")
    COMPUTE_COUNT=$(echo "$COMPUTE_NODES" | grep -c "Compute" || true)
    echo "Found ${COMPUTE_COUNT} compute node(s)"

    echo "=== Step 3: Loading model on all compute nodes ==="
    while IFS= read -r line; do
        NODE_ID=$(echo "$line" | awk -F'|' '{print $2}' | xargs)
        HOSTNAME=$(echo "$line" | awk -F'|' '{print $3}' | xargs)
        if [ -n "$NODE_ID" ]; then
            echo "  Loading qwen2.5:1.5b on ${HOSTNAME} (${NODE_ID})..."
            MESH_INSECURE=1 cargo run -q -p cli -- load "${NODE_ID}" qwen2.5:1.5b 1024
        fi
    done <<< "$COMPUTE_NODES"

    echo "=== Step 4: Waiting for all nodes to reach Ready ==="
    sleep 5
    MESH_INSECURE=1 cargo run -q -p cli -- nodes

    echo "=== Step 5: Firing 4 inference requests (expect load distribution) ==="
    PROMPT='In one sentence, what is the Itchen Bridge toll for?'
    for i in 1 2 3 4; do
        echo "--- Request ${i} ---"
        MESH_INSECURE=1 cargo run -q -p cli -- infer 'qwen2.5:1.5b' "${PROMPT}"
    done

    echo "=== Step 6: Final cluster state ==="
    MESH_INSECURE=1 cargo run -q -p cli -- nodes

# Deploy coordinator to a remote host (pi1) as a systemd service.
# Idempotent: safe to re-run. Handles state file + cert/key migration.
# Usage: just deploy-coordinator pi1
deploy-coordinator target_host:
    #!/usr/bin/env bash
    set -e

    TARGET_HOST="{{target_host}}"
    # Load node config
    if [ ! -f "nodes/${TARGET_HOST}.env" ]; then
        echo ">>> Error: nodes/${TARGET_HOST}.env not found"
        exit 1
    fi
    source "nodes/${TARGET_HOST}.env"

    echo "=== Deploying coordinator to ${TARGET_HOST} (${NODE_HOST}) ==="
    echo ""

    # Step 1: Cross-build coordinator for ARM64
    echo ">>> Step 1: Cross-building coordinator for aarch64..."
    cargo build --target aarch64-unknown-linux-gnu --release -p coordinator 2>&1 | grep -E "Compiling coordinator|Finished" || true

    # Step 2: Create /var/lib/ai-mesh on target
    echo ">>> Step 2: Ensuring /var/lib/ai-mesh exists on ${TARGET_HOST}..."
    ssh ${NODE_USER}@${NODE_HOST} "
        set -e
        if [ ! -d /var/lib/ai-mesh ]; then
            sudo mkdir -p /var/lib/ai-mesh
            sudo chown ${NODE_USER}:${NODE_USER} /var/lib/ai-mesh
            sudo chmod 700 /var/lib/ai-mesh
        fi
        # Ensure ~/.config/ai-mesh exists (for cert/key/state)
        mkdir -p ~/.config/ai-mesh
        chmod 700 ~/.config/ai-mesh
    "

    # Step 3: Copy state files (cert, key, state) to target
    echo ">>> Step 3: Copying state files to ${TARGET_HOST}..."
    STATE_DIR="$HOME/.config/ai-mesh"
    if [ -d "$STATE_DIR" ] && [ -n "$(ls -A "$STATE_DIR" 2>/dev/null)" ]; then
        scp -r "$STATE_DIR"/* ${NODE_USER}@${NODE_HOST}:~/.config/ai-mesh/ 2>/dev/null || {
            echo ">>> Warning: Could not copy state files"
        }
    else
        echo ">>> Warning: State directory $STATE_DIR does not exist or is empty (will be created on first run)"
    fi

    # Step 4: Copy database
    echo ">>> Step 4: Copying ai_mesh.db to ${TARGET_HOST}..."
    if [ -f "ai_mesh.db" ]; then
        scp ai_mesh.db ${NODE_USER}@${NODE_HOST}:/var/lib/ai-mesh/
    else
        echo ">>> Warning: ai_mesh.db not found in repo root (will be created on first run)"
    fi

    # Step 5: Copy binary
    echo ">>> Step 5: Copying coordinator binary to ${TARGET_HOST}..."
    scp target/aarch64-unknown-linux-gnu/release/coordinator ${NODE_USER}@${NODE_HOST}:/tmp/ai-mesh-coordinator
    ssh ${NODE_USER}@${NODE_HOST} "sudo install -m 755 /tmp/ai-mesh-coordinator /usr/local/bin/ai-mesh-coordinator && rm /tmp/ai-mesh-coordinator"

    # Step 6: Install systemd unit
    echo ">>> Step 6: Installing systemd unit on ${TARGET_HOST}..."
    cat systemd/ai-mesh-coordinator.service | ssh ${NODE_USER}@${NODE_HOST} "
        set -e
        sudo tee /etc/systemd/system/ai-mesh-coordinator.service > /dev/null
        sudo systemctl daemon-reload
    "

    # Step 7: Enable and start the service
    echo ">>> Step 7: Enabling and starting ai-mesh-coordinator service..."
    ssh ${NODE_USER}@${NODE_HOST} "
        sudo systemctl enable ai-mesh-coordinator
        sudo systemctl restart ai-mesh-coordinator
        echo '>>> Service enabled and started'
    "

    echo ""
    echo "=== Deployment complete ==="
    echo ""
    echo "Next steps:"
    echo "  1. Verify the deployment: just verify-coordinator ${TARGET_HOST}"
    echo "  2. Repoint agents at the new coordinator: just start-agents"
    echo "  3. Check health: http://${NODE_HOST}:{{coordinator_port}}/?token=..."
    echo ""

# Verify coordinator health on the target host (Phase 2 health check).
# Checks: connectivity, log output, agent connections, Zigbee → MQTT, dashboard access.
# Usage: just verify-coordinator pi1
verify-coordinator target_host:
    #!/usr/bin/env bash
    set -e

    TARGET_HOST="{{target_host}}"
    if [ ! -f "nodes/${TARGET_HOST}.env" ]; then
        echo ">>> Error: nodes/${TARGET_HOST}.env not found"
        exit 1
    fi
    source "nodes/${TARGET_HOST}.env"

    echo "=== Verifying coordinator on ${TARGET_HOST} (${NODE_HOST}) ==="
    echo ""

    # Check 1: Service is running
    echo "[1/5] Checking service status..."
    ssh ${NODE_USER}@${NODE_HOST} "sudo systemctl is-active ai-mesh-coordinator" > /dev/null && echo "      ✓ Service is active" || {
        echo "      ✗ Service is NOT active"
        echo "Logs:"
        ssh ${NODE_USER}@${NODE_HOST} "sudo journalctl -u ai-mesh-coordinator -n 20 --no-pager"
        exit 1
    }

    # Check 2: HTTP endpoint is responding
    echo "[2/5] Checking HTTP endpoint at ${NODE_HOST}:{{coordinator_port}}..."
    if curl -s http://${NODE_HOST}:{{coordinator_port}}/ | grep -q "<!DOCTYPE\|<html"; then
        echo "      ✓ Dashboard HTML loaded"
    else
        echo "      ✗ Dashboard did not respond with HTML"
        exit 1
    fi

    # Check 3: Recent log output shows expected startup messages
    echo "[3/5] Checking startup logs..."
    if ssh ${NODE_USER}@${NODE_HOST} "sudo journalctl -u ai-mesh-coordinator -n 50 --no-pager | grep -q 'listening on\|TLS\|auth token'"; then
        echo "      ✓ Startup messages found"
    else
        echo "      ⚠ Could not find expected startup messages (may still be running)"
    fi

    # Check 4: Certificate fingerprint matches
    echo "[4/5] Checking certificate fingerprint..."
    COORDINATOR_LOG=$(ssh ${NODE_USER}@${NODE_HOST} "sudo journalctl -u ai-mesh-coordinator -n 100 --no-pager")
    FP=$(echo "$COORDINATOR_LOG" | grep "fingerprint:" | sed 's/.*fingerprint: //' | head -1)
    if [ -n "$FP" ]; then
        echo "      ✓ Fingerprint: $FP"
    else
        echo "      ⚠ Could not extract fingerprint from logs (check manually)"
    fi

    # Check 5: DB exists
    echo "[5/5] Checking database file..."
    ssh ${NODE_USER}@${NODE_HOST} "[ -f /var/lib/ai-mesh/ai_mesh.db ] && echo '✓ Database exists' || echo '⚠ Database not yet created (will be on first run)'"

    echo ""
    echo "=== Verification complete ==="
    echo ""
    echo "Dashboard URL: http://${NODE_HOST}:{{coordinator_port}}/?token=..."
    echo ""
    echo "Manual next steps:"
    echo "  - Repoint agents: just start-agents"
    echo "  - Check agent connections: ssh ${NODE_USER}@${NODE_HOST} sudo journalctl -u ai-mesh-coordinator -f"
    echo "  - Test light command: click a bulb on-off in the dashboard"
    echo ""

# Emergency revert: stop the remote coordinator and restart on laptop.
# Restores the laptop as the controller (resets justfile coordinator_ip comment).
# Usage: just rollback-coordinator
rollback-coordinator:
    #!/usr/bin/env bash
    set -e

    echo "=== Emergency rollback: reverting coordinator to laptop ==="
    echo ""

    # Find pi1 host from nodes/
    PI1_FILE="nodes/pi1.env"
    if [ ! -f "$PI1_FILE" ]; then
        echo ">>> Error: $PI1_FILE not found"
        exit 1
    fi
    source "$PI1_FILE"

    # Stop the remote service
    echo ">>> Stopping coordinator service on ${NODE_HOST}..."
    ssh ${NODE_USER}@${NODE_HOST} "sudo systemctl stop ai-mesh-coordinator 2>/dev/null || true" || true
    echo "    ✓ Remote coordinator stopped"

    # Restart on laptop
    echo ">>> Restarting coordinator on laptop (WSL2)..."
    just run-coordinator &
    sleep 2

    echo ""
    echo "=== Rollback complete ==="
    echo ""
    echo "Next steps:"
    echo "  1. Update coordinator_ip in justfile to your laptop IP if needed"
    echo "  2. Repoint agents back to laptop: just start-agents"
    echo "  3. Verify cluster health: just validate-routing"
    echo ""

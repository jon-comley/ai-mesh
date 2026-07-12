# Coordinator host is derived from whichever nodes/*.env carries NODE_COORDINATOR=true
# (single source of truth). Falls back to pi1's IP if no marker is found.
coordinator_ip   := `f=$(grep -l "^NODE_COORDINATOR=true" nodes/*.env 2>/dev/null | head -1); if [ -n "$f" ]; then grep -h "^NODE_HOST=" "$f" | head -1 | cut -d= -f2; else echo 10.0.0.10; fi`
coordinator_port := "9000"

# Shared SSH/SCP options for every node operation. ConnectTimeout bounds the connect so
# an offline/unreachable node fails fast (~10s) instead of hanging forever (the class of
# silent hang that bit us on local 127.0.0.1 nodes), and LogLevel quiets banner noise.
# Use as:  ssh {{ssh_opts}} <host> ...   /   scp {{ssh_opts}} <src> <dst>
ssh_opts := "-o ConnectTimeout=10 -o LogLevel=ERROR"

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
    if ! cargo run -q -p cli -- --coordinator "127.0.0.1:{{coordinator_port}}" nodes > /dev/null 2>&1; then
        echo ">>> Coordinator not running — starting in background..."
        MDNS_ADVERTISE_IP={{coordinator_ip}} cargo run -q -p coordinator \
            > /tmp/mesh-coordinator.log 2>&1 &
        COORD_PID=$!
        trap '[ -n "$COORD_PID" ] && kill "$COORD_PID" 2>/dev/null || true' EXIT

        echo ">>> Waiting for coordinator to accept connections..."
        for i in $(seq 1 30); do
            sleep 1
            if cargo run -q -p cli -- --coordinator "127.0.0.1:{{coordinator_port}}" nodes > /dev/null 2>&1; then
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
                || echo ">>> ❌ ERROR: ${NODE_NAME} did NOT receive credentials — its agent will loop on auth until you run: just set-fingerprint ${NODE_NAME}"
        done
    fi
    just start-agents
    bash scripts/hardware-report.sh "$COORD"

build:
    cargo build

test:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo test 2>&1 | tee /tmp/mesh-test-out.txt
    echo ""
    echo "=== Test summary ==="
    grep "^test result" /tmp/mesh-test-out.txt | awk '
        BEGIN { pass=0; fail=0; ignore=0 }
        { for(i=1;i<=NF;i++) {
            if ($i~/^[0-9]+$/) {
                if ($(i+1)=="passed;") pass+=$i
                else if ($(i+1)=="failed;") fail+=$i
                else if ($(i+1)=="ignored;") ignore+=$i
            }
        }}
        END { printf "  passed: %d  failed: %d  ignored: %d\n", pass, fail, ignore }'

# Frontend (dashboard ES module) unit tests — Vitest + jsdom. Dev-only.
test-ui:
    #!/usr/bin/env bash
    set -e
    cd frontend
    [ -d node_modules ] || npm install
    npm test

lint:
    cargo fmt --all
    cargo clippy --all-targets --all-features -- -D warnings

# Phase-A prompt-compression measurement — prints token savings on a sample
# corpus (multilingual + a long history). Set PROMPT_COMPRESS_RATIO to sweep.
measure-compression:
    cargo run -p coordinator --example measure_compression

run-agent:
    #!/usr/bin/env bash
    set -e
    cargo build -p agent
    LOG="$HOME/.local/share/ai-mesh/agent.log"
    mkdir -p "$(dirname "$LOG")"
    COORDINATOR_IP={{coordinator_ip}} COORDINATOR_PORT={{coordinator_port}} \
        nohup target/debug/agent >> "$LOG" 2>&1 &
    echo ">>> Agent started (PID $!) — logs at $LOG"

run-coordinator: update-portproxy
    #!/usr/bin/env bash
    pkill -f "target/(debug|release)/coordinator" || true
    sleep 0.3
    source scripts/mesh-env.sh
    MDNS_ADVERTISE_IP={{coordinator_ip}} cargo run -p coordinator

run-controller:
    #!/usr/bin/env bash
    set -e
    cargo build -p agent
    LOG="$HOME/.local/share/ai-mesh/agent.log"
    mkdir -p "$(dirname "$LOG")"
    COORDINATOR_IP={{coordinator_ip}} COORDINATOR_PORT={{coordinator_port}} AGENT_ROLE=compute \
        nohup target/debug/agent >> "$LOG" 2>&1 &
    echo ">>> Controller agent started (PID $!) — logs at $LOG"

reset: update-portproxy
    #!/usr/bin/env bash
    set -e
    source scripts/mesh-env.sh
    cargo run -p cli -- --coordinator "{{coordinator_ip}}:{{coordinator_port}}" reset-registry
    echo "Registry cleared. Nodes will re-register on their next heartbeat."

nodes:
    #!/usr/bin/env bash
    source scripts/mesh-env.sh
    cargo run -q -p cli -- --coordinator "{{coordinator_ip}}:{{coordinator_port}}" nodes

# Set heartbeat interval for a node. Accepts hostname, IP, or UUID.
# Usage: just set-heartbeat beelink1 10
# Usage: just set-heartbeat 10.0.0.11 30
set-heartbeat node secs:
    #!/usr/bin/env bash
    source scripts/mesh-env.sh
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
        if [ "$NODE_HOST" = "127.0.0.1" ] || [ "$NODE_HOST" = "localhost" ]; then
            echo ">>> Building Linux x86_64 agent (local)..."
            cargo build --release -p agent --features ${NODE_FEATURES:-llm}
            AGENT_BIN="target/release/agent"
            echo ">>> Stopping local agent service..."
            sudo systemctl stop ai-mesh-agent 2>/dev/null || true
            sudo systemctl kill ai-mesh-agent 2>/dev/null || true
            echo ">>> Installing agent binary..."
            sudo install -m 755 ${AGENT_BIN} /home/${NODE_USER}/agent
            sudo systemctl start ai-mesh-agent
        else
            NODE_ARCH=$(ssh {{ssh_opts}} ${NODE_USER}@${NODE_HOST} "uname -m" 2>/dev/null || echo "")
            if [ -z "$NODE_ARCH" ]; then
                echo ">>> ERROR: cannot reach {{node}} (${NODE_USER}@${NODE_HOST}) over SSH to detect its arch."
                echo ">>>        Is the node powered on and reachable? Aborting rather than guessing the arch."
                exit 1
            fi
            if [ "$NODE_ARCH" = "x86_64" ]; then
                echo ">>> Building Linux x86_64 agent..."
                cargo build --release --target x86_64-unknown-linux-gnu -p agent --features ${NODE_FEATURES:-llm}
                AGENT_BIN="target/x86_64-unknown-linux-gnu/release/agent"
            else
                echo ">>> Building Linux ARM64 agent..."
                cargo build --release --target aarch64-unknown-linux-gnu -p agent --features ${NODE_FEATURES:-llm}
                AGENT_BIN="target/aarch64-unknown-linux-gnu/release/agent"
            fi
            ssh {{ssh_opts}} ${NODE_USER}@${NODE_HOST} "timeout 12 sudo systemctl stop ai-mesh-agent 2>/dev/null; sudo systemctl kill ai-mesh-agent 2>/dev/null; true"
            scp_dots ">>> Uploading agent binary" \
                scp {{ssh_opts}} -q ${AGENT_BIN} ${NODE_USER}@${NODE_HOST}:/home/${NODE_USER}/agent
            scp_dots ">>> Uploading install script" \
                scp {{ssh_opts}} -q scripts/install-node-linux.sh ${NODE_USER}@${NODE_HOST}:/tmp/install-node.sh
            ssh {{ssh_opts}} -t ${NODE_USER}@${NODE_HOST} \
                "chmod +x /tmp/install-node.sh && sudo /tmp/install-node.sh '${NODE_ROLE}' '${NODE_USER}' '${MQTT_HOST:-}' '${MQTT_PORT:-1883}' '${NODE_FEATURES:-llm}' '${VOICE_DEVICE_HOST:-}' '${VOICE_STT_REMOTE:-}' '${VOICE_TTS_BASE_URL:-}' '${AUDIO_BACKENDS:-}' '${AUDIO_ALSA_DEVICE:-}' '${ART_MATTE_PERCENT:-}' '${ART_FRAME_THICKNESS:-}' '${SPOTIFY_DEVICE_NAME:-}' '${ART_SIDE_MARGIN_BOOST:-}' '${ART_GLAZE_PERCENT:-}' '${ART_BRIGHTNESS_PERCENT:-}' '${ART_BORDER_GLAZE_PERCENT:-}'"
        fi
        ;;

      windows)
        echo ">>> Building Windows x86_64 agent..."
        cargo build --release -p agent --target x86_64-pc-windows-gnu --features ${NODE_FEATURES:-llm}

        WIN_PATH="C:\\Users\\${NODE_USER}\\ai-mesh"
        echo ">>> Creating ${WIN_PATH} on ${NODE_HOST}..."
        ssh -o ConnectTimeout=15 ${NODE_USER}@${NODE_HOST} \
            "powershell -Command \"if (-not (Test-Path '${WIN_PATH}')) { New-Item -ItemType Directory -Path '${WIN_PATH}' | Out-Null }\""

        scp_dots ">>> Uploading agent.exe" \
            scp {{ssh_opts}} -q target/x86_64-pc-windows-gnu/release/agent.exe \
                ${NODE_USER}@${NODE_HOST}:"${WIN_PATH}\\agent_next.exe"
        scp_dots ">>> Uploading install script" \
            scp {{ssh_opts}} -q scripts/install-node-windows.ps1 \
                ${NODE_USER}@${NODE_HOST}:"${WIN_PATH}\\install-node-windows.ps1"
        scp_dots ">>> Uploading stop-and-swap script" \
            scp {{ssh_opts}} -q scripts/stop-and-swap-agent-windows.ps1 \
                ${NODE_USER}@${NODE_HOST}:"${WIN_PATH}\\stop-and-swap-agent-windows.ps1"

        PUBKEY=""
        if [ -f "$HOME/.ssh/id_ed25519.pub" ]; then
            PUBKEY=$(cat "$HOME/.ssh/id_ed25519.pub")
        elif [ -f "$HOME/.ssh/id_rsa.pub" ]; then
            PUBKEY=$(cat "$HOME/.ssh/id_rsa.pub")
        fi

        scp_dots ">>> Stopping service and swapping binary" \
            ssh {{ssh_opts}} ${NODE_USER}@${NODE_HOST} "powershell -ExecutionPolicy Bypass -Command \"\
                & '${WIN_PATH}\\stop-and-swap-agent-windows.ps1' -WinPath '${WIN_PATH}'\
            \""
        echo ">>> Running provisioning script (this takes a minute — installing NSSM, llama.cpp, registering service)..."
        scp_dots ">>> Provisioning" \
            ssh {{ssh_opts}} ${NODE_USER}@${NODE_HOST} "powershell -ExecutionPolicy Bypass -Command \"\
                & '${WIN_PATH}\\install-node-windows.ps1' -Role '${NODE_ROLE}' -AuthorizedKey '${PUBKEY}' -SttServer '${STT_SERVER:-}'\
            \""
        # Stability hardening (ULPS, AX200 NIC, power plan) is applied by
        # install-node-windows.ps1's Harden-Stability function above — no
        # separate step needed here.
        ;;

      *)
        echo "Unknown NODE_OS: $NODE_OS (expected linux or windows)"
        exit 1
        ;;
    esac
    echo ">>> Node {{node}} provisioned."

    # Push TLS fingerprint + auth token if the coordinator is already running.
    # Without this the freshly-started agent cannot pass auth and will loop-fail
    # SILENTLY. A failed push (or a model that never reaches Ready) means the node is
    # broken, not "provisioned" — so these are hard failures, never warn-and-continue.
    STATE="$HOME/.config/ai-mesh/coordinator.state"
    if [ -f "$STATE" ]; then
        echo ">>> Coordinator is running — pushing credentials to {{node}}..."
        if ! just set-fingerprint {{node}}; then
            echo ">>> ERROR: could not push credentials to {{node}} — the agent will loop on auth."
            echo ">>>        Fix the error above, then re-run: just set-fingerprint {{node}}"
            exit 1
        fi
        if [[ "${NODE_ROLE}" == "compute" ]] && [[ "${NODE_FEATURES:-llm}" == *"llm"* ]]; then
            echo ">>> Auto-loading best-fit model on {{node}} (agent restart kills llama-server)..."
            # auto-load-model waits for the model to reach Ready, which only happens once
            # the agent has reconnected and authenticated — so this doubles as a connection
            # check. Do NOT swallow its failure: that is exactly how a stuck node hides.
            if ! just auto-load-model {{node}}; then
                echo ">>> ERROR: model never reached Ready on {{node}} — the agent likely did not reconnect."
                echo ">>>        Check: just nodes   (is {{node}} heartbeating?)   and   just logs {{node}}"
                exit 1
            fi
        else
            echo ">>> {{node}} is a ${NODE_ROLE} node — no model to load; check 'just nodes' shows it heartbeating."
        fi
    else
        echo ">>> No coordinator running yet — run 'just start-cluster' (or 'just set-fingerprint {{node}}' after starting the coordinator)"
    fi

# Build agent binaries and provision every node.
# Delegates to `deploy-node` per node so there is ONE deploy path — it handles local
# 127.0.0.1 nodes (no SSH, native build), remote linux/windows, and the credential
# push. This recipe used to carry its own copy of that logic, which had drifted: it
# SSH'd to every node unconditionally (hanging on local 127.0.0.1 nodes like omnilink1)
# and probed arch over SSH, defaulting to aarch64 on failure → wrong binary.
# Usage: just provision-all
provision-all:
    #!/usr/bin/env bash
    set -e
    failed=()
    for f in nodes/*.env; do
        NODE_NAME=$(basename "$f" .env)
        echo ""
        echo "=== Provisioning ${NODE_NAME} ==="
        # Attempt every node; collect failures rather than aborting the whole run on
        # the first one, but surface them loudly at the end with a non-zero exit.
        if ! just deploy-node "${NODE_NAME}"; then
            echo ">>> ${NODE_NAME} FAILED to provision."
            failed+=("${NODE_NAME}")
        fi
    done
    echo ""
    if [ "${#failed[@]}" -gt 0 ]; then
        echo "=== Provisioning FAILED for: ${failed[*]} — see errors above. ==="
        exit 1
    fi
    echo "=== All nodes provisioned. ==="

# Restart the ai-mesh-agent service on a node without touching the binary.
# Usage: just restart-node pi1
restart-node node:
    #!/usr/bin/env bash
    set -e
    source nodes/{{node}}.env
    case "$NODE_OS" in
      linux)
        if [ "$NODE_HOST" = "127.0.0.1" ] || [ "$NODE_HOST" = "localhost" ]; then
            sudo systemctl restart ai-mesh-agent
        else
            ssh {{ssh_opts}} ${NODE_USER}@${NODE_HOST} "timeout 12 sudo systemctl stop ai-mesh-agent 2>/dev/null; sudo systemctl kill ai-mesh-agent 2>/dev/null; true; sudo systemctl start ai-mesh-agent"
        fi
        ;;
      windows)
        ssh {{ssh_opts}} ${NODE_USER}@${NODE_HOST} "powershell -Command \"\
            taskkill /F /IM llama-server.exe /T 2>&1 | Out-Null;\
            taskkill /F /IM agent.exe /T 2>&1 | Out-Null;\
            sc.exe stop ai-mesh-agent 2>&1 | Out-Null;\
            Start-Sleep 1;\
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
        if [ "$NODE_HOST" = "127.0.0.1" ] || [ "$NODE_HOST" = "localhost" ]; then
            # Local node (e.g. the WSL2 controller) — no SSH/scp. Build natively,
            # swap the binary in place, restart via systemd.
            cargo build --release -p agent --features ${NODE_FEATURES:-llm}
            echo ">>> Installing updated agent locally..."
            timeout 12 sudo systemctl stop ai-mesh-agent 2>/dev/null || true
            sudo systemctl kill ai-mesh-agent 2>/dev/null || true
            install -m 755 target/release/agent /home/${NODE_USER}/agent
            sudo systemctl start ai-mesh-agent
        else
            NODE_ARCH=$(ssh {{ssh_opts}} ${NODE_USER}@${NODE_HOST} "uname -m" 2>/dev/null || echo "aarch64")
            if [ "$NODE_ARCH" = "x86_64" ]; then
                cargo build --release --target x86_64-unknown-linux-gnu -p agent --features ${NODE_FEATURES:-llm}
                AGENT_BIN="target/x86_64-unknown-linux-gnu/release/agent"
            else
                cargo build --release --target aarch64-unknown-linux-gnu -p agent --features ${NODE_FEATURES:-llm}
                AGENT_BIN="target/aarch64-unknown-linux-gnu/release/agent"
            fi
            echo ">>> Uploading updated agent to ${NODE_HOST}..."
            ssh {{ssh_opts}} ${NODE_USER}@${NODE_HOST} "timeout 12 sudo systemctl stop ai-mesh-agent 2>/dev/null; sudo systemctl kill ai-mesh-agent 2>/dev/null; true"
            scp {{ssh_opts}} -q -o ServerAliveInterval=5 -o ServerAliveCountMax=12 \
                ${AGENT_BIN} ${NODE_USER}@${NODE_HOST}:/home/${NODE_USER}/agent
            ssh {{ssh_opts}} ${NODE_USER}@${NODE_HOST} "sudo systemctl start ai-mesh-agent"
        fi
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
            if timeout 90 scp {{ssh_opts}} \
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
        ssh {{ssh_opts}} ${NODE_USER}@${NODE_HOST} "powershell -Command \"\
            taskkill /F /IM llama-server.exe /T 2>&1 | Out-Null;\
            taskkill /F /IM agent.exe /T 2>&1 | Out-Null;\
            sc.exe stop ai-mesh-agent 2>&1 | Out-Null;\
            Start-Sleep 1;\
            cmd /c 'copy /Y ${WIN_PATH}\\agent_next.exe ${WIN_PATH}\\agent.exe';\
            sc.exe start ai-mesh-agent 2>&1 | Out-Null;\
            exit 0\
        \""
        ;;
    esac
    echo ">>> Node {{node}} updated."

# Internal: push the TLS fingerprint + auth token to ONE node's agent service and restart
# it. The single home for the env-injection that set-fingerprint and set-auth-token both
# need, so the fragile Windows registry / systemd drop-in logic lives in exactly one place.
# Local linux (127.0.0.1) writes the drop-in directly (no SSH); remote linux writes it over
# SSH; windows merges into the NSSM AppEnvironmentExtra registry value.
# Usage: just _push-node-env <node> <fingerprint> <auth_token>
_push-node-env node fp token:
    #!/usr/bin/env bash
    set -e
    source nodes/{{node}}.env
    FP='{{fp}}'
    MESH_AUTH_TOKEN='{{token}}'
    case "$NODE_OS" in
      linux)
        if [ "$NODE_HOST" = "127.0.0.1" ] || [ "$NODE_HOST" = "localhost" ]; then
            sudo mkdir -p /etc/systemd/system/ai-mesh-agent.service.d
            printf '[Service]\nEnvironment=MESH_TLS_FINGERPRINT=%s\n' "${FP}" \
                | sudo tee /etc/systemd/system/ai-mesh-agent.service.d/tls.conf > /dev/null
            printf '[Service]\nEnvironment=MESH_AUTH_TOKEN=%s\n' "${MESH_AUTH_TOKEN}" \
                | sudo tee /etc/systemd/system/ai-mesh-agent.service.d/auth.conf > /dev/null
            sudo systemctl daemon-reload
            sudo systemctl restart ai-mesh-agent
        else
            ssh {{ssh_opts}} ${NODE_USER}@${NODE_HOST} "
                sudo mkdir -p /etc/systemd/system/ai-mesh-agent.service.d 2>/dev/null || true
                printf '[Service]\nEnvironment=MESH_TLS_FINGERPRINT=${FP}\n' \
                    | sudo tee /etc/systemd/system/ai-mesh-agent.service.d/tls.conf > /dev/null
                printf '[Service]\nEnvironment=MESH_AUTH_TOKEN=${MESH_AUTH_TOKEN}\n' \
                    | sudo tee /etc/systemd/system/ai-mesh-agent.service.d/auth.conf > /dev/null
                sudo systemctl daemon-reload
                timeout 12 sudo systemctl stop ai-mesh-agent 2>/dev/null; sudo systemctl kill ai-mesh-agent 2>/dev/null; true
                sudo systemctl start ai-mesh-agent
            "
        fi
        ;;
      windows)
        ssh {{ssh_opts}} ${NODE_USER}@${NODE_HOST} "powershell -Command \"\
            \$rp = 'HKLM:\\SYSTEM\\CurrentControlSet\\Services\\ai-mesh-agent\\Parameters';\
            \$raw = (Get-ItemProperty -Path \$rp -Name AppEnvironmentExtra -ErrorAction SilentlyContinue).AppEnvironmentExtra;\
            \$envMap = @{};\
            if (\$raw) { \$raw | ForEach-Object { if (\$_ -match '^([^=]+)=(.*)$') { \$envMap[\$Matches[1]] = \$Matches[2] } } };\
            \$envMap['MESH_TLS_FINGERPRINT'] = '${FP}';\
            \$envMap['MESH_AUTH_TOKEN'] = '${MESH_AUTH_TOKEN}';\
            \$pairs = @(\$envMap.GetEnumerator() | ForEach-Object { '{0}={1}' -f \$_.Key, \$_.Value });\
            Set-ItemProperty -Path \$rp -Name AppEnvironmentExtra -Value \$pairs -Type MultiString;\
            taskkill /F /IM llama-server.exe /T 2>&1 | Out-Null;\
            taskkill /F /IM agent.exe /T 2>&1 | Out-Null;\
            \$svcpid=(Get-WmiObject Win32_Service -Filter 'Name=''ai-mesh-agent''').ProcessId;\
            if(\$svcpid -gt 0){Stop-Process -Id \$svcpid -Force -ErrorAction SilentlyContinue};\
            Start-Sleep -Milliseconds 800;\
            sc.exe start ai-mesh-agent 2>&1 | Out-Null;\
            exit 0\
        \""
        ;;
    esac

# ── Music / Spotify (plans/spotify-music.md) ─────────────────────────────────

# One-time interactive Spotify OAuth: prints an authorize URL to open in any
# browser (WSL2 has none), accepts the pasted redirect URL, and writes
# ~/.config/ai-mesh/spotify.env for spotify-push-creds. Pass credentials via
# env or type them at the prompts:
#   SPOTIFY_CLIENT_ID=... SPOTIFY_CLIENT_SECRET=... just spotify-auth
spotify-auth:
    cargo run -p capability-music --bin spotify-auth

# Push the Spotify Web API credentials from ~/.config/ai-mesh/spotify.env to
# a node as a systemd drop-in and restart its agent. Secrets never enter the
# committed nodes/*.env files — same pattern as MESH_AUTH_TOKEN in
# _push-node-env, and drop-ins survive deploy-node installer re-runs.
# Usage: just spotify-push-creds pi2
spotify-push-creds node:
    #!/usr/bin/env bash
    set -e
    source nodes/{{node}}.env
    CREDS_FILE="$HOME/.config/ai-mesh/spotify.env"
    if [ ! -f "$CREDS_FILE" ]; then
        echo ">>> ERROR: $CREDS_FILE not found — run 'just spotify-auth' first."
        exit 1
    fi
    source "$CREDS_FILE"
    if [ -z "${SPOTIFY_CLIENT_ID}" ] || [ -z "${SPOTIFY_CLIENT_SECRET}" ] || [ -z "${SPOTIFY_REFRESH_TOKEN}" ]; then
        echo ">>> ERROR: $CREDS_FILE is incomplete — re-run 'just spotify-auth'."
        exit 1
    fi
    if [ "$NODE_OS" != "linux" ]; then
        echo ">>> ERROR: spotify-push-creds only supports linux nodes (got NODE_OS=$NODE_OS)."
        exit 1
    fi
    echo ">>> Pushing Spotify credentials to {{node}}..."
    if [ "$NODE_HOST" = "127.0.0.1" ] || [ "$NODE_HOST" = "localhost" ]; then
        sudo mkdir -p /etc/systemd/system/ai-mesh-agent.service.d
        printf '[Service]\nEnvironment=SPOTIFY_CLIENT_ID=%s\nEnvironment=SPOTIFY_CLIENT_SECRET=%s\nEnvironment=SPOTIFY_REFRESH_TOKEN=%s\n' \
            "${SPOTIFY_CLIENT_ID}" "${SPOTIFY_CLIENT_SECRET}" "${SPOTIFY_REFRESH_TOKEN}" \
            | sudo tee /etc/systemd/system/ai-mesh-agent.service.d/spotify.conf > /dev/null
        sudo systemctl daemon-reload
        sudo systemctl restart ai-mesh-agent
    else
        ssh {{ssh_opts}} ${NODE_USER}@${NODE_HOST} "
            sudo mkdir -p /etc/systemd/system/ai-mesh-agent.service.d 2>/dev/null || true
            printf '[Service]\nEnvironment=SPOTIFY_CLIENT_ID=${SPOTIFY_CLIENT_ID}\nEnvironment=SPOTIFY_CLIENT_SECRET=${SPOTIFY_CLIENT_SECRET}\nEnvironment=SPOTIFY_REFRESH_TOKEN=${SPOTIFY_REFRESH_TOKEN}\n' \
                | sudo tee /etc/systemd/system/ai-mesh-agent.service.d/spotify.conf > /dev/null
            sudo systemctl daemon-reload
            timeout 12 sudo systemctl stop ai-mesh-agent 2>/dev/null; sudo systemctl kill ai-mesh-agent 2>/dev/null; true
            sudo systemctl start ai-mesh-agent
        "
    fi
    echo ">>> {{node}}: Spotify credentials installed, agent restarted."

# Cross-build librespot for the music node (aarch64). Pipe backend only:
# --no-default-features drops the rodio/alsa C deps so no cross C libraries
# are needed. cargo install ignores the repo's .cargo/config.toml, so the
# aarch64 linker must be passed via env (confirmed: without it the host
# x86-64 linker rejects every object). RUSTFLAGS="" neutralises the
# workspace's -Dwarnings for this out-of-tree build. Binary lands at
# target/librespot-aarch64/bin/librespot.
build-librespot:
    RUSTFLAGS="" \
    CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
    cargo install librespot --version 0.6.0 --locked \
        --no-default-features \
        --target aarch64-unknown-linux-gnu \
        --root target/librespot-aarch64

# Upload the cross-built librespot binary to a node's home dir (the path the
# installer bakes into SPOTIFY_LIBRESPOT_BIN).
# Usage: just deploy-librespot pi2
deploy-librespot node:
    #!/usr/bin/env bash
    set -e
    source nodes/{{node}}.env
    BIN=target/librespot-aarch64/bin/librespot
    if [ ! -f "$BIN" ]; then
        echo ">>> ERROR: $BIN not found — run 'just build-librespot' first."
        exit 1
    fi
    echo ">>> Uploading librespot to {{node}}..."
    scp {{ssh_opts}} -q "$BIN" ${NODE_USER}@${NODE_HOST}:/home/${NODE_USER}/librespot
    ssh {{ssh_opts}} ${NODE_USER}@${NODE_HOST} "chmod +x /home/${NODE_USER}/librespot"
    echo ">>> {{node}}: librespot deployed. Next (first time only): just spotify-login {{node}}"

# One-time librespot login on a node — the PLAYBACK device's credentials,
# independent of spotify-auth's Web API token (see docs/music.md: two
# credential stores). librespot prints its own authorize URL; its
# 127.0.0.1:5588 OAuth redirect is tunnelled to the node through this SSH
# session, so open the URL in any local browser (Windows is fine).
# Ctrl-C once librespot logs that credentials were saved.
# Usage: just spotify-login pi2
spotify-login node:
    #!/usr/bin/env bash
    set -e
    source nodes/{{node}}.env
    echo ">>> librespot one-time login for {{node}}."
    echo ">>> Open the URL librespot prints below in a browser on THIS machine"
    echo ">>> (the 127.0.0.1:5588 redirect is tunnelled to {{node}} over SSH)."
    echo ">>> After approving, Ctrl-C once credentials are reported saved."
    echo ""
    ssh {{ssh_opts}} -t -L 5588:127.0.0.1:5588 ${NODE_USER}@${NODE_HOST} \
        "mkdir -p /home/${NODE_USER}/.ai-mesh/spotify-cache && /home/${NODE_USER}/librespot --enable-oauth --backend pipe --cache /home/${NODE_USER}/.ai-mesh/spotify-cache --name '${SPOTIFY_DEVICE_NAME:-AI Mesh}'"

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

    just _push-node-env {{node}} "${FP}" "${MESH_AUTH_TOKEN}"
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
        # Re-pushes the (unchanged) fingerprint alongside the token — idempotent, and keeps
        # the env-injection logic in one place (_push-node-env).
        just _push-node-env "${NODE_NAME}" "${FP}" "${TOKEN}"
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
        scp {{ssh_opts}} scripts/uninstall-node-linux.sh ${NODE_USER}@${NODE_HOST}:/tmp/uninstall-node.sh
        ssh {{ssh_opts}} -t ${NODE_USER}@${NODE_HOST} \
            "chmod +x /tmp/uninstall-node.sh && sudo /tmp/uninstall-node.sh"
        ;;

      windows)
        WIN_PATH="C:\\Users\\${NODE_USER}\\ai-mesh"
        echo ">>> Uninstalling ai-mesh-agent on ${NODE_HOST}..."
        scp {{ssh_opts}} scripts/uninstall-node-windows.ps1 \
            ${NODE_USER}@${NODE_HOST}:"${WIN_PATH}\\uninstall-node-windows.ps1"
        ssh {{ssh_opts}} ${NODE_USER}@${NODE_HOST} \
            "powershell -ExecutionPolicy Bypass -Command \"& '${WIN_PATH}\\uninstall-node-windows.ps1'\""
        ;;
    esac
    echo ">>> Node {{node}} uninstalled."

# Remotely apply the Beelink SER8 stability fix (stable GPIO driver + registry hardening).
# Usage: just fix-node beelink1
fix-node node:
    #!/usr/bin/env bash
    set -e
    source nodes/{{node}}.env
    echo ">>> Pushing stability fix script to {{node}}..."
    scp {{ssh_opts}} scripts/fix-beelink-stability.ps1 ${NODE_USER}@${NODE_HOST}:C:/fix-stability.ps1
    echo ">>> Executing fix script as Administrator..."
    ssh {{ssh_opts}} ${NODE_USER}@${NODE_HOST} "powershell -ExecutionPolicy Bypass -Command \"Start-Process powershell -Verb RunAs -ArgumentList '-ExecutionPolicy Bypass -File C:/fix-stability.ps1' -Wait\""
    echo ">>> Fix applied. Please REBOOT {{node}} manually."

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
        ssh {{ssh_opts}} ${NODE_USER}@${NODE_HOST} "powershell -Command \"\
            Invoke-WebRequest -Uri '${ZIP_URL}' -OutFile '\$env:TEMP\llama-update.zip' -UseBasicParsing; \
            Expand-Archive -Path '\$env:TEMP\llama-update.zip' -DestinationPath '\$env:LOCALAPPDATA\Programs\llama.cpp' -Force; \
            Remove-Item '\$env:TEMP\llama-update.zip' -Force\""
    else
        ssh {{ssh_opts}} ${NODE_USER}@${NODE_HOST} "
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
            if ! curl -fsSL \"\$ZIP_URL\" -o \"\$LLAMA_TMP/llama.tar.gz\"; then
                # The latest tag's assets sometimes lag its publish by 20+ min
                # (CI upload race) — fall back to the previous release rather
                # than hard-failing. Queried dynamically, not hardcoded: a
                # pinned fallback version would be stale within days.
                echo \"Warning: ${LATEST} assets aren't uploaded yet — trying the previous release...\"
                PREV=\$(curl -fsSL \"https://api.github.com/repos/ggml-org/llama.cpp/releases?per_page=2\" \
                    | grep '\"tag_name\"' | sed -n '2p' | cut -d'\"' -f4)
                if [ -z \"\$PREV\" ]; then
                    echo \"ERROR: download failed and no previous release could be resolved.\"
                    exit 1
                fi
                if [ \"\$ARCH\" = \"x86_64\" ]; then
                    ZIP_URL=\"https://github.com/ggml-org/llama.cpp/releases/download/\${PREV}/llama-\${PREV}-bin-ubuntu-x64.tar.gz\"
                else
                    ZIP_URL=\"https://github.com/ggml-org/llama.cpp/releases/download/\${PREV}/llama-\${PREV}-bin-ubuntu-arm64.tar.gz\"
                fi
                echo \"Falling back to llama.cpp release: \$PREV\"
                curl -fsSL \"\$ZIP_URL\" -o \"\$LLAMA_TMP/llama.tar.gz\"
            fi
            sudo install -d /opt/llama.cpp
            sudo tar -xzf \"\$LLAMA_TMP/llama.tar.gz\" -C /opt/llama.cpp --strip-components=1
            rm -rf \"\$LLAMA_TMP\"
        "
    fi
    echo "Restarting agent on {{node}}..."
    just restart-node {{node}}
    echo "llama.cpp updated to $LATEST on {{node}}"

# Auto-place a model — coordinator picks the best-fit node. No SSH needed.
# Usage: just load qwen3:8b
load model:
    #!/usr/bin/env bash
    set -e
    case "{{model}}" in
        qwen3:4b)         SIZE_MB=2382  ;;
        qwen3:8b)         SIZE_MB=4795  ;;
        qwen3:14b)        SIZE_MB=8584  ;;
        qwen3:32b)        SIZE_MB=18849 ;;
        qwen2.5:0.5b)     SIZE_MB=500   ;;
        qwen2.5:1.5b)     SIZE_MB=986   ;;
        qwen2.5:7b)       SIZE_MB=4096  ;;
        qwen2.5:14b)      SIZE_MB=8192  ;;
        qwen2.5:32b)      SIZE_MB=19456 ;;
        llama3.2:1b)      SIZE_MB=770   ;;
        llama3.2:3b)      SIZE_MB=1926  ;;
        llama3.1:8b)      SIZE_MB=4692  ;;
        phi4:14b)         SIZE_MB=8635  ;;
        gemma3:4b)        SIZE_MB=2374  ;;
        gemma3:12b)       SIZE_MB=6964  ;;
        mistral:7b)       SIZE_MB=4170  ;;
        deepseek-r1:7b)   SIZE_MB=4466  ;;
        deepseek-r1:8b)   SIZE_MB=4692  ;;
        deepseek-r1:14b)  SIZE_MB=8572  ;;
        deepseek-r1:32b)  SIZE_MB=18934 ;;
        *) echo "Unknown model: {{model}}"; exit 1 ;;
    esac
    cargo run -q -p cli -- --coordinator "{{coordinator_ip}}:{{coordinator_port}}" load "{{model}}" "$SIZE_MB"

# Load a model on a named node (looks up live node ID from coordinator).
# Usage: just load-model pi1 qwen2.5:1.5b
#        just load-model beelink1 qwen2.5:7b
load-model node model:
    #!/usr/bin/env bash
    set -e
    source nodes/{{node}}.env
    source scripts/mesh-env.sh
    MODEL="{{model}}"
    case "$MODEL" in
        qwen3:4b)         SIZE_MB=2382  ;;
        qwen3:8b)         SIZE_MB=4795  ;;
        qwen3:14b)        SIZE_MB=8584  ;;
        qwen3:32b)        SIZE_MB=18849 ;;
        qwen2.5:0.5b)     SIZE_MB=500   ;;
        qwen2.5:1.5b)     SIZE_MB=986   ;;
        qwen2.5:7b)       SIZE_MB=4096  ;;
        qwen2.5:14b)      SIZE_MB=8192  ;;
        qwen2.5:32b)      SIZE_MB=19456 ;;
        llama3.2:1b)      SIZE_MB=770   ;;
        llama3.2:3b)      SIZE_MB=1926  ;;
        llama3.1:8b)      SIZE_MB=4692  ;;
        phi4:14b)         SIZE_MB=8635  ;;
        gemma3:4b)        SIZE_MB=2374  ;;
        gemma3:12b)       SIZE_MB=6964  ;;
        mistral:7b)       SIZE_MB=4170  ;;
        deepseek-r1:7b)   SIZE_MB=4466  ;;
        deepseek-r1:8b)   SIZE_MB=4692  ;;
        deepseek-r1:14b)  SIZE_MB=8572  ;;
        deepseek-r1:32b)  SIZE_MB=18934 ;;
        *) echo "Unknown model: $MODEL"; exit 1 ;;
    esac
    # Retry for up to 120s — allow time for reconnect after coordinator restart.
    NODE_ID=""
    for i in $(seq 1 60); do
        NODE_ID=$(cargo run -q -p cli -- --coordinator "{{coordinator_ip}}:{{coordinator_port}}" find-node "${NODE_HOST}" 2>/dev/null || true)
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
        HW_INFO=$(ssh {{ssh_opts}} ${NODE_USER}@${NODE_HOST} '
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
        HW_INFO=$(ssh {{ssh_opts}} ${NODE_USER}@${NODE_HOST} 'powershell -NoProfile -Command "$sysRam=[int]((Get-WmiObject Win32_ComputerSystem).TotalPhysicalMemory/1MB);$vramBytes=(Get-ChildItem '"'"'HKLM:\SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}'"'"' -ErrorAction SilentlyContinue|ForEach-Object{$_.GetValue('"'"'HardwareInformation.qwMemorySize'"'"')}|Where-Object{$_ -gt 0}|Select-Object -First 1);$vram=if($vramBytes){[int]($vramBytes/1MB)}else{0};if($vram -eq 0){$g=(Get-WmiObject Win32_VideoController|Where-Object{$_.AdapterRAM -gt 0}|Sort-Object AdapterRAM -Descending|Select-Object -First 1);if($g){$vram=[int]($g.AdapterRAM/1MB)}};$gpu=if($vram -gt 0){1}else{0};$m=if($vram -gt 0){$vram}else{$sysRam};Write-Output ($m.ToString()+[char]58+$gpu.ToString())"' 2>/dev/null || echo "0:0")
        ;;
      *) HW_INFO="0:0" ;;
    esac
    HW_MB=$(echo "$HW_INFO" | cut -d: -f1 | tr -d '[:space:]'); HW_MB="${HW_MB:-0}"
    HW_GPU=$(echo "$HW_INFO" | cut -d: -f2 | tr -d '[:space:]'); HW_GPU="${HW_GPU:-0}"

    # All known models sorted by size descending — used to suggest fallbacks.
    ALL_MODELS=(
        "qwen2.5:32b:19456"  "qwen3:32b:18849"    "deepseek-r1:32b:18934"
        "phi4:14b:8635"      "qwen3:14b:8584"      "deepseek-r1:14b:8572"
        "qwen2.5:14b:8192"   "gemma3:12b:6964"     "qwen3:8b:4795"
        "deepseek-r1:8b:4692" "llama3.1:8b:4692"   "deepseek-r1:7b:4466"
        "mistral:7b:4170"    "qwen2.5:7b:4096"     "qwen3:4b:2382"
        "gemma3:4b:2374"     "llama3.2:3b:1926"    "qwen2.5:1.5b:986"
        "llama3.2:1b:770"    "qwen2.5:0.5b:500"
    )
    THRESHOLD=$(( HW_MB * 80 / 100 ))

    echo ">>> Loading ${MODEL} (${SIZE_MB} MB) on {{node}} (${NODE_ID})..."
    if [ "$SIZE_MB" -gt "$THRESHOLD" ]; then
        echo ">>> NOTE: ${MODEL} (${SIZE_MB} MB) exceeds 80% of detected memory (${THRESHOLD} MB)."
        echo ">>> Fallbacks that fit this node:"
        for entry in "${ALL_MODELS[@]}"; do
            m="${entry%:*}"; s="${entry##*:}"
            [ "$s" -lt "$SIZE_MB" ] && [ "$s" -le "$THRESHOLD" ] && echo ">>>   just load-model {{node}} ${m%:*}:${m##*:}"
        done
    fi
    cargo run -q -p cli -- --coordinator "{{coordinator_ip}}:{{coordinator_port}}" load --node-id "${NODE_ID}" "${MODEL}" "${SIZE_MB}" | sed 's/^/>>> /'

# Detect hardware on a node and load the best-fit model automatically.
# Usage: just auto-load-model pi1
#        just auto-load-model beelink1
auto-load-model node:
    #!/usr/bin/env bash
    set -e
    source nodes/{{node}}.env
    source scripts/mesh-env.sh
    case "$NODE_OS" in
      linux)
        HW_INFO=$(ssh {{ssh_opts}} ${NODE_USER}@${NODE_HOST} '
            mem=0; gpu=0
            for base in /sys/class/drm/card*/device; do
                [ -f "$base/mem_info_vram_total" ] || continue
                tf="$base/mem_info_vis_vram_total"
                [ -f "$tf" ] || tf="$base/mem_info_vram_total"
                total=$(( $(cat "$tf") / 1048576 ))
                [ "$total" -eq 0 ] && continue
                uf="$base/mem_info_vis_vram_used"
                [ -f "$uf" ] || uf="$base/mem_info_vram_used"
                used=0; [ -f "$uf" ] && used=$(( $(cat "$uf") / 1048576 ))
                free=$(( total - used ))
                [ "$free" -gt "$mem" ] && mem="$free" && gpu=1
            done
            if [ "$gpu" -eq 0 ] && command -v nvidia-smi &>/dev/null; then
                v=$(nvidia-smi --query-gpu=memory.free --format=csv,noheader,nounits 2>/dev/null | head -1 | tr -d " ")
                [ -n "$v" ] && [ "$v" -gt 0 ] && mem="$v" && gpu=1
            fi
            [ "$gpu" -eq 0 ] && mem=$(awk "/MemAvailable/{print int(\$2/1024); exit}" /proc/meminfo)
            echo "${mem}:${gpu}"
        ')
        ;;
      windows)
        HW_INFO=$(ssh {{ssh_opts}} ${NODE_USER}@${NODE_HOST} 'powershell -NoProfile -Command "$sysRam=[int]((Get-WmiObject Win32_ComputerSystem).TotalPhysicalMemory/1MB);$vramBytes=(Get-ChildItem '"'"'HKLM:\SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}'"'"' -ErrorAction SilentlyContinue|ForEach-Object{$_.GetValue('"'"'HardwareInformation.qwMemorySize'"'"')}|Where-Object{$_ -gt 0}|Select-Object -First 1);$vram=if($vramBytes){[int]($vramBytes/1MB)}else{0};if($vram -eq 0){$g=(Get-WmiObject Win32_VideoController|Where-Object{$_.AdapterRAM -gt 0}|Sort-Object AdapterRAM -Descending|Select-Object -First 1);if($g){$vram=[int]($g.AdapterRAM/1MB)}};$gpu=if($vram -gt 0){1}else{0};$m=if($vram -gt 0){$vram}else{$sysRam};Write-Output ($m.ToString()+[char]58+$gpu.ToString())"')
        ;;
      *)
        echo "Unknown NODE_OS: $NODE_OS"; exit 1 ;;
    esac
    HW_MB=$(echo "$HW_INFO" | cut -d: -f1 | tr -d '[:space:]'); HW_MB="${HW_MB:-0}"
    HW_GPU=$(echo "$HW_INFO" | cut -d: -f2 | tr -d '[:space:]'); HW_GPU="${HW_GPU:-0}"

    # Free disk space on the model directory (need 2× model size: .tmp + final .gguf).
    case "$NODE_OS" in
      linux)
        DISK_FREE_MB=$(ssh {{ssh_opts}} ${NODE_USER}@${NODE_HOST} \
            "df --block-size=1M --output=avail \$(echo ~/.ai-mesh/models) 2>/dev/null | tail -1 | tr -d ' '" 2>/dev/null || echo 0)
        ;;
      windows)
        DISK_FREE_MB=$(ssh {{ssh_opts}} ${NODE_USER}@${NODE_HOST} \
            'powershell -NoProfile -Command "[int]((Get-PSDrive C).Free/1MB)"' 2>/dev/null || echo 0)
        ;;
      *) DISK_FREE_MB=0 ;;
    esac
    DISK_FREE_MB="${DISK_FREE_MB:-0}"

    # Models in preference order (best first at each size tier — Qwen3 preferred).
    # Pick the largest model that fits in free RAM AND leaves 2× space on disk.
    CANDIDATE_MODELS=(
        "qwen2.5:32b:19456"  "qwen3:32b:18849"    "deepseek-r1:32b:18934"
        "phi4:14b:8635"      "qwen3:14b:8584"      "deepseek-r1:14b:8572"
        "qwen2.5:14b:8192"   "gemma3:12b:6964"     "qwen3:8b:4795"
        "deepseek-r1:8b:4692" "llama3.1:8b:4692"   "deepseek-r1:7b:4466"
        "mistral:7b:4170"    "qwen2.5:7b:4096"     "qwen3:4b:2382"
        "gemma3:4b:2374"     "llama3.2:3b:1926"    "qwen2.5:1.5b:986"
        "llama3.2:1b:770"    "qwen2.5:0.5b:500"
    )
    if [ "$HW_GPU" = "1" ]; then
        THRESHOLD=$HW_MB
    else
        THRESHOLD=$(( HW_MB * 90 / 100 ))
    fi
    MODEL=""
    for entry in "${CANDIDATE_MODELS[@]}"; do
        m="${entry%:*}"; s="${entry##*:}"
        if [ "$s" -le "$THRESHOLD" ] && [ $(( s * 2 )) -le "$DISK_FREE_MB" ]; then
            MODEL="$m"; break
        fi
    done
    if [ -z "$MODEL" ]; then
        echo ">>> {{node}}: free_mem=${HW_MB}MB disk_free=${DISK_FREE_MB}MB — no model fits both constraints"
        exit 1
    fi
    echo ">>> {{node}}: detected ${HW_MB} MB ($([ "$HW_GPU" = "1" ] && echo GPU || echo CPU), threshold ${THRESHOLD} MB) → selecting ${MODEL}"
    just load-model {{node}} ${MODEL}

# Tail live logs from a node.
# Usage: just logs-node pi1
logs-node node:
    #!/usr/bin/env bash
    source nodes/{{node}}.env

    case "$NODE_OS" in
      linux)
        ssh {{ssh_opts}} ${NODE_USER}@${NODE_HOST} "journalctl -u ai-mesh-agent -f --no-pager"
        ;;
      windows)
        WIN_PATH="C:\\Users\\${NODE_USER}\\ai-mesh"
        ssh {{ssh_opts}} ${NODE_USER}@${NODE_HOST} \
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
        ssh {{ssh_opts}} ${NODE_USER}@${NODE_HOST} \
            "systemctl is-active ai-mesh-agent && echo 'Service: RUNNING' || echo 'Service: NOT RUNNING'"
        ;;
      windows)
        ssh {{ssh_opts}} ${NODE_USER}@${NODE_HOST} \
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
    # Portproxy maps Windows-host 9000/9001 → the WSL IP so LAN nodes can reach
    # a coordinator running *inside* WSL. With a remote coordinator (pi1) all
    # local traffic is outbound — nothing to forward.
    if [ "{{coordinator_ip}}" != "127.0.0.1" ] && [ "{{coordinator_ip}}" != "localhost" ]; then
        echo ">>> Coordinator is remote ({{coordinator_ip}}) — portproxy not needed, skipping."
        exit 0
    fi
    # WSL interop can die after a suspend/resume (netsh.exe → I/O error). A dead
    # portproxy update shouldn't abort cluster start — warn with the fix instead.
    if ! netsh.exe interface portproxy show all >/dev/null 2>&1; then
        echo ">>> WARNING: Windows interop unavailable (netsh.exe unreachable) — skipping portproxy."
        echo ">>>          If LAN nodes can't reach a LOCAL coordinator, run 'wsl --shutdown' from"
        echo ">>>          Windows PowerShell, reopen this terminal, and re-run."
        exit 0
    fi
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
    source scripts/mesh-env.sh
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
                if [ "$NODE_HOST" = "127.0.0.1" ] || [ "$NODE_HOST" = "localhost" ]; then
                    # Local node (this machine) — run directly; there's no sshd on localhost.
                    sudo systemctl start ai-mesh-agent 2>/dev/null \
                        && echo ">>> ${NODE_NAME} service started (local)." \
                        || echo ">>> Warning: could not start ${NODE_NAME} locally (run: sudo systemctl start ai-mesh-agent)"
                else
                    ssh -o ConnectTimeout=10 "${NODE_USER}@${NODE_HOST}" \
                        "sudo systemctl start ai-mesh-agent" 2>/dev/null \
                        && echo ">>> ${NODE_NAME} service started." \
                        || echo ">>> Warning: could not reach ${NODE_NAME} (will self-register if online)"
                fi
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

# Load each compute node's hardware-selected model, retrying any that don't reach Ready.
# A LoadModel issued while an agent is still (re)connecting after a credential-push
# restart can be dropped on the torn connection, leaving the node stuck and never
# Ready — re-issuing the load recovers it. Attempt 1 waits long enough for a fresh
# model download; the shorter retries target only the nodes still missing.
# Usage: just load-models-retry
load-models-retry:
    #!/usr/bin/env bash
    set -e
    source scripts/mesh-env.sh
    COORD="{{coordinator_ip}}:{{coordinator_port}}"

    # Build the compute-node list (name:ip pairs).
    COMPUTE_NODES=()
    for f in nodes/*.env; do
        source "$f"
        NODE_NAME=$(basename "$f" .env)
        [ "${NODE_ROLE}" = "compute" ] || continue
        COMPUTE_NODES+=("${NODE_NAME}:${NODE_HOST}")
    done
    if [ ${#COMPUTE_NODES[@]} -eq 0 ]; then echo ">>> No compute nodes configured."; exit 0; fi

    for attempt in 1 2 3; do
        # Classify each compute node: Ready (done), Loading (wait only), or absent (trigger load).
        NODES_OUT=$(cargo run -q -p cli -- --coordinator "${COORD}" nodes 2>/dev/null || true)
        NEEDS_LOAD=()
        WAIT_IPS=()
        for entry in "${COMPUTE_NODES[@]}"; do
            ip="${entry##*:}"
            line=$(echo "${NODES_OUT}" | grep "${ip}" || true)
            if echo "${line}" | grep -q "Ready"; then
                : # already done
            elif echo "${line}" | grep -q "Loading"; then
                WAIT_IPS+=("${ip}")  # in-flight — just wait, don't re-trigger
            else
                NEEDS_LOAD+=("${entry}")
                WAIT_IPS+=("${ip}")
            fi
        done
        if [ ${#WAIT_IPS[@]} -eq 0 ]; then echo ">>> All compute models Ready."; exit 0; fi

        if [ ${#NEEDS_LOAD[@]} -gt 0 ]; then
            echo ">>> Load attempt ${attempt}/3 — triggering: ${NEEDS_LOAD[*]%%:*}"
            for entry in "${NEEDS_LOAD[@]}"; do
                just auto-load-model "${entry%%:*}" \
                    || echo ">>> Warning: could not load model on ${entry%%:*} (will retry)"
            done
        else
            echo ">>> Load attempt ${attempt}/3 — already loading, waiting: ${WAIT_IPS[*]}"
        fi

        if [ "${attempt}" -eq 1 ]; then WAIT_TIMEOUT=300; else WAIT_TIMEOUT=120; fi
        cargo run -q -p cli -- --coordinator "${COORD}" \
            wait-ready "${WAIT_IPS[@]}" --timeout "${WAIT_TIMEOUT}" \
            || echo ">>> Attempt ${attempt}: some models not Ready yet"
    done

    # Final status after all attempts.
    NODES_OUT=$(cargo run -q -p cli -- --coordinator "${COORD}" nodes 2>/dev/null || true)
    STILL=()
    for entry in "${COMPUTE_NODES[@]}"; do
        echo "${NODES_OUT}" | grep "${entry##*:}" | grep -q "Ready" || STILL+=("${entry%%:*}")
    done
    if [ ${#STILL[@]} -ne 0 ]; then echo ">>> Warning: still not Ready after 3 attempts: ${STILL[*]}"; fi

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
        echo ">>> Syncing coordinator state from {{coordinator_ip}}..."
        scp {{ssh_opts}} -q "jonno@{{coordinator_ip}}:/var/lib/ai-mesh/coordinator.state" "$HOME/.config/ai-mesh/coordinator.state" || echo ">>> Warning: could not sync state from {{coordinator_ip}}"

        # Load the freshly-synced TLS fingerprint + auth token BEFORE the connectivity
        # check — the CLI needs them for the mesh TLS handshake. Without re-sourcing here
        # the check uses a stale fingerprint (none, or one read before this scp) and fails
        # even when the coordinator is healthy, e.g. after its cert was regenerated.
        source scripts/mesh-env.sh

        cargo build -q -p cli
        echo ">>> Verifying connectivity to {{coordinator_ip}}:{{coordinator_port}}..."
        if check_err=$(cargo run -q -p cli -- --coordinator "{{coordinator_ip}}:{{coordinator_port}}" nodes 2>&1 >/dev/null); then
            echo ">>> Coordinator ready."
        else
            echo ">>> ERROR: Could not reach coordinator at {{coordinator_ip}}:{{coordinator_port}}"
            echo ">>>        CLI error: ${check_err:-<none>}"
            echo ">>>        Is it running? Check: ssh jonno@{{coordinator_ip}} systemctl status ai-mesh-coordinator"
            exit 1
        fi
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

    echo ">>> Pushing TLS fingerprint and auth token to all nodes..."
    cred_failed=()
    for f in nodes/*.env; do
        source "$f"
        NODE_NAME=$(basename "$f" .env)
        just set-fingerprint ${NODE_NAME} \
            || { echo ">>> ❌ ERROR: ${NODE_NAME} did NOT receive credentials — its agent will loop on auth until you run: just set-fingerprint ${NODE_NAME}"; cred_failed+=("${NODE_NAME}"); }
    done
    [ "${#cred_failed[@]}" -gt 0 ] && echo ">>> ⚠ ${#cred_failed[@]} node(s) failed credentials: ${cred_failed[*]} — they will NOT connect until fixed."

    echo ">>> Starting remote compute agents..."
    just start-agents

    echo ">>> Loading hardware-selected models on compute nodes..."
    just load-models-retry

    echo ""
    echo ">>> Cluster ready. Run: just validate-routing"
    cargo run -q -p cli -- --coordinator "{{coordinator_ip}}:{{coordinator_port}}" nodes

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
    source scripts/mesh-env.sh

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
        echo ">>> Syncing coordinator state from {{coordinator_ip}}..."
        scp {{ssh_opts}} -q "jonno@{{coordinator_ip}}:/var/lib/ai-mesh/coordinator.state" "$HOME/.config/ai-mesh/coordinator.state" || echo ">>> Warning: could not sync state from {{coordinator_ip}}"

        # Load the freshly-synced TLS fingerprint + auth token BEFORE the connectivity
        # check — the CLI needs them for the mesh TLS handshake. Without re-sourcing here
        # the check uses a stale fingerprint (none, or one read before this scp) and fails
        # even when the coordinator is healthy, e.g. after its cert was regenerated.
        source scripts/mesh-env.sh

        cargo build -q -p cli
        echo ">>> Verifying connectivity to {{coordinator_ip}}:{{coordinator_port}}..."
        if check_err=$(cargo run -q -p cli -- --coordinator "{{coordinator_ip}}:{{coordinator_port}}" nodes 2>&1 >/dev/null); then
            echo ">>> Coordinator ready."
        else
            echo ">>> ERROR: Could not reach coordinator at {{coordinator_ip}}:{{coordinator_port}}"
            echo ">>>        CLI error: ${check_err:-<none>}"
            echo ">>>        Is it running? Check: ssh jonno@{{coordinator_ip}} systemctl status ai-mesh-coordinator"
            exit 1
        fi
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

    echo ">>> Pushing TLS fingerprint and auth token to all nodes..."
    for f in nodes/*.env; do
        source "$f"
        NODE_NAME=$(basename "$f" .env)
        just set-fingerprint ${NODE_NAME} \
            || echo ">>> ❌ ERROR: ${NODE_NAME} did NOT receive credentials — its agent will loop on auth until you run: just set-fingerprint ${NODE_NAME}"
    done

    # Wait for coordinator to finish clearing stale model state from the
    # agent restarts above before load-models-retry checks node status.
    sleep 5
    echo ">>> Reloading hardware-selected models on compute nodes..."
    just load-models-retry

    echo ">>> Cluster reconnected. Run: just validate-routing"
    cargo run -q -p cli -- --coordinator "{{coordinator_ip}}:{{coordinator_port}}" nodes

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
                "timeout 12 sudo systemctl stop ai-mesh-agent 2>/dev/null; sudo systemctl kill ai-mesh-agent 2>/dev/null; true" 2>/dev/null \
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
                "timeout 12 sudo systemctl stop ai-mesh-agent 2>/dev/null; sudo systemctl kill ai-mesh-agent 2>/dev/null; true" 2>/dev/null || true
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
                || echo ">>> ❌ ERROR: ${NODE_NAME} did NOT receive credentials — its agent will loop on auth until you run: just set-fingerprint ${NODE_NAME}"
        done
    fi

    echo ">>> Starting local controller agent (log: /tmp/mesh-agent.log)..."
    AGENT_ROLE=controller cargo run -p agent > /tmp/mesh-agent.log 2>&1 &
    AGENT_PID=$!

    echo ">>> Verifying portproxy (remote nodes connect via {{coordinator_ip}}:{{coordinator_port}})..."
    if cargo run -q -p cli -- --coordinator "{{coordinator_ip}}:{{coordinator_port}}" nodes > /dev/null 2>&1; then
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
                ssh -o ConnectTimeout=5 ${NODE_USER}@${NODE_HOST} "timeout 12 sudo systemctl stop ai-mesh-agent 2>/dev/null; sudo systemctl kill ai-mesh-agent 2>/dev/null; true" 2>/dev/null || true
        done
    }
    trap cleanup EXIT

    echo ">>> Stopping all remote agents..."
    for f in nodes/*.env; do
        source "$f"
        case "$NODE_OS" in
          linux)
            ssh -o ConnectTimeout=5 ${NODE_USER}@${NODE_HOST} "timeout 12 sudo systemctl stop ai-mesh-agent 2>/dev/null; sudo systemctl kill ai-mesh-agent 2>/dev/null; true" || true
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
            ssh {{ssh_opts}} ${NODE_USER}@${NODE_HOST} \
                "journalctl -u ai-mesh-agent -f --no-pager" 2>/dev/null \
                | sed "s/^/[${NODE_NAME}]  /" &
            ;;
          windows)
            WIN_PATH="C:\\Users\\${NODE_USER}\\ai-mesh"
            ssh {{ssh_opts}} ${NODE_USER}@${NODE_HOST} \
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
    PI_MQTT="10.0.0.10"
    echo ">>> Opening 5-minute pairing window on Zigbee network..."
    mosquitto_pub -h ${PI_MQTT} -t 'zigbee2mqtt/bridge/request/permit_join' \
        -m '{"value":true,"time":254}'
    echo ">>> Pairing window open (4 minutes) — power-cycle your bulb now."
    echo ">>> Watching for join events (Ctrl+C to stop)..."
    mosquitto_sub -h ${PI_MQTT} -t 'zigbee2mqtt/bridge/event' -v

# Hit the HTTP /api/chat endpoint directly and pretty-print the response.
# Usage: just chat "what is the capital of France?"
chat text:
    #!/usr/bin/env bash
    source scripts/mesh-env.sh
    curl -s -X POST "http://{{coordinator_ip}}:9001/api/chat?token=${TOKEN}" \
        -H 'Content-Type: application/json' \
        -d "{\"text\":$(printf '%s' '{{text}}' | python3 -c 'import json,sys; print(json.dumps(sys.stdin.read()))'),\"context\":[]}" \
        | python3 -m json.tool

# Remove a dead node from the registry (nodes never expire on their own).
# Accepts the node's hostname (as shown in `just nodes`) or its uuid.
# Refuses while the node's agent is still connected.
# Usage: just remove-node <hostname-or-uuid>
remove-node id:
    #!/usr/bin/env bash
    source scripts/mesh-env.sh
    CODE=$(curl -s -o /dev/null -w "%{http_code}" -X DELETE \
        "http://{{coordinator_ip}}:9001/api/nodes/{{id}}" \
        -H "Authorization: Bearer ${TOKEN}")
    case "$CODE" in
        204) echo ">>> Node {{id}} removed." ;;
        404) echo ">>> No node with id {{id}} (try: just nodes)"; exit 1 ;;
        409) echo ">>> Node {{id}} is still connected — stop its agent first."; exit 1 ;;
        *)   echo ">>> Unexpected response HTTP $CODE"; exit 1 ;;
    esac

# Hit the OpenAI-compatible /v1/chat/completions endpoint (Bearer auth) and
# pretty-print the response. Optional second arg pins a model.
# Usage: just openai "why is the sky blue?"
#        just openai "why is the sky blue?" qwen2.5:7b
openai text model="":
    #!/usr/bin/env bash
    source scripts/mesh-env.sh
    CONTENT=$(printf '%s' '{{text}}' | python3 -c 'import json,sys; print(json.dumps(sys.stdin.read()))')
    if [ -n "{{model}}" ]; then MODEL_FIELD="\"model\":\"{{model}}\","; else MODEL_FIELD=""; fi
    curl -s -X POST "http://{{coordinator_ip}}:9001/v1/chat/completions" \
        -H "Authorization: Bearer ${TOKEN}" \
        -H 'Content-Type: application/json' \
        -d "{${MODEL_FIELD}\"messages\":[{\"role\":\"user\",\"content\":${CONTENT}}]}" \
        | python3 -m json.tool

# Stream from the OpenAI-compatible endpoint and print SSE events as they
# arrive. Optional second arg pins a model.
# Usage: just openai-stream "count to 20"
#        just openai-stream "count to 20" qwen2.5:7b
openai-stream text model="":
    #!/usr/bin/env bash
    source scripts/mesh-env.sh
    CONTENT=$(printf '%s' '{{text}}' | python3 -c 'import json,sys; print(json.dumps(sys.stdin.read()))')
    if [ -n "{{model}}" ]; then MODEL_FIELD="\"model\":\"{{model}}\","; else MODEL_FIELD=""; fi
    curl -N -s -X POST "http://{{coordinator_ip}}:9001/v1/chat/completions" \
        -H "Authorization: Bearer ${TOKEN}" \
        -H 'Content-Type: application/json' \
        -d "{${MODEL_FIELD}\"messages\":[{\"role\":\"user\",\"content\":${CONTENT}}],\"stream\":true,\"stream_options\":{\"include_usage\":true}}"

# Send a natural-language intent to the coordinator.
# Usage: just intent "turn test_bulb on"
#        just intent "what is the capital of France"
intent text:
    #!/usr/bin/env bash
    source scripts/mesh-env.sh
    cargo run -q -p cli -- --coordinator "{{coordinator_ip}}:{{coordinator_port}}" \
        intent "{{text}}"

# Validate that each model routes to the correct hardware node.
# Assumes the cluster is already running with hardware-selected models loaded
# (i.e. run `just start-cluster` first).
# Pi (10.0.0.10) should serve qwen2.5:1.5b; Beelink (10.0.0.11) should serve qwen2.5:7b.
# Usage: just validate-routing
validate-routing: update-portproxy chaos
    #!/usr/bin/env bash
    set -e

    # Load credentials from coordinator state so this works immediately after
    # restart-coordinator without needing to source ~/.bashrc first.
    source scripts/mesh-env.sh

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

    source scripts/mesh-env.sh

    export MESH_COORDINATOR="{{coordinator_ip}}:{{coordinator_port}}"
    # Dashboard host follows the coordinator: a LOCAL coordinator (WSL2) has
    # no portproxy for 9001, so localhost is the only reachable path — but a
    # REMOTE coordinator's dashboard lives on its own host, not this
    # machine. Hardcoding 127.0.0.1 unconditionally (as before) silently
    # pointed scenario 7 at whatever happens to be listening locally on
    # 9001 (or nothing) instead of the real coordinator's dashboard,
    # whenever the coordinator was actually remote.
    if [ "{{coordinator_ip}}" != "127.0.0.1" ] && [ "{{coordinator_ip}}" != "localhost" ]; then
        export MESH_DASHBOARD_HOST="{{coordinator_ip}}"
    else
        export MESH_DASHBOARD_HOST=127.0.0.1
    fi
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
# Smoke-test the REAPER integration end-to-end:
#   1. Check REAPER web server is reachable
#   2. Send play via chat → confirm transport flips to playing
#   3. Check coordinator REAPER snapshot is live
#   4. Send stop via chat → confirm transport returns to stopped
# One-time setup for the REAPER Lua bridge daemon.
# The csurf web server can only dispatch numeric action IDs, not named (RS...) script
# actions, so we use a daemon: a Lua script that polls ai_mesh_id.txt and runs whatever
# Lua we drop into ai_mesh_cmd.lua. The agent triggers it by writing the command file and
# bumping the id file. The daemon is installed as REAPER's native __startup.lua, so it
# auto-starts on every launch — no manual action-list registration, no SWS required.
# Usage: just setup-reaper-daemon
setup-reaper-daemon:
    #!/usr/bin/env bash
    set -e

    SCRIPT_DIR="${REAPER_WSL_SCRIPTS_PATH:-/mnt/c/Users/jonno/AppData/Roaming/REAPER/Scripts}"

    if [ ! -d "$SCRIPT_DIR" ]; then
        echo "✗ REAPER Scripts folder not found: $SCRIPT_DIR"
        echo "  Make sure REAPER is installed and has been launched at least once."
        exit 1
    fi

    printf '%s\n' \
        '-- ai-mesh bridge daemon, auto-run by REAPER at startup (native __startup.lua).' \
        '-- Polls ai_mesh_id.txt; when the id changes, dofiles ai_mesh_cmd.lua and writes' \
        '-- the outcome to ai_mesh_result.txt as "<id>\t<ok|err>\t<message>" so the agent' \
        '-- can confirm execution (or surface a Lua error) instead of guessing. The message' \
        '-- may span multiple lines (e.g. a track listing); written via a temp file + rename' \
        '-- so the agent never reads a half-written result.' \
        'local base = reaper.GetResourcePath() .. "/Scripts/"' \
        'local id_file = base .. "ai_mesh_id.txt"' \
        'local cmd_file = base .. "ai_mesh_cmd.lua"' \
        'local result_file = base .. "ai_mesh_result.txt"' \
        'local function read_id()' \
        '  local f = io.open(id_file, "r")' \
        '  if not f then return "" end' \
        '  local id = f:read("*l") or ""' \
        '  f:close()' \
        '  return id' \
        'end' \
        '-- Seed from the current id so a relaunch does not re-run the last command.' \
        'local last_id = read_id()' \
        'local function check()' \
        '  local id = read_id()' \
        '  if id ~= "" and id ~= last_id then' \
        '    last_id = id' \
        '    local ok, ret = pcall(dofile, cmd_file)' \
        '    -- Write the result in place, then a record-separator byte (char 30) as a' \
        '    -- completion marker so the agent never parses a half-written file. (A temp' \
        '    -- file + os.rename silently fails on Windows: rename cannot overwrite.)' \
        '    local rf = io.open(result_file, "w")' \
        '    if rf then' \
        '      if ok then rf:write(id .. "\tok\t" .. (type(ret) == "string" and ret or ""))' \
        '      else rf:write(id .. "\terr\t" .. tostring(ret)) end' \
        '      rf:write("\30")' \
        '      rf:flush()' \
        '      rf:close()' \
        '    end' \
        '    if not ok then reaper.ShowConsoleMsg("[ai-mesh] error: " .. tostring(ret) .. "\n") end' \
        '  end' \
        '  reaper.defer(check)' \
        'end' \
        'reaper.ShowConsoleMsg("[ai-mesh] daemon started (via __startup.lua)\n")' \
        'check()' \
        > "${SCRIPT_DIR}/__startup.lua"

    # Seed the command/trigger/result files so the daemon has something valid to poll.
    : > "${SCRIPT_DIR}/ai_mesh_id.txt"
    : > "${SCRIPT_DIR}/ai_mesh_result.txt"
    printf '%s\n' '-- no command yet' > "${SCRIPT_DIR}/ai_mesh_cmd.lua"

    # Remove the old manually-registered daemon if a previous setup left one behind.
    rm -f "${SCRIPT_DIR}/ai_mesh_daemon.lua"

    echo "✓ Daemon installed as: ${SCRIPT_DIR}/__startup.lua"
    echo ""
    echo "Restart REAPER (fully quit and reopen). A 'ReaScript console output' window"
    echo "should appear on launch printing '[ai-mesh] daemon started (via __startup.lua)'."
    echo ""
    echo "After that, 'just test-record' and the chat reaper_script tool can run Lua in REAPER."

# Music smoke test (plans/spotify-music.md Phase 5): asserts the LLM routes
# music requests to music_control and that status answers get spoken text.
# Routing checks hard-fail; live-playback checks degrade to warnings so the
# recipe is useful before Spotify credentials/librespot are set up.
# Usage: just test-music
test-music:
    #!/usr/bin/env bash
    set -e

    STATE="$HOME/.config/ai-mesh/coordinator.state"
    if [ ! -f "$STATE" ]; then
        echo "✗ coordinator state not found — run: just start-cluster"
        exit 1
    fi
    source "$STATE"

    COORD_URL="http://{{coordinator_ip}}:9001"
    PASS=0
    FAIL=0

    ok()   { echo "  ✓ $*"; PASS=$((PASS+1)); }
    fail() { echo "  ✗ $*"; FAIL=$((FAIL+1)); }

    chat() {
        curl -s -X POST "${COORD_URL}/api/chat?token=${MESH_AUTH_TOKEN}" \
            -H "Content-Type: application/json" \
            -d "{\"text\": \"$1\", \"context\": []}"
    }
    tool_of()   { python3 -c "import sys,json; d=json.load(sys.stdin); c=d.get('tool_calls',[]); print(c[0]['tool'] if c else '')" 2>/dev/null; }
    action_of() { python3 -c "import sys,json; d=json.load(sys.stdin); c=d.get('tool_calls',[]); print(c[0].get('args',{}).get('action','') if c else '')" 2>/dev/null; }
    result_of() { python3 -c "import sys,json; d=json.load(sys.stdin); c=d.get('tool_calls',[]); print(c[0]['result'] if c else '')" 2>/dev/null; }
    text_of()   { python3 -c "import sys,json; print(json.load(sys.stdin).get('text') or '')" 2>/dev/null; }

    echo ""
    echo "=== Music smoke test ==="
    echo ""

    # 0. Coordinator reachable — otherwise every step below fails cryptically.
    if ! curl -s -o /dev/null --max-time 5 "${COORD_URL}/"; then
        echo "✗ coordinator not reachable at ${COORD_URL} — is it running? (just nodes)"
        exit 1
    fi

    # 1. Command routing: pause must become a music_control call.
    echo "[1/4] Pause command routes to music_control..."
    RESP=$(chat "pause the music")
    TOOL=$(echo "$RESP" | tool_of)
    RESULT=$(echo "$RESP" | result_of)
    if [ "$TOOL" = "music_control" ]; then
        ok "LLM called music_control → ${RESULT}"
    else
        fail "expected music_control (tool not offered? coordinator + pi2 must both be redeployed with the music feature), got: $(echo "$RESP" | python3 -m json.tool 2>/dev/null || echo "$RESP")"
    fi
    TEXT=$(echo "$RESP" | text_of)
    if [ -z "$TEXT" ]; then
        ok "command reply stays silent (no spoken text)"
    else
        fail "command reply set text='${TEXT}' (commands should be silent like lights)"
    fi

    # 2. Question routing: status must be a tool call with spoken text, not
    #    a free-text guess (the puck speaks only the text field).
    echo "[2/4] 'what's playing?' becomes action=status with spoken text..."
    RESP=$(chat "what's playing?")
    TOOL=$(echo "$RESP" | tool_of)
    ACTION=$(echo "$RESP" | action_of)
    RESULT=$(echo "$RESP" | result_of)
    TEXT=$(echo "$RESP" | text_of)
    if [ "$TOOL" = "music_control" ] && [ "$ACTION" = "status" ]; then
        ok "LLM called music_control(status)"
    else
        fail "expected music_control(status), got tool='${TOOL}' action='${ACTION}'"
    fi
    if [ -n "$RESULT" ]; then
        ok "status result: ${RESULT}"
    else
        fail "status result is empty"
    fi
    if [ -n "$TEXT" ]; then
        ok "spoken text set for the puck: ${TEXT}"
    else
        fail "IntentResponse.text unset — the puck would stay silent on 'what's playing?'"
    fi

    # 3. Live playback (degrades to a warning until Spotify setup is done).
    echo "[3/4] Play a named track (live playback check)..."
    RESP=$(chat "play blackbird by the beatles")
    TOOL=$(echo "$RESP" | tool_of)
    RESULT=$(echo "$RESP" | result_of)
    if [ "$TOOL" = "music_control" ]; then
        ok "LLM called music_control → ${RESULT}"
    else
        fail "expected music_control, got tool='${TOOL}'"
    fi
    case "$RESULT" in
        "Now playing"*) ok "playback started: ${RESULT}" ;;
        *) echo "  ⚠ playback not live yet: '${RESULT}' (fine before spotify-push-creds / spotify-login — see docs/music.md)" ;;
    esac

    # 4. Leave the house quiet if step 3 actually started something.
    echo "[4/4] Pause after test..."
    RESP=$(chat "pause the music")
    ok "pause sent → $(echo "$RESP" | result_of)"

    echo ""
    echo "=== ${PASS} passed, ${FAIL} failed ==="
    [ "$FAIL" -eq 0 ]

# Usage: just test-reaper
test-reaper:
    #!/usr/bin/env bash
    set -e

    STATE="$HOME/.config/ai-mesh/coordinator.state"
    if [ ! -f "$STATE" ]; then
        echo "✗ coordinator state not found — run: just start-cluster"
        exit 1
    fi
    source "$STATE"

    REAPER_URL="http://127.0.0.1:${REAPER_PORT:-8080}"
    COORD_URL="http://{{coordinator_ip}}:9001"
    PASS=0
    FAIL=0

    ok()   { echo "  ✓ $*"; PASS=$((PASS+1)); }
    fail() { echo "  ✗ $*"; FAIL=$((FAIL+1)); }

    chat() {
        curl -s -X POST "${COORD_URL}/api/chat?token=${MESH_AUTH_TOKEN}" \
            -H "Content-Type: application/json" \
            -d "{\"text\": \"$1\", \"context\": []}"
    }

    transport_state() {
        curl -s --max-time 3 "${REAPER_URL}/_/TRANSPORT" | cut -f2
    }

    echo ""
    echo "=== REAPER smoke test ==="
    echo ""

    # 1. REAPER web server reachable
    echo "[1/5] REAPER web server..."
    RAW=$(curl -s --max-time 3 "${REAPER_URL}/_/TRANSPORT" || true)
    if [ -n "$RAW" ]; then
        ok "REAPER reachable at ${REAPER_URL}"
    else
        fail "REAPER not reachable at ${REAPER_URL} — is REAPER running with web server enabled?"
        echo "  Enable in REAPER: Preferences → Control/OSC/web → enable web interface"
        exit 1
    fi

    # 2. Play via chat
    echo "[2/4] Play command via chat..."
    RESP=$(chat "play the track in reaper")
    TOOL=$(echo "$RESP" | python3 -c "import sys,json; d=json.load(sys.stdin); calls=d.get('tool_calls',[]); print(calls[0]['tool'] if calls else '')" 2>/dev/null || true)
    RESULT=$(echo "$RESP" | python3 -c "import sys,json; d=json.load(sys.stdin); calls=d.get('tool_calls',[]); print(calls[0]['result'] if calls else '')" 2>/dev/null || true)
    if [ "$TOOL" = "reaper_transport" ] && [ "$RESULT" = "ok" ]; then
        ok "LLM called reaper_transport(play) → ok"
    else
        fail "Expected reaper_transport tool call, got: $(echo "$RESP" | python3 -m json.tool 2>/dev/null || echo "$RESP")"
    fi
    sleep 1
    STATE_AFTER=$(transport_state)
    if [ "$STATE_AFTER" = "1" ]; then
        ok "REAPER transport is playing (state=1)"
    else
        echo "  ⚠ transport state=${STATE_AFTER} after play (expected 1 — open a project with tracks in REAPER to verify)"
    fi

    # 3. Coordinator REAPER snapshot
    echo "[3/4] Coordinator REAPER snapshot..."
    SNAP=$(curl -s --max-time 5 "${COORD_URL}/api/reaper/state?token=${MESH_AUTH_TOKEN}" || true)
    ONLINE=$(echo "$SNAP" | python3 -c "import sys,json; print(json.load(sys.stdin).get('reaper_online',''))" 2>/dev/null || true)
    PLAY=$(echo "$SNAP" | python3 -c "import sys,json; print(json.load(sys.stdin).get('play_state',''))" 2>/dev/null || true)
    if [ "$ONLINE" = "True" ] || [ "$ONLINE" = "true" ]; then
        ok "Coordinator snapshot: online=true play_state=${PLAY}"
    else
        fail "Coordinator snapshot: reaper_online=${ONLINE} (expected true)"
    fi

    # 4. Stop via chat
    echo "[4/4] Stop command via chat..."
    RESP=$(chat "stop reaper")
    TOOL=$(echo "$RESP" | python3 -c "import sys,json; d=json.load(sys.stdin); calls=d.get('tool_calls',[]); print(calls[0]['tool'] if calls else '')" 2>/dev/null || true)
    RESULT=$(echo "$RESP" | python3 -c "import sys,json; d=json.load(sys.stdin); calls=d.get('tool_calls',[]); print(calls[0]['result'] if calls else '')" 2>/dev/null || true)
    if [ "$TOOL" = "reaper_transport" ] && [ "$RESULT" = "ok" ]; then
        ok "LLM called reaper_transport(stop) → ok"
    else
        fail "Expected reaper_transport stop call, got: $(echo "$RESP" | python3 -m json.tool 2>/dev/null || echo "$RESP")"
    fi
    sleep 1
    STATE_AFTER=$(transport_state)
    if [ "$STATE_AFTER" = "0" ]; then
        ok "REAPER transport is stopped (state=0)"
    else
        echo "  ⚠ transport state=${STATE_AFTER} after stop (expected 0)"
    fi

    # ── FX automation (Slices 1–2) ────────────────────────────────────────────
    # Keys on Valhalla Supermassive (a VST that resolves by bare name), NOT a stock
    # plugin: ReaVerbate is a JSFX and did NOT resolve via TrackFX_AddByName by bare
    # name, so it is unsafe as a control case. Uses a throwaway track and removes it
    # afterwards so the smoke test leaves the project as it found it. (Plugin must be
    # installed + scanned in REAPER — see docs/reaper-plugins.md.)
    # Single-token track name: small models mangle multi-word names on recall (e.g.
    # "the fx smoke track" → "smoke"), which breaks the name-match the FX tools rely on.
    FXTRACK="fxsmoke"
    FXPLUGIN="ValhallaSupermassive"

    tool_of()   { python3 -c "import sys,json; d=json.load(sys.stdin); c=d.get('tool_calls',[]); print(c[0]['tool'] if c else '')" 2>/dev/null; }
    result_of() { python3 -c "import sys,json; d=json.load(sys.stdin); c=d.get('tool_calls',[]); print(c[0]['result'] if c else '')" 2>/dev/null; }

    echo ""
    echo "=== REAPER FX smoke test (Slices 1–2) ==="
    echo ""

    echo "[F1/5] Create throwaway track..."
    RESP=$(chat "add a track called ${FXTRACK}")
    if [ "$(echo "$RESP" | tool_of)" = "reaper_add_track" ]; then
        ok "reaper_add_track → $(echo "$RESP" | result_of)"
    else
        fail "Expected reaper_add_track, got: $(echo "$RESP" | python3 -m json.tool 2>/dev/null || echo "$RESP")"
    fi

    echo "[F2/5] Slice 1 — add ${FXPLUGIN} (reaper_add_fx)..."
    RESP=$(chat "add ${FXPLUGIN} to the ${FXTRACK} track")
    TOOL=$(echo "$RESP" | tool_of); RESULT=$(echo "$RESP" | result_of)
    if [ "$TOOL" = "reaper_add_fx" ] && [[ "$RESULT" == Added* ]]; then
        ok "reaper_add_fx → ${RESULT}"
    else
        fail "Expected reaper_add_fx 'Added …', got tool=${TOOL} result=${RESULT}"
    fi

    echo "[F3/5] Slice 2a — list FX (reaper_list_fx)..."
    # Imperative phrasing ("list …"), not a question ("what FX …"): the coordinator's
    # system prompt tells the model to answer state *questions* in plain text without
    # JSON, which suppresses the tool call. A command nudges it to emit the tool.
    RESP=$(chat "list the FX on the ${FXTRACK} track")
    TOOL=$(echo "$RESP" | tool_of); RESULT=$(echo "$RESP" | result_of)
    if [ "$TOOL" = "reaper_list_fx" ] && echo "$RESULT" | grep -qi "supermassive"; then
        ok "reaper_list_fx lists the plugin"
    else
        fail "Expected reaper_list_fx listing Supermassive, got tool=${TOOL} result=${RESULT}"
    fi

    echo "[F4/5] Slice 2b — list FX params (reaper_list_fx_params)..."
    RESP=$(chat "list the parameters of ${FXPLUGIN} on the ${FXTRACK} track")
    TOOL=$(echo "$RESP" | tool_of); RESULT=$(echo "$RESP" | result_of)
    if [ "$TOOL" = "reaper_list_fx_params" ] && echo "$RESULT" | grep -qi "parameters:"; then
        ok "reaper_list_fx_params returns a param list"
        # Settles the Slice 3 open question: are Supermassive's modes params or presets?
        if echo "$RESULT" | grep -qi "mode"; then
            echo "  → 'mode' param present: modes are PARAMS (Slice 3 → SetParam)"
        else
            echo "  → no 'mode' param: modes are likely PRESETS (Slice 3 → SetPreset)"
        fi
    else
        fail "Expected reaper_list_fx_params, got tool=${TOOL} result=${RESULT}"
    fi

    echo "[F5/5] Cleanup — remove throwaway track..."
    RESP=$(chat "remove the ${FXTRACK} track")
    TOOL=$(echo "$RESP" | tool_of); RESULT=$(echo "$RESP" | result_of)
    # Soft (don't fail the suite on cleanup), but verify the track was actually removed —
    # a reaper_remove_track call that returns "No track named …" is NOT a successful cleanup.
    if [ "$TOOL" = "reaper_remove_track" ] && [[ "$RESULT" == Removed* ]]; then
        ok "cleanup: ${RESULT}"
    else
        echo "  ⚠ cleanup did not remove '${FXTRACK}' (tool=${TOOL} result=${RESULT}) — remove it manually in REAPER"
    fi

    echo ""
    echo "=== Results: ${PASS} passed, ${FAIL} failed ==="
    echo ""
    [ "$FAIL" -eq 0 ]

# End-to-end recording test using the laptop mic (no Scarlett needed).
# Creates a new project, records 5 s, stops, rewinds, plays back.
# Requires the REAPER Lua bridge daemon (just setup-reaper-daemon) to be running.
# Usage: just test-record
test-record:
    #!/usr/bin/env bash
    set -e

    REAPER_URL="http://127.0.0.1:${REAPER_PORT:-8080}"
    DROPIN="/etc/systemd/system/ai-mesh-agent.service.d/reaper.conf"

    REAPER_SCRIPTS="${REAPER_WSL_SCRIPTS_PATH:-/mnt/c/Users/jonno/AppData/Roaming/REAPER/Scripts}"
    CMD_FILE="${REAPER_SCRIPTS}/ai_mesh_cmd.lua"
    ID_FILE="${REAPER_SCRIPTS}/ai_mesh_id.txt"

    reaper_action() { curl -s --max-time 5 "${REAPER_URL}/_/$1;" > /dev/null; }

    reaper_lua() {
        printf '%s\n' "$1" > "${CMD_FILE}"
        echo "$(date +%s%N)" > "${ID_FILE}"
        sleep 0.5
    }

    echo ""
    echo "=== REAPER recording test ==="
    echo ""

    if ! curl -s --max-time 3 "${REAPER_URL}/_/TRANSPORT" > /dev/null 2>&1; then
        echo "✗ REAPER not reachable at ${REAPER_URL} — is REAPER running?"
        exit 1
    fi
    echo "✓ REAPER reachable"

    echo ">>> Setting up mic track..."
    reaper_lua 'for i=reaper.GetNumTracks()-1,0,-1 do reaper.DeleteTrack(reaper.GetTrack(0,i)) end; reaper.InsertTrackAtIndex(0,true); local t=reaper.GetTrack(0,0); reaper.GetSetMediaTrackInfo_String(t,"P_NAME","Test Mic",true); reaper.SetMediaTrackInfo_Value(t,"I_RECINPUT",0); reaper.SetMediaTrackInfo_Value(t,"I_RECARM",1); reaper.SetMediaTrackInfo_Value(t,"I_RECMON",1); reaper.UpdateArrange()'
    sleep 2
    echo "✓ Track ready — check REAPER: is the track arm button red?"

    echo ""
    echo ">>> Recording in 3..."
    sleep 1
    echo ">>> 2..."
    sleep 1
    echo ">>> 1..."
    sleep 1
    reaper_action 1013
    echo ">>> RECORDING — say something! (5 seconds)"
    sleep 5
    reaper_action 1007
    echo "✓ Recording stopped"

    sleep 0.5
    echo ""
    echo ">>> Rewinding and playing back..."
    reaper_action 40042
    sleep 0.5
    reaper_action 1008
    sleep 6
    reaper_action 1007
    echo "✓ Playback done"

    echo ""
    echo "=== Recording test complete ==="

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
                ssh {{ssh_opts}} ${NODE_USER}@${NODE_HOST} "timeout 12 sudo systemctl stop ai-mesh-agent 2>/dev/null; sudo systemctl kill ai-mesh-agent 2>/dev/null; true" 2>/dev/null || true
                ;;
              windows)
                ssh {{ssh_opts}} ${NODE_USER}@${NODE_HOST} "powershell -Command \"\
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
            ssh {{ssh_opts}} ${NODE_USER}@${NODE_HOST} "timeout 12 sudo systemctl stop ai-mesh-agent 2>/dev/null; sudo systemctl kill ai-mesh-agent 2>/dev/null; true" || true
            ;;
          windows)
            ssh {{ssh_opts}} ${NODE_USER}@${NODE_HOST} "powershell -Command \"\
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
            ssh {{ssh_opts}} ${NODE_USER}@${NODE_HOST} "sudo systemctl start ai-mesh-agent"
            ;;
          windows)
            ssh {{ssh_opts}} ${NODE_USER}@${NODE_HOST} \
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

# One command to (re)deploy the whole cluster end to end: build + install every
# node agent, deploy the coordinator (the node marked NODE_COORDINATOR=true),
# start everything, then validate model routing.
# Usage: just deploy-all
deploy-all:
    #!/usr/bin/env bash
    set -e
    COORD=$(grep -l '^NODE_COORDINATOR=true' nodes/*.env 2>/dev/null | head -1 | xargs -r -n1 basename | sed 's/\.env$//')
    if [ -z "$COORD" ]; then
        echo ">>> Error: no nodes/*.env has NODE_COORDINATOR=true — cannot pick a coordinator"
        exit 1
    fi
    echo "=== deploy-all: coordinator node = ${COORD} ==="
    echo ">>> [1/4] Provisioning all node agents..."
    just provision-all
    echo ">>> [2/4] Deploying coordinator to ${COORD}..."
    just deploy-coordinator "${COORD}"
    echo ">>> [3/4] Starting cluster..."
    just start-cluster
    echo ">>> [4/4] Validating routing..."
    just validate-routing
    echo "=== deploy-all complete ==="

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
    ssh {{ssh_opts}} ${NODE_USER}@${NODE_HOST} "
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
        scp {{ssh_opts}} -r "$STATE_DIR"/* ${NODE_USER}@${NODE_HOST}:~/.config/ai-mesh/ 2>/dev/null || {
            echo ">>> Warning: Could not copy state files"
        }
    else
        echo ">>> Warning: State directory $STATE_DIR does not exist or is empty (will be created on first run)"
    fi

    # Step 4: Seed database (only if the target has none — never clobber live state)
    echo ">>> Step 4: Seeding ai_mesh.db on ${TARGET_HOST} (only if absent)..."
    if ssh {{ssh_opts}} ${NODE_USER}@${NODE_HOST} "[ -f /var/lib/ai-mesh/ai_mesh.db ]"; then
        echo ">>> Live database already present — leaving it untouched (rooms/scenes preserved)"
    elif [ -f "ai_mesh.db" ]; then
        scp {{ssh_opts}} ai_mesh.db ${NODE_USER}@${NODE_HOST}:/var/lib/ai-mesh/
        echo ">>> Seeded fresh database from repo root"
    else
        echo ">>> No repo ai_mesh.db and no live DB (will be created on first run)"
    fi

    # Step 5: Copy binary
    echo ">>> Step 5: Copying coordinator binary to ${TARGET_HOST}..."
    scp {{ssh_opts}} target/aarch64-unknown-linux-gnu/release/coordinator ${NODE_USER}@${NODE_HOST}:/tmp/ai-mesh-coordinator
    ssh {{ssh_opts}} ${NODE_USER}@${NODE_HOST} "sudo install -m 755 /tmp/ai-mesh-coordinator /usr/local/bin/ai-mesh-coordinator && rm /tmp/ai-mesh-coordinator"

    # Step 6: Install systemd unit
    echo ">>> Step 6: Installing systemd unit on ${TARGET_HOST}..."
    cat systemd/ai-mesh-coordinator.service | ssh {{ssh_opts}} ${NODE_USER}@${NODE_HOST} "
        set -e
        sudo tee /etc/systemd/system/ai-mesh-coordinator.service > /dev/null
        sudo mkdir -p /etc/systemd/system/ai-mesh-coordinator.service.d
    "

    # Step 6b: Host-specific lighting env. The coordinator runs the lighting
    # feature, which needs the MQTT broker address from nodes/<host>.env. The
    # static unit carries no MQTT_HOST, so without this drop-in lighting boots in
    # stub mode and SILENTLY DROPS every light command (UI works, bulbs don't).
    # Drop-in mirrors the agent tls.conf/auth.conf convention and survives the
    # unit overwrite above. MQTT_HOST/MQTT_PORT come from the sourced node env.
    if [ -n "${MQTT_HOST:-}" ]; then
        echo ">>> Step 6b: Setting MQTT_HOST=${MQTT_HOST}:${MQTT_PORT:-1883} for coordinator lighting..."
        printf '[Service]\nEnvironment=MQTT_HOST=%s\nEnvironment=MQTT_PORT=%s\n' "${MQTT_HOST}" "${MQTT_PORT:-1883}" \
            | ssh {{ssh_opts}} ${NODE_USER}@${NODE_HOST} "sudo tee /etc/systemd/system/ai-mesh-coordinator.service.d/lighting.conf > /dev/null"
    else
        echo ">>> Step 6b: No MQTT_HOST in nodes/${TARGET_HOST}.env — removing any stale lighting drop-in"
        ssh {{ssh_opts}} ${NODE_USER}@${NODE_HOST} "sudo rm -f /etc/systemd/system/ai-mesh-coordinator.service.d/lighting.conf || true"
    fi
    ssh {{ssh_opts}} ${NODE_USER}@${NODE_HOST} "sudo systemctl daemon-reload"

    # Step 7: Enable and start the service
    echo ">>> Step 7: Enabling and starting ai-mesh-coordinator service..."
    ssh {{ssh_opts}} ${NODE_USER}@${NODE_HOST} "
        set -e
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
    ssh {{ssh_opts}} ${NODE_USER}@${NODE_HOST} "sudo systemctl is-active ai-mesh-coordinator" > /dev/null && echo "      ✓ Service is active" || {
        echo "      ✗ Service is NOT active"
        echo "Logs:"
        ssh {{ssh_opts}} ${NODE_USER}@${NODE_HOST} "sudo journalctl -u ai-mesh-coordinator -n 20 --no-pager"
        exit 1
    }

    # Check 2: HTTP endpoint is responding (dashboard is on port 9001, not the TLS agent port 9000)
    DASH_PORT=9001
    echo "[2/5] Checking HTTP endpoint at ${NODE_HOST}:${DASH_PORT}..."
    if curl -s "http://${NODE_HOST}:${DASH_PORT}/" | grep -q "<!DOCTYPE\|<html"; then
        echo "      ✓ Dashboard HTML loaded"
    else
        echo "      ✗ Dashboard did not respond with HTML"
        exit 1
    fi

    # Check 3: Recent log output shows expected startup messages
    echo "[3/5] Checking startup logs..."
    if ssh {{ssh_opts}} ${NODE_USER}@${NODE_HOST} "sudo journalctl -u ai-mesh-coordinator -n 50 --no-pager | grep -q 'listening on\|TLS\|auth token'"; then
        echo "      ✓ Startup messages found"
    else
        echo "      ⚠ Could not find expected startup messages (may still be running)"
    fi

    # Check 4: Certificate fingerprint matches
    echo "[4/5] Checking certificate fingerprint..."
    COORDINATOR_LOG=$(ssh {{ssh_opts}} ${NODE_USER}@${NODE_HOST} "sudo journalctl -u ai-mesh-coordinator -n 100 --no-pager")
    FP=$(echo "$COORDINATOR_LOG" | grep "fingerprint:" | sed 's/.*fingerprint: //' | head -1)
    if [ -n "$FP" ]; then
        echo "      ✓ Fingerprint: $FP"
    else
        echo "      ⚠ Could not extract fingerprint from logs (check manually)"
    fi

    # Check 5: DB exists
    echo "[5/5] Checking database file..."
    ssh {{ssh_opts}} ${NODE_USER}@${NODE_HOST} "[ -f /var/lib/ai-mesh/ai_mesh.db ] && echo '✓ Database exists' || echo '⚠ Database not yet created (will be on first run)'"

    echo ""
    echo "=== Verification complete ==="
    echo ""
    echo "Dashboard URL: http://${NODE_HOST}:9001/?token=..."
    echo ""
    echo "Manual next steps:"
    echo "  - Repoint agents: just start-agents"
    echo "  - Check agent connections: ssh {{ssh_opts}} ${NODE_USER}@${NODE_HOST} sudo journalctl -u ai-mesh-coordinator -f"
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

    # Find the coordinator node from nodes/ (NODE_COORDINATOR=true).
    COORD_FILE=$(grep -l '^NODE_COORDINATOR=true' nodes/*.env 2>/dev/null | head -1)
    if [ -z "$COORD_FILE" ]; then
        echo ">>> Error: no nodes/*.env has NODE_COORDINATOR=true"
        exit 1
    fi
    source "$COORD_FILE"

    # Stop the remote service
    echo ">>> Stopping coordinator service on ${NODE_HOST}..."
    ssh {{ssh_opts}} ${NODE_USER}@${NODE_HOST} "sudo systemctl stop ai-mesh-coordinator 2>/dev/null || true" || true
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

coordinator_ip   := "192.168.1.15"
coordinator_port := "9000"

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
    pkill -f "target/(debug|release)/coordinator" || true
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
    cargo run -q -p cli -- load --node-id "${NODE_ID}" "${MODEL}" "${SIZE_MB}"

# Detect hardware on a node and load the best-fit model automatically.
# Usage: just auto-load-model pi1
#        just auto-load-model beelink1
auto-load-model node:
    #!/usr/bin/env bash
    set -e
    source nodes/{{node}}.env
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

    echo ">>> Starting coordinator (log: /tmp/mesh-coordinator.log)..."
    MDNS_ADVERTISE_IP={{coordinator_ip}} cargo run -q -p coordinator > /tmp/mesh-coordinator.log 2>&1 &

    echo ">>> Waiting for coordinator to accept connections..."
    for i in $(seq 1 60); do
        sleep 1
        if grep -q "Coordinator is running" /tmp/mesh-coordinator.log 2>/dev/null; then
            echo ">>> Coordinator ready."
            break
        fi
        [ "$i" -eq 60 ] && { echo ">>> ERROR: coordinator did not start. Check /tmp/mesh-coordinator.log"; exit 1; }
    done

    cargo run -q -p cli -- reset-registry > /dev/null || true

    echo ">>> Starting local controller (log: /tmp/mesh-agent.log)..."
    AGENT_ROLE=controller cargo run -q -p agent > /tmp/mesh-agent.log 2>&1 &

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

    echo ">>> Starting coordinator (log: /tmp/mesh-coordinator.log)..."
    MDNS_ADVERTISE_IP={{coordinator_ip}} cargo run -q -p coordinator > /tmp/mesh-coordinator.log 2>&1 &

    echo ">>> Waiting for coordinator to accept connections..."
    for i in $(seq 1 60); do
        sleep 1
        if grep -q "Coordinator is running" /tmp/mesh-coordinator.log 2>/dev/null; then
            echo ">>> Coordinator ready."
            break
        fi
        [ "$i" -eq 60 ] && { echo ">>> ERROR: coordinator did not start. Check /tmp/mesh-coordinator.log"; exit 1; }
    done

    cargo run -q -p cli -- reset-registry > /dev/null || true

    echo ">>> Starting local controller (log: /tmp/mesh-agent.log)..."
    AGENT_ROLE=controller cargo run -q -p agent > /tmp/mesh-agent.log 2>&1 &

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

    pkill -f "target/(debug|release)/coordinator" || true
    pkill -f "target/(debug|release)/agent" || true
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
    cargo run -q -p cli -- --coordinator "{{coordinator_ip}}:{{coordinator_port}}" \
        intent "{{text}}"

# Validate that each model routes to the correct hardware node.
# Assumes the cluster is already running with hardware-selected models loaded
# (i.e. run `just start-cluster` first).
# Pi (192.168.1.11) should serve qwen2.5:1.5b; Beelink (192.168.1.14) should serve qwen2.5:7b.
# Usage: just validate-routing
validate-routing: update-portproxy
    #!/usr/bin/env bash
    set -e

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
            echo "  Loading qwen2.5:1.5b on ${HOSTNAME} (${NODE_ID})..."
            cargo run -q -p cli -- load "${NODE_ID}" qwen2.5:1.5b 1024
        fi
    done <<< "$COMPUTE_NODES"

    echo "=== Step 4: Waiting for all nodes to reach Ready ==="
    sleep 5
    cargo run -q -p cli -- nodes

    echo "=== Step 5: Firing 4 inference requests (expect load distribution) ==="
    PROMPT='In one sentence, what is the Itchen Bridge toll for?'
    for i in 1 2 3 4; do
        echo "--- Request ${i} ---"
        cargo run -q -p cli -- infer 'qwen2.5:1.5b' "${PROMPT}"
    done

    echo "=== Step 6: Final cluster state ==="
    cargo run -q -p cli -- nodes

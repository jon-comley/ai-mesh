#!/usr/bin/env bash
# Install or re-install the ai-mesh-agent systemd service on a Linux node.
# Assumes agent binary is already uploaded to ~/agent on the remote machine.
# Run via SSH: ssh user@host "sudo bash /tmp/install-node.sh <coordinator_ip> <role> <user> [mqtt_host] [mqtt_port]"
set -e

COORDINATOR_IP="$1"
ROLE="${2:-compute}"
AGENT_USER="$3"
MQTT_HOST="${4:-}"
MQTT_PORT="${5:-1883}"

if [ -z "$COORDINATOR_IP" ] || [ -z "$AGENT_USER" ]; then
    echo "Usage: $0 <coordinator_ip> <role> <user> [mqtt_host] [mqtt_port]"
    exit 1
fi

# Select the best model based on available GPU VRAM (AMD/Nvidia) or system RAM.
detect_default_model() {
    local mem_mb=0 gpu=0

    # AMD GPU via ROCm sysfs
    for f in /sys/class/drm/card*/device/mem_info_vram_total; do
        [ -f "$f" ] || continue
        local v
        v=$(( $(cat "$f") / 1048576 ))
        [ "$v" -gt "$mem_mb" ] && mem_mb="$v" && gpu=1
    done

    # Nvidia GPU
    if [ "$gpu" -eq 0 ] && command -v nvidia-smi &>/dev/null; then
        local v
        v=$(nvidia-smi --query-gpu=memory.total --format=csv,noheader,nounits 2>/dev/null \
            | head -1 | tr -d ' ')
        [ -n "$v" ] && [ "$v" -gt 0 ] && mem_mb="$v" && gpu=1
    fi

    # Fall back to system RAM
    if [ "$gpu" -eq 0 ]; then
        mem_mb=$(awk '/MemTotal/{print int($2/1024)}' /proc/meminfo)
    fi

    if [ "$gpu" -eq 1 ]; then
        if   [ "$mem_mb" -ge 22000 ]; then echo "qwen2.5:32b"
        elif [ "$mem_mb" -ge 9000  ]; then echo "qwen2.5:14b"
        elif [ "$mem_mb" -ge 4000  ]; then echo "qwen2.5:7b"
        elif [ "$mem_mb" -ge 1000  ]; then echo "qwen2.5:1.5b"
        else                               echo "qwen2.5:0.5b"
        fi
    else
        if   [ "$mem_mb" -ge 44000 ]; then echo "qwen2.5:32b"
        elif [ "$mem_mb" -ge 18000 ]; then echo "qwen2.5:14b"
        elif [ "$mem_mb" -ge 10000 ]; then echo "qwen2.5:7b"
        elif [ "$mem_mb" -ge 3000  ]; then echo "qwen2.5:1.5b"
        else                               echo "qwen2.5:0.5b"
        fi
    fi
}

DEFAULT_MODEL="$(detect_default_model)"
echo ">>> Detected hardware → default model: ${DEFAULT_MODEL}"
echo ">>> To load it after provisioning: just auto-load-model <node-name>"

echo ">>> Installing system dependencies..."
apt-get install -y -q git curl

echo ">>> Installing llama-server (llama.cpp latest release)..."
if ! LLAMA_VERSION="$(curl -fsSL --connect-timeout 5 \
        https://api.github.com/repos/ggml-org/llama.cpp/releases/latest \
        | grep '"tag_name"' | head -1 | cut -d'"' -f4)" \
   || [ -z "$LLAMA_VERSION" ]; then
    echo ">>> Warning: GitHub API unavailable. Falling back to b5581."
    LLAMA_VERSION="b5581"
fi
echo ">>> llama.cpp release: ${LLAMA_VERSION}"
ARCH="$(uname -m)"
if [ "$ARCH" = "x86_64" ]; then
    LLAMA_URL="https://github.com/ggml-org/llama.cpp/releases/download/${LLAMA_VERSION}/llama-${LLAMA_VERSION}-bin-ubuntu-x64.tar.gz"
else
    LLAMA_URL="https://github.com/ggml-org/llama.cpp/releases/download/${LLAMA_VERSION}/llama-${LLAMA_VERSION}-bin-ubuntu-arm64.tar.gz"
fi
LLAMA_TMP="$(mktemp -d)"
curl -fsSL "$LLAMA_URL" -o "$LLAMA_TMP/llama.tar.gz"
# Extract everything — llama-server depends on several .so files in the same archive.
install -d /opt/llama.cpp
tar -xzf "$LLAMA_TMP/llama.tar.gz" -C /opt/llama.cpp --strip-components=1
rm -rf "$LLAMA_TMP"
echo ">>> llama-server ${LLAMA_VERSION} installed at /opt/llama.cpp/llama-server"
# Models are downloaded on first ModelLoad — no pre-cache step needed.

echo ">>> Installing ai-mesh-agent systemd service..."
tee /etc/systemd/system/ai-mesh-agent.service > /dev/null <<EOF
[Unit]
Description=ai-mesh compute agent
After=network-online.target
Wants=network-online.target

[Service]
ExecStart=/home/${AGENT_USER}/agent
Environment=COORDINATOR_IP=${COORDINATOR_IP}
Environment=AGENT_ROLE=${ROLE}
Environment=LLAMA_MODEL_DIR=/home/${AGENT_USER}/.ai-mesh/models
Environment=LLAMA_SERVER_BIN=/opt/llama.cpp/llama-server
Environment=LD_LIBRARY_PATH=/opt/llama.cpp
Environment=LLAMA_GPU_LAYERS=0
Environment=LLAMA_CTX_SIZE=4096
Environment=DEFAULT_MODEL=${DEFAULT_MODEL}
$([ -n "${MQTT_HOST}" ] && echo "Environment=MQTT_HOST=${MQTT_HOST}" || true)
$([ -n "${MQTT_HOST}" ] && echo "Environment=MQTT_PORT=${MQTT_PORT}" || true)
Restart=always
RestartSec=5
TimeoutStopSec=15
User=${AGENT_USER}
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=multi-user.target
EOF

systemctl daemon-reload
systemctl enable ai-mesh-agent
systemctl restart ai-mesh-agent
systemctl is-active ai-mesh-agent
echo ">>> ai-mesh-agent installed and started."

# Allow the controller machine to push TLS fingerprints and restart the service
# over SSH without a password prompt (needed by `just set-fingerprint`).
echo "${AGENT_USER} ALL=(ALL) NOPASSWD: /usr/bin/tee, /bin/systemctl" \
    > /etc/sudoers.d/ai-mesh-agent
chmod 440 /etc/sudoers.d/ai-mesh-agent
echo ">>> Passwordless sudo configured for tee and systemctl."

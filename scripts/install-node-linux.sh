#!/usr/bin/env bash
# Install or re-install the ai-mesh-agent systemd service on a Linux node.
# Assumes agent binary is already uploaded to ~/agent on the remote machine.
# Run via SSH: ssh user@host "sudo bash /tmp/install-node.sh <coordinator_ip> <role> <user>"
set -e

COORDINATOR_IP="$1"
ROLE="${2:-compute}"
AGENT_USER="$3"

if [ -z "$COORDINATOR_IP" ] || [ -z "$AGENT_USER" ]; then
    echo "Usage: $0 <coordinator_ip> <role> <user>"
    exit 1
fi

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
Restart=always
RestartSec=5
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

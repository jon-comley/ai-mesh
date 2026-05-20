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

if ! command -v ollama &>/dev/null; then
    echo ">>> Installing Ollama..."
    curl -fsSL https://ollama.com/install.sh | sh
fi

echo ">>> Enabling Ollama service..."
systemctl daemon-reload
systemctl enable --now ollama

echo ">>> Pre-caching model qwen2.5:0.5b..."
ollama pull qwen2.5:0.5b

echo ">>> Installing ai-mesh-agent systemd service..."
tee /etc/systemd/system/ai-mesh-agent.service > /dev/null <<EOF
[Unit]
Description=ai-mesh compute agent
After=network-online.target ollama.service
Wants=network-online.target

[Service]
ExecStart=/home/${AGENT_USER}/agent
Environment=COORDINATOR_IP=${COORDINATOR_IP}
Environment=AGENT_ROLE=${ROLE}
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

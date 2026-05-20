#!/usr/bin/env bash
# Install or re-install the ai-mesh-agent systemd service on a Linux node.
# Assumes agent binary is already uploaded to ~/agent on the remote machine.
# Run via SSH: ssh user@host "sudo bash /tmp/install-node.sh <coordinator_ip> <role> <user> [model]"
set -e

COORDINATOR_IP="$1"
ROLE="${2:-compute}"
AGENT_USER="$3"
MODEL="${4:-}"

if [ -z "$COORDINATOR_IP" ] || [ -z "$AGENT_USER" ]; then
    echo "Usage: $0 <coordinator_ip> <role> <user> [model]"
    exit 1
fi

# Select best Qwen2.5 variant based on total RAM if no model override given.
select_model() {
    local ram_gb
    ram_gb=$(free -g | awk '/^Mem:/{print $2}')
    if   [ "$ram_gb" -lt 6  ]; then echo "qwen2.5:1.5b"
    elif [ "$ram_gb" -lt 12 ]; then echo "qwen2.5:7b"
    elif [ "$ram_gb" -lt 32 ]; then echo "qwen2.5:14b"
    else                             echo "qwen2.5:32b"
    fi
}

if [ -z "$MODEL" ]; then
    MODEL="$(select_model)"
    echo ">>> Auto-selected model for $(free -g | awk '/^Mem:/{print $2}')GB RAM: $MODEL"
else
    echo ">>> Using specified model: $MODEL"
fi

if ! command -v ollama &>/dev/null; then
    echo ">>> Installing Ollama..."
    curl -fsSL https://ollama.com/install.sh | sh
fi

echo ">>> Enabling Ollama service..."
systemctl daemon-reload
systemctl enable --now ollama

echo ">>> Pre-caching model $MODEL..."
ollama pull "$MODEL"

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

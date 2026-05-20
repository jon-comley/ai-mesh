#!/usr/bin/env bash
# Remove the ai-mesh-agent systemd service from a Linux node.
# Run via SSH: ssh user@host "sudo bash /tmp/uninstall-node.sh"
set -e

echo ">>> Stopping ai-mesh-agent..."
systemctl stop ai-mesh-agent 2>/dev/null || true
systemctl disable ai-mesh-agent 2>/dev/null || true

echo ">>> Removing service file..."
rm -f /etc/systemd/system/ai-mesh-agent.service
systemctl daemon-reload

echo ">>> ai-mesh-agent service removed."

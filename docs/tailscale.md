# Tailscale Remote Access

The ai-mesh dashboard is accessible from outside the local network via **Tailscale**. This provides a secure, encrypted tunnel (tailnet) without requiring port forwarding or public TLS certificates (like Let's Encrypt).

---

## 1. Network Topology

- **Coordinator (pi1):** `100.100.100.100`
- **Dashboard URL:** `http://100.100.100.100:9001/?token=<auth_token>`
- **SSH over Tailscale:** Supported on all nodes with Tailscale installed.

---

## 2. Setting up a New Node (Headless)

To add a new node (like a new Pi or a remote compute node) to the mesh tailnet without a monitor:

### Linux (Pi / Ubuntu)
1. Install Tailscale:
   ```bash
   curl -fsSL https://tailscale.com/install.sh | sh
   ```
2. Authenticate headlessly using the `--qr` flag to get a login link:
   ```bash
   sudo tailscale up --qr --ssh
   ```
3. Scan the QR code or copy the URL to a browser on your phone/laptop to approve the node.

### Windows (Beelink)
1. Download and install the Tailscale MSI.
2. The browser will open automatically for authentication.
3. Ensure "Run at Startup" is enabled.

---

## 3. Remote Dashboard Access

To access the dashboard on a mobile device:

1. Install the Tailscale app on your phone.
2. Log in to the same tailnet account.
3. Bookmark the URL: `http://100.100.100.100:9001/`
   - *Note: You must include the `?token=...` parameter on first load to authenticate with the coordinator.*
4. **DNS Resolution:** Tailscale has **built-in DNS** (formerly called MagicDNS) enabled by default. You can use the node name directly, e.g., `http://pi1:9001/`.
   - If the name doesn't resolve immediately, the IP (`100.100.100.100`) is the definitive fallback.
   - All nodes are automatically assigned a stable name within the `ts.net` domain.

---

## 4. Operational Notes

- **Exit Nodes:** Do not configure mesh nodes as exit nodes unless specifically required for routing, as this can add latency to inference heartbeats.
- **SSH over Tailscale:** With the `--ssh` flag enabled during `tailscale up`, you can SSH into nodes using their tailnet IP even if the local LAN IP changes or is unreachable.
- **Service Resilience:** The Tailscale daemon (`tailscaled`) is managed by systemd on Linux and handles its own restarts. If the tunnel drops, check:
  ```bash
  tailscale status
  ```

---

## 5. Troubleshooting

### Node shows "Offline" in Tailscale
- Verify the service is running: `sudo systemctl status tailscaled`
- Check for expired authentication: `sudo tailscale up`

### Dashboard unreachable over Tailscale
- Verify the coordinator is running: `sudo systemctl status ai-mesh-coordinator`
- Ensure the node hasn't been "Key Expired" in the Tailscale admin console. Mesh nodes should ideally have **Key Expiry Disabled** for 24/7 availability.

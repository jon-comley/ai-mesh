# Cross-Platform Compute Node Support

ai-mesh is designed to run **heterogeneous compute nodes** across
**Windows, Linux, and macOS**, with a single coordinator and a unified
Rust-based agent architecture. This document defines the supported
platforms, provisioning model, and expectations for each OS.

---

## 1. Supported Operating Systems

ai-mesh supports the following compute node environments:

- **Windows 11 / Windows Server**
  (native `.exe` agent, PowerShell provisioning, llama-server for Windows via Vulkan ZIP)

- **Linux (Ubuntu / Debian / Raspberry Pi OS)**
  (ELF agent, systemd provisioning, llama-server via tarball)

- **macOS (Apple Silicon)**
  (macOS agent, launchd provisioning, llama-server for macOS)

The coordinator typically runs on Linux (WSL or native), but this is not
required — only the compute nodes must match the platform-specific agent.

---

## 2. Cluster Composition (Current Hardware)

| Node        | OS                | Role       | Notes |
|-------------|-------------------|------------|-------|
| OmniBook7   | WSL Ubuntu        | Controller | Runs coordinator + CLI only |
| beelink1    | Windows 11 Pro    | Compute    | Windows agent + llama-server (Vulkan, AMD Radeon 780M) |
| pi1         | Ubuntu (ARM64)    | Compute    | Linux ARM64 agent + llama-server |
| mac1        | macOS (Apple)     | Compute    | Pending (~end of July 2026) |

This mixed-OS cluster is **intentional** and fully supported.

---

## 3. Agent Build Targets

Each compute node requires a platform-specific agent binary:

- **Windows (cross-compiled from WSL/Linux via MinGW):**
  `cargo build --release -p agent --target x86_64-pc-windows-gnu`

- **Linux x86_64:**
  `cargo build --release -p agent --target x86_64-unknown-linux-gnu`

- **Linux ARM64 (Pi):**
  `cargo build --release -p agent --target aarch64-unknown-linux-gnu`

- **macOS (Apple Silicon):**
  `cargo build --release -p agent --target aarch64-apple-darwin`

All agents speak the same wire protocol and are fully interoperable.

The `deploy-node` and `update-node` justfile recipes detect the remote
architecture automatically via `uname -m` and select the correct binary.

---

## 4. llama-server Support by Platform

ai-mesh uses llama-server (llama.cpp) for model loading and inference.
Each platform gets the correct build:

- **Windows:**
  Vulkan-enabled ZIP (`llama-<ver>-bin-win-vulkan-x64.zip`) downloaded from
  the llama.cpp GitHub releases. Extracted to
  `%LOCALAPPDATA%\Programs\llama.cpp\`. No installer — ZIP extraction only.

- **Linux x86_64:**
  `llama-<ver>-bin-ubuntu-x64.tar.gz` extracted to `/opt/llama.cpp/`.

- **Linux ARM64 (Pi):**
  `llama-<ver>-bin-ubuntu-arm64.tar.gz` extracted to `/opt/llama.cpp/`.

- **macOS:**
  Pending — will use the macOS release tarball.

Platform differences:

- **Windows:**
  No systemd. Agent runs as an NSSM service. llama-server started
  in-process by the agent on first model load.

- **Linux:**
  Uses systemd (`ai-mesh-agent.service`). Provisioning uses Bash.

- **macOS:**
  Will use launchd. Provisioning will use zsh/bash.

---

## 5. Provisioning Model

ai-mesh uses **per-OS provisioning flows**:

### Windows Compute Nodes
- Install llama-server (Vulkan) via GitHub ZIP
- Install Windows agent `.exe`
- Configure as a Windows service (NSSM)
- Use PowerShell for automation
- No `/home/...`, no systemd

See [windows-node-setup.md](windows-node-setup.md) for the full step-by-step guide including all known gotchas.

#### SSH Elevation (one-time, required)

Windows SSH sessions use a filtered (non-elevated) token by default, even
for Administrator accounts. This prevents remote PowerShell from installing
services or writing to `HKLM`. The provisioner fixes this automatically by
setting:

```
HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System
  LocalAccountTokenFilterPolicy = 1 (DWORD)
```

This is written by `scripts/install-node-windows.ps1` during first-time setup
and persists across reboots. After it is set, `just update-node <node>` and
`just sanity-node <node>` work fully from WSL with no manual steps on the node.

**First-time only:** run `install-node-windows.ps1` from an elevated PowerShell
on the Windows machine itself (Start → PowerShell → Run as Administrator). All
subsequent deploys go through `just update-node <node>` from WSL.

### Linux Compute Nodes
- Install llama-server via tarball
- Install agent ELF binary
- Use systemd for service management
- Use Bash for automation

### macOS Compute Nodes
- Install llama-server via macOS release tarball
- Install agent macOS binary
- Use launchd for service management
- Use zsh/bash for automation

---

## 6. Scheduler Expectations

The scheduler treats all compute nodes equally regardless of OS:

- All nodes report `HardwareSpec`
- All nodes report `NodeCapabilities`
- All nodes report model lifecycle state
- All nodes participate in placement decisions

OS differences do **not** affect scheduling.

---

## 7. Design Rationale

ai-mesh is written in Rust specifically to:

- Support **Windows, Linux, and macOS**
- Provide consistent networking and serialization
- Allow heterogeneous clusters
- Enable low/medium/high-power nodes for model testing

This cross-platform design is intentional and permanent.

---

## 8. Summary

- ai-mesh supports **mixed-OS compute nodes**
- Windows compute nodes are first-class citizens
- Linux and macOS nodes behave identically at the protocol level
- Provisioning differs by OS, but the agent and wire protocol do not
- The Beelink SER8 **must** run the Windows agent
- The Pi runs the Linux ARM64 agent
- The Mac mini will run the macOS agent

This document is authoritative for all future development.

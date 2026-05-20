# Cross‑Platform Compute Node Support

ai‑mesh is designed to run **heterogeneous compute nodes** across
**Windows, Linux, and macOS**, with a single coordinator and a unified
Rust‑based agent architecture. This document defines the supported
platforms, provisioning model, and expectations for each OS.

---

## 1. Supported Operating Systems

ai‑mesh supports the following compute node environments:

- **Windows 11 / Windows Server**  
  (native `.exe` agent, PowerShell provisioning, Ollama for Windows)

- **Linux (Ubuntu / Debian / Raspberry Pi OS)**  
  (ELF agent, systemd provisioning, Ollama for Linux)

- **macOS (Apple Silicon)**  
  (macOS agent, launchd provisioning, Ollama for macOS)

The coordinator typically runs on Linux (WSL or native), but this is not
required — only the compute nodes must match the platform‑specific agent.

---

## 2. Cluster Composition (Current Hardware)

| Node        | OS                | Role       | Notes |
|-------------|-------------------|------------|-------|
| OmniBook7   | WSL Ubuntu        | Controller | Runs coordinator + CLI only |
| beelink1    | Windows 11 Pro    | Compute    | Runs Windows agent + Ollama for Windows |
| pi1         | Ubuntu (ARM64)    | Compute    | Runs Linux ARM64 agent + Ollama |
| mac1        | macOS (Apple)     | Compute    | Runs macOS agent + Ollama |

This mixed‑OS cluster is **intentional** and fully supported.

---

## 3. Agent Build Targets

Each compute node requires a platform‑specific agent binary:

- **Windows (cross-compiled from WSL/Linux via MinGW):**  
  `cargo build --release -p agent --target x86_64-pc-windows-gnu`

- **Linux x86_64:**  
  `cargo build --release -p agent`

- **Linux ARM64 (Pi):**  
  `cargo build --release -p agent --target aarch64-unknown-linux-gnu`

- **macOS (Apple Silicon):**  
  `cargo build --release -p agent --target aarch64-apple-darwin`

All agents speak the same wire protocol and are fully interoperable.

---

## 4. Ollama Support by Platform

ai‑mesh relies on Ollama for model loading and inference.  
Ollama is available on:

- **Windows** (native installer)
- **Linux** (install.sh + systemd)
- **macOS** (native app + launchd)

Platform differences:

- **Windows:**  
  No systemd. Ollama runs as a Windows service.  
  Provisioning uses PowerShell, not Bash.

- **Linux:**  
  Uses systemd (`ollama.service`).  
  Provisioning uses Bash.

- **macOS:**  
  Uses launchd (`/Library/LaunchDaemons/io.ollama.plist`).

---

## 5. Provisioning Model

ai‑mesh uses **per‑OS provisioning flows**:

### Windows Compute Nodes
- Install Ollama for Windows  
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
- Install Ollama via `install.sh`  
- Install agent ELF binary  
- Use systemd for service management  
- Use Bash for automation

### macOS Compute Nodes
- Install Ollama.app  
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

ai‑mesh is written in Rust specifically to:

- Support **Windows, Linux, and macOS**  
- Provide consistent networking and serialization  
- Allow heterogeneous clusters  
- Enable low/medium/high‑power nodes for model testing  

This cross‑platform design is intentional and permanent.

---

## 8. Summary

- ai‑mesh supports **mixed‑OS compute nodes**  
- Windows compute nodes are first‑class citizens  
- Linux and macOS nodes behave identically at the protocol level  
- Provisioning differs by OS, but the agent and wire protocol do not  
- The Beelink SER8 **must** run the Windows agent  
- The Pi runs the Linux ARM64 agent  
- The Mac mini will run the macOS agent  

This document is authoritative for all future development.

# Windows Compute Node Setup

Step-by-step guide for adding a fresh Windows 11 machine to the ai-mesh cluster as a compute node. Captures every gotcha encountered during the Beelink SER8 provisioning.

---

## Prerequisites

### On the Windows machine
- Windows 11 (Windows 10 may work but is untested)
- An administrator account (local, not domain)
- OpenSSH Server enabled and running
- winget available (built-in on Windows 11)
- PowerShell 5+
- **AMD GPU only:** AMD Adrenalin driver 26.5.2 or later (see [AMD GPU Acceleration](#amd-gpu-acceleration) below)

#### Enable OpenSSH Server (if not already)
In an elevated PowerShell:
```powershell
Add-WindowsCapability -Online -Name OpenSSH.Server~~~~0.0.1.0
Set-Service -Name sshd -StartupType Automatic
Start-Service sshd
```

Add your WSL public key to `C:\Users\<user>\.ssh\authorized_keys` so that SSH from WSL works without a password prompt.

### On the coordinator machine (WSL/Linux)
- Rust toolchain (`rustup`)
- MinGW cross-compiler: `sudo apt install gcc-mingw-w64-x86-64`
- Windows cross-compile target: `rustup target add x86_64-pc-windows-gnu`
- SSH access to the Windows machine (key-based recommended)

---

## 1. Network Setup

### Firewall — allow port 9000 inbound (Windows)

The agent connects *outbound* from the Windows machine to the coordinator, but the coordinator lives in WSL2 and must be reachable from the LAN. Windows exposes it via a portproxy rule. Allow the port through the Windows firewall first:

```powershell
# Run in elevated PowerShell on the Windows machine
netsh advfirewall firewall add rule `
    name="ai-mesh coordinator" dir=in action=allow protocol=TCP localport=9000
```

### Portproxy — forward LAN:9000 → WSL2:9000

WSL2 gets a new internal IP every time Windows restarts, so the portproxy rule goes stale. Run this from WSL whenever you need to update it:

```bash
just update-portproxy   # updates automatically if IP has changed, no-op if current
```

Or manually in elevated PowerShell:
```powershell
$wslIp = (wsl hostname -I).Trim().Split()[0]
netsh interface portproxy delete v4tov4 listenport=9000 listenaddress=0.0.0.0
netsh interface portproxy add    v4tov4 listenport=9000 listenaddress=0.0.0.0 connectport=9000 connectaddress=$wslIp
```

`update-portproxy` is a dependency of `just run-coordinator` and `just sanity-node beelink1`, so it runs automatically for those recipes.

---

## 2. Enable SSH Elevation (one-time, on the Windows machine)

By default, Windows SSH sessions give a **filtered (non-elevated) token** even for Administrator accounts. Without fixing this, remote PowerShell cannot install services or write to `HKLM` — provisioning will fail silently or with access-denied errors.

Run this once from an **elevated PowerShell on the Windows machine itself** (Start → PowerShell → Run as Administrator):

```powershell
Set-ItemProperty `
    -Path "HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System" `
    -Name "LocalAccountTokenFilterPolicy" `
    -Value 1 -Type DWord
```

This persists across reboots. After it is set, all subsequent operations (`just update-node beelink1`, `just sanity-node beelink1`, etc.) work over SSH with no manual steps on the Windows machine.

The provision script (step 4) sets this automatically — but it must be run elevated, so you still need to do step 4 in person the first time.

---

## 3. Build the Windows Agent (from WSL)

```bash
# One-time toolchain setup (if not already done)
sudo apt install gcc-mingw-w64-x86-64
rustup target add x86_64-pc-windows-gnu

# Build the release binary
cargo build --release -p agent --target x86_64-pc-windows-gnu
# Output: target/x86_64-pc-windows-gnu/release/agent.exe

# Or via justfile
just deploy-node beelink1
```

---

## 4. First-Time Provisioning (on the Windows machine)

This step **must be done locally on the Windows machine** (not over SSH) because SSH elevation is not yet enabled. Do it once; everything after this is remote.

**Step 1:** Copy `agent.exe` to `C:\Users\<user>\ai-mesh\agent.exe` on the Windows machine (USB, file share, or manual scp).

**Step 2:** Open PowerShell **as Administrator** on the Windows machine and run:

```powershell
Set-ExecutionPolicy Bypass -Scope Process -Force
& "C:\Users\<user>\ai-mesh\install-node-windows.ps1" -CoordinatorIp 192.168.1.12
```

The provision script does:
- Creates `C:\Users\<user>\ai-mesh\` and `logs\` directories
- Sets `LocalAccountTokenFilterPolicy = 1` (SSH elevation)
- Installs **Ollama v0.30.0-rc21** from GitHub ZIP (not winget — winget is pinned to 0.24.0 which lacks AMD Vulkan support)
- Installs **NSSM** via winget
- Registers `ollama-serve` as a Windows service with `OLLAMA_VULKAN=1` (enables AMD iGPU acceleration)
- Registers `ai-mesh-agent` as a Windows service (auto-start)
- Sets `COORDINATOR_IP` and `AGENT_ROLE=compute` as service environment vars
- Configures log rotation (10 MB per file)
- Configures restart throttle (5 s delay between restarts)
- Auto-selects and pulls the best Qwen2.5 model for the node's RAM

---

## 5. Verify

From WSL, after provisioning completes:

```bash
just sanity-node beelink1
```

The node table should show the Windows machine as a Compute node:

```
| BEELINK1 | 192.168.1.14 | Compute | 1200 | - |
```

---

## Ongoing Operations

| Task | Command |
|------|---------|
| Deploy updated agent | `just update-node beelink1` |
| Check agent logs (live tail) | `just logs-node beelink1` |
| Full sanity check | `just sanity-node beelink1` |
| Check service state | `ssh user@host "sc.exe query ai-mesh-agent"` |
| Restart service | `ssh user@host "sc.exe stop ai-mesh-agent && sc.exe start ai-mesh-agent"` |

`just update-node beelink1` rebuilds, uploads via temp file (to avoid file lock), stops the service, swaps the binary, and restarts — all from WSL.

---

---

## AMD GPU Acceleration

Machines with an AMD Radeon iGPU (e.g. the Radeon 780M in the Beelink SER8) can run inference on the GPU via Ollama's Vulkan backend. This gives a meaningful speedup over CPU-only inference.

### Driver requirement — AMD Adrenalin 26.5.2+

**This is mandatory.** Older AMD drivers have a bug (Windows C++ SEH exception `0xe06d7363`) that crashes Ollama whenever Vulkan compute accesses shared iGPU memory. The crash was fixed in Adrenalin 26.5.2 (May 2026).

Install from: [https://www.amd.com/en/support/download/drivers.html](https://www.amd.com/en/support/download/drivers.html)

After installing the driver, reboot before running `just deploy-node`.

### How it works

The `install-node-windows.ps1` script automatically:
1. Installs Ollama 0.30.0+ from the GitHub release ZIP (winget is pinned to 0.24.0 which has no Vulkan support)
2. Configures the `ollama-serve` NSSM service with `OLLAMA_VULKAN=1`

With this in place, Ollama detects the AMD Radeon iGPU via Vulkan and offloads as many model layers as possible onto it.

### Measured performance on Beelink SER8 (Radeon 780M, 32 GB RAM)

| Model | Layers on GPU | Generation speed | Notes |
|-------|--------------|-----------------|-------|
| `qwen2.5:0.5b` | 29/29 | ~153 t/s | Fully within dedicated VRAM (3.9 GB) |
| `qwen2.5:1.5b` | 29/29 | ~97 t/s | Fully within dedicated VRAM |
| `qwen2.5:7b` | 29/29 | ~17.6 t/s | Spills into shared RAM; ~37% faster than CPU-only |
| `qwen2.5:14b` | partial | ~CPU speed | Too large for iGPU to help significantly |

The 780M has 3.9 GB dedicated VRAM. Models that fit entirely within dedicated VRAM see the biggest speedup. The 7b model (4.7 GB) slightly overflows into shared system RAM, so the speedup is more modest — but all 29 layers still run on the GPU with no crash.

### Manual verification

After provisioning, check that Vulkan is active:

```bash
# From WSL
ssh jonno@192.168.1.14 'type "C:\Users\jonno\ai-mesh\logs\ollama.log" | findstr /i "vulkan\|Vulkan"'
```

You should see a line like:
```
inference compute id=... library=Vulkan name=Vulkan0 description="AMD Radeon 780M Graphics" total="17.8 GiB"
```

If it shows `library=cpu`, either the driver is too old or `OLLAMA_VULKAN=1` is not set:
```bash
ssh jonno@192.168.1.14 'nssm get ollama-serve AppEnvironmentExtra'
# Should print: OLLAMA_VULKAN=1
```

To fix manually:
```bash
ssh jonno@192.168.1.14 'nssm set ollama-serve AppEnvironmentExtra "OLLAMA_VULKAN=1" && nssm restart ollama-serve'
```

### Upgrading Ollama (ZIP method)

The standard Ollama installer (`OllamaSetup.exe`) hangs silently when run over SSH because it requires a desktop session for UAC. Always use the ZIP method for remote upgrades:

```bash
# From WSL — example upgrade to a new version
VERSION="v0.30.0-rc21"
ssh jonno@192.168.1.14 "nssm stop ollama-serve"
ssh jonno@192.168.1.14 "powershell -Command \"
  \$zip = '\$env:TEMP\\ollama.zip'
  Invoke-WebRequest 'https://github.com/ollama/ollama/releases/download/$VERSION/ollama-windows-amd64.zip' -OutFile \$zip -UseBasicParsing
  Expand-Archive \$zip '\$env:LOCALAPPDATA\\Programs\\Ollama' -Force
  Remove-Item \$zip
\""
ssh jonno@192.168.1.14 "nssm start ollama-serve"
```

The NSSM service log is at `C:\Users\<user>\ai-mesh\logs\ollama.log`.

---

## Troubleshooting

### Service stuck in STOP_PENDING

Symptom: `sc.exe query ai-mesh-agent` shows `STATE: 3 STOP_PENDING` and never changes.

Cause: NSSM sent a stop signal but is waiting for something that never clears (often the agent spawned a child process that outlived the parent). NSSM hangs waiting for the process tree to fully exit.

Fix — find and kill the NSSM process directly:
```bash
ssh user@host "tasklist /FI \"IMAGENAME eq nssm.exe\""
# Note the PID, then:
ssh user@host "taskkill /F /PID <pid>"
# Service will now show STOPPED; start it:
ssh user@host "sc.exe start ai-mesh-agent"
```

Prevention: the agent must **not spawn child processes** during normal operation. Hardware and GPU detection use the `sysinfo` crate (no subprocess spawning) for exactly this reason. If you add any `Command::output()` calls to the agent, test that the service can be stopped cleanly.

### Node not appearing in the node table

Check in order:
1. Is the service RUNNING? `ssh user@host "sc.exe query ai-mesh-agent"`
2. Is the portproxy current? `just update-portproxy`
3. Can the Windows machine reach the coordinator? On the Windows machine: `Test-NetConnection 192.168.1.12 -Port 9000`
4. Are there stale registry entries obscuring the new entry? `just reset` clears them.
5. Check the agent log: `just logs-node beelink1`

### SCP fails because agent.exe is locked

The running service holds agent.exe open. Never upload directly to `agent.exe`:
```bash
# Upload as a temp file first, then move atomically
scp agent.exe user@host:"C:\path\agent_next.exe"
ssh user@host "Move-Item -Force C:\path\agent_next.exe C:\path\agent.exe"
```

`just update-node beelink1` does this automatically.

### SSH commands fail with access denied

`LocalAccountTokenFilterPolicy` is not set. Run step 2 of provisioning locally on the Windows machine (elevated PowerShell), then retry.

### Portproxy stale after WSL2 restart

WSL2 gets a new internal IP every time Windows restarts. The portproxy rule points to the old IP. Run:
```bash
just update-portproxy
```
A UAC prompt will appear on the Windows host to allow the elevation needed to update the netsh rule.

### Stale node entries accumulating in the registry

The agent generates a new UUID on each start, so every service restart adds a new row. Clean up stale entries with:
```bash
just reset          # calls: cargo run -p cli -- reset-registry
```

### NSSM — environment variables not being set

NSSM `AppEnvironmentExtra` requires each variable as a **separate argument**, not semicolon-separated:
```powershell
# CORRECT
nssm set ai-mesh-agent AppEnvironmentExtra "COORDINATOR_IP=192.168.1.12" "AGENT_ROLE=compute"

# WRONG — produces a single malformed variable
nssm set ai-mesh-agent AppEnvironmentExtra "COORDINATOR_IP=192.168.1.12;AGENT_ROLE=compute"
```

Verify what NSSM has stored:
```powershell
nssm get ai-mesh-agent AppEnvironmentExtra
```

### Trailing spaces in env vars (cmd.exe pitfall)

When setting env vars inline in cmd.exe, it is easy to include a trailing space:
```cmd
REM WRONG — COORDINATOR_IP = "192.168.1.12 " (note trailing space)
set COORDINATOR_IP=192.168.1.12 && agent.exe

REM RIGHT — quotes prevent trailing space
set "COORDINATOR_IP=192.168.1.12" && agent.exe
```

The coordinator address ends up as `192.168.1.12 :9000` which fails to parse. The agent trims these values, but the underlying issue is in how you set the variable.

### AMD GPU not detected / Ollama crashes during inference

Symptoms: Ollama log shows `library=cpu` instead of `library=Vulkan`, or inference crashes with a Windows C++ exception.

**Check 1:** Is `OLLAMA_VULKAN=1` set?
```bash
ssh user@host 'nssm get ollama-serve AppEnvironmentExtra'
```
If not, set it: `nssm set ollama-serve AppEnvironmentExtra "OLLAMA_VULKAN=1"` then `nssm restart ollama-serve`.

**Check 2:** Is the AMD driver 26.5.2 or later?  
Older drivers crash with `Exception 0xe06d7363` when Vulkan compute tries to use shared iGPU memory. Update from https://www.amd.com/en/support/download/drivers.html then reboot.

**Check 3:** Is Ollama 0.30.0 or later installed?  
The winget package is pinned to 0.24.0 which has no Vulkan support. Re-run `just deploy-node <node>` to upgrade via ZIP.

**Note on ROCm:** ROCm does not detect the 780M iGPU on Windows at all (it only sees discrete GPUs, and even then requires Linux for iGPU support). ROCm is not used — Vulkan is the correct backend for this hardware.

### wmic — deprecated / encoding issues on Windows 11

Do not use `wmic` for hardware detection. It is deprecated on Windows 11, may not be installed, and outputs UTF-16 which is awkward to parse from Rust. Use the `sysinfo` crate instead (already used in `hardware.rs`).

### PowerShell cold-start delay

PowerShell takes 4–40 seconds to start cold, depending on system load. If the agent spawns PowerShell at startup (e.g. for hardware detection), the service will appear to hang during NSSM's startup window and may never reach RUNNING state before the stop signal arrives from a test recipe. Use `sysinfo` for all in-process system queries.

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
- Downloads the latest **llama.cpp Vulkan release** ZIP from GitHub and extracts it to `%LOCALAPPDATA%\Programs\llama.cpp\`
- Installs **NSSM** via winget
- Registers `ai-mesh-agent` as a Windows service (auto-start)
- Sets `COORDINATOR_IP`, `AGENT_ROLE`, `LLAMA_MODEL_DIR`, `LLAMA_SERVER_BIN`, `LLAMA_GPU_LAYERS=99`, and `LLAMA_FLASH_ATTN=1` as service environment vars
- Configures log rotation (10 MB per file) and restart throttle (5 s delay)

Models are downloaded on first load — nothing is pre-cached during provisioning. The install script detects the GPU and logs the recommended model; use `just auto-load-model beelink1` to load it automatically after provisioning.

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

Then load the hardware-selected model:

```bash
just auto-load-model beelink1
```

Or load a specific model manually:

```bash
just load-model beelink1 qwen2.5:7b
```

---

## Ongoing Operations

| Task | Command |
|------|---------|
| Deploy updated agent | `just update-node beelink1` |
| Load hardware-selected model | `just auto-load-model beelink1` |
| Load a specific model | `just load-model beelink1 qwen2.5:7b` |
| Check agent logs (live tail) | `just logs-node beelink1` |
| Full sanity check | `just sanity-node beelink1` |
| Check service state | `ssh user@host "sc.exe query ai-mesh-agent"` |
| Restart service | `ssh user@host "sc.exe stop ai-mesh-agent && sc.exe start ai-mesh-agent"` |
| Update llama-server | `just update-llama beelink1` |

`just update-node beelink1` rebuilds, uploads via temp file (to avoid file lock), kills NSSM by PID, swaps the binary, and restarts — all from WSL.

---

## AMD GPU Acceleration

Machines with an AMD Radeon iGPU (e.g. the Radeon 780M in the Beelink SER8) can run inference on the GPU via llama-server's Vulkan backend. This gives a meaningful speedup over CPU-only inference.

### Driver requirement — AMD Adrenalin 26.5.2+

**This is mandatory.** Older AMD drivers have a bug (Windows C++ SEH exception `0xe06d7363`) that crashes the llama-server Vulkan runner whenever it accesses shared iGPU memory. The crash was fixed in Adrenalin 26.5.2 (May 2026).

Install from: [https://www.amd.com/en/support/download/drivers.html](https://www.amd.com/en/support/download/drivers.html)

After installing the driver, reboot before running `just deploy-node`.

### How it works

The `install-node-windows.ps1` script automatically:
1. Downloads the Vulkan-enabled llama.cpp release ZIP from GitHub
2. Configures the agent service with `LLAMA_GPU_LAYERS=99` and `LLAMA_FLASH_ATTN=1`

With this in place, llama-server detects the AMD Radeon iGPU via Vulkan and offloads all model layers onto it.

### Measured performance on Beelink SER8 (Radeon 780M, 32 GB RAM)

| Model | Layers on GPU | Generation speed | Notes |
|-------|--------------|-----------------|-------|
| `qwen2.5:1.5b` | 29/29 | ~97 t/s | Fully within dedicated VRAM |
| `qwen2.5:7b` | 29/29 | ~17.6 t/s | Spills into shared RAM; ~37% faster than CPU-only |
| `qwen2.5:14b` | partial | ~CPU speed | Too large for iGPU to help significantly |

The 780M has ~4 GB dedicated VRAM. Models that fit entirely within dedicated VRAM see the biggest speedup. The 7b model (~4.7 GB including KV cache) slightly overflows into shared system RAM, so the speedup is more modest — but all 29 layers still run on the GPU with no crash.

### Manual verification

After provisioning, check that Vulkan is active by examining the llama-server startup log:

```bash
just logs-node beelink1
# Look for: "llama_new_context_with_model: n_ctx_per_seq = ..." and
#            GPU layer count lines confirming offload
```

Or check the NSSM service environment:
```bash
ssh jonno@192.168.1.14 'nssm get ai-mesh-agent AppEnvironmentExtra'
# Should show LLAMA_GPU_LAYERS=99 LLAMA_FLASH_ATTN=1
```

### Upgrading llama-server

Use `just update-llama beelink1` to download the latest llama.cpp Vulkan release and restart the agent service — fully remote from WSL.

---

## Troubleshooting

### Service stuck in STOP_PENDING

Symptom: `sc.exe query ai-mesh-agent` shows `STATE: 3 STOP_PENDING` and never changes.

Cause: NSSM sent a stop signal but is waiting for something that never clears (often the agent spawned a child process that outlived the parent, or NSSM's restart throttle kicked in after repeated crashes).

Fix — find and kill the NSSM process directly:
```bash
ssh user@host "tasklist /FI \"IMAGENAME eq nssm.exe\""
# Note the PID, then:
ssh user@host "taskkill /F /PID <pid>"
# Service will now show STOPPED; start it:
ssh user@host "sc.exe start ai-mesh-agent"
```

Prevention: the agent must **not spawn child processes** during normal operation. Hardware and GPU detection use the `sysinfo` crate (no subprocess spawning) for exactly this reason.

### Node not appearing in the node table

Check in order:
1. Is the service RUNNING? `ssh user@host "sc.exe query ai-mesh-agent"`
2. Is the portproxy current? `just update-portproxy`
3. Can the Windows machine reach the coordinator? On the Windows machine: `Test-NetConnection 192.168.1.12 -Port 9000`
4. Are there stale registry entries obscuring the new entry? `just reset` clears them.
5. Check the agent log: `just logs-node beelink1`

### SCP fails because agent.exe is locked

The running service holds agent.exe open. Never upload directly to `agent.exe`. `just update-node <node>` always uploads as `agent_next.exe`, kills NSSM by PID, then uses `cmd /c copy /Y` to swap — this avoids the file lock completely.

If you need to do it manually:
```bash
scp agent.exe user@host:"C:\path\agent_next.exe"
ssh user@host "tasklist /FI \"IMAGENAME eq nssm.exe\""   # get NSSM PID
ssh user@host "taskkill /F /PID <nssm-pid>"
ssh user@host "cmd /c 'copy /Y C:\path\agent_next.exe C:\path\agent.exe'"
ssh user@host "sc.exe start ai-mesh-agent"
```

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

### AMD GPU not detected / llama-server crashes during inference

Symptoms: GPU layers show 0 in logs, or inference crashes with a Windows C++ exception.

**Check 1:** Is `LLAMA_GPU_LAYERS=99` set?
```bash
ssh user@host 'nssm get ai-mesh-agent AppEnvironmentExtra'
```
If not, re-run `just deploy-node beelink1` or set it manually and restart.

**Check 2:** Is the AMD driver 26.5.2 or later?
Older drivers crash with `Exception 0xe06d7363` when Vulkan compute tries to use shared iGPU memory. Update from https://www.amd.com/en/support/download/drivers.html then reboot.

**Check 3:** Is the llama-server Vulkan build installed?
The install script downloads the `-win-vulkan-x64.zip` variant. Verify:
```bash
ssh user@host 'dir "%LOCALAPPDATA%\Programs\llama.cpp\llama-server.exe"'
```

**Note on ROCm:** ROCm does not detect the 780M iGPU on Windows at all (it only sees discrete GPUs, and even then requires Linux for iGPU support). ROCm is not used — Vulkan is the correct backend for this hardware.

### wmic — deprecated / encoding issues on Windows 11

Do not use `wmic` for hardware detection. It is deprecated on Windows 11, may not be installed, and outputs UTF-16 which is awkward to parse from Rust. Use the `sysinfo` crate instead (already used in `hardware.rs`).

### PowerShell cold-start delay

PowerShell takes 4–40 seconds to start cold, depending on system load. If the agent spawns PowerShell at startup (e.g. for hardware detection), the service will appear to hang during NSSM's startup window and may never reach RUNNING state before the stop signal arrives from a test recipe. Use `sysinfo` for all in-process system queries.

### Windows sleep / hibernate

Windows will turn off the machine if sleep is enabled. Disable it:
```powershell
powercfg /h off          # disable hibernate
powercfg /change standby-timeout-ac 0   # disable sleep on AC power
```

### Beelink SER8 — CMOS reset recovery

**Symptom:** Beelink shows an "unrecoverable" BIOS screen on boot (has happened twice). Requires a physical CMOS reset (button on the back of the unit).

**What a CMOS reset wipes:**
- BIOS settings (boot order, etc.) — usually fine after reset
- The SSH `administrators_authorized_keys` may survive, but in practice the machine needs to be treated as freshly provisioned

**Recovery steps (from WSL on OmniLink1):**

1. Once the machine boots and is reachable on the network, re-copy the SSH key:
   ```bash
   ssh-copy-id jonno@192.168.1.14
   ```
   If `ssh-copy-id` fails because the key isn't in `authorized_keys` yet, add it manually — open a PowerShell on the Beelink and run:
   ```powershell
   Add-Content 'C:\ProgramData\ssh\administrators_authorized_keys' '<paste public key here>'
   ```
   Or from WSL:
   ```bash
   cat ~/.ssh/id_ed25519.pub | ssh jonno@192.168.1.14 "powershell -Command \"Add-Content 'C:\\ProgramData\\ssh\\administrators_authorized_keys' '\$(cat)'\""
   ```

2. Re-add the firewall rule (CMOS reset can clear custom rules):
   ```bash
   powershell.exe -Command "Start-Process powershell -Verb RunAs -ArgumentList '-NoProfile -Command New-NetFirewallRule -DisplayName ''ai-mesh coordinator'' -Direction Inbound -Protocol TCP -LocalPort 9000 -Action Allow' -Wait"
   ```

3. Re-provision the node (binary, service, env vars):
   ```bash
   just deploy-node beelink1
   ```

4. Bring the cluster back up:
   ```bash
   just start-cluster
   just validate-routing
   ```

**Notes:**
- `LocalAccountTokenFilterPolicy` (SSH elevation) is set by the provision script — it will be re-applied by `just deploy-node`
- The NSSM service and agent binary are wiped by a CMOS reset if Windows was reinstalled, but survive a simple CMOS clear if the OS volume is intact
- If the node doesn't re-appear after `deploy-node`, run `just reset` to clear stale registry entries then `just start-cluster` again

### Beelink SER8 — OS hang (power light on, no network response)

**Symptom:** Machine appears powered on (front LED lit), monitor connected and also unresponsive (black/frozen screen), no ping or SSH response. Recovered by holding the power button for a hard reset. Booted cleanly after. First observed 2026-05-23.

This is distinct from the CMOS/BIOS issue above — no BIOS screen, system hung with display stack dead.

**Confirmed not:** NIC power management (monitor was connected and also unresponsive, so the whole system was hung, not just the network).

**Primary suspect: GPU driver crash (TDR).** AMD Radeon 780M running Vulkan at `LLAMA_GPU_LAYERS=99` via llama-server. Windows has a watchdog called TDR (Timeout Detection and Recovery) that kills and restarts the GPU driver if the GPU stops responding. On a sustained compute workload it sometimes fails to recover and takes the entire display stack down, leaving the system hung with a black screen and no way to recover except a hard reset.

**Secondary suspect: Fast Startup.** `powercfg /h off` disables hibernate but not Windows Fast Startup (hybrid shutdown), which can produce a half-suspended state on restart.

**Diagnostics — run after next recovery, before restarting agent:**

Check for GPU TDR events and kernel-power (ID 41 = unexpected shutdown):
```bash
ssh jonno@192.168.1.14 "powershell -Command \"Get-WinEvent -LogName System -MaxEvents 200 | Where-Object { \$_.LevelDisplayName -eq 'Critical' -or \$_.Id -eq 41 -or \$_.Id -eq 1001 -or \$_.Id -eq 4101 } | Select-Object TimeCreated, Id, Message | Format-List\""
```
Event ID 4101 = TDR (display driver stopped responding). Event ID 41 = unexpected power loss.

Also check the AMD driver log:
```bash
ssh jonno@192.168.1.14 "powershell -Command \"Get-WinEvent -LogName 'System' | Where-Object { \$_.ProviderName -like '*amd*' -or \$_.ProviderName -like '*display*' } | Select-Object -First 20 TimeCreated, Id, Message | Format-List\""
```

**Mitigations — baked into `install-node-windows.ps1` (`Harden-Stability` function), applied on every `just deploy-node beelink1`:**

- **TDR timeout 2s → 60s** — gives AMD driver time to finish a large GPU dispatch before Windows declares it hung
- **Fast Startup disabled** — prevents hybrid-shutdown half-suspended state
- **NIC power management off** — belt-and-braces

To apply immediately without a full re-provision:
```bash
ssh jonno@192.168.1.14 "powershell -Command \"reg add 'HKLM\\SYSTEM\\CurrentControlSet\\Control\\GraphicsDrivers' /v TdrDelay /t REG_DWORD /d 60 /f; reg add 'HKLM\\SYSTEM\\CurrentControlSet\\Control\\Session Manager\\Power' /v HiberbootEnabled /t REG_DWORD /d 0 /f; Get-NetAdapter | Set-NetAdapterPowerManagement -AllowComputerToTurnOffDevice Disabled\""
```

If TDR events are confirmed in the logs after a future incident, consider also reducing `LLAMA_GPU_LAYERS` from 99 to a lower value (e.g. 20–30) to reduce sustained GPU pressure.

**Confirmed root cause (2026-05-23):** BSOD 0x00000133 DPC_WATCHDOG_VIOLATION — not a TDR. AMD GPU driver's DPC routine running too long under Vulkan load. **Most likely fix: update AMD GPU drivers** via Device Manager → Display Adapters → AMD Radeon 780M → Update Driver. Minidumps at `C:\WINDOWS\Minidump\052326-8968-01.dmp` and `052326-9078-01.dmp` if deeper analysis needed.

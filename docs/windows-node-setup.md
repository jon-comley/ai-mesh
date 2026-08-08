# Windows Compute Node Setup

Step-by-step guide for adding a fresh Windows 11 machine to the ai-mesh cluster as a compute node. Captures every gotcha encountered during the Beelink SER8 provisioning.

> **Network note (2026-06-25 the mesh router migration).** The home subnet moved
> `192.168.1.x` → `10.0.0.x`. Current addresses: **pi1 `pi1.local`**,
> **beelink1 `beelink1.local`**, **SLZB-06 Zigbee `10.0.0.12`** (set the mesh router DHCP
> reservations so they don't move). **Any `192.168.1.x` address below is
> historical** — substitute the current address when following the steps. The
> troubleshooting section near the end intentionally keeps the old IPs because it
> documents the migration itself.

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

The agent connects **outbound** from the Windows machine to the coordinator on **pi1** (`pi1.local:9000`). Outbound connections are allowed by default, so **no inbound firewall rule or portproxy is required on the Windows node**. Just confirm the node can reach pi1:

```powershell
# Run on the Windows machine
Test-NetConnection pi1.local -Port 9000   # expect TcpTestSucceeded : True
```

> **Legacy:** earlier versions ran the coordinator in WSL2 on the laptop, which required a Windows firewall rule plus a `netsh` portproxy (`just update-portproxy`) to expose it on the LAN. With the coordinator on pi1 that setup is no longer needed for compute nodes.

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
& "C:\Users\<user>\ai-mesh\install-node-windows.ps1" -CoordinatorIp pi1.local
```

The provision script does:
- Creates `C:\Users\<user>\ai-mesh\` and `logs\` directories
- Sets `LocalAccountTokenFilterPolicy = 1` (SSH elevation)
- Downloads the latest **llama.cpp Vulkan release** ZIP from GitHub and extracts it to `%LOCALAPPDATA%\Programs\llama.cpp\` (e.g. `C:\Users\jonno\AppData\Local\Programs\llama.cpp\llama-server.exe`)
- Installs **NSSM** via winget
- Registers `ai-mesh-agent` as a Windows service (auto-start)
- Sets `COORDINATOR_IP`, `AGENT_ROLE`, `LLAMA_MODEL_DIR`, `LLAMA_SERVER_BIN`, `LLAMA_GPU_LAYERS=99`, and `LLAMA_FLASH_ATTN=1` as service environment vars
- Configures log rotation (10 MB per file) and restart throttle (5 s delay)

Models are downloaded on first load — nothing is pre-cached during provisioning. The install script detects the GPU and logs the recommended model; use `just auto-load-model beelink1` to load it automatically after provisioning.

The service is installed **without a TLS fingerprint** at this stage — that is expected. The fingerprint is pushed automatically when you run `just restart-coordinator` on the controller machine. Until then, the agent will log TLS handshake failures and retry every 5 seconds.

---

## 5. Start the coordinator (from WSL)

Run this from the controller machine after provisioning is done:

```bash
just restart-coordinator
```

This starts the coordinator, pushes the TLS fingerprint to beelink1 (and all other
compute nodes), and loads the best model automatically. The fingerprint is also written
to `~/.bashrc` on the controller so the CLI works without extra configuration.

After it completes, verify the node is registered and the model is ready:

```bash
just nodes
# BEELINK1 | beelink1.local | Compute | ... | qwen2.5:7b (Ready)

just validate-routing
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
| Verify llama-server path | `ssh user@host 'dir "%LOCALAPPDATA%\Programs\llama.cpp\llama-server.exe"'` |

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
ssh jonno@beelink1.local 'nssm get ai-mesh-agent AppEnvironmentExtra'
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
2. Can the Windows machine reach the coordinator on pi1? On the Windows machine: `Test-NetConnection pi1.local -Port 9000`
3. Is the service env pointed at pi1? `COORDINATOR_IP` should be `pi1.local` (see "Service environment variables" below).
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

### (Legacy) Portproxy stale after WSL2 restart

No longer relevant for compute nodes — they connect directly to pi1. This only applied when the coordinator ran in WSL2 on the laptop, where the portproxy rule went stale on each Windows restart (`just update-portproxy` refreshed it).

### Stale node entries accumulating in the registry

The agent generates a new UUID on each start, so every service restart adds a new row. Clean up stale entries with:
```bash
just reset          # calls: cargo run -p cli -- reset-registry
```

### NSSM — `nssm.exe not found on PATH` during deploy

Two causes, often together, on a fresh/recovered Windows image:

1. **Broken winget source** — the install fails with
   `Failed when opening source(s)` / `0x8a15000f : Data required by the source is missing`,
   so NSSM never actually installs. Repair the source:
   ```powershell
   winget source reset --force
   winget source update
   ```
2. **PATH not refreshed in-session** — winget adds NSSM to the registry PATH, but the
   *running* PowerShell session doesn't see it, so `Get-Command nssm.exe` fails until a
   new shell is opened.

`install-node-windows.ps1` now handles both automatically: `Ensure-WingetPackage` resets
the winget source and retries on failure, and `Get-Nssm` refreshes `$env:Path` from the
registry and falls back to winget's install dirs
(`%LOCALAPPDATA%\Microsoft\WinGet\Links\nssm.exe` and `…\WinGet\Packages\…\win64\`).
If it still can't be found, reset the winget source (above) and re-run the deploy — it's
idempotent (llama-server and other already-present steps are skipped).

### NSSM — environment variables not being set

NSSM `AppEnvironmentExtra` requires each variable as a **separate argument**, not semicolon-separated:
```powershell
# CORRECT
nssm set ai-mesh-agent AppEnvironmentExtra "COORDINATOR_IP=pi1.local" "AGENT_ROLE=compute"

# WRONG — produces a single malformed variable
nssm set ai-mesh-agent AppEnvironmentExtra "COORDINATOR_IP=pi1.local;AGENT_ROLE=compute"
```

Verify what NSSM has stored:
```powershell
nssm get ai-mesh-agent AppEnvironmentExtra
```

### Trailing spaces in env vars (cmd.exe pitfall)

When setting env vars inline in cmd.exe, it is easy to include a trailing space:
```cmd
REM WRONG — COORDINATOR_IP = "pi1.local " (note trailing space)
set COORDINATOR_IP=pi1.local && agent.exe

REM RIGHT — quotes prevent trailing space
set "COORDINATOR_IP=pi1.local" && agent.exe
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

### Smart App Control blocks the agent (service won't start, no logs) ⚠️

**Symptom:** `ai-mesh-agent` service refuses to start. `Start-Service` reports `StartServiceFailed`; SCM logs Event `7034` "service terminated unexpectedly". Critically, **`agent.log` gets no new lines at all** — the binary dies before it can initialise logging. The node shows as a stale entry on the coordinator (old `Last Seen`).

**Root cause:** **Smart App Control (SAC)** — a Windows 11 reputation-based code-integrity policy — blocks the self-built `agent.exe` because it is unsigned/unknown. Windows 11 **auto-enables SAC after a clean install**, so this appears specifically after the box has done a self-reinstall (see incident history). A previously-launched agent keeps running because it started *before* the policy took effect, but any fresh start is blocked.

**Detect it:**
```powershell
# 1 = Enforced (blocking), 2 = Evaluation, 0 = Off
(Get-ItemProperty 'HKLM:\SYSTEM\CurrentControlSet\Control\CI\Policy').VerifiedAndReputablePolicyState
# Confirm the block event (look for id 3077 / 3118 "Smart App Control Block")
Get-WinEvent -LogName 'Microsoft-Windows-CodeIntegrity/Operational' -MaxEvents 5
```
Or prove it directly by running the binary by hand — the error is unambiguous:
```powershell
Start-Process 'C:\Users\jonno\ai-mesh\agent.exe' -PassThru   # -> "An Application Control policy has blocked this file."
```

**Fix — turn Smart App Control off (⚠️ one-way switch):** Once turned **Off, SAC cannot be re-enabled without a clean Windows reinstall.** This is the correct trade for a dedicated compute node running a self-built binary.
- GUI: Windows Security → App & browser control → Smart App Control → Settings → **Off**.
- Or registry + reboot (elevated): `Set-ItemProperty 'HKLM:\SYSTEM\CurrentControlSet\Control\CI\Policy' -Name VerifiedAndReputablePolicyState -Value 0; Restart-Computer`

After reboot the agent launches normally. Longer term, code-signing `agent.exe` would let SAC stay on, but that is not currently set up.

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
   ssh-copy-id jonno@beelink1.local
   ```
   If `ssh-copy-id` fails because the key isn't in `authorized_keys` yet, add it manually — open a PowerShell on the Beelink and run:
   ```powershell
   Add-Content 'C:\ProgramData\ssh\administrators_authorized_keys' '<paste public key here>'
   ```
   Or from WSL:
   ```bash
   cat ~/.ssh/id_ed25519.pub | ssh jonno@beelink1.local "powershell -Command \"Add-Content 'C:\\ProgramData\\ssh\\administrators_authorized_keys' '\$(cat)'\""
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

### Beelink SER8 — DPC_WATCHDOG_VIOLATION crash log

**Stop code:** `0x00000133 DPC_WATCHDOG_VIOLATION` — a driver DPC (Deferred Procedure Call) routine ran longer than the Windows watchdog timeout. Always param2=`0x1e00` (same clock-interval threshold every crash). Machine BSODs and reboots automatically.

**Symptom:** No ping or SSH response, or machine reboots unexpectedly. Confirmed via Event ID 1001 (BugCheck) in System event log.

**Key observation:** Crashes occur when the GPU is **idle or under light load**, not during heavy inference. This rules out sustained thermal load as the trigger.

**✅ CONFIRMED ROOT CAUSE (2026-05-28):** AMD fTPM (firmware TPM) — see incident entry below. Fix: disable fTPM in BIOS.

**⚠️ REGRESSED 2026-06-02:** the `0x133`/`param2=0x1e00` storm returned (5 dumps) — fTPM appears to have re-enabled itself (BIOS defaults restored after a power event / suspected weak CMOS battery). Re-disable fTPM in BIOS. See 2026-06-02 entry.

**⚠️ REGRESSED 2026-06-24/25, then ROOT-CAUSE CORRECTED + RESOLVED:** 8× `0x133`/`param2=0x1e00` across 06-24→06-25 with TPM-WMI 1025 on every boot. This was **not** a wiped golden state — Pluton was Disabled the whole time. **Disabling Pluton alone never stopped fTPM** (it kept running via the TPM level). The actual fix (2026-06-25) was setting the **Trusted Platform Module level itself to `Disabled`**, after which fTPM stayed off (`Get-Tpm`=False, no 1025) and the box ran 7+ h with zero `0x133`. See the 2026-06-25 entry + corrected golden-state caution.

**⚠️ REGRESSED 2026-07-01/02 — worst storm on record.** The 06-11 stable state regressed again. Live event-log pull: **20× `0x133`/`param2=0x1e00` bugchecks in 24h** (07-01 03:57 → 07-02 01:28), accelerating into a **crash every ~22 min** since ~21:46 on 07-01 (11 in a row on a near-exact 22-min cadence); uptime never exceeded ~40 min → node effectively unusable. Kernel-Power 41/6008 + minidumps (`070126-*.dmp`) confirm. Fix unchanged (TPM level = Disabled), but this is the **fourth regression** (05-28, 06-02, 06-04, 07-01) → the board clearly isn't holding BIOS settings across power events: **replace the CMOS battery** is now the prime durable fix, else execute the standing move-to-Linux/LTSC plan. Fallout while crashing: constant model loss, `deploy-node` hangs, TLS-fingerprint crash-loop, coordinator read-timeout closes — all downstream of the reboots, not ai-mesh bugs.

**Diagnostics — run after any recovery:**
```bash
ssh jonno@beelink1.local "powershell -Command \"Get-WinEvent -LogName System -MaxEvents 500 | Where-Object { \$_.Id -eq 41 -or \$_.Id -eq 1001 -or \$_.Id -eq 4101 -or \$_.Id -eq 109 } | Select-Object TimeCreated, Id, @{N='Msg';E={\$_.Message.Substring(0,[Math]::Min(300,\$_.Message.Length))}} | Format-List\""
```

**Check current driver version:**
```bash
ssh jonno@beelink1.local "powershell -Command \"Get-WmiObject Win32_VideoController | Select-Object Caption, DriverVersion, DriverDate | Format-List\""
```

---

#### 2026-05-23 — First confirmed BSOD incidents

- **Driver:** unknown at time of writing (pre-26.5.2)
- **Minidumps:** `052326-8968-01.dmp`, `052326-9078-01.dmp`
- **Finding:** Confirmed `0x00000133` DPC_WATCHDOG_VIOLATION. Initially suspected TDR (display driver timeout under load) but logs showed no TDR event IDs.
- **Action taken:** Upgraded AMD driver to Adrenalin 26.5.2 / `32.0.31007.5012` (May 2026). Also set `TdrDelay=60` in registry to give GPU more time to finish dispatches.

---

#### 2026-05-25/26 — Mass crash storm (~25+ BSODs)

- **Driver:** `32.0.31007.5012` (installed ~2026-05-12)
- **Frequency:** Every 30–90 minutes, day and night. Crashes at idle — not load-gated.
- **Minidumps:** `052526-9375`, `052526-10046`, `052526-9750`, `052626-9468`, `052626-9609` (and more)
- **param2:** `0x1e00` every single crash — same DPC code path every time
- **2026-05-26 escalation:** Machine overheated → required 2× CMOS resets and cooling period before it would POST. Root cause of overheating: `TdrDelay=60` meant the GPU ran at full load for 60s before Windows attempted reset, cooking the hardware.
- **Actions taken:**
  - `TdrDelay=60` **reverted** — registry key deleted. Default (2s) restored. Documented in `install-node-windows.ps1` with a DO NOT SET warning.
  - `install-node-windows.ps1` updated: TdrDelay removed, `ai-mesh-harden-boot` startup task added to re-apply NIC power management at boot.
- **Remaining suspected cause:** AMD driver `32.0.31007.5012` itself. Rolling back considered, but the previous driver (`32.0.21025.10016`, Aug 2025) was known-bad for a different reason: `0xe06d7363` SEH exception crashes under Vulkan load (unusable for GPU inference).

---

#### 2026-05-28 — Crash storm; ULPS investigated; fTPM confirmed as root cause ✅

- **Context:** Machine went offline. User reverted AMD driver to Beelink-recommended version `32.0.21025.10016` (Aug 2025 build). Multiple BSODs occurred during and after driver activity, and also during plain desktop use with no driver installs in progress.
- **Driver at time:** `32.0.21025.10016`
- **Minidumps:** `052826-9578-01.dmp` (16:52), `052826-9859-01.dmp` (17:09), `052826-9953-01.dmp` (17:20), `052826-10078-01.dmp` (17:43), `052826-9281-01.dmp` (18:10), `052826-9312-01.dmp` (19:19), `052826-9625-01.dmp` (19:49)
- **Critical finding:** Both the May 2026 driver and the Aug 2025 driver produced identical crash signatures. Machine crashed during plain idle desktop use with no GPU activity. Root cause is not the GPU driver at all.
- **ULPS investigated (red herring):** `EnableUlps=0` and `PP_SclkDeepSleepDisable=1` were applied and baked into `install-node-windows.ps1`. Machine still crashed. ULPS is harmless to leave disabled but was not the cause.
- **✅ Actual root cause: AMD fTPM (firmware TPM).** Event log showed `Microsoft-Windows-TPM-WMI` re-provisioning the TPM after every single crash. The AMD PSP (Platform Security Processor — a separate ARM core on the Ryzen die) implements fTPM by periodically writing to SPI flash. During that write it holds a bus lock on the SPI interface, stalling all CPU cores. Windows sees a DPC that cannot complete within `0x1e00` clock intervals and fires `0x00000133`. This is an AMD-acknowledged bug present since Ryzen 5000, not fixed on the SER8's Ryzen 8000 series.
- **Fix applied (2026-05-28 ~20:00):** fTPM **disabled in BIOS** (Advanced → SOC Misc Control → Trusted Platform Modules → dTPM Level 3 without Pluton Security Processor). Machine stable immediately after.
- **What is lost by disabling fTPM:** Nothing relevant to this use case. BitLocker not enabled, no Windows Hello, no MDM attestation. Machine boots and runs identically.

> **BIOS navigation (confirmed on Beelink SER8 / AMI v2.22.1293, 2025):**
> 1. Enter BIOS (spam **Delete** on boot)
> 2. **Advanced → Trusted Computing → Security Device Support → Disabled**
>    - Confirms as disabled when it shows "No Security Device Found"
> 3. **Find the hardware-level switch (Crucial — hiding from Windows is not enough):**
>    On the SER8 (Ryzen 8000), "fTPM" is replaced by **Microsoft Pluton**. You must disable the hardware processor here:
>    - **Advanced → SOC Misc Control → Trusted Platform Modules → dTPM Level 3 without Pluton Security Processor**
>    - **Advanced → SOC Misc Control → Pluton Security Processor → Disabled**
>    - *Note: If these aren't visible, press **Ctrl + F1** on the main screen to reveal hidden menus.*
> 4. Optional: **Advanced → AMD CBS → CPU Common Options → Global C-state Control → Disabled**
> 5. F10 → Save & Exit
>
> Note: "Secure Boot" (Boot menu) is a completely separate setting — does NOT fix the crashes.
> After any CMOS reset, both switches revert to Enabled — always re-disable before booting Windows.

---

#### 2026-05-31 — Type B hang (kernel hang, new symptom) — under investigation

- **Incident:** 2026-05-31 ~12:10 UTC. Light on, no screen/GPU output. SSH works (TCP connection ESTABLISHED to coordinator), but agent process unresponsive. Tasklist shows agent.exe with 0:00:00 CPU time and 16 MB memory (vs. expected GBs for running inference). HTTP health check times out. Required hard power button reset.
- **Difference from Type A:** Type A was system BSOD (0x00000133 DPC_WATCHDOG). Type B is process hung in kernel but system stays up.
- **Signature:** Agent never fully initialized (only 16 MB memory, no models loaded). Process stuck in kernel I/O wait or preempted at a non-cancellable point. Likely GPU driver hang during Vulkan initialization or model load.
- **Probable root cause:** GPU driver issue during inference startup. Type A was fTPM (now fixed); Type B appears to be GPU-related and driver-version dependent.
- **Current driver at time of hang:** `32.0.31007.5012` (May 12, 2026) — pre-26.5.2. Adrenalin 26.5.2 upgrade was attempted but did not persist.
- **Remediation in progress:**
  1. Re-upgrading to AMD Adrenalin 26.5.2 (should include Ryzen 8000 iGPU fixes)
  2. Re-upgrading chipset drivers to 8.05.04.516 (latest May 2026)
  3. WoL (Wake on LAN) disabled in initial provisioning script to rule out network-triggered half-sleep states
- **Diagnostics if it happens again:**
  - Collect Windows minidump from System event log (Event ID 1001 or 41)
  - Check GPU driver version after reboot
  - Monitor logs for timing pattern (always after N minutes? during specific operations?)

---

#### 2026-05-31 — Type B hang during AMD driver install — resolved with clean reinstall

- **Incident:** During AMD driver install (~12:45 UTC), system became unresponsive (black screen, no GPU output). SSH remained responsive. No crash event in System log (rules out Type A/DPC_WATCHDOG). Driver version check showed install did not complete — still showed `32.0.31007.5012` (May 12, pre-26.5.2).
- **Symptom signature:** GPU/display frozen, but kernel still responsive. No BugCheck event. Matches Type B hang pattern (GPU driver hang during initialization).
- **Root cause hypothesis:** AMD driver installer triggered GPU initialization that hung or was incompatible with the pre-26.5.2 state. Multiple outdated AMD apps (old Radeon Settings + new Adrenalin) may have conflicted.
- **Remediation applied:**
  1. Identified duplicate AMD apps installed (old Radeon Settings + new Adrenalin) — uninstalled legacy Radeon Settings
  2. Ran AMD Clean Utility to completely remove all driver files and registry entries
  3. Downloaded fresh AMD Adrenalin 26.5.2 driver + chipset 8.05.04.516
  4. Performed clean reinstall of both driver and chipset
  5. Rebooted after install completed
- **Outcome:** Install completed without hanging. Driver version pending verification post-reboot (may be cached until system refresh). No new crash events in log.
- **Lesson:** Duplicate AMD driver applications can cause conflicts. Always use Clean Utility before major driver reinstalls, not just the standard uninstall.

---

#### 2026-05-31 — Machine triggered Windows self-reinstall; Type B crashes accelerating

- **Incident:** Beelink triggered an automatic Windows reinstall from online recovery. Cause unknown — likely repeated BSODs triggered Windows recovery mode.
- **Post-reinstall crash pattern:** Crashing within 1-2 minutes of boot (faster than before). Freeze → black screen → "Your device ran into a problem and needs to restart". No minidump captured (crashes too fast). No entries in System Event Log before crash.
- **BIOS check:** On AMI v2.22.1293, confirmed "Security Device Support" under Advanced → Trusted Computing was already Disabled. Note: this may only hide TPM from Windows — AMD PSP fTPM may still be running at firmware level.
- **SSH state:** Each reinstall wipes SSH config; must re-enable via `Add-WindowsCapability` each time.
- **Suspected cause:** AMD driver missing/default after reinstall. User plans to install AMD Adrenalin 26.5.2 + chipset 8.05.04.516 when stable enough to do so. Machine was stable for days before instability resumed — software (driver) regression, not hardware failure.
- **Not hardware:** RAM/SSD physical issues ruled out by user. Pattern matches driver-triggered crash, not component failure.
- **Next steps:**
  1. Get machine stable long enough to install AMD Adrenalin 26.5.2 + chipset 8.05.04.516
  2. Verify `Security Device Support` is Disabled in BIOS after every reinstall
  3. Run `install-node-windows.ps1` once SSH is stable to apply all hardening settings

---

#### 2026-06-02 — fTPM DPC_WATCHDOG storm RETURNS (regression) + household power cut

- **Two distinct events today, do not conflate them:**

  **(A) DPC_WATCHDOG crash storm — regression of the 2026-05-28 fTPM fix.** Five `0x00000133` bugchecks since the previous evening, all with the fTPM signature `param2 = 0x1e00` (sampled two; identical to every Type A crash):
    | Local time (BST) | Minidump |
    |------------------|----------|
    | 2026-06-01 22:17:53 | `060126-8640-01.dmp` |
    | 2026-06-02 06:10:50 | `060226-8531-01.dmp` |
    | 2026-06-02 06:33:04 | `060226-8671-01.dmp` (0x133, param2 `0x1e00` confirmed) |
    | 2026-06-02 06:55:19 | `060226-8500-01.dmp` (0x133, param2 `0x1e00` confirmed) |
    | 2026-06-02 07:01:40 | `060226-8687-01.dmp` |

    Same code path, same param2, idle-triggered — textbook fTPM. The 2026-05-28 BIOS fix has come undone. Most likely **fTPM got re-enabled in BIOS** (BIOS defaults restored — the documented trigger is a CMOS reset; a power event or a weakening CMOS battery is the prime suspect here since no one touched the BIOS). The 2026-05-31 note already warned that "Security Device Support = Disabled" may only hide TPM from Windows while the AMD PSP keeps running at firmware level — consistent with crashes resuming despite that setting appearing disabled.

  **(B) Household power cut — separate, not a crash.** `Event 41` + `6008` at **16:36:43**, with **no `1001` bugcheck** = clean power loss, not a BSOD. This matches the whole-house power cut (pi1 also lost power but recovered cleanly). After power returned the machine **did not auto-recover** — down ~56 min until a physical power-button reset at **17:32:45**. Either the BIOS power-state-after-AC-loss is set to "stay off," or it hung on the first post-cut boot. Worth setting BIOS "Restore on AC Power Loss = Power On" so an always-on compute node self-heals after a cut without a house call.

- **Driver at time:** still `32.0.31007.5012` (2026-05-12). The Adrenalin 26.5.2 upgrade attempted 2026-05-31 **still has not persisted** — but per established root cause the driver is innocent for `0x133`; fTPM is the cause.
- **Current state (post physical reset 17:32:45):** stable, agent reconnected to coordinator, `qwen2.5:7b` Ready, heartbeat ~220 ms. Healthy for now — but it **will** storm again until fTPM is genuinely disabled.
- **Action required (in priority order):**
  1. **Re-enter BIOS and verify/disable fTPM** — Advanced → SOC Misc Control → Trusted Platform Modules → dTPM Level 3 without Pluton Security Processor (and Pluton Security Processor → Disabled) (and Trusted Computing → Security Device Support = Disabled). This is the actual fix; everything else is secondary.
  2. Set BIOS **Restore on AC Power Loss = Power On** so the node recovers itself after a power cut.
  3. Re-attempt the Adrenalin 26.5.2 + chipset 8.05.04.516 upgrade (clean-utility first) while it's stable.
  4. Reinforces the standing plan to rebuild beelink on a leaner OS (Win IoT Enterprise LTSC) or move it to Linux to escape the Windows/AMD-PSP problem class entirely.

---

#### 2026-06-04 — fTPM storm ACTIVE again after CMOS reset (worst run yet) + Intel AX200 NIC errors

Logs pulled live from BEELINK1 at **2026-06-04 21:15Z** (SSH; ICMP is firewalled but TCP/22 was up — the box looked "down" to ping earlier today but was reachable over SSH). **The machine is in an active `0x00000133` fTPM crash-loop right now.** It had been unreachable earlier; the **CMOS reset done to recover it re-enabled fTPM and it was not re-disabled in BIOS**, so the storm resumed and has been bug-checking every few minutes-to-hours across 06-03 and 06-04. At collection, uptime was **3m45s** (booted 21:11:42, immediately after the 21:11 crash).

**Bugchecks (System Event 1001) — all `0x00000133`, param2 `0x1e00` (the fTPM signature):**
```
2026-06-04 21:11:50Z  0x133 (...,0x1e00,...)  Minidump\060426-8718-01.dmp   (latest — 4 min before collection)
2026-06-04 07:30:09Z  0x133 (...,0x1e00,...)  060426-8765-01.dmp
2026-06-04 01:38:13Z  0x133 (...,0x1e00,...)  060426-8656-01.dmp
2026-06-04 01:25:51Z  0x133 (...,0x1e00,...)  060426-9343-01.dmp
2026-06-04 01:23:30Z  0x133 (...,0x1e00,...)  060426-8640-01.dmp
2026-06-03 23:59:35Z  0x133 (...,0x1e00,...)  060326-8671-01.dmp
2026-06-03 10:50:45Z  0x133 ... (+ 8 more 0x133/0x1e00 back to 2026-06-03 03:54)
```
All 15 most-recent 1001 events are `0x133`/`0x1e00`. (Prior `0x133` cluster + one `0x9f` on 2026-05-31 also still in the log — see earlier entries.)

**Kernel-Power 41 cascade (06-04):** 01:01, 01:23, 01:25, 01:38, 07:20, 07:30, 09:54, 17:34, 21:11 — ~9 crash-reboots on 06-04 alone (plus a similar run on 06-03). Matching Event 6008 unexpected-shutdown chain confirms none were clean.

**TPM re-provisioning fingerprint (Microsoft-Windows-TPM-WMI) — confirms fTPM is active:**
```
2026-06-04 21:11:59Z  id=1025  The TPM was successfully provisioned and is now ready for use.
2026-06-04 21:04:32Z  id=1025  The TPM was successfully provisioned and is now ready for use.
```
TPM re-provisions on every boot after each crash — the exact signature that nailed fTPM as root cause on 2026-05-28.

**Minidumps retained (Windows keeps ~5, ~1.7–3.4 MB each):** `060426-9343-01` (01:25), `-8656-01` (01:38), `-8765-01` (07:30), `-8656-02` (09:52), `-8718-01` (21:11).

**NEW secondary signal — Intel Wi-Fi AX200 NIC instability (2026-05-31 cluster):**
```
NDIS    id=10317  Miniport Intel(R) Wi-Fi 6 AX200 ... Fatal error: internal error
Netwtw10 id=5007  TX/CMD timeout (TfdQueue hanged)
Netwtw10 id=5032  Driver Miniport reset watchdog
Netwtw10 id=5002/5005  adapter not functioning / internal error
```
The node is on **Intel Wi-Fi AX200 (wireless)**, not wired. These fatal miniport resets coincide with the "no route to host / unreachable" episodes (incl. earlier today) and are a plausible contributor to apparent "down" periods independent of the fTPM BSODs. Candidate mitigations: disable AX200 power management, or move the node to **wired Ethernet**.

**Ruled OUT (confirmed good):**
- AMD display driver `32.0.31007.5012` (2026-05-12) — innocent per established root cause.
- Registry hardening intact: `EnableUlps=0`, `PP_SclkDeepSleepDisable=1`, `TdrDelay` absent.

**Agent (`C:\Users\jonno\ai-mesh\logs\agent.log`):** reconnects cleanly every boot (e.g. `2026-06-04T20:08:35`/`20:11:51` → "Connected to coordinator (TLS) … startup sequence complete", node_id `856466cd-…`) — then the box BSODs again. Steady benign `WARN agent::dispatch: no capability handles: Acknowledge` between heartbeats.

**Action — URGENT and unchanged:** fTPM is enabled again and actively storming. **Disable fTPM/Pluton in BIOS now** (Advanced → SOC Misc Control → Trusted Platform Modules → dTPM Level 3 without Pluton Security Processor; Pluton Security Processor → Disabled; Trusted Computing → Security Device Support → Disabled). Until then it keeps crash-looping. This is the **third** recurrence (05-28 fixed → 06-02 regressed → 06-04 regressed after CMOS reset) — strong case for retiring this Windows config: rebuild on **Win IoT Enterprise LTSC** or **Linux**, and consider **wired Ethernet** to sidestep the AX200 issue.

**Follow-up (same session):** fTPM disable **not yet applied** — user is mid-recovery and will revisit the BIOS shortly. Box was still crash-looping at time of writing (last `0x133` at 21:11Z, then a cleanup SSH failed a few min later — likely another BSOD). Next step is to **re-pull these logs after the BIOS change** to confirm uptime climbs with no new Event 1001 / `0x133`.

---

#### 2026-06-05 — Full recovery + automation overhaul

**What happened:** Another CMOS reset wiped all BIOS settings (same recurring trap). Pluton re-enabled → `0x133` storm resumed — three bugchecks logged on 06-05 (19:30, 20:48, 20:51), uptime only 7 minutes at collection. Intel AX200 Wi-Fi also dropped (fatal miniport resets), making the box unreachable over wireless; had to plug in ethernet to get SSH access.

**Root cause (same as every time):** CMOS reset → BIOS defaults restored → Pluton re-enabled → fTPM SPI bus lock → `0x133` DPC_WATCHDOG.

**What was fixed (2026-06-05):**

1. **BIOS Golden State re-applied** — all settings from the table above (Pluton Disabled, Global C-state Disabled, UMA 8G, Fan 45%, Restore on AC Power Loss, WoL Disabled, TDP Balanced).

2. **`fix-beelink-stability.ps1` repaired** — the AX200 Wi-Fi power management fix was using the **GPU class GUID** (`{4d36e968...}`) instead of the **NIC class GUID** (`{4D36E972...}`), so it silently did nothing on every previous run. Now correct. Also removed the unverified GPIO driver download (shortlink redirect, no hash, not a confirmed fix for any logged crash) and the overly-broad `SearchOrderConfig` driver-search block. Adds verification output.

3. **`install-node-windows.ps1` updated** — added High Performance power plan activation to `Harden-Stability` (was missing; only `fix-beelink-stability.ps1` had it).

4. **`just deploy-node beelink1` fixed** — a redundant second stability-hardening step was added that used `Start-Process -Verb RunAs` over SSH, which hangs waiting for a UAC prompt that never arrives. Removed — `install-node-windows.ps1` already runs `Harden-Stability` inline (elevated by the install script) so no second pass is needed.

5. **Ethernet** — AX200 Wi-Fi is unreliable (fatal miniport resets correlate with crash/unreachable episodes). Plugged in wired ethernet; this should be the permanent connection for this node.

**Verified state post-recovery (SSH):**
```
AMD EnableUlps=0  PP_SclkDeepSleepDisable=1  ✅
AX200 PnPCapabilities=24                      ✅  (NIC fix now actually applied)
Power plan: High Performance                  ✅
```

**Current state:** BIOS settings applied, software hardening applied, wired ethernet in, reboot pending to activate. Once rebooted, run `just start-cluster` to bring the full cluster back online.

**Recovery checklist going forward (after any CMOS reset):**
1. BIOS → apply the Golden State table above
2. Boot Windows → `just fix-node beelink1` (or `just deploy-node beelink1` for a full reinstall)
3. Reboot to activate
4. `just start-cluster`

---

#### 2026-06-24/25 — the mesh router network migration + fTPM storm regression (4th) + Smart App Control discovered

Context: home network migrated from the ISP router to a mesh router. Subnet changed `192.168.1.x` → `10.0.0.x` (new: pi1 `pi1.local`, beelink1 `beelink1.local`, SLZB-06 `10.0.0.12` — dynamic leases, reservations pending). Getting beelink back on the mesh surfaced three stacked problems:

**1. Stale `COORDINATOR_IP` (network migration).** Every agent had `COORDINATOR_IP=192.168.1.11` baked in from the old network and looped `No route to host` / `os error 10060`. On beelink this lives in the nssm registry env, not a file:
```powershell
# read it
(Get-ItemProperty 'HKLM:\SYSTEM\CurrentControlSet\Services\ai-mesh-agent\Parameters').AppEnvironmentExtra
# surgically rewrite only COORDINATOR_IP (elevated), preserving the other 8 entries + the fingerprint/token
$k='HKLM:\SYSTEM\CurrentControlSet\Services\ai-mesh-agent\Parameters'
$e=(Get-ItemProperty $k).AppEnvironmentExtra | ForEach-Object { $_ -replace '^COORDINATOR_IP=.*','COORDINATOR_IP=pi1.local' }
Set-ItemProperty $k -Name AppEnvironmentExtra -Value $e -Type MultiString
```
The TLS fingerprint and auth token already matched (both persist in coordinator state across restarts), so `set-fingerprint` was **not** needed — only the IP. (On pi1 the equivalent fix was a drop-in `coordinator.conf` pointing the co-located agent at `127.0.0.1`, immune to future LAN changes.)

**2. Orphan process + nssm "failed to start".** Env is read once at process start, so the registry change had no effect until the running agent was restarted — but a stale `agent.exe` **orphan** (nssm lost track of it; service showed STOPPED while the child kept running on the old IP) blocked a clean restart. Killing all `agent.exe` first is required: `Get-Process agent | Stop-Process -Force` then `Start-Service`.

**3. Smart App Control blocked the binary (the real blocker).** Even after the IP fix and killing the orphan, the service still wouldn't start — `agent.exe` was blocked by **Smart App Control** (`VerifiedAndReputablePolicyState=1`, CodeIntegrity Event 3118 "Smart App Control Block"), auto-enabled after the box's earlier self-reinstall. See the dedicated troubleshooting section above. Fix: SAC off + reboot. After that the agent launched, connected to `pi1.local`, and loaded `phi4:14b` (the 16 G UMA golden state now auto-selects 14b over the old 7b).

**Note — non-elevated SSH.** `LocalAccountTokenFilterPolicy` was lost in the reinstall, so SSH sessions land **non-elevated**: registry writes under `HKLM\SYSTEM`, `Start-Service`, and BIOS-adjacent work must be done from a **local elevated** PowerShell (or re-enable it: `New-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System' -Name LocalAccountTokenFilterPolicy -Value 1 -PropertyType DWord -Force`). ICMP stays firewalled (ping is useless for liveness — use TCP/22).

**fTPM/Pluton `0x133` storm — REGRESSED (4th time).** Event-log pull on 2026-06-25 (uptime ~6 min):
```
BugCheck (Event 1001), all 0x00000133 param2=0x1e00 (fTPM signature):
  06-25 02:46:58   06-24 17:47:26   06-24 15:25:08   06-24 13:02:49
  06-24 11:15:28   06-24 10:08:12   06-24 06:15:49   06-24 03:08:29   (~every 2-3 h)
Kernel-Power 41 + Event 6008 (dirty shutdown) chains match each crash.
TPM-WMI 1025 (fTPM re-provision) at 06-25 04:19:55 — i.e. immediately after the latest boot => fTPM ACTIVE NOW.
Minidumps: 062526-8859-01.dmp (02:46) + four 062426-*.dmp, ~3.4-3.8 MB each.
```
**Root cause was NOT a wiped golden state** — Pluton was Disabled the whole time (BIOS settings unchanged since the last apply). The real cause: **disabling Pluton alone leaves fTPM running via the TPM level** — TPM-WMI 1025 fired on every boot, SPI bus lock → DPC watchdog. **RESOLVED 2026-06-25**: set the Trusted Platform Module level itself to `Disabled` (not just Pluton). After the 14:55 reboot — `Get-Tpm` reports `TpmPresent=False`, no Event 1025, and **7+ hours uptime with zero `0x133`** (vs. one every 2–3 h before). Fix confirmed; see the corrected golden-state caution below. Also set the mesh router DHCP reservations so the `10.0.0.x` leases don't move.

---

#### Definitive BIOS "Golden State" (2026-06-05 — confirmed settings)

These are the exact settings required for stable 24/7 operation. **Every CMOS reset wipes them all — reapply after any CMOS reset.**

**1. Performance & Graphics**

| Setting | Value | Path |
|---|---|---|
| iGPU Configuration | **UMA_SPECIFIED** | Advanced → AMD CBS → NBIO Common Options → GFX Configuration → iGPU Configuration |
| UMA Frame Buffer Size | **16G** (was 8G; raised 2026-06-11) | Advanced → AMD CBS → NBIO Common Options → GFX Configuration → UMA Frame Buffer Size |

> A fixed UMA size stops stuttering when the GPU dynamically resizes memory during inference. 16G gives headroom for larger models on the 780M.

**2. Stability & Thermal**

| Setting | Value | Path |
|---|---|---|
| Global C-state Control | **Disabled** | Advanced → AMD CBS → CPU Common Options |
| Smart Fan — CPU Fan Mode | **Automatic Mode** | Advanced → Hardware Monitor → Smart Fan Function → CPU Fan Setting |
| Smart Fan — SMF Temp Limit (fan OFF) | **30°C** (default) | Advanced → Hardware Monitor → Smart Fan Function → CPU Fan Setting |
| Smart Fan — SMF Fan Limit (fan ON) | **35°C** (default) | Advanced → Hardware Monitor → Smart Fan Function → CPU Fan Setting |
| Smart Fan — SMF Start PWM | **45%** (default: 80) | Advanced → Hardware Monitor → Smart Fan Function → CPU Fan Setting |
| Smart Fan — Full PWM Temperature | **90°C** (default) | Advanced → Hardware Monitor → Smart Fan Function → CPU Fan Setting |
| Smart Fan — SMF Slope PWM | **1** (default) | Advanced → Hardware Monitor → Smart Fan Function → CPU Fan Setting |

> Fan curve: kicks in at 35°C at 45% speed, ramps 1%/°C, hits 100% at 90°C. Reducing Start PWM from the 80% default makes it quiet enough for 24/7 operation while still keeping the Hawks Point chip cool. Only SMF Start PWM needs changing — the rest are fine at defaults.

**3. Reliability & Security**

| Setting | Value | Path |
|---|---|---|
| **Trusted Platform Module** | **Disabled** | Advanced → AMD CBS → SOC Misc Control → Trusted Platform Modules |
| Pluton Security Processor | **Disabled** | Advanced → AMD CBS → SOC Misc Control |
| Restore on AC Power Loss | **Always On** | Advanced → FCH Common Options → Ac Power Loss Options → Ac Loss Control |

> The firmware TPM (fTPM) holding a SPI bus lock during flash writes is what triggers the `0x133` storms. **CRUCIAL.** Restore on AC = self-heals after power cuts.

**⚠️ Trusted Platform Modules — CORRECTED 2026-06-25 (earlier guidance here was wrong):**
Disabling **only** "Pluton Security Processor" is **NOT enough** — the fTPM keeps running via the TPM level and re-provisions on **every boot** (TPM-WMI Event 1025), so the `0x133` storm continues even with Pluton off. This was misdiagnosed for four recurrences as "the golden state got wiped"; in fact Pluton was disabled the whole time and fTPM was still active. **You must also set "Trusted Platform Module" itself to `Disabled`.** Verified menu (`Advanced → AMD CBS → SOC Misc Control → Trusted Platform Modules`): Trusted Platform Module = **Disabled**, Pluton Security Processor = **Disabled** (Microsoft Security Levels then reads "Customized" — that's a status, leave it). Confirm it took: `Get-Tpm` should show `TpmPresent=False`, and **no** new Event 1025 after boot.
Do **not** use "dTPM Level 3" — that caused an "Automatic Repair" boot loop (it tries to enable a discrete TPM chip the SER8 doesn't have). `Disabled` boots fine; nothing on this box uses the TPM (no BitLocker / Windows Hello).

**After BIOS — run the software hardening script:**
```
just fix-node beelink1
```
This applies: AMD ULPS/sleep registry tweaks, Intel AX200 NIC power management fix (PnPCapabilities=24), High Performance power plan.

---

#### Verification checklist after any reboot, driver change, or CMOS reset

```bash
# 1. fTPM disabled — MOST IMPORTANT. CMOS reset restores BIOS defaults (fTPM enabled).
#    After any CMOS reset, go back into BIOS and disable fTPM before booting Windows.
#    Verify in BIOS: Advanced → AMD CBS → SOC Misc Control → Trusted Platform Modules →
#    Trusted Platform Module = Disabled AND Pluton Security Processor = Disabled (NOT dTPM Level 3 — boot loop).
#    Software check: Get-Tpm should report TpmPresent=False, and no TPM-WMI Event 1025 after boot.

# 2. ULPS disabled (harmless belt-and-braces, baked into boot task)
ssh jonno@beelink1.local "reg query \"HKLM\SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}\0000\" /v EnableUlps"
# Expect: 0x0

# 3. Shader deep sleep disabled
ssh jonno@beelink1.local "reg query \"HKLM\SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}\0000\" /v PP_SclkDeepSleepDisable"
# Expect: 0x1

# 4. TdrDelay NOT set (should error — absence is correct)
ssh jonno@beelink1.local "reg query \"HKLM\SYSTEM\CurrentControlSet\Control\GraphicsDrivers\" /v TdrDelay"
# Expect: ERROR (key absent)

# 5. Driver version
ssh jonno@beelink1.local "powershell -Command \"Get-WmiObject Win32_VideoController | Select-Object DriverVersion, DriverDate | Format-List\""

# 6. No recent unexpected shutdowns (Event ID 41)
ssh jonno@beelink1.local "powershell -Command \"Get-WinEvent -LogName System -MaxEvents 100 | Where-Object { \$_.Id -eq 41 -or \$_.Id -eq 1001 } | Select-Object TimeCreated, Id, Message | Format-List\""
```

---

#### 2026-07-01/02 — fTPM storm regression (5th), worst on record (~22-min cycle)

- **Trigger:** the 2026-06-11 golden state regressed again — fTPM back on. No CMOS reset was consciously done this time, which reinforces the weak-CMOS-battery theory (settings not surviving power events on their own).
- **Evidence (live event-log pull, SSH):** 20× `0x00000133` / `param2=0x1e00` bugchecks in 24h — 07-01 03:57, 04:19, 07:12, 07:34, 09:41, 13:47, 16:39, 17:01, 19:39, then a tight run every ~22 min: 21:46, 22:08, 22:30, 22:53, 23:15, 23:37, 23:59, 00:21, 00:43, 01:06, 01:28. Kernel-Power 41 + 6008 on each; minidumps `070126-*.dmp` / `070226-*.dmp` in `C:\Windows\Minidump`.
- **Impact:** uptime capped at ~22–40 min → node unusable. Everything else observed this session was fallout: models vanish on every reboot (agent + llama-server die), `deploy-node beelink1` hangs (deploying mid-crash), a TLS-fingerprint crash-loop after reboots (`MESH_TLS_FINGERPRINT` unset on some restarts), and repeated coordinator read-timeout closes as heartbeats stalled toward each crash. pi1/coordinator/lighting unaffected.
- **Action:** reapply the golden state (TPM level = Disabled). This is the **4th BIOS-setting regression** (05-28, 06-02, 06-04, 07-01) — **replace the CMOS battery** is now the leading durable fix; otherwise execute the standing plan to move beelink to Linux / Win IoT LTSC to eliminate the fTPM/PSP class entirely. Until then, route models to pi1 and treat beelink as offline.

---

#### Known-bad driver versions

| Version | Date | Issue |
|---------|------|-------|
| pre-26.5.2 | pre-May 2026 | `0xe06d7363` SEH exception under Vulkan load — GPU inference unusable |
| `32.0.31007.5012` | 2026-05-12 | Appeared to cause DPC_WATCHDOG — actually fTPM. Fine to use with fTPM disabled. |
| `32.0.21025.10016` | 2025-08-25 | Appeared to cause DPC_WATCHDOG — actually fTPM. Fine to use with fTPM disabled. |

#### DO NOTs

- **Never leave fTPM enabled.** AMD PSP holds a SPI bus lock during flash writes, stalling all CPU cores and triggering `0x00000133` at idle. Disable in BIOS: `Advanced → AMD CBS → SOC Misc Control → Trusted Platform Modules` → set **Trusted Platform Module = Disabled AND Pluton Security Processor = Disabled** — disabling Pluton alone is NOT enough; fTPM keeps running via the TPM level (confirmed 2026-06-25: 7+ h crash-free only after the TPM level itself was disabled). Do not use dTPM Level 3 (boot loop). **Reset by a CMOS clear** — re-disable after any CMOS reset; verify with `Get-Tpm` (TpmPresent=False).
- **Never set `TdrDelay` > 2s.** Caused GPU to overheat in May 2026 — let Windows reset a stuck GPU quickly rather than waiting. Documented in `install-node-windows.ps1`.
- **Never enable ULPS** (`EnableUlps=1`) — belt-and-braces, leave disabled via boot task.

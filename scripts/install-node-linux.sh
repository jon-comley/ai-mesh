#!/usr/bin/env bash
# Install or re-install the ai-mesh-agent systemd service on a Linux node.
# Assumes agent binary is already uploaded to ~/agent on the remote machine.
# Run via SSH: ssh user@host "sudo bash /tmp/install-node.sh <role> <user> [mqtt_host] [mqtt_port] [node_features] [voice_device_host] [voice_stt_remote] [voice_tts_base_url] [audio_backends] [audio_alsa_device] [art_matte_percent] [art_frame_thickness] [spotify_device_name]"
# The agent finds the coordinator via mDNS discovery — no coordinator IP is baked in.
# (Set COORDINATOR_IP in the agent's environment to override discovery for debugging.)
set -e

ROLE="${1:-compute}"
AGENT_USER="$2"
MQTT_HOST="${3:-}"
MQTT_PORT="${4:-1883}"
NODE_FEATURES="${5:-llm}"
VOICE_DEVICE_HOST="${6:-}"
VOICE_STT_REMOTE="${7:-}"
VOICE_TTS_BASE_URL="${8:-}"
AUDIO_BACKENDS="${9:-}"
AUDIO_ALSA_DEVICE="${10:-}"
ART_MATTE_PERCENT="${11:-}"
ART_FRAME_THICKNESS="${12:-}"
SPOTIFY_DEVICE_NAME="${13:-}"

if [ -z "$AGENT_USER" ]; then
    echo "Usage: $0 <role> <user> [mqtt_host] [mqtt_port] [node_features]"
    exit 1
fi

has_feature() {
    case ",${NODE_FEATURES}," in
        *",$1,"*) return 0 ;;
        *) return 1 ;;
    esac
}

# Select the best model based on available GPU VRAM (AMD/Nvidia) or system RAM.
detect_default_model() {
    local mem_mb=0 gpu=0

    # AMD GPU via ROCm sysfs
    for f in /sys/class/drm/card*/device/mem_info_vram_total; do
        [ -f "$f" ] || continue
        local v
        v=$(( $(cat "$f") / 1048576 ))
        [ "$v" -gt "$mem_mb" ] && mem_mb="$v" && gpu=1
    done

    # Nvidia GPU
    if [ "$gpu" -eq 0 ] && command -v nvidia-smi &>/dev/null; then
        local v
        v=$(nvidia-smi --query-gpu=memory.total --format=csv,noheader,nounits 2>/dev/null \
            | head -1 | tr -d ' ')
        [ -n "$v" ] && [ "$v" -gt 0 ] && mem_mb="$v" && gpu=1
    fi

    # Fall back to system RAM
    if [ "$gpu" -eq 0 ]; then
        mem_mb=$(awk '/MemTotal/{print int($2/1024)}' /proc/meminfo)
    fi

    if [ "$gpu" -eq 1 ]; then
        if   [ "$mem_mb" -ge 22000 ]; then echo "qwen2.5:32b"
        elif [ "$mem_mb" -ge 9000  ]; then echo "qwen2.5:14b"
        elif [ "$mem_mb" -ge 4000  ]; then echo "qwen2.5:7b"
        elif [ "$mem_mb" -ge 1000  ]; then echo "qwen2.5:1.5b"
        else                               echo "qwen2.5:0.5b"
        fi
    else
        if   [ "$mem_mb" -ge 44000 ]; then echo "qwen2.5:32b"
        elif [ "$mem_mb" -ge 18000 ]; then echo "qwen2.5:14b"
        elif [ "$mem_mb" -ge 10000 ]; then echo "qwen2.5:7b"
        elif [ "$mem_mb" -ge 3000  ]; then echo "qwen2.5:1.5b"
        else                               echo "qwen2.5:0.5b"
        fi
    fi
}

echo ">>> Installing system dependencies..."
apt-get install -y -q git curl

if has_feature art; then
    # fbi against the raw framebuffer, not feh/pqiv/mpv: a Lite install has
    # no X server (feh/pqiv can't start), and while mpv --vo=drm does work,
    # Debian's mpv package drags in a full GTK/X11/audio stack as unused
    # linked dependencies (~600 MB, 265 packages — confirmed on the actual
    # first node) for a single-purpose kiosk display. fbi needs only a
    # handful of small deps.
    echo ">>> Installing fbi (art-display fullscreen viewer, framebuffer)..."
    apt-get install -y -q --no-install-recommends fbi

    # This SoC's default full-KMS driver (vc4-kms-v3d) exposes no /dev/fb0
    # at all, which fbi needs — confirmed on the actual first node. The
    # legacy "fake KMS" overlay is still hardware-accelerated and does
    # expose it. hdmi_force_hotplug=1 makes the Pi assume a display is
    # present at boot even before the TV is wired up/powered on, which is
    # also required for /dev/fb0 to appear at all.
    CONFIG_TXT="/boot/firmware/config.txt"
    CONFIG_CHANGED=0
    if [ -f "$CONFIG_TXT" ]; then
        # Tolerate leading whitespace and trailing overlay params (e.g.
        # ",cma-128") rather than anchoring to the exact line seen on the
        # image this was first tested against — a differently-formatted
        # future OS image would otherwise silently skip this fix, leaving
        # fbi non-functional with no error at all.
        if grep -qE '^[[:space:]]*dtoverlay=vc4-kms-v3d(,|$)' "$CONFIG_TXT"; then
            echo ">>> Switching ${CONFIG_TXT} to vc4-fkms-v3d (needed for /dev/fb0)..."
            sed -i -E 's/^([[:space:]]*)dtoverlay=vc4-kms-v3d/\1dtoverlay=vc4-fkms-v3d/' "$CONFIG_TXT"
            CONFIG_CHANGED=1
        fi
        if ! grep -qE '^[[:space:]]*hdmi_force_hotplug=1([[:space:]]|$)' "$CONFIG_TXT"; then
            echo ">>> Adding hdmi_force_hotplug=1 to ${CONFIG_TXT}..."
            sed -i '/^\[all\]/a hdmi_force_hotplug=1' "$CONFIG_TXT"
            CONFIG_CHANGED=1
        fi
        if [ "$CONFIG_CHANGED" = "1" ]; then
            echo ""
            echo ">>> ################################################################"
            echo ">>> #  DISPLAY CONFIG CHANGED — REBOOT NEEDED before /dev/fb0       #"
            echo ">>> #  exists. Run 'sudo reboot' once, manually, before testing      #"
            echo ">>> #  the art display.                                             #"
            echo ">>> ################################################################"
            echo ""
        fi
    fi
fi

DEFAULT_MODEL=""
if has_feature llm; then
    DEFAULT_MODEL="$(detect_default_model)"
    echo ">>> Detected hardware → default model: ${DEFAULT_MODEL}"
    echo ">>> To load it after provisioning: just auto-load-model <node-name>"

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
    llama_url_for() {
        if [ "$ARCH" = "x86_64" ]; then
            echo "https://github.com/ggml-org/llama.cpp/releases/download/$1/llama-$1-bin-ubuntu-x64.tar.gz"
        else
            echo "https://github.com/ggml-org/llama.cpp/releases/download/$1/llama-$1-bin-ubuntu-arm64.tar.gz"
        fi
    }
    LLAMA_URL="$(llama_url_for "$LLAMA_VERSION")"
    LLAMA_TMP="$(mktemp -d)"
    if ! curl -fsSL "$LLAMA_URL" -o "$LLAMA_TMP/llama.tar.gz"; then
        # "latest" is a tag, published as soon as it's created — its release-asset
        # upload is a separate, sometimes-lagging CI step. Seen live 2026-07-04:
        # b9871 sat at zero uploaded assets for 20+ minutes. Fall back to the
        # previous release by querying the releases list (not a hardcoded version
        # — llama.cpp cuts a new tag roughly daily, so a pinned fallback would be
        # stale within days) rather than hard-failing the whole node install.
        echo ">>> Warning: ${LLAMA_VERSION} assets aren't uploaded yet — trying the previous release..."
        PREV_VERSION="$(curl -fsSL --connect-timeout 5 \
                "https://api.github.com/repos/ggml-org/llama.cpp/releases?per_page=2" \
                | grep '"tag_name"' | sed -n '2p' | cut -d'"' -f4)"
        if [ -z "$PREV_VERSION" ]; then
            echo ">>> ERROR: llama.cpp download failed and no previous release could be resolved."
            exit 1
        fi
        LLAMA_VERSION="$PREV_VERSION"
        LLAMA_URL="$(llama_url_for "$LLAMA_VERSION")"
        echo ">>> Falling back to llama.cpp release: ${LLAMA_VERSION}"
        curl -fsSL "$LLAMA_URL" -o "$LLAMA_TMP/llama.tar.gz"
    fi
    # Extract everything — llama-server depends on several .so files in the same archive.
    install -d /opt/llama.cpp
    tar -xzf "$LLAMA_TMP/llama.tar.gz" -C /opt/llama.cpp --strip-components=1
    rm -rf "$LLAMA_TMP"
    echo ">>> llama-server ${LLAMA_VERSION} installed at /opt/llama.cpp/llama-server"
    # Models are downloaded on first ModelLoad — no pre-cache step needed.
else
    echo ">>> No 'llm' feature requested — skipping llama.cpp install."
fi

VOICE_MODEL_FILE="ggml-base.en.bin"
if has_feature voice; then
    echo ">>> Installing whisper-server (whisper.cpp latest release)..."
    if ! WHISPER_VERSION="$(curl -fsSL --connect-timeout 5 \
            https://api.github.com/repos/ggml-org/whisper.cpp/releases/latest \
            | grep '"tag_name"' | head -1 | cut -d'"' -f4)" \
       || [ -z "$WHISPER_VERSION" ]; then
        echo ">>> Warning: GitHub API unavailable. Falling back to v1.9.1."
        WHISPER_VERSION="v1.9.1"
    fi
    echo ">>> whisper.cpp release: ${WHISPER_VERSION}"
    ARCH="$(uname -m)"
    whisper_url_for() {
        if [ "$ARCH" = "x86_64" ]; then
            echo "https://github.com/ggml-org/whisper.cpp/releases/download/$1/whisper-bin-ubuntu-x64.tar.gz"
        else
            echo "https://github.com/ggml-org/whisper.cpp/releases/download/$1/whisper-bin-ubuntu-arm64.tar.gz"
        fi
    }
    WHISPER_URL="$(whisper_url_for "$WHISPER_VERSION")"
    WHISPER_TMP="$(mktemp -d)"
    if ! curl -fsSL "$WHISPER_URL" -o "$WHISPER_TMP/whisper.tar.gz"; then
        # Same "latest tag published before its assets finish uploading" gap as
        # llama.cpp above — fall back to the previous release rather than hard-fail.
        echo ">>> Warning: ${WHISPER_VERSION} assets aren't uploaded yet — trying the previous release..."
        PREV_VERSION="$(curl -fsSL --connect-timeout 5 \
                "https://api.github.com/repos/ggml-org/whisper.cpp/releases?per_page=2" \
                | grep '"tag_name"' | sed -n '2p' | cut -d'"' -f4)"
        if [ -z "$PREV_VERSION" ]; then
            echo ">>> ERROR: whisper.cpp download failed and no previous release could be resolved."
            exit 1
        fi
        WHISPER_VERSION="$PREV_VERSION"
        WHISPER_URL="$(whisper_url_for "$WHISPER_VERSION")"
        echo ">>> Falling back to whisper.cpp release: ${WHISPER_VERSION}"
        curl -fsSL "$WHISPER_URL" -o "$WHISPER_TMP/whisper.tar.gz"
    fi
    # Extract everything — whisper-server depends on several .so files in the same archive.
    install -d /opt/whisper.cpp
    tar -xzf "$WHISPER_TMP/whisper.tar.gz" -C /opt/whisper.cpp --strip-components=1
    rm -rf "$WHISPER_TMP"
    echo ">>> whisper-server ${WHISPER_VERSION} installed at /opt/whisper.cpp/whisper-server"

    VOICE_MODEL_DIR="/home/${AGENT_USER}/.ai-mesh/voice-models"
    VOICE_MODEL_PATH="${VOICE_MODEL_DIR}/${VOICE_MODEL_FILE}"
    if [ -f "$VOICE_MODEL_PATH" ]; then
        echo ">>> whisper model already present at ${VOICE_MODEL_PATH} — skipping download"
    else
        echo ">>> Downloading whisper model ${VOICE_MODEL_FILE}..."
        sudo -u "${AGENT_USER}" install -d "$VOICE_MODEL_DIR"
        sudo -u "${AGENT_USER}" curl -fsSL \
            "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/${VOICE_MODEL_FILE}" \
            -o "$VOICE_MODEL_PATH"
        echo ">>> whisper model installed at ${VOICE_MODEL_PATH}"
    fi

    # Piper TTS. Unlike whisper.cpp/llama.cpp, Piper's actively-maintained
    # fork (OHF-Voice/piper1-gpl) ships only as a pip package — no
    # standalone binary — so this installs into a venv rather than
    # downloading a release tarball. Still just a subprocess we talk HTTP
    # to (see capabilities/voice/src/tts.rs), so its GPL-3.0 license
    # doesn't reach ai-mesh's own code.
    echo ">>> Installing Piper TTS (python venv)..."
    apt-get install -y -q python3-venv
    PIPER_DIR="/opt/piper"
    if [ ! -x "${PIPER_DIR}/bin/python3" ]; then
        install -d "$PIPER_DIR"
        python3 -m venv "$PIPER_DIR"
        "${PIPER_DIR}/bin/pip" install --quiet --upgrade pip
        # flask: piper.http_server imports it directly but piper-tts
        # doesn't declare it as a dependency — confirmed live 2026-07-09
        # (ModuleNotFoundError on first real deploy without it).
        "${PIPER_DIR}/bin/pip" install --quiet piper-tts flask
        echo ">>> Piper TTS installed at ${PIPER_DIR}"
    else
        echo ">>> Piper TTS venv already present at ${PIPER_DIR} — skipping"
    fi

    # 5 voices, always warm (see tts.rs) so switching between them from the
    # dashboard is instant — no reload delay. Licenses verified per-voice
    # against each MODEL_CARD on huggingface.co/rhasspy/piper-voices; see
    # plans/audio-output-integration.md for the full table.
    TTS_MODEL_DIR="/home/${AGENT_USER}/.ai-mesh/tts-models"
    sudo -u "${AGENT_USER}" install -d "$TTS_MODEL_DIR"
    for TTS_VOICE in en_US-joe-medium en_US-kristin-medium en_US-ljspeech-medium en_GB-alan-medium en_GB-alba-medium; do
        TTS_LANG="$(echo "$TTS_VOICE" | cut -d- -f1)"
        TTS_NAME="$(echo "$TTS_VOICE" | cut -d- -f2)"
        TTS_BASE_URL="https://huggingface.co/rhasspy/piper-voices/resolve/main/${TTS_LANG%_*}/${TTS_LANG}/${TTS_NAME}/medium/${TTS_VOICE}"
        if [ -f "${TTS_MODEL_DIR}/${TTS_VOICE}.onnx" ]; then
            echo ">>> Piper voice ${TTS_VOICE} already present — skipping"
            continue
        fi
        echo ">>> Downloading Piper voice ${TTS_VOICE}..."
        sudo -u "${AGENT_USER}" curl -fsSL "${TTS_BASE_URL}.onnx" -o "${TTS_MODEL_DIR}/${TTS_VOICE}.onnx"
        sudo -u "${AGENT_USER}" curl -fsSL "${TTS_BASE_URL}.onnx.json" -o "${TTS_MODEL_DIR}/${TTS_VOICE}.onnx.json"
        echo ">>> Piper voice ${TTS_VOICE} installed"
    done
else
    echo ">>> No 'voice' feature requested — skipping whisper.cpp/Piper install."
fi

LLM_ENV_BLOCK=""
if has_feature llm; then
    LLM_ENV_BLOCK="Environment=LLAMA_MODEL_DIR=/home/${AGENT_USER}/.ai-mesh/models
Environment=LLAMA_SERVER_BIN=/opt/llama.cpp/llama-server
Environment=LD_LIBRARY_PATH=/opt/llama.cpp
Environment=LLAMA_GPU_LAYERS=0
Environment=LLAMA_CTX_SIZE=4096
Environment=DEFAULT_MODEL=${DEFAULT_MODEL}"
fi

VOICE_ENV_BLOCK=""
if has_feature voice; then
    # No LD_LIBRARY_PATH here deliberately: llama.cpp and whisper.cpp each
    # ship a same-named, binary-incompatible ggml backend plugin
    # (libggml-cpu.so), so merging both dirs into one process-wide
    # LD_LIBRARY_PATH risks one server loading the other's plugin (broke
    # llama-server live, 2026-07-08: "no CPU backend found"). capability-voice
    # sets LD_LIBRARY_PATH=/opt/whisper.cpp itself when it spawns
    # whisper-server, scoped to that child process only.
    VOICE_ENV_BLOCK="Environment=VOICE_STT_SERVER_BIN=/opt/whisper.cpp/whisper-server
Environment=VOICE_STT_MODEL=/home/${AGENT_USER}/.ai-mesh/voice-models/${VOICE_MODEL_FILE}
Environment=VOICE_TTS_VENV=/opt/piper
Environment=VOICE_TTS_MODEL_DIR=/home/${AGENT_USER}/.ai-mesh/tts-models"
    if [ -n "$VOICE_STT_REMOTE" ]; then
        VOICE_ENV_BLOCK="${VOICE_ENV_BLOCK}
Environment=VOICE_STT_REMOTE=${VOICE_STT_REMOTE}"
    fi
    if [ -n "$VOICE_TTS_BASE_URL" ]; then
        # Must be pi1's real LAN address, never 127.0.0.1 — this URL is
        # handed to the ESPHome device (a different physical machine) to
        # fetch a TTS clip from. See the comment on tts_media_base_url()
        # in capabilities/voice/src/tts.rs for how this was confirmed
        # live (silent failure: the device just couldn't reach loopback).
        VOICE_ENV_BLOCK="${VOICE_ENV_BLOCK}
Environment=VOICE_TTS_BASE_URL=${VOICE_TTS_BASE_URL}"
    fi
    if [ -n "$VOICE_DEVICE_HOST" ]; then
        VOICE_ENV_BLOCK="${VOICE_ENV_BLOCK}
Environment=VOICE_DEVICE_HOST=${VOICE_DEVICE_HOST}"
    else
        echo ">>> Warning: 'voice' feature requested but no voice_device_host given — capability will run as a stub."
    fi
fi

AUDIO_ENV_BLOCK=""
if has_feature audio; then
    if [ -n "$AUDIO_BACKENDS" ]; then
        AUDIO_ENV_BLOCK="Environment=AUDIO_BACKENDS=${AUDIO_BACKENDS}"
    else
        echo ">>> Warning: 'audio' feature requested but no audio_backends given — defaulting to bluetooth (capability-audio's own default)."
    fi
    if [ -n "$AUDIO_ALSA_DEVICE" ]; then
        AUDIO_ENV_BLOCK="${AUDIO_ENV_BLOCK}
Environment=AUDIO_ALSA_DEVICE=${AUDIO_ALSA_DEVICE}"
    fi
    # The agent runs as a system service, which gets no XDG_RUNTIME_DIR —
    # without it, pactl/paplay can't find the user-session PipeWire socket
    # and Bluetooth sink resolution/playback fail with "connection refused".
    AGENT_UID="$(id -u "${AGENT_USER}")"
    AUDIO_ENV_BLOCK="${AUDIO_ENV_BLOCK}
Environment=XDG_RUNTIME_DIR=/run/user/${AGENT_UID}"

    # Bluetooth playback needs a full A2DP stack, none of which a Lite
    # image ships: PipeWire's BlueZ SPA plugin registers the A2DP endpoint
    # with bluetoothd (without it every connect fails
    # br-connection-profile-unavailable), pipewire-pulse + pulseaudio-utils
    # provide the pactl/paplay interface capability-audio shells out to,
    # and lingering keeps those user services alive with nobody logged in.
    case ",${AUDIO_BACKENDS}," in *,bluetooth,*)
        echo ">>> Installing PipeWire Bluetooth audio stack (A2DP endpoint for bluetoothd)..."
        apt-get install -y -q --no-install-recommends \
            pipewire pipewire-pulse wireplumber libspa-0.2-bluetooth pulseaudio-utils
        # A persisted rfkill soft-block (survives reboots via systemd-rfkill)
        # leaves the adapter off-blocked and every scan silently empty.
        # Unblocking is safe to repeat — never disconnects anything live.
        rfkill unblock bluetooth 2>/dev/null || true
        mkdir -p /etc/wireplumber/wireplumber.conf.d
        WPCONF=/etc/wireplumber/wireplumber.conf.d/50-bluez-headless.conf
        WPCONF_NEW="$(mktemp)"
        cat > "$WPCONF_NEW" <<'WPEOF'
# Headless node: no logind seat session ever becomes active, so the default
# seat-monitoring feature keeps WirePlumber's bluez monitor dormant and
# BlueZ never gets an A2DP endpoint (every connect then fails with
# br-connection-profile-unavailable).
wireplumber.profiles = {
  main = {
    monitor.bluez.seat-monitoring = disabled
  }
}
WPEOF
        # Restarting pipewire/wireplumber drops any live Bluetooth audio
        # connection (confirmed live 2026-07-11 — the Fishman Loudbox amp's
        # module wedges on disconnect and needs a mains power-cycle plus a
        # manual re-pair to recover). So only restart when something this
        # deploy actually changed: the config differs from what's already
        # there, or the services aren't running yet at all. A routine
        # redeploy with no audio-stack changes must never touch a live
        # connection.
        CONFIG_CHANGED=false
        if ! cmp -s "$WPCONF_NEW" "$WPCONF" 2>/dev/null; then
            cp "$WPCONF_NEW" "$WPCONF"
            CONFIG_CHANGED=true
        fi
        rm -f "$WPCONF_NEW"
        loginctl enable-linger "${AGENT_USER}"
        # enable --now starts each unit only if it isn't already running —
        # always safe, never restarts a live one.
        runuser -u "${AGENT_USER}" -- env XDG_RUNTIME_DIR="/run/user/${AGENT_UID}" \
            systemctl --user enable --now pipewire pipewire-pulse wireplumber 2>/dev/null || true
        SERVICES_ACTIVE=true
        runuser -u "${AGENT_USER}" -- env XDG_RUNTIME_DIR="/run/user/${AGENT_UID}" \
            systemctl --user is-active --quiet pipewire wireplumber 2>/dev/null || SERVICES_ACTIVE=false
        if [ "$CONFIG_CHANGED" = true ] || [ "$SERVICES_ACTIVE" = false ]; then
            echo ">>> Bluetooth audio config changed or services not running — restarting pipewire/wireplumber."
            runuser -u "${AGENT_USER}" -- env XDG_RUNTIME_DIR="/run/user/${AGENT_UID}" \
                systemctl --user restart wireplumber pipewire pipewire-pulse 2>/dev/null || true
        else
            echo ">>> Bluetooth audio config unchanged and services already running — leaving them alone (a restart would drop any live connection)."
        fi
        ;;
    esac
fi

# ART_MATTE_PERCENT=0 (and ART_FRAME_THICKNESS=0) gives a true edge-to-edge
# fullscreen image instead of capability-art's default museum-mat border —
# see compose_matte() in capabilities/art/src/lib.rs.
ART_ENV_BLOCK=""
if has_feature art; then
    if [ -n "$ART_MATTE_PERCENT" ]; then
        ART_ENV_BLOCK="Environment=ART_MATTE_PERCENT=${ART_MATTE_PERCENT}"
    fi
    if [ -n "$ART_FRAME_THICKNESS" ]; then
        ART_ENV_BLOCK="${ART_ENV_BLOCK}
Environment=ART_FRAME_THICKNESS=${ART_FRAME_THICKNESS}"
    fi
fi

# Spotify secrets (SPOTIFY_CLIENT_ID/SECRET/REFRESH_TOKEN) are deliberately
# NOT here — they ship as a systemd drop-in via `just spotify-push-creds`,
# which survives installer re-runs (this script rewrites only the main unit
# file; daemon-reload merges drop-ins back in). Same pattern as
# MESH_AUTH_TOKEN in _push-node-env.
MUSIC_ENV_BLOCK=""
if has_feature music; then
    MUSIC_ENV_BLOCK="Environment=SPOTIFY_LIBRESPOT_BIN=/home/${AGENT_USER}/librespot"
    if [ -n "$SPOTIFY_DEVICE_NAME" ]; then
        MUSIC_ENV_BLOCK="${MUSIC_ENV_BLOCK}
Environment=SPOTIFY_DEVICE_NAME=${SPOTIFY_DEVICE_NAME}"
    fi
    # pacat needs the user-session PipeWire socket, same as the audio block
    # above; only add XDG_RUNTIME_DIR here if audio didn't already.
    if ! has_feature audio; then
        AGENT_UID="$(id -u "${AGENT_USER}")"
        MUSIC_ENV_BLOCK="${MUSIC_ENV_BLOCK}
Environment=XDG_RUNTIME_DIR=/run/user/${AGENT_UID}"
    fi
fi

echo ">>> Installing ai-mesh-agent systemd service..."
tee /etc/systemd/system/ai-mesh-agent.service > /dev/null <<EOF
[Unit]
Description=ai-mesh compute agent
After=network-online.target
Wants=network-online.target

[Service]
ExecStart=/home/${AGENT_USER}/agent
Environment=AGENT_ROLE=${ROLE}
${LLM_ENV_BLOCK}
${VOICE_ENV_BLOCK}
${AUDIO_ENV_BLOCK}
${ART_ENV_BLOCK}
${MUSIC_ENV_BLOCK}
$([ -n "${MQTT_HOST}" ] && echo "Environment=MQTT_HOST=${MQTT_HOST}" || true)
$([ -n "${MQTT_HOST}" ] && echo "Environment=MQTT_PORT=${MQTT_PORT}" || true)
Restart=always
RestartSec=5
TimeoutStopSec=15
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

# Allow the controller machine to drive this node over SSH without a password
# prompt: pushing TLS fingerprints (`just set-fingerprint`), and re-running this
# installer headlessly (`just deploy-node`, which calls `sudo /tmp/install-node.sh`
# — a target that can't be safely whitelisted by path since /tmp is world-writable).
# Solo home-lab node: full NOPASSWD for the owner account is the pragmatic choice.
echo "${AGENT_USER} ALL=(ALL) NOPASSWD: ALL" \
    > /etc/sudoers.d/ai-mesh-agent
chmod 440 /etc/sudoers.d/ai-mesh-agent
echo ">>> Passwordless sudo configured for ${AGENT_USER} (headless deploys)."

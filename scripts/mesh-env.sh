# Source the coordinator's state file (if present) and export mesh credentials.
# Used by justfile recipes via:  source scripts/mesh-env.sh
#
# Exports MESH_TLS_FINGERPRINT / MESH_AUTH_TOKEN / MESH_AUTH_TOKEN_NEXT when the
# coordinator has written them, and sets TOKEN for curl-based recipes. Absent
# state file is not an error here (dev mode) — recipes that *require* a running
# coordinator do their own existence check with a context-specific message.
STATE="$HOME/.config/ai-mesh/coordinator.state"
TOKEN=""
if [ -f "$STATE" ]; then
    # shellcheck disable=SC1090
    source "$STATE"
    export MESH_TLS_FINGERPRINT MESH_AUTH_TOKEN
    [ -n "${MESH_AUTH_TOKEN_NEXT:-}" ] && export MESH_AUTH_TOKEN_NEXT
    [ -n "${MESH_HTTP_PORT:-}" ] && export MESH_HTTP_PORT
    TOKEN="${MESH_AUTH_TOKEN:-}"
fi

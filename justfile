pi_host := "192.168.1.11"
pi_user := "jonno"
coordinator_ip := "192.168.1.12"
coordinator_port := "9000"

default: build

build:
    cargo build

test:
    cargo test

lint:
    cargo fmt --all
    cargo clippy --all-targets --all-features -- -D warnings

run-agent:
    cargo run -p agent

run-coordinator:
    cargo run -p coordinator

sanity:
    #!/usr/bin/env bash
    set -e
    echo "=== Starting coordinator ==="
    cargo run -p coordinator > /tmp/mesh-coordinator.log 2>&1 &
    COORD_PID=$!
    sleep 1

    echo "=== Starting controller agent ==="
    AGENT_ROLE=controller cargo run -p agent > /tmp/mesh-agent.log 2>&1 &
    AGENT_PID=$!
    sleep 1

    echo "=== Checking node list ==="
    cargo run -p cli -- nodes || true

    echo "=== Skipping Phase 6 message injection ==="
    echo "Note: ai-mesh uses a 4-byte little-endian length prefix for all wire messages."
    echo "Raw nc cannot be used. A framed test client will be added in Phase 6 proper."

    echo "=== Coordinator log tail ==="
    tail -n 20 /tmp/mesh-coordinator.log

    echo "=== Agent log tail ==="
    tail -n 20 /tmp/mesh-agent.log

    echo "=== Cleaning up ==="
    kill $COORD_PID $AGENT_PID 2>/dev/null || true
    echo "Sanity check complete."

build-pi:
    cargo build --release --target aarch64-unknown-linux-gnu -p agent

deploy-pi: build-pi
    @echo "=== Checking Ollama installation on target compute node ==="
    ssh -t {{pi_user}}@{{pi_host}} "command -v ollama >/dev/null 2>&1 || (echo 'Ollama missing! Installing natively now...' && curl -fsSL https://ollama.com/install.sh | sh)"

    @echo "=== Ensuring Ollama daemon service is active ==="
    ssh -t {{pi_user}}@{{pi_host}} "sudo systemctl daemon-reload && sudo systemctl enable --now ollama"

    @echo "=== Pre-caching model weights on target hardware ==="
    ssh {{pi_user}}@{{pi_host}} "ollama pull qwen2.5:1.5b"

    @echo "=== Shipping compiled agent binary to remote filesystem ==="
    scp target/aarch64-unknown-linux-gnu/release/agent {{pi_user}}@{{pi_host}}:/home/{{pi_user}}/agent

run-pi:
    ssh {{pi_user}}@{{pi_host}} "COORDINATOR_IP={{coordinator_ip}} AGENT_ROLE=compute /home/{{pi_user}}/agent"

run-controller:
    AGENT_ROLE=controller cargo run -p agent

reset:
    cargo run -p cli -- reset-registry

sanity-pi:
    @echo "=== Checking Multi-Node Mesh State ==="
    cargo run -p cli -- nodes
    @echo "=== Active Diagnostic Monitoring ==="
    cargo run -p cli -- watch

# Full cluster sanity test (coordinator + controller + Pi)
sanity-all:
    #!/usr/bin/env bash
    set -e

    # Kill any stale coordinator holding port 9000
    pkill -f "target/debug/coordinator" || true
    sleep 0.5

    # Start coordinator
    cargo run -p coordinator &
    COORD_PID=$!
    sleep 1

    # Reset registry now that coordinator is live
    just reset || true

    # Start controller agent
    AGENT_ROLE=controller cargo run -p agent &
    AGENT_PID=$!

    # Run CLI nodes
    cargo run -p cli -- nodes

    # Cleanup
    kill $COORD_PID $AGENT_PID || true

# Full cluster sanity test including Pi (coordinator + controller + Pi + nodes check)
sanity-full:
    #!/usr/bin/env bash
    set -e

    # Kill any stale coordinator holding port 9000
    pkill -f "target/debug/coordinator" || true
    sleep 0.5

    # Start coordinator
    cargo run -p coordinator &
    COORD_PID=$!
    sleep 1

    # Reset registry
    just reset || true

    # Start controller agent
    AGENT_ROLE=controller cargo run -p agent &
    AGENT_PID=$!
    sleep 1

    # Ensure SSH is ready then start Pi agent
    ssh {{pi_user}}@{{pi_host}} "echo Connected to Pi OK"
    ssh {{pi_user}}@{{pi_host}} "COORDINATOR_IP={{coordinator_ip}} AGENT_ROLE=compute /home/{{pi_user}}/agent" &
    sleep 3

    # Validate full cluster
    cargo run -p cli -- nodes

    # Cleanup
    kill $COORD_PID $AGENT_PID || true
    ssh {{pi_user}}@{{pi_host}} "pkill -f agent" || true

# Run an end-to-end live inference loop across the whole cluster automatically
test-inference:
    #!/usr/bin/env bash
    set -e

    cleanup() {
        echo "=== Cleaning up cluster ==="
        kill $COORD_PID $AGENT_PID 2>/dev/null || true
        ssh {{pi_user}}@{{pi_host}} "pkill -f agent" 2>/dev/null || true
    }
    trap cleanup EXIT

    echo "=== Step 1: Cleaning workspace and starting cluster fresh ==="
    pkill -f "target/debug/coordinator" || true
    pkill -f "target/debug/agent" || true
    sleep 0.5

    cargo run -p coordinator &
    COORD_PID=$!
    sleep 1

    just reset || true

    AGENT_ROLE=controller cargo run -p agent &
    AGENT_PID=$!
    sleep 1

    ssh {{pi_user}}@{{pi_host}} "echo Connected to Pi OK"
    ssh {{pi_user}}@{{pi_host}} "COORDINATOR_IP={{coordinator_ip}} AGENT_ROLE=compute /home/{{pi_user}}/agent" &

    # Give the cluster a moment to stabilize heartbeats and populate the registry
    sleep 3

    echo "=== Step 2: Fetching the dynamic Compute node ID ==="
    NODE_ID=$(cargo run -q -p cli -- nodes | grep -E "Compute" | head -n 1 | awk -F'|' '{print $2}' | xargs)

    if [ -z "$NODE_ID" ]; then
        echo "Error: Could not find an active Compute node in the registry!"
        exit 1
    fi
    echo "Found Compute node ID: ${NODE_ID}"

    echo "=== Step 3: Triggering model load on target node ==="
    cargo run -q -p cli -- load "${NODE_ID}" qwen2.5:1.5b 4200

    echo "=== Step 4: Waiting for model state transition to Ready ==="
    sleep 3

    echo "=== Step 5: Verifying model status in cluster table ==="
    cargo run -q -p cli -- nodes

    echo "=== Step 6: Dispatching load-balanced inference prompt ==="
    cargo run -p cli -- infer 'qwen2.5:1.5b' 'Context: The Itchen Bridge is a high-level bridge in Southampton, England. Why does it have a toll?'

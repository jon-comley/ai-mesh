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

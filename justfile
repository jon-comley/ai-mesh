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

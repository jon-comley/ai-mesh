use agent::agent::Agent;
use agent::config::AgentConfig;
use agent::dispatch::{build_capabilities, dispatch};
use agent::identity::detect_identity;
use agent::tls::make_connector;
use rustls::crypto::ring;
use rustls::pki_types::ServerName;
use shared::MeshMessage;
use shared::frame::{
    FrameReadError, FrameVerifyError, SignedFrame, derive_hmac_key, read_bounded_frame,
};
use shared::hardware::NodeRole;
use socket2::{SockRef, TcpKeepalive};
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::sync::{Notify, mpsc};
use tracing::{error, info, warn};

const RECONNECT_BASE_BACKOFF: tokio::time::Duration = tokio::time::Duration::from_secs(5);
const RECONNECT_MAX_BACKOFF: tokio::time::Duration = tokio::time::Duration::from_secs(60);

#[tokio::main]
async fn main() {
    ring::default_provider()
        .install_default()
        .expect("failed to install ring crypto provider");
    tracing_subscriber::fmt().init();

    let role = read_role_from_env();

    // Resolve node_id once — persisted to ~/.ai-mesh/node-id so it's stable
    // across reconnects. Capabilities are built once and survive reconnects via Arc.
    let node_id = detect_identity(role.clone())
        .map(|i| i.id)
        .unwrap_or_else(|_| {
            warn!("identity detection failed; using 'unknown' as node_id");
            "unknown".into()
        });

    let caps = build_capabilities(&node_id);
    // Built once and shared (via Arc) across every reconnect, same lifetime
    // as `caps` itself — see `DispatchTable`'s doc comment for why the
    // per-capability locks must survive reconnects.
    let dispatch_table = agent::dispatch::DispatchTable::new(caps.clone());
    // Floor for the reader's read timeout (see the reader loop). LLM nodes go
    // silent for a long stretch during a model load / inference (heartbeats stall
    // while llama-server is CPU-pegged), so their timeout must be generous enough
    // never to trip mid-load — the coordinator's own 15s close is their normal
    // recovery path anyway, and they're always-on (don't suspend). Non-LLM nodes
    // (e.g. a laptop controller that does suspend) get a tight floor for fast
    // suspend recovery, where the agent-side timeout is the only thing that fires.
    let read_floor_secs: u64 = if caps.iter().any(|c| c.name() == "llm") {
        1200
    } else {
        30
    };
    if caps.is_empty() {
        warn!("no capabilities loaded — agent will not handle inference or lighting commands");
    } else {
        info!(
            "capabilities: {}",
            caps.iter().map(|c| c.name()).collect::<Vec<_>>().join(", ")
        );
    }

    info!("Agent starting");

    // Backoff for connection-establishment failures (TCP connect exhausted,
    // TLS handshake, auth token write, or the startup burst) — flagged
    // repeatedly by third-party review as a flat 5s retry forever, which
    // hammers a genuinely-down coordinator at a constant rate indefinitely.
    // Doubles on each such failure, capped at RECONNECT_MAX_BACKOFF, and
    // resets to RECONNECT_BASE_BACKOFF the moment a connection actually
    // gets established (the startup burst succeeds) — a brief, otherwise-
    // healthy disconnect still recovers in 5s; only repeated failures to
    // even get connected slow down.
    let mut backoff = RECONNECT_BASE_BACKOFF;

    'reconnect: loop {
        // Re-resolved every reconnect, not once at startup: a transient mDNS
        // failure (e.g. Wi-Fi still settling right after boot, or radio
        // contention from another capability's Bluetooth activity) used to
        // wedge the agent onto the `127.0.0.1` fallback forever, since the
        // old resolved-once `addr` was reused for every retry — confirmed
        // live 2026-07-10 on pi2, requiring a manual service restart to
        // recover. Each iteration now gets its own chance to find the real
        // coordinator via mDNS again.
        let addr = resolve_coordinator_addr().await;
        info!("Connecting to coordinator at {}", addr);

        let connector = make_connector();
        let server_name = ServerName::try_from("ai-mesh-coordinator")
            .expect("invalid server name")
            .to_owned();

        // Bounded: a resolved address that's simply wrong (e.g. the
        // `127.0.0.1` fallback, or an mDNS-found IP that's since gone stale)
        // must not be retried forever — that's exactly the wedge above,
        // just moved one level down. After a few failures, fall back out to
        // `'reconnect` so the next iteration re-resolves instead of
        // hammering a dead address indefinitely. Kept at 3 (not higher):
        // each failure sleeps 5s, so this is also the worst-case delay
        // before a node with a bad resolved address re-attempts mDNS —
        // higher counts trade cold-boot/reconnect latency for very little
        // extra tolerance of transient failures.
        const MAX_CONNECT_ATTEMPTS: u32 = 3;
        let mut stream = None;
        for attempt in 1..=MAX_CONNECT_ATTEMPTS {
            match TcpStream::connect(&addr).await {
                Ok(s) => {
                    stream = Some(s);
                    break;
                }
                Err(e) => {
                    warn!(
                        "Failed to connect to {} (attempt {}/{}): {}. Retrying in 5s...",
                        addr, attempt, MAX_CONNECT_ATTEMPTS, e
                    );
                    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                }
            }
        }
        let Some(stream) = stream else {
            warn!(
                backoff_secs = backoff.as_secs(),
                "giving up on {} after {} attempts — re-resolving coordinator address",
                addr,
                MAX_CONNECT_ATTEMPTS
            );
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(RECONNECT_MAX_BACKOFF);
            continue 'reconnect;
        };

        // Enable TCP keepalive so NIC power-management or network idle timeouts
        // don't drop the connection during a long inference.
        {
            let sock = SockRef::from(&stream);
            let ka = TcpKeepalive::new()
                .with_time(std::time::Duration::from_secs(10))
                .with_interval(std::time::Duration::from_secs(5));
            if let Err(e) = sock.set_tcp_keepalive(&ka) {
                warn!("Failed to set TCP keepalive: {}", e);
            }
        }

        let tls_stream = match connector.connect(server_name.clone(), stream).await {
            Ok(s) => s,
            Err(e) => {
                warn!(
                    backoff_secs = backoff.as_secs(),
                    "TLS handshake failed: {}. Retrying in {:?}...", e, backoff
                );
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(RECONNECT_MAX_BACKOFF);
                continue;
            }
        };

        info!("Connected to coordinator (TLS)");

        let (mut reader, mut writer) = tokio::io::split(tls_stream);

        // Send AuthToken (unsigned) as the first frame if configured.
        // Derive the per-connection HMAC key from the same token for all subsequent frames.
        let hmac_key: Option<[u8; 32]> = if let Ok(token) = std::env::var("MESH_AUTH_TOKEN") {
            let token = token.trim().to_string();
            if !token.is_empty() {
                let data = serde_json::to_vec(&MeshMessage::AuthToken(token.clone())).unwrap();
                let len = (data.len() as u32).to_le_bytes();
                if writer.write_all(&len).await.is_err() || writer.write_all(&data).await.is_err() {
                    warn!(
                        backoff_secs = backoff.as_secs(),
                        "Failed to send AuthToken. Retrying in {:?}...", backoff
                    );
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(RECONNECT_MAX_BACKOFF);
                    continue;
                }
                Some(derive_hmac_key(&token))
            } else {
                None
            }
        } else {
            None
        };
        let (tx, mut rx) = mpsc::channel::<MeshMessage>(32);

        // Heartbeat loop.
        let config = AgentConfig {
            role: role.clone(),
            heartbeat_interval_secs: 5,
        };
        let agent = Agent::new_with_config(config, tx.clone());
        let interval_handle = agent.interval_handle();
        // Send the startup burst (heartbeat first) BEFORE the capabilities start,
        // so the coordinator's clear-on-first-heartbeat lands ahead of any
        // capability re-report. Otherwise the LLM capability re-reporting a loaded
        // model could race ahead of the clearing heartbeat and get wiped. The
        // heartbeat is enqueued on the FIFO channel here, before the caps below.
        match agent.start_once().await {
            Ok(true) => {
                // A connection is only "established" once the startup burst
                // actually reaches the coordinator — reset backoff here so a
                // brief, otherwise-healthy disconnect later still recovers
                // in RECONNECT_BASE_BACKOFF rather than inheriting whatever
                // this attempt's backoff had grown to.
                backoff = RECONNECT_BASE_BACKOFF;
            }
            Ok(false) => {
                // Channel closed — connection already gone; reconnect.
                warn!(
                    backoff_secs = backoff.as_secs(),
                    "connection dropped during startup burst. Retrying in {:?}...", backoff
                );
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(RECONNECT_MAX_BACKOFF);
                continue;
            }
            Err(e) => warn!("startup burst failed: {e}"),
        }
        tokio::spawn(async move {
            agent.run_periodic().await;
        });

        // Spawn start() for each capability. LLM's start re-reports any loaded
        // model; lighting's start runs the MQTT event loop. Both get the current
        // connection's tx.
        for cap in &caps {
            let cap = Arc::clone(cap);
            let tx = tx.clone();
            tokio::spawn(async move {
                if let Err(e) = cap.start(tx).await {
                    warn!("capability '{}' failed to start: {}", cap.name(), e);
                }
            });
        }

        // Reader task — routes inbound coordinator commands to capabilities.
        // SetHeartbeatInterval is handled here; everything else goes to dispatch().
        let tx_in = tx.clone();
        let dispatch_table_reader = Arc::clone(&dispatch_table);
        let reader_key = hmac_key;
        let reader_interval = interval_handle.clone();
        // Signalled when the read half closes or errors. This is the *reliable*
        // dead-connection signal: a vanished coordinator (e.g. after a WSL2
        // suspend/resume) shows up here as EOF, whereas the writer can keep
        // buffering small 5s heartbeats into a half-open socket for many minutes
        // without ever returning an error — so the read side must drive reconnect.
        let conn_dead = Arc::new(Notify::new());
        let reader_dead = conn_dead.clone();
        let reader_handle = tokio::spawn(async move {
            use std::sync::atomic::Ordering;
            loop {
                // Read timeout — a backup to the coordinator's own close. The
                // coordinator acks every heartbeat, so we should hear from it at
                // least once per interval; if we hear nothing for 3× that (floored
                // at `read_floor_secs` — tight for controllers, generous for LLM
                // nodes that go silent during loads), the connection is dead. This
                // catches the case the EOF signal can't: a long WSL2 suspend where,
                // on resume, neither EOF nor a write error surfaces on the half-open
                // socket. The coordinator's 15s close stays the normal path.
                let timeout_secs = reader_interval
                    .load(Ordering::Relaxed)
                    .saturating_mul(3)
                    .max(read_floor_secs);
                let buf = match tokio::time::timeout(
                    tokio::time::Duration::from_secs(timeout_secs),
                    read_bounded_frame(&mut reader),
                )
                .await
                {
                    Ok(Ok(buf)) => buf,
                    Ok(Err(FrameReadError::Closed)) => break,
                    Ok(Err(FrameReadError::TooLarge(n))) => {
                        warn!(
                            "dropping coordinator connection: frame length {n} exceeds MAX_FRAME_LEN"
                        );
                        break;
                    }
                    Err(_elapsed) => {
                        warn!(
                            timeout_secs,
                            "no frame from coordinator within read timeout — assuming dead, reconnecting"
                        );
                        break;
                    }
                };
                let msg: MeshMessage = if let Some(key) = &reader_key {
                    match serde_json::from_slice::<SignedFrame>(&buf) {
                        Ok(frame) => match frame.verify(key) {
                            Ok(payload) => match serde_json::from_slice(payload) {
                                Ok(m) => m,
                                Err(_) => continue,
                            },
                            Err(FrameVerifyError::Stale { ts, now, max_skew }) => {
                                // Length-prefixed framing means this frame's bytes are
                                // still fully consumed above — skipping it can't desync
                                // the stream. A stale timestamp is just a delayed
                                // delivery (e.g. under Wi-Fi/Bluetooth radio contention
                                // on the sending node), not evidence of a compromised
                                // connection, so don't tear down the whole session over
                                // one late frame — verified live 2026-07-10, where doing
                                // so killed the connection mid-Bluetooth-pairing.
                                warn!(
                                    ts,
                                    now,
                                    max_skew,
                                    "dropping stale inbound frame — keeping connection open"
                                );
                                continue;
                            }
                            Err(e) => {
                                warn!("dropping inbound frame: {}", e);
                                break;
                            }
                        },
                        Err(_) => continue,
                    }
                } else {
                    match serde_json::from_slice(&buf) {
                        Ok(m) => m,
                        Err(_) => continue,
                    }
                };

                match msg {
                    MeshMessage::SetHeartbeatInterval { secs } => {
                        info!(secs, "heartbeat interval updated");
                        reader_interval.store(secs, Ordering::Relaxed);
                    }
                    // Spawned, not awaited: a slow capability call (a
                    // Bluetooth scan/pair can run tens of seconds) must not
                    // stall this reader loop from processing the next
                    // inbound frame. See `dispatch`'s doc comment for why
                    // this is safe to do unconditionally.
                    other => {
                        tokio::spawn(dispatch(
                            other,
                            Arc::clone(&dispatch_table_reader),
                            tx_in.clone(),
                        ));
                    }
                }
            }
            // Read half closed/errored — wake the writer loop so we reconnect.
            reader_dead.notify_one();
        });

        // Writer loop — drains the outbound mpsc channel onto the TCP stream.
        // When HMAC is active, every outgoing message is wrapped in a SignedFrame.
        loop {
            let msg = tokio::select! {
                maybe = rx.recv() => match maybe {
                    Some(msg) => msg,
                    None => break, // all senders dropped
                },
                _ = conn_dead.notified() => {
                    warn!("Coordinator closed the connection (read side).");
                    break;
                }
            };
            let data = if let Some(key) = &hmac_key {
                let payload = serde_json::to_vec(&msg).unwrap();
                let frame = SignedFrame::sign(key, payload);
                serde_json::to_vec(&frame).unwrap()
            } else {
                serde_json::to_vec(&msg).unwrap()
            };
            let len = (data.len() as u32).to_le_bytes();

            if let Err(e) = writer.write_all(&len).await {
                warn!("Write error: {}", e);
                break;
            }
            if let Err(e) = writer.write_all(&data).await {
                warn!("Write error: {}", e);
                break;
            }
        }

        // Abort the reader so it doesn't linger on the dead read half while we
        // reconnect; the next iteration spawns a fresh reader on the new socket.
        reader_handle.abort();

        // Reached only after a connection was actually established (backoff
        // was reset to RECONNECT_BASE_BACKOFF above), so this is always a
        // quick fixed-delay retry — a live connection dropping is a
        // different, lower-risk case than repeated failure to connect at
        // all, and doesn't need to back off.
        warn!(
            "Disconnected from coordinator. Reconnecting in {:?}...",
            backoff
        );
        tokio::time::sleep(backoff).await;
    }
}

fn read_role_from_env() -> NodeRole {
    match std::env::var("AGENT_ROLE").as_deref() {
        Ok("controller") => NodeRole::Controller,
        _ => NodeRole::Compute,
    }
}

async fn resolve_coordinator_addr() -> String {
    let port = std::env::var("COORDINATOR_PORT").unwrap_or_else(|_| "9000".into());
    let port = port.trim().to_string();

    if let Ok(ip) = std::env::var("COORDINATOR_IP") {
        return format!("{}:{}", ip.trim(), port);
    }

    if let Some(addr) =
        agent::discovery::discover_coordinator(std::time::Duration::from_secs(5)).await
    {
        return addr;
    }

    // No node's deploy config (nodes/*.env, install-node-linux.sh) sets
    // COORDINATOR_IP today — every node, including pi1's own co-located
    // agent, reaches this fallback on any sustained mDNS failure. It only
    // ever resolves correctly for a node that happens to run on the same
    // machine as the coordinator (currently true only for pi1) — for every
    // other node this is a wrong address the agent will spin against
    // forever, once per reconnect attempt, with the agent never able to
    // reach the coordinator to surface the problem any other way. error!
    // (not warn!) so it stands out in `journalctl -p err` on a node that
    // mysteriously never shows up on the dashboard.
    error!(
        "mDNS: no coordinator found after 5s; falling back to 127.0.0.1:{port} — this only \
         works if this agent runs on the same machine as the coordinator. If this node is \
         remote, set COORDINATOR_IP explicitly (nodes/<node>.env) instead of relying on mDNS."
    );
    format!("127.0.0.1:{}", port)
}

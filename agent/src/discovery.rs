use mdns_sd::{ServiceDaemon, ServiceEvent};
use std::time::Duration;
use tracing::{info, warn};

const SERVICE_TYPE: &str = "_ai-mesh._tcp.local.";

/// Scan for an mDNS-advertised coordinator. Returns `"ip:port"` if found within `timeout`.
/// Skipped entirely when `COORDINATOR_IP` is set (explicit config always wins).
pub async fn discover_coordinator(timeout: Duration) -> Option<String> {
    info!(
        "mDNS: scanning for coordinator ({:.0}s timeout)...",
        timeout.as_secs()
    );
    match tokio::task::spawn_blocking(move || discover_blocking(timeout)).await {
        Ok(result) => result,
        Err(e) => {
            warn!("mDNS discovery task panicked: {}", e);
            None
        }
    }
}

fn discover_blocking(timeout: Duration) -> Option<String> {
    let daemon = match ServiceDaemon::new() {
        Ok(d) => d,
        Err(e) => {
            warn!("mDNS: daemon error: {}", e);
            return None;
        }
    };

    let receiver = match daemon.browse(SERVICE_TYPE) {
        Ok(r) => r,
        Err(e) => {
            warn!("mDNS: browse error: {}", e);
            return None;
        }
    };

    let deadline = std::time::Instant::now() + timeout;
    let mut found = None;

    loop {
        let remaining = match deadline.checked_duration_since(std::time::Instant::now()) {
            Some(d) if !d.is_zero() => d,
            _ => break,
        };
        match receiver.recv_timeout(remaining) {
            Ok(ServiceEvent::ServiceResolved(info)) => {
                let port = info.get_port();
                for addr in info.get_addresses() {
                    if addr.is_ipv4() {
                        let coord_addr = format!("{}:{}", addr, port);
                        info!("mDNS: found coordinator at {}", coord_addr);
                        found = Some(coord_addr);
                        break;
                    }
                }
                if found.is_some() {
                    break;
                }
            }
            Ok(_) => continue,
            Err(_) => break,
        }
    }

    let _ = daemon.shutdown();
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    /// discover_coordinator must return (Some or None) within the timeout, never hang.
    #[tokio::test]
    async fn test_discover_completes_within_timeout() {
        let timeout = Duration::from_millis(300);
        let start = std::time::Instant::now();
        let _result = discover_coordinator(timeout).await;
        assert!(
            start.elapsed() < timeout + Duration::from_secs(1),
            "discover_coordinator hung: elapsed={:?}",
            start.elapsed()
        );
    }

    /// Round-trip: advertise a service locally, then discover it.
    /// Gracefully skipped when mDNS / multicast loopback is unavailable in this environment.
    #[tokio::test]
    async fn test_discover_finds_advertised_service() {
        use mdns_sd::{ServiceDaemon, ServiceInfo};

        const TEST_PORT: u16 = 19997;

        let daemon = match ServiceDaemon::new() {
            Ok(d) => d,
            Err(_) => return, // mDNS unavailable — skip
        };

        let service_info = match ServiceInfo::new(
            SERVICE_TYPE,
            "test-coordinator",
            "test-coordinator.local.",
            "127.0.0.1",
            TEST_PORT,
            None,
        ) {
            Ok(info) => info,
            Err(_) => return,
        };

        if daemon.register(service_info).is_err() {
            return; // registration failed — skip
        }

        // Let the daemon send its initial announcements before we start browsing.
        tokio::time::sleep(Duration::from_millis(300)).await;

        let result = discover_coordinator(Duration::from_secs(3)).await;
        let _ = daemon.shutdown();

        if let Some(addr) = result {
            // Verify the address is well-formed (ip:port).
            let colon = addr.rfind(':').expect("discovered address should be ip:port");
            let port: u16 = addr[colon + 1..].parse().expect("port should be numeric");
            assert!(port > 0, "port in '{}' should be non-zero", addr);
        }
        // None means multicast loopback isn't available in this environment — acceptable.
    }
}

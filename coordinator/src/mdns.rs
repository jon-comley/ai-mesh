use mdns_sd::{ServiceDaemon, ServiceInfo};
use std::net::UdpSocket;
use tracing::{info, warn};

const SERVICE_TYPE: &str = "_ai-mesh._tcp.local.";
const INSTANCE_NAME: &str = "ai-mesh-coordinator";

fn advertised_ip() -> Option<String> {
    // MDNS_ADVERTISE_IP lets callers override auto-detection (e.g. WSL2 portproxy address).
    if let Ok(ip) = std::env::var("MDNS_ADVERTISE_IP") {
        let ip = ip.trim().to_string();
        if !ip.is_empty() {
            return Some(ip);
        }
    }
    // Probe the outbound interface by "connecting" a UDP socket (no packets sent).
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    match socket.local_addr().ok()?.ip() {
        std::net::IpAddr::V4(addr) => Some(addr.to_string()),
        _ => None,
    }
}

/// Advertise the coordinator on mDNS. Returns the daemon handle — drop it to stop advertising.
pub fn advertise(port: u16) -> Option<ServiceDaemon> {
    let ip = match advertised_ip() {
        Some(ip) => ip,
        None => {
            warn!("mDNS: could not determine advertise IP; skipping");
            return None;
        }
    };

    let daemon = match ServiceDaemon::new() {
        Ok(d) => d,
        Err(e) => {
            warn!("mDNS: failed to start daemon: {}", e);
            return None;
        }
    };

    let host_name = format!("{}.local.", INSTANCE_NAME);
    let service_info = match ServiceInfo::new(
        SERVICE_TYPE,
        INSTANCE_NAME,
        &host_name,
        ip.as_str(),
        port,
        None,
    ) {
        Ok(info) => info,
        Err(e) => {
            warn!("mDNS: invalid service info: {}", e);
            return None;
        }
    };

    match daemon.register(service_info) {
        Ok(_) => {
            info!("mDNS: advertising {} at {}:{}", SERVICE_TYPE, ip, port);
            Some(daemon)
        }
        Err(e) => {
            warn!("mDNS: register failed: {}", e);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Serialise tests that touch MDNS_ADVERTISE_IP to avoid parallel interference.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn test_advertised_ip_reads_env_override() {
        let _g = ENV_LOCK.lock().unwrap();
        // SAFETY: ENV_LOCK serialises all tests that touch this var.
        unsafe { std::env::set_var("MDNS_ADVERTISE_IP", "10.1.2.3") };
        let ip = advertised_ip();
        unsafe { std::env::remove_var("MDNS_ADVERTISE_IP") };
        assert_eq!(ip, Some("10.1.2.3".to_string()));
    }

    #[test]
    fn test_advertised_ip_trims_whitespace() {
        let _g = ENV_LOCK.lock().unwrap();
        // SAFETY: ENV_LOCK serialises all tests that touch this var.
        unsafe { std::env::set_var("MDNS_ADVERTISE_IP", "  192.168.1.12  ") };
        let ip = advertised_ip();
        unsafe { std::env::remove_var("MDNS_ADVERTISE_IP") };
        assert_eq!(ip, Some("192.168.1.12".to_string()));
    }

    #[test]
    fn test_advertised_ip_blank_env_falls_back_to_probe() {
        let _g = ENV_LOCK.lock().unwrap();
        // SAFETY: ENV_LOCK serialises all tests that touch this var.
        unsafe { std::env::set_var("MDNS_ADVERTISE_IP", "   ") };
        let ip = advertised_ip();
        unsafe { std::env::remove_var("MDNS_ADVERTISE_IP") };
        // Falls through to UDP probe; any networked machine returns Some,
        // offline/CI may return None — both are correct, just must not panic.
        let _ = ip;
    }

    #[test]
    fn test_advertise_does_not_panic() {
        // Smoke test: must return without panicking on any system.
        // Returns Some on systems where mDNS works, None gracefully otherwise.
        // Drop the daemon immediately — avoids leaving a stale mDNS record that
        // confuses the local controller agent's discovery.
        drop(advertise(19996));
    }
}

use shared::{NodeIdentity, NodeRole};
use std::fs;
use std::net::{IpAddr, UdpSocket};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum IdentityError {
    #[error("Failed to read hostname")]
    HostnameError,
    #[error("Failed to determine local IP address")]
    IpDetectionError,
}

pub fn detect_identity(role: NodeRole) -> Result<NodeIdentity, IdentityError> {
    let id = generate_node_id();
    let hostname = detect_hostname()?;
    let ip = detect_local_ip()?;

    Ok(NodeIdentity {
        id,
        hostname,
        ip,
        role,
    })
}

fn generate_node_id() -> String {
    Uuid::new_v4().to_string()
}

fn detect_hostname() -> Result<String, IdentityError> {
    fs::read_to_string("/etc/hostname")
        .map(|s| s.trim().to_string())
        .map_err(|_| IdentityError::HostnameError)
}

fn detect_local_ip() -> Result<String, IdentityError> {
    // Use a UDP socket trick to determine outbound IP
    let socket = UdpSocket::bind("0.0.0.0:0").map_err(|_| IdentityError::IpDetectionError)?;

    socket
        .connect("8.8.8.8:80")
        .map_err(|_| IdentityError::IpDetectionError)?;

    let local_addr = socket
        .local_addr()
        .map_err(|_| IdentityError::IpDetectionError)?;

    match local_addr.ip() {
        IpAddr::V4(ip) => Ok(ip.to_string()),
        IpAddr::V6(ip) => Ok(ip.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared::NodeRole;

    #[test]
    fn test_generate_node_id() {
        let id1 = generate_node_id();
        let id2 = generate_node_id();
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_detect_hostname() {
        let hostname = detect_hostname().unwrap();
        assert!(!hostname.is_empty());
    }

    #[test]
    fn test_detect_local_ip() {
        let ip = detect_local_ip().unwrap();
        assert!(!ip.is_empty());
        assert!(ip.contains('.')); // IPv4 expected in most cases
    }

    #[test]
    fn test_detect_identity() {
        let ident = detect_identity(NodeRole::Compute).unwrap();
        assert!(!ident.id.is_empty());
        assert!(!ident.hostname.is_empty());
        assert!(!ident.ip.is_empty());
    }
}

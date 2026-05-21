use shared::{NodeIdentity, NodeRole};
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
    let id_file = dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".ai-mesh")
        .join("node-id");

    if let Ok(contents) = std::fs::read_to_string(&id_file) {
        let trimmed = contents.trim();
        if Uuid::parse_str(trimmed).is_ok() {
            return trimmed.to_string();
        }
    }

    let id = Uuid::new_v4().to_string();
    if let Some(parent) = id_file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&id_file, &id);
    id
}

#[cfg(target_os = "windows")]
fn detect_hostname() -> Result<String, IdentityError> {
    std::env::var("COMPUTERNAME").map_err(|_| IdentityError::HostnameError)
}

#[cfg(not(target_os = "windows"))]
fn detect_hostname() -> Result<String, IdentityError> {
    std::fs::read_to_string("/etc/hostname")
        .map(|s| s.trim().to_string())
        .map_err(|_| IdentityError::HostnameError)
}

fn detect_local_ip() -> Result<String, IdentityError> {
    // UDP socket trick: connecting a UDP socket reveals the outbound IP without sending data.
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
        // ID is persisted — same machine always returns the same UUID.
        assert_eq!(id1, id2);
        assert!(Uuid::parse_str(&id1).is_ok());
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

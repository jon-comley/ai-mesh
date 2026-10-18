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
    let hostname = match configured_name(std::env::var("MESH_NODE_NAME").ok()) {
        Some(name) => name,
        None => detect_hostname()?,
    };
    let ip = detect_local_ip()?;

    Ok(NodeIdentity {
        id,
        hostname,
        ip,
        role,
    })
}

/// The node's name in the mesh, when one is configured. The deploy tooling
/// sets `MESH_NODE_NAME` to the node's `nodes/<name>.env` name, so the
/// dashboard shows the name a node is managed by rather than the computer's
/// own computer name (which Windows reports in capitals). Blank means unset.
fn configured_name(value: Option<String>) -> Option<String> {
    value
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
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
    // Linux keeps it in /etc/hostname; macOS has no such file, which made the
    // whole identity fail there and the node register as "unknown". The
    // `hostname` command works on both, so it's the fallback.
    if let Ok(s) = std::fs::read_to_string("/etc/hostname") {
        let s = s.trim();
        if !s.is_empty() {
            return Ok(s.to_string());
        }
    }
    let out = std::process::Command::new("hostname")
        .output()
        .map_err(|_| IdentityError::HostnameError)?;
    let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if out.status.success() && !name.is_empty() {
        Ok(name)
    } else {
        Err(IdentityError::HostnameError)
    }
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
    fn a_configured_name_wins_and_blank_means_unset() {
        assert_eq!(
            configured_name(Some("mac1".into())).as_deref(),
            Some("mac1")
        );
        assert_eq!(
            configured_name(Some(" beelink1\n".into())).as_deref(),
            Some("beelink1")
        );
        assert_eq!(configured_name(Some("  ".into())), None);
        assert_eq!(configured_name(None), None);
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

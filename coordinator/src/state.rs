use std::path::PathBuf;
use tracing::{info, warn};

pub fn state_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("ai-mesh")
        .join("coordinator.state")
}

/// Write a shell-sourceable state file with the active TLS fingerprint and
/// primary auth token. Recipes source this file instead of grepping the log.
///
/// File lives at `~/.config/ai-mesh/coordinator.state` (0600).
/// Format is KEY=VALUE so the justfile can `source` it directly.
pub fn write(fingerprint: &str, tokens: &[String]) {
    let path = state_path();

    if let Some(parent) = path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        warn!(error = %e, "failed to create ai-mesh config dir; skipping state file");
        return;
    }

    let mut content = format!("MESH_TLS_FINGERPRINT={}\n", fingerprint);
    if let Some(token) = tokens.first() {
        content.push_str(&format!("MESH_AUTH_TOKEN={}\n", token));
    }

    match std::fs::write(&path, &content) {
        Ok(_) => {
            // Restrict to owner-read/write only (best-effort on non-Unix).
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
            }
            info!("coordinator state written to {}", path.display());
        }
        Err(e) => warn!(error = %e, "failed to write coordinator state file"),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use tempfile::TempDir;

    fn write_to(dir: &TempDir, fingerprint: &str, tokens: &[String]) -> String {
        let path = dir.path().join("coordinator.state");
        let mut content = format!("MESH_TLS_FINGERPRINT={}\n", fingerprint);
        if let Some(token) = tokens.first() {
            content.push_str(&format!("MESH_AUTH_TOKEN={}\n", token));
        }
        fs::write(&path, &content).unwrap();
        fs::read_to_string(&path).unwrap()
    }

    #[test]
    fn state_contains_fingerprint() {
        let dir = TempDir::new().unwrap();
        let content = write_to(&dir, "AA:BB:CC:DD", &[]);
        assert!(content.contains("MESH_TLS_FINGERPRINT=AA:BB:CC:DD"));
    }

    #[test]
    fn state_contains_auth_token_when_set() {
        let dir = TempDir::new().unwrap();
        let content = write_to(&dir, "AA:BB", &["mysecret".into()]);
        assert!(content.contains("MESH_AUTH_TOKEN=mysecret"));
    }

    #[test]
    fn state_omits_auth_token_when_empty() {
        let dir = TempDir::new().unwrap();
        let content = write_to(&dir, "AA:BB", &[]);
        assert!(!content.contains("MESH_AUTH_TOKEN"));
    }

    #[test]
    fn state_uses_first_token_only() {
        let dir = TempDir::new().unwrap();
        let content = write_to(&dir, "AA:BB", &["primary".into(), "secondary".into()]);
        assert!(content.contains("MESH_AUTH_TOKEN=primary"));
        assert!(!content.contains("secondary"));
    }
}

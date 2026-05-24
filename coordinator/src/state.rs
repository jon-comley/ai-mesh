use std::path::PathBuf;
use tracing::{info, warn};

pub fn state_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("ai-mesh")
        .join("coordinator.state")
}

/// Write a shell-sourceable state file with the active TLS fingerprint,
/// primary auth token, and optionally the next token during a rotation window.
/// Recipes source this file instead of grepping the log.
///
/// File lives at `~/.config/ai-mesh/coordinator.state` (0600).
/// Format is KEY=VALUE so the justfile can `source` it directly.
/// Write is atomic: content goes to a `.tmp` file which is then renamed,
/// preventing shell scripts from reading a partially written file.
pub fn write(fingerprint: &str, tokens: &[String], next_token: Option<&str>) {
    let path = state_path();

    if let Some(parent) = path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        warn!(error = %e, "failed to create ai-mesh config dir; skipping state file");
        return;
    }

    match write_to_path(&path, fingerprint, tokens, next_token) {
        Ok(_) => info!("coordinator state written to {}", path.display()),
        Err(e) => warn!(error = %e, "failed to write coordinator state file"),
    }
}

/// Inner implementation: writes to a caller-supplied path via atomic rename.
/// Extracted so tests can exercise the real write logic without touching the
/// production `~/.config/ai-mesh/coordinator.state` path.
pub(crate) fn write_to_path(
    path: &std::path::Path,
    fingerprint: &str,
    tokens: &[String],
    next_token: Option<&str>,
) -> std::io::Result<()> {
    let mut content = format!("MESH_TLS_FINGERPRINT={}\n", fingerprint);
    if let Some(token) = tokens.first() {
        content.push_str(&format!("MESH_AUTH_TOKEN={}\n", token));
    }
    if let Some(next) = next_token {
        content.push_str(&format!("MESH_AUTH_TOKEN_NEXT={}\n", next));
    }

    let tmp_path = path.with_extension("tmp");
    std::fs::write(&tmp_path, &content)?;

    // Set permissions on the tmp file before rename so the final file
    // inherits 0o600 atomically (best-effort on non-Unix).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o600));
    }

    std::fs::rename(&tmp_path, path)
}

#[cfg(test)]
mod tests {
    use super::write_to_path;
    use std::fs;
    use tempfile::TempDir;

    fn run(
        dir: &TempDir,
        fingerprint: &str,
        tokens: &[String],
        next_token: Option<&str>,
    ) -> String {
        let path = dir.path().join("coordinator.state");
        write_to_path(&path, fingerprint, tokens, next_token).unwrap();
        fs::read_to_string(&path).unwrap()
    }

    #[test]
    fn state_contains_fingerprint() {
        let dir = TempDir::new().unwrap();
        let content = run(&dir, "AA:BB:CC:DD", &[], None);
        assert!(content.contains("MESH_TLS_FINGERPRINT=AA:BB:CC:DD"));
    }

    #[test]
    fn state_contains_auth_token_when_set() {
        let dir = TempDir::new().unwrap();
        let content = run(&dir, "AA:BB", &["mysecret".into()], None);
        assert!(content.contains("MESH_AUTH_TOKEN=mysecret"));
    }

    #[test]
    fn state_omits_auth_token_when_empty() {
        let dir = TempDir::new().unwrap();
        let content = run(&dir, "AA:BB", &[], None);
        assert!(!content.contains("MESH_AUTH_TOKEN"));
    }

    #[test]
    fn state_uses_first_token_only() {
        let dir = TempDir::new().unwrap();
        let content = run(&dir, "AA:BB", &["primary".into(), "secondary".into()], None);
        assert!(content.contains("MESH_AUTH_TOKEN=primary"));
        assert!(!content.contains("secondary"));
    }

    #[test]
    fn state_contains_next_token_when_set() {
        let dir = TempDir::new().unwrap();
        let content = run(&dir, "AA:BB", &["primary".into()], Some("nexttoken"));
        assert!(content.contains("MESH_AUTH_TOKEN=primary"));
        assert!(content.contains("MESH_AUTH_TOKEN_NEXT=nexttoken"));
    }

    #[test]
    fn state_omits_next_token_when_absent() {
        let dir = TempDir::new().unwrap();
        let content = run(&dir, "AA:BB", &["primary".into()], None);
        assert!(!content.contains("MESH_AUTH_TOKEN_NEXT"));
    }

    #[test]
    fn atomic_write_leaves_no_tmp_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("coordinator.state");
        write_to_path(&path, "FP", &["tok".into()], None).unwrap();
        assert!(path.exists(), "state file should exist");
        assert!(
            !path.with_extension("tmp").exists(),
            "tmp file should be cleaned up"
        );
    }

    #[test]
    fn second_write_overwrites_first() {
        let dir = TempDir::new().unwrap();
        run(&dir, "FP1", &["tok1".into()], None);
        let content = run(&dir, "FP2", &["tok2".into()], Some("tok3"));
        assert!(content.contains("FP2"));
        assert!(content.contains("tok2"));
        assert!(content.contains("tok3"));
        assert!(!content.contains("FP1"));
        assert!(!content.contains("tok1"));
    }
}

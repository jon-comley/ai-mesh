//! Running git, ssh and claude. Every call has a time limit, and a failure
//! comes back as the command's own words, which is what the page shows.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

pub type Result<T> = std::result::Result<T, String>;

/// Runs `cmd`, returning stdout, or stderr (then stdout) when it fails.
pub async fn output(mut cmd: Command, limit: Duration) -> Result<String> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let program = format!("{:?}", cmd.as_std().get_program());
    let child = cmd.spawn().map_err(|e| format!("{program}: {e}"))?;
    let out = tokio::time::timeout(limit, child.wait_with_output())
        .await
        .map_err(|_| format!("{program} took longer than {}s", limit.as_secs()))?
        .map_err(|e| format!("{program}: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if out.status.success() {
        Ok(stdout)
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let said = if stderr.trim().is_empty() {
            stdout.trim().to_string()
        } else {
            stderr.trim().to_string()
        };
        Err(format!("{program} failed: {said}"))
    }
}

/// `git` in `dir`, using `key` for GitHub.
pub fn git(dir: &Path, key: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.current_dir(dir)
        .env(
            "GIT_SSH_COMMAND",
            format!(
                "ssh -i {} -o IdentitiesOnly=yes -o BatchMode=yes -o StrictHostKeyChecking=accept-new",
                key.display()
            ),
        )
        .env("GIT_TERMINAL_PROMPT", "0");
    cmd
}

/// `script` on `host` over ssh with lenovo1's own key.
pub fn ssh(host: &str, script: &str) -> Command {
    let mut cmd = Command::new("ssh");
    cmd.args([
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=10",
        host,
        script,
    ]);
    cmd
}

pub const GIT_LIMIT: Duration = Duration::from_secs(120);
pub const SSH_LIMIT: Duration = Duration::from_secs(30);

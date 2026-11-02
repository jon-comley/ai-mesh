//! A cloud agent's repo: the page's own clone on lenovo1, kept equal to
//! GitHub's `main`, and the one write it makes (Jon's reply in the hub).

use std::path::{Path, PathBuf};

use agentwatch::state::Commit;
use chrono::{DateTime, Utc};

use super::run::{self, GIT_LIMIT, Result, git};

pub struct Repo {
    pub dir: PathBuf,
    pub key: PathBuf,
    pub url: String,
}

impl Repo {
    /// Clones on first use, then makes the clone equal to `origin/main`. The
    /// clone is the page's alone, so anything local is thrown away.
    pub async fn sync(&self) -> Result<()> {
        if !self.dir.join(".git").exists() {
            let parent = self.dir.parent().expect("repo dir has a parent");
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            let mut cmd = git(parent, &self.key);
            cmd.args(["clone", "--branch", "main", &self.url])
                .arg(&self.dir);
            run::output(cmd, GIT_LIMIT).await?;
            return Ok(());
        }
        self.git(&["fetch", "origin", "main"]).await?;
        self.git(&["reset", "--hard", "origin/main"]).await?;
        Ok(())
    }

    pub fn read(&self, path: &str) -> Option<String> {
        std::fs::read_to_string(self.dir.join(path)).ok()
    }

    /// The last commit that changed `path`.
    pub async fn last_change(&self, path: &str) -> Result<Option<(String, DateTime<Utc>)>> {
        let out = self
            .git(&["log", "-1", "--format=%H%x09%cI", "--", path])
            .await?;
        Ok(out
            .trim()
            .split_once('\t')
            .and_then(|(hash, at)| Some((hash.to_string(), parse_time(at)?))))
    }

    /// Commits after `hash` on `main`, oldest first.
    pub async fn commits_since(&self, hash: &str) -> Result<Vec<Commit>> {
        let range = format!("{hash}..HEAD");
        let out = self
            .git(&["log", "--reverse", "--format=%cI%x09%s", &range])
            .await?;
        Ok(out
            .lines()
            .filter_map(|l| {
                let (at, subject) = l.split_once('\t')?;
                Some(Commit {
                    at: parse_time(at)?,
                    subject: subject.to_string(),
                })
            })
            .collect())
    }

    /// Writes `contents` to `path`, commits as Jon and pushes to `main`.
    pub async fn commit_and_push(&self, path: &str, contents: &str, message: &str) -> Result<()> {
        std::fs::write(self.dir.join(path), contents).map_err(|e| e.to_string())?;
        self.git(&["add", "--", path]).await?;
        self.git(&[
            "-c",
            "user.name=jon",
            "-c",
            "user.email=joncomley@googlemail.com",
            "commit",
            "-m",
            message,
        ])
        .await?;
        if let Err(e) = self.git(&["push", "origin", "HEAD:main"]).await {
            // Leave the clone as GitHub has it, so the caller can retry from there.
            let _ = self.sync().await;
            return Err(e);
        }
        Ok(())
    }

    async fn git(&self, args: &[&str]) -> Result<String> {
        let mut cmd = git(&self.dir, &self.key);
        cmd.args(args);
        run::output(cmd, GIT_LIMIT).await
    }
}

fn parse_time(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s.trim())
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// Where an agent's clone lives under the page's directory.
pub fn clone_dir(base: &Path, agent: &str) -> PathBuf {
    base.join("repos")
        .join(agent.to_lowercase().replace(' ', "-"))
}

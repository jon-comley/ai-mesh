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
        } else {
            self.git(&["fetch", "origin", "main"]).await?;
            self.git(&["reset", "--hard", "origin/main"]).await?;
        }
        self.configure().await
    }

    /// So any git run in the clone (a chat's own commits and pushes included)
    /// uses this repo's key and commits as Jon. Set on every sync, not only a
    /// fresh clone: Desk's chat clone predated it and pushed with lenovo1's own
    /// key, which GitHub refuses (5 Nov).
    async fn configure(&self) -> Result<()> {
        let ssh = format!(
            "ssh -i {} -o IdentitiesOnly=yes -o BatchMode=yes -o StrictHostKeyChecking=accept-new",
            self.key.display()
        );
        self.git(&["config", "core.sshCommand", &ssh]).await?;
        self.git(&["config", "user.name", "jon"]).await?;
        self.git(&["config", "user.email", "joncomley@googlemail.com"])
            .await?;
        Ok(())
    }

    /// Brings a clone up to date without throwing away local commits: for
    /// clones a chat writes in. Once GitHub's history is rewritten a
    /// fast-forward can never succeed again: if every
    /// local commit is already on GitHub by content and nothing is uncommitted,
    /// the clone takes GitHub's. Desk's chat clone sat 27 commits apart from
    /// 2 to 7 Nov, reading a stale hub, because this only pulled.
    pub async fn sync_keep(&self) -> Result<()> {
        if !self.dir.join(".git").exists() {
            return self.sync().await;
        }
        self.configure().await?;
        self.git(&["fetch", "origin", "main"]).await?;
        if self
            .git(&["merge", "--ff-only", "origin/main"])
            .await
            .is_ok()
        {
            return Ok(());
        }
        let unpushed = self.git(&["cherry", "origin/main", "HEAD"]).await?;
        let dirty = self.git(&["status", "--porcelain"]).await?;
        if unpushed.lines().any(|l| l.starts_with('+')) || !dirty.trim().is_empty() {
            return Err(format!(
                "{} has split from GitHub's main with work of its own: sort it by hand",
                self.dir.display()
            ));
        }
        self.git(&["reset", "--hard", "origin/main"])
            .await
            .map(|_| ())
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

    /// Commits after `hash` on `main`, oldest first, with the files each changed.
    pub async fn commits_since(&self, hash: &str) -> Result<Vec<Commit>> {
        let range = format!("{hash}..HEAD");
        let out = self
            .git(&[
                "log",
                "--reverse",
                "--name-only",
                "--format=%x00%cI%x09%s",
                &range,
            ])
            .await?;
        Ok(out
            .split('\0')
            .filter_map(|block| {
                let mut lines = block.lines();
                let (at, subject) = lines.next()?.split_once('\t')?;
                Some(Commit {
                    at: parse_time(at)?,
                    subject: subject.to_string(),
                    files: lines
                        .map(str::trim)
                        .filter(|l| !l.is_empty())
                        .map(String::from)
                        .collect(),
                })
            })
            .collect())
    }

    /// Writes `contents` to `path`, commits as Jon and pushes to `main`.
    pub async fn commit_and_push(&self, path: &str, contents: &str, message: &str) -> Result<()> {
        self.commit_files(&[(path, contents)], message).await
    }

    /// Writes each `(path, contents)`, commits them as one commit by Jon and
    /// pushes to `main`. If the push is refused the clone goes back to GitHub's.
    pub async fn commit_files(&self, files: &[(&str, &str)], message: &str) -> Result<()> {
        for (path, contents) in files {
            std::fs::write(self.dir.join(path), contents).map_err(|e| e.to_string())?;
            self.git(&["add", "--", path]).await?;
        }
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
            let _ = self.sync().await;
            return Err(e);
        }
        Ok(())
    }

    /// When `main` last changed.
    pub async fn last_commit_time(&self) -> Result<Option<DateTime<Utc>>> {
        let out = self.git(&["log", "-1", "--format=%cI"]).await?;
        Ok(parse_time(&out))
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn sh(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .current_dir(dir)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// A bare "GitHub" with two commits, and a clone of it.
    fn setup(name: &str) -> (PathBuf, PathBuf, Repo) {
        let base = std::env::temp_dir().join(format!("agentwatch-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let seed = base.join("seed");
        std::fs::create_dir_all(&seed).unwrap();
        sh(&seed, &["init", "-q", "-b", "main"]);
        for n in ["a", "b"] {
            std::fs::write(seed.join(n), n).unwrap();
            sh(&seed, &["add", n]);
            sh(&seed, &["commit", "-qm", n]);
        }
        let bare = base.join("origin.git");
        sh(&base, &["clone", "-q", "--bare", "seed", "origin.git"]);
        sh(&base, &["clone", "-q", "origin.git", "clone"]);
        let repo = Repo {
            dir: base.join("clone"),
            key: base.join("no-key"),
            url: bare.display().to_string(),
        };
        (base, seed, repo)
    }

    /// GitHub's history rewritten with the same content, as the trailer strip did.
    fn rewrite(seed: &Path, base: &Path) {
        sh(seed, &["commit", "-q", "--amend", "-m", "b, reworded"]);
        sh(
            seed,
            &[
                "push",
                "-q",
                "-f",
                base.join("origin.git").to_str().unwrap(),
                "main",
            ],
        );
    }

    #[tokio::test]
    async fn a_rewritten_github_with_nothing_of_its_own_takes_githubs() {
        let (base, seed, repo) = setup("rewrite");
        rewrite(&seed, &base);
        repo.sync_keep().await.unwrap();
        assert_eq!(
            sh(&repo.dir, &["log", "-1", "--format=%s"]).trim(),
            "b, reworded"
        );
        let _ = std::fs::remove_dir_all(base);
    }

    #[tokio::test]
    async fn a_rewritten_github_never_throws_away_work_of_its_own() {
        let (base, seed, repo) = setup("own-work");
        rewrite(&seed, &base);
        std::fs::write(repo.dir.join("c"), "c").unwrap();
        sh(&repo.dir, &["add", "c"]);
        sh(&repo.dir, &["commit", "-qm", "c"]);
        assert!(repo.sync_keep().await.is_err());
        assert_eq!(sh(&repo.dir, &["log", "-1", "--format=%s"]).trim(), "c");
        let _ = std::fs::remove_dir_all(base);
    }

    #[tokio::test]
    async fn work_of_its_own_on_top_of_githubs_is_kept() {
        let (base, _seed, repo) = setup("ahead");
        std::fs::write(repo.dir.join("c"), "c").unwrap();
        sh(&repo.dir, &["add", "c"]);
        sh(&repo.dir, &["commit", "-qm", "c"]);
        repo.sync_keep().await.unwrap();
        assert_eq!(sh(&repo.dir, &["log", "-1", "--format=%s"]).trim(), "c");
        let _ = std::fs::remove_dir_all(base);
    }
}

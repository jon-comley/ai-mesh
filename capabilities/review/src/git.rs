//! Read-only git: clone, fetch, and read files and diffs straight from the
//! object store. Clones are made with `--no-checkout`, so there is no working
//! tree and nothing from a repo is ever run — hooks are switched off too.

use std::path::{Path, PathBuf};
use tokio::process::Command;

/// Arguments every git call starts with: no hooks, no prompts, no
/// `file://`/`ext::` transports (the URL check already refuses those).
fn base(dir: Option<&Path>) -> Command {
    let mut cmd = Command::new("git");
    if let Some(d) = dir {
        cmd.arg("-C").arg(d);
    }
    cmd.args([
        "-c",
        "core.hooksPath=/dev/null",
        "-c",
        "protocol.ext.allow=never",
        "-c",
        "core.quotePath=false",
    ])
    .env("GIT_TERMINAL_PROMPT", "0")
    .kill_on_drop(true);
    cmd
}

async fn run(mut cmd: Command) -> Result<Vec<u8>, String> {
    let out = cmd
        .output()
        .await
        .map_err(|e| format!("could not run git: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("git failed: {}", err.trim()));
    }
    Ok(out.stdout)
}

async fn run_text(cmd: Command) -> Result<String, String> {
    run(cmd)
        .await
        .map(|b| String::from_utf8_lossy(&b).into_owned())
}

pub struct Repo {
    pub dir: PathBuf,
    pub branch: String,
}

impl Repo {
    /// Clone `url` into `dir` if it is not there yet, then fetch `branch`.
    pub async fn sync(dir: &Path, url: &str, branch: &str) -> Result<Repo, String> {
        if !dir.join("HEAD").exists() && !dir.join(".git").exists() {
            if let Some(parent) = dir.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let mut cmd = base(None);
            // A full bare clone, not a partial one: questions search every
            // file with `git grep`, which would otherwise fetch each blob one
            // by one. These repos are small beside mac1's disk.
            cmd.args(["clone", "--bare", "--", url]).arg(dir);
            run(cmd).await?;
        }
        let mut cmd = base(Some(dir));
        cmd.args([
            "fetch",
            "--prune",
            "--",
            "origin",
            &format!("+refs/heads/{branch}:refs/remotes/origin/{branch}"),
        ]);
        run(cmd).await?;
        Ok(Repo {
            dir: dir.to_path_buf(),
            branch: branch.to_string(),
        })
    }

    fn git(&self) -> Command {
        base(Some(&self.dir))
    }

    /// The commit the branch points at now.
    pub async fn head(&self) -> Result<String, String> {
        let mut cmd = self.git();
        cmd.args(["rev-parse", "--verify"])
            .arg(format!("refs/remotes/origin/{}^{{commit}}", self.branch));
        Ok(run_text(cmd).await?.trim().to_string())
    }

    /// Whether `commit` exists here (a force-push can make an old one vanish).
    pub async fn has_commit(&self, commit: &str) -> bool {
        let mut cmd = self.git();
        cmd.args(["cat-file", "-e"])
            .arg(format!("{commit}^{{commit}}"));
        run(cmd).await.is_ok()
    }

    /// The commit `n` commits before `head`, or the first commit if the
    /// history is shorter. Where a first nightly run starts.
    pub async fn commit_before(&self, head: &str, n: u32) -> Result<String, String> {
        let mut cmd = self.git();
        cmd.args([
            "rev-list",
            "--first-parent",
            &format!("--max-count={}", n + 1),
            head,
        ]);
        let list = run_text(cmd).await?;
        list.lines()
            .last()
            .map(str::to_string)
            .ok_or_else(|| "empty history".into())
    }

    /// Files added, modified or renamed between `base` and `head`.
    pub async fn changed_files(&self, base: &str, head: &str) -> Result<Vec<String>, String> {
        let mut cmd = self.git();
        cmd.args(["diff", "--name-only", "--diff-filter=AMR", "-M", base, head]);
        Ok(run_text(cmd)
            .await?
            .lines()
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect())
    }

    /// The unified diff of one file between `base` and `head`.
    pub async fn diff(&self, base: &str, head: &str, path: &str) -> Result<String, String> {
        let mut cmd = self.git();
        cmd.args(["diff", "-U5", base, head, "--", path]);
        run_text(cmd).await
    }

    /// Every file at `rev`.
    pub async fn files(&self, rev: &str) -> Result<Vec<String>, String> {
        let mut cmd = self.git();
        cmd.args(["ls-tree", "-r", "--name-only", rev]);
        Ok(run_text(cmd).await?.lines().map(str::to_string).collect())
    }

    /// A file's text at `rev`. `None` for binary files (a NUL byte) or ones
    /// that are not text.
    pub async fn read(&self, rev: &str, path: &str) -> Result<Option<String>, String> {
        let mut cmd = self.git();
        cmd.arg("show").arg(format!("{rev}:{path}"));
        let bytes = run(cmd).await?;
        if bytes.contains(&0) {
            return Ok(None);
        }
        Ok(String::from_utf8(bytes).ok())
    }

    /// Commits between `base` and `head`, for the report's scope line.
    pub async fn commit_count(&self, base: &str, head: &str) -> Result<u32, String> {
        let mut cmd = self.git();
        cmd.args(["rev-list", "--count", &format!("{base}..{head}")]);
        Ok(run_text(cmd).await?.trim().parse().unwrap_or(0))
    }

    /// Fetch another branch (for an on-demand review of it) and return its
    /// head commit. The name is checked by the caller.
    pub async fn fetch_branch(&self, branch: &str) -> Result<String, String> {
        let mut cmd = self.git();
        cmd.args([
            "fetch",
            "--",
            "origin",
            &format!("+refs/heads/{branch}:refs/remotes/origin/{branch}"),
        ]);
        run(cmd).await?;
        let mut cmd = self.git();
        cmd.args(["rev-parse", "--verify"])
            .arg(format!("refs/remotes/origin/{branch}^{{commit}}"));
        Ok(run_text(cmd).await?.trim().to_string())
    }

    /// Where `a` and `b` last shared history.
    pub async fn merge_base(&self, a: &str, b: &str) -> Result<String, String> {
        let mut cmd = self.git();
        cmd.args(["merge-base", a, b]);
        Ok(run_text(cmd).await?.trim().to_string())
    }

    /// Files at `rev` containing `term` (case-insensitive, fixed string), with
    /// how many lines match. No match is an empty list, not an error.
    pub async fn grep_count(&self, rev: &str, term: &str) -> Result<Vec<(String, u32)>, String> {
        let mut cmd = self.git();
        cmd.args(["grep", "-c", "-i", "-I", "-F", "-e", term, rev, "--"]);
        let out = cmd
            .output()
            .await
            .map_err(|e| format!("could not run git: {e}"))?;
        // git grep exits 1 when nothing matches.
        if !out.status.success() && out.status.code() != Some(1) {
            return Err(format!(
                "git grep failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        let prefix = format!("{rev}:");
        Ok(String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| {
                let l = l.strip_prefix(&prefix).unwrap_or(l);
                let (path, n) = l.rsplit_once(':')?;
                Some((path.to_string(), n.parse().ok()?))
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(dir: &Path, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
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
            ok.status.success(),
            "{}",
            String::from_utf8_lossy(&ok.stderr)
        );
    }

    #[tokio::test]
    async fn clone_fetch_diff_and_read_without_a_working_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let origin = tmp.path().join("origin");
        std::fs::create_dir_all(&origin).unwrap();
        sh(&origin, &["init", "-q", "-b", "main"]);
        std::fs::write(origin.join("a.ts"), "export const a = 1;\n").unwrap();
        sh(&origin, &["add", "."]);
        sh(&origin, &["commit", "-qm", "one"]);
        std::fs::create_dir_all(origin.join("src")).unwrap();
        std::fs::write(origin.join("src/b.ts"), "export const b = 2;\n").unwrap();
        std::fs::write(origin.join("a.ts"), "export const a = 3;\n").unwrap();
        std::fs::write(origin.join("bin.dat"), [0u8, 1, 2]).unwrap();
        sh(&origin, &["add", "."]);
        sh(&origin, &["commit", "-qm", "two"]);

        let clone = tmp.path().join("clones/origin");
        // A local path stands in for GitHub here; the URL check that refuses
        // local paths in real use is tested in `files`.
        let repo = Repo::sync(&clone, origin.to_str().unwrap(), "main")
            .await
            .unwrap();
        assert!(!clone.join("a.ts").exists(), "no working tree");
        let head = repo.head().await.unwrap();
        let first = repo.commit_before(&head, 20).await.unwrap();
        assert_ne!(first, head);
        assert!(repo.has_commit(&first).await);
        assert!(
            !repo
                .has_commit("0000000000000000000000000000000000000000")
                .await
        );

        let mut changed = repo.changed_files(&first, &head).await.unwrap();
        changed.sort();
        assert_eq!(changed, vec!["a.ts", "bin.dat", "src/b.ts"]);
        assert!(
            repo.diff(&first, &head, "a.ts")
                .await
                .unwrap()
                .contains("+export const a = 3;")
        );
        assert_eq!(
            repo.read(&head, "src/b.ts").await.unwrap().as_deref(),
            Some("export const b = 2;\n")
        );
        assert_eq!(repo.read(&head, "bin.dat").await.unwrap(), None);
        assert_eq!(repo.files(&head).await.unwrap().len(), 3);
        assert_eq!(repo.commit_count(&first, &head).await.unwrap(), 1);

        let mut hits = repo.grep_count(&head, "EXPORT CONST").await.unwrap();
        hits.sort();
        assert_eq!(
            hits,
            vec![("a.ts".to_string(), 1), ("src/b.ts".to_string(), 1)]
        );
        assert!(
            repo.grep_count(&head, "nowhere-to-be-found")
                .await
                .unwrap()
                .is_empty()
        );

        // A feature branch, fetched on demand, and where it split off.
        sh(&origin, &["checkout", "-q", "-b", "feature"]);
        std::fs::write(origin.join("src/c.ts"), "export const c = 5;\n").unwrap();
        sh(&origin, &["add", "."]);
        sh(&origin, &["commit", "-qm", "feature"]);
        sh(&origin, &["checkout", "-q", "main"]);
        let fhead = repo.fetch_branch("feature").await.unwrap();
        assert_eq!(repo.merge_base(&head, &fhead).await.unwrap(), head);
        assert_eq!(
            repo.changed_files(&head, &fhead).await.unwrap(),
            vec!["src/c.ts"]
        );

        // A second sync fetches new commits into the existing clone.
        std::fs::write(origin.join("a.ts"), "export const a = 4;\n").unwrap();
        sh(&origin, &["commit", "-qam", "three"]);
        let repo = Repo::sync(&clone, origin.to_str().unwrap(), "main")
            .await
            .unwrap();
        assert_ne!(repo.head().await.unwrap(), head);
    }
}

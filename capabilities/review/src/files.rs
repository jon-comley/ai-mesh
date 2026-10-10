//! Which files get reviewed, and which repo URLs may be cloned at all.

/// Extensions of files worth reviewing. Everything else (images, lockfiles,
/// generated JSON, docs) is skipped.
const REVIEWABLE_EXTS: &[&str] = &[
    "ts", "tsx", "js", "jsx", "mjs", "cjs", "rs", "py", "go", "swift", "kt", "java", "rb", "php",
    "cs", "sql", "sh", "ps1", "rules", "vue", "svelte",
];

/// Path segments that mean vendored, generated or build output.
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "dist",
    "build",
    "vendor",
    "target",
    "coverage",
    ".git",
    "Pods",
    ".next",
    ".expo",
    "__generated__",
];

/// Largest file reviewed as a target; bigger ones are almost always generated.
pub const MAX_TARGET_BYTES: usize = 200_000;

fn ext(path: &str) -> &str {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rsplit_once('.').map(|(_, e)| e).unwrap_or("")
}

fn in_skipped_dir(path: &str) -> bool {
    path.split('/').any(|seg| SKIP_DIRS.contains(&seg))
}

fn is_test(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.contains(".test.")
        || name.contains(".spec.")
        || path.contains("__tests__/")
        || path.split('/').any(|s| s == "e2e")
}

/// Whether `path` should be reviewed (as opposed to only used as context).
/// Tests are skipped as targets: findings in test code cost a reader's time
/// and fix nothing a user sees.
pub fn is_reviewable(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    REVIEWABLE_EXTS.contains(&ext(path))
        && !in_skipped_dir(path)
        && !is_test(path)
        && !name.ends_with(".min.js")
        && !name.ends_with(".d.ts")
}

/// Whether `path` may be read as context for another file.
pub fn is_context_candidate(path: &str) -> bool {
    REVIEWABLE_EXTS.contains(&ext(path)) && !in_skipped_dir(path)
}

/// `owner/repo` from an accepted GitHub URL, or why it was refused.
///
/// Accepted shapes: `https://github.com/owner/repo(.git)`,
/// `git@github.com:owner/repo(.git)` and `git@github-<alias>:owner/repo(.git)`
/// — the last being how a per-repo deploy key is selected through
/// `~/.ssh/config`. Anything else (another host, a local path, `file://`,
/// options smuggled in as `-u…`) is refused, so the dashboard cannot make
/// mac1 fetch from arbitrary places. With `allowed_owners` non-empty, the
/// owner must be one of them.
pub fn validate_repo_url(url: &str, allowed_owners: &[String]) -> Result<String, String> {
    let url = url.trim();
    let rest = if let Some(r) = url.strip_prefix("https://github.com/") {
        r
    } else if let Some(r) = url.strip_prefix("git@") {
        let (host, path) = r
            .split_once(':')
            .ok_or_else(|| "an SSH URL needs host:owner/repo".to_string())?;
        let alias_ok = host == "github.com"
            || host.strip_prefix("github-").is_some_and(|a| {
                !a.is_empty()
                    && a.chars()
                        .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
            });
        if !alias_ok {
            return Err(format!(
                "host '{host}' is not GitHub (use github.com or a github-<name> deploy-key alias)"
            ));
        }
        path
    } else {
        return Err("only https://github.com/… and git@github…: URLs are accepted".into());
    };
    let rest = rest
        .strip_suffix(".git")
        .unwrap_or(rest)
        .trim_end_matches('/');
    let mut parts = rest.split('/');
    let (Some(owner), Some(repo), None) = (parts.next(), parts.next(), parts.next()) else {
        return Err("the URL must name exactly owner/repo".into());
    };
    let ok = |s: &str| {
        !s.is_empty()
            && !s.starts_with('-')
            && !s.starts_with('.')
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    };
    if !ok(owner) || !ok(repo) {
        return Err("owner or repo name has characters GitHub does not allow".into());
    }
    if !allowed_owners.is_empty() && !allowed_owners.iter().any(|o| o.eq_ignore_ascii_case(owner)) {
        return Err(format!(
            "'{owner}' is not an allowed owner (REVIEW_ALLOWED_OWNERS)"
        ));
    }
    Ok(format!("{owner}/{repo}"))
}

/// A repo name safe to use as a folder name.
pub fn valid_repo_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
}

/// The folders a weekly sweep walks through, in order: every top-level folder
/// holding a reviewable file, plus "." for reviewable files at the root.
pub fn sweep_folders(paths: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for p in paths.iter().filter(|p| is_reviewable(p)) {
        let folder = match p.split_once('/') {
            Some((top, _)) => top.to_string(),
            None => ".".to_string(),
        };
        if !out.contains(&folder) {
            out.push(folder);
        }
    }
    out.sort();
    out
}

/// The folder after `last` in `folders`, wrapping round.
pub fn next_folder(folders: &[String], last: Option<&str>) -> Option<String> {
    if folders.is_empty() {
        return None;
    }
    let idx = last
        .and_then(|l| folders.iter().position(|f| f == l))
        .map(|i| (i + 1) % folders.len())
        .unwrap_or(0);
    Some(folders[idx].clone())
}

/// Whether `path` is inside sweep `folder` ("." = files at the repo root).
pub fn in_folder(path: &str, folder: &str) -> bool {
    if folder == "." {
        !path.contains('/')
    } else {
        path.starts_with(&format!("{folder}/"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reviewable_files() {
        assert!(is_reviewable("src/pages/JobDetailPage.tsx"));
        assert!(is_reviewable("firestore.rules"));
        assert!(is_reviewable("coordinator/src/inference.rs"));
        assert!(!is_reviewable("src/pages/JobDetailPage.test.tsx"));
        assert!(!is_reviewable("node_modules/react/index.js"));
        assert!(!is_reviewable("package-lock.json"));
        assert!(!is_reviewable("README.md"));
        assert!(!is_reviewable("src/brand/expoConstants.d.ts"));
        assert!(!is_reviewable("e2e/portal.spec.ts"));
        assert!(is_context_candidate("src/brand/expoConstants.d.ts"));
    }

    #[test]
    fn github_urls_are_accepted_in_three_shapes() {
        let none: Vec<String> = vec![];
        assert_eq!(
            validate_repo_url("https://github.com/jon-comley/dashboard", &none).unwrap(),
            "jon-comley/dashboard"
        );
        assert_eq!(
            validate_repo_url("git@github.com:jon-comley/guv.git", &none).unwrap(),
            "jon-comley/guv"
        );
        assert_eq!(
            validate_repo_url("git@github-dashboard:jon-comley/dashboard.git", &none).unwrap(),
            "jon-comley/dashboard"
        );
    }

    #[test]
    fn anything_else_is_refused() {
        let none: Vec<String> = vec![];
        for bad in [
            "https://gitlab.com/a/b",
            "file:///etc",
            "/tmp/repo",
            "git@evil.example:a/b.git",
            "https://github.com/a/b/c",
            "https://github.com/-uexploit/b",
            "git@github-:a/b",
            "ext::sh -c touch% /tmp/x",
        ] {
            assert!(validate_repo_url(bad, &none).is_err(), "{bad} accepted");
        }
    }

    #[test]
    fn owner_allowlist_applies() {
        let owners = vec!["jon-comley".to_string()];
        assert!(validate_repo_url("https://github.com/Jon-Comley/guv", &owners).is_ok());
        assert!(validate_repo_url("https://github.com/someone/guv", &owners).is_err());
    }

    #[test]
    fn repo_names_are_folder_safe() {
        assert!(valid_repo_name("ai-mesh"));
        assert!(!valid_repo_name("../etc"));
        assert!(!valid_repo_name(".hidden"));
        assert!(!valid_repo_name(""));
    }

    #[test]
    fn sweep_walks_top_level_folders_in_turn() {
        let paths: Vec<String> = [
            "src/a.ts",
            "src/b.ts",
            "functions/x.ts",
            "README.md",
            "firestore.rules",
            "docs/notes.md",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let f = sweep_folders(&paths);
        assert_eq!(f, vec![".", "functions", "src"]);
        assert_eq!(next_folder(&f, None).as_deref(), Some("."));
        assert_eq!(next_folder(&f, Some("functions")).as_deref(), Some("src"));
        assert_eq!(next_folder(&f, Some("src")).as_deref(), Some("."));
        assert_eq!(next_folder(&f, Some("gone")).as_deref(), Some("."));
        assert!(in_folder("firestore.rules", "."));
        assert!(!in_folder("src/a.ts", "."));
        assert!(in_folder("src/a.ts", "src"));
        assert!(!in_folder("srcx/a.ts", "src"));
    }
}

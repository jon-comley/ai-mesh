//! Work queued for a machine that is not always on (the work laptop): the
//! tile's terminal on lenovo1 adds tasks, and agentwatch's dispatcher runs
//! each on the machine when it is reachable (plans/agent-terminals.md,
//! "Work laptop"). One JSON file per task under `term/<agent>/queue/`,
//! private to lenovo1.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Queued,
    Running,
    Done,
    /// Stopped on something only Jon can say yes to (a push, an install).
    NeedsYou,
    /// The machine went off while it ran: waits for Jon's Retry.
    Interrupted,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub task: String,
    /// The directory on the machine to work in (`~/repos/little-theo-firmware`).
    pub dir: String,
    pub state: TaskState,
    pub created: DateTime<Utc>,
    #[serde(default)]
    pub started: Option<DateTime<Utc>>,
    #[serde(default)]
    pub finished: Option<DateTime<Utc>>,
    /// What it said at the end.
    #[serde(default)]
    pub result: String,
    #[serde(default)]
    pub cost_usd: f64,
}

pub fn queue_dir(term_dir: &Path) -> PathBuf {
    term_dir.join("queue")
}

/// Where a task works: a repo under `~/repos/` on the machine, nothing a
/// shell would read. lenovo1 sees that folder through its sshfs mount.
pub fn dir_ok(dir: &str) -> bool {
    dir.starts_with(REPOS)
        && dir.len() > REPOS.len()
        && !dir.contains("..")
        && dir
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "~._/-".contains(c))
}

/// The machine's repos folder, as tasks name it.
pub const REPOS: &str = "~/repos/";

/// Where lenovo1 mounts it (systemd/agentterm-wl-repos.service).
pub const MOUNT: &str = "~/.ai-mesh/wl-repos";

/// A task's directory as lenovo1 sees it, through the mount.
pub fn local_dir(dir: &str) -> String {
    format!("{MOUNT}/{}", dir.strip_prefix(REPOS).unwrap_or(dir))
}

pub fn add(term_dir: &Path, task: &str, dir: &str, now: DateTime<Utc>) -> io::Result<Task> {
    if task.trim().is_empty() {
        return Err(io::Error::other("the task is empty"));
    }
    if !dir_ok(dir) {
        return Err(io::Error::other(format!(
            "{dir} is not a directory it can work in"
        )));
    }
    let q = queue_dir(term_dir);
    fs::create_dir_all(&q)?;
    let base = now.format("%Y%m%d-%H%M%S").to_string();
    let id = (0..)
        .map(|n| {
            if n == 0 {
                base.clone()
            } else {
                format!("{base}-{n}")
            }
        })
        .find(|id| !q.join(format!("{id}.json")).exists())
        .expect("some id is free");
    let t = Task {
        id,
        task: task.trim().to_string(),
        dir: dir.to_string(),
        state: TaskState::Queued,
        created: now,
        started: None,
        finished: None,
        result: String::new(),
        cost_usd: 0.0,
    };
    save(term_dir, &t)?;
    Ok(t)
}

pub fn save(term_dir: &Path, t: &Task) -> io::Result<()> {
    let q = queue_dir(term_dir);
    fs::create_dir_all(&q)?;
    let tmp = q.join(format!("{}.json.tmp", t.id));
    fs::write(&tmp, serde_json::to_vec_pretty(t)?)?;
    fs::rename(tmp, q.join(format!("{}.json", t.id)))
}

/// Every task, oldest first.
pub fn list(term_dir: &Path) -> Vec<Task> {
    let mut tasks: Vec<Task> = fs::read_dir(queue_dir(term_dir))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| serde_json::from_slice(&fs::read(e.path()).ok()?).ok())
        .collect();
    tasks.sort_by(|a, b| a.id.cmp(&b.id));
    tasks
}

/// The next task to start: the oldest queued one, and only when none is
/// running (one task at a time on the machine).
pub fn next(tasks: &[Task]) -> Option<&Task> {
    if tasks.iter().any(|t| t.state == TaskState::Running) {
        return None;
    }
    tasks.iter().find(|t| t.state == TaskState::Queued)
}

/// What a finished run's `claude -p --output-format json` says, as the
/// task's end state. A denied push or install shows in the result text,
/// which the task's prompt tells it to start with `NEEDS YOU:`.
pub fn finish(t: &mut Task, output: &str, now: DateTime<Utc>) {
    let v: serde_json::Value = serde_json::from_str(output.trim()).unwrap_or_default();
    let result = v
        .get("result")
        .and_then(|r| r.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let error = v
        .get("is_error")
        .and_then(|e| e.as_bool())
        .unwrap_or(v.is_null());
    t.cost_usd = v
        .get("total_cost_usd")
        .and_then(|c| c.as_f64())
        .unwrap_or(0.0);
    t.finished = Some(now);
    t.state = if result.starts_with("NEEDS YOU:") {
        TaskState::NeedsYou
    } else if error {
        TaskState::Failed
    } else {
        TaskState::Done
    };
    t.result = if result.is_empty() && v.is_null() {
        "It ended without a result.".into()
    } else {
        result
    };
}

/// What the task is told on top of Jon's words.
pub fn prompt(t: &Task) -> String {
    format!(
        "{}\n\nYou are working alone: Jon is not here to answer. You run on lenovo1. The work \
laptop's {} folder is mounted where you are, so read and edit its files directly. Run every \
command (git, builds, tests) on the laptop with `wl <command>`: it runs in the same folder \
there. You cannot run anything else. Commits are made as JPComley; never add AI attribution. \
Finish with a short result: what you changed, what you committed, what is left. If the task \
needs something wl refuses (a push, gh, sudo, an install, another machine), stop and start \
your result with \"NEEDS YOU:\" and the exact command. Never use an em dash.",
        t.task, t.dir
    )
}

/// What a task may do, enforced by Claude's settings: its files and `wl`.
/// `wl` itself refuses pushes, gh, sudo, installs and other machines.
pub fn settings() -> serde_json::Value {
    serde_json::json!({
        "permissions": {
            "allow": ["Read", "Glob", "Grep", "Edit", "MultiEdit", "Write", "Task", "TodoWrite", "Bash(wl:*)"],
            "deny": ["WebFetch", "WebSearch"]
        }
    })
}

/// The bash script that runs a task on lenovo1, detached in tmux. `results`
/// is lenovo1's results folder for the agent. Texts go in through quoted
/// heredocs; `dir` was checked by [`dir_ok`].
pub fn task_script(t: &Task, results: &Path) -> String {
    let settings = settings().to_string();
    let prompt = prompt(t);
    let tag = (0..)
        .map(|n| format!("AGENTTERM_{n}"))
        .find(|g| !settings.contains(g.as_str()) && !prompt.contains(g.as_str()))
        .expect("some tag is free");
    let out = results.join(&t.id).display().to_string();
    let local = local_dir(&t.dir);
    format!(
        "#!/bin/bash\n# Written by agentwatch (plans/agent-terminals.md, Work laptop).\n\
         export PATH=\"$HOME/.local/bin:/usr/local/bin:$PATH\"\n\
         mkdir -p {results}\n\
         cd {local} || {{ echo '{{\"result\":\"Cannot open {dir}: is the laptop mounted?\",\"is_error\":true}}' > {out}.json; exit 1; }}\n\
         settings=$(cat <<'{tag}'\n{settings}\n{tag}\n)\n\
         prompt=$(cat <<'{tag}'\n{prompt}\n{tag}\n)\n\
         claude -p \"$prompt\" --model haiku --output-format json --settings \"$settings\" \
         --append-system-prompt 'You run on Haiku to keep costs down. When the work needs more (code, careful multi-step work), hand it to one subagent with the Task tool, model sonnet, and wait for it. Never use Opus.' \
         --max-turns 80 > {out}.part 2> {out}.err\n\
         mv {out}.part {out}.json\n",
        results = results.display(),
        dir = t.dir,
    )
}

/// The tmux session a task runs in, on lenovo1.
pub fn task_session(t: &Task) -> String {
    format!("task-{}", t.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        "2026-11-04T15:00:00Z".parse().unwrap()
    }

    #[test]
    fn add_list_and_one_at_a_time() {
        let d = tempfile::tempdir().unwrap();
        let a = add(d.path(), "build it", "~/repos/x", now()).unwrap();
        let b = add(d.path(), "test it", "~/repos/x", now()).unwrap();
        assert_ne!(a.id, b.id);
        let tasks = list(d.path());
        assert_eq!(next(&tasks).unwrap().id, a.id);
        let mut running = tasks[0].clone();
        running.state = TaskState::Running;
        save(d.path(), &running).unwrap();
        assert_eq!(next(&list(d.path())), None);
    }

    #[test]
    fn refuses_odd_dirs_and_empty_tasks() {
        let d = tempfile::tempdir().unwrap();
        assert!(add(d.path(), "x", "~/repos/x; rm -rf ~", now()).is_err());
        assert!(add(d.path(), "x", "../etc", now()).is_err());
        assert!(add(d.path(), "x", "~/.ssh", now()).is_err());
        assert!(add(d.path(), "x", "~/repos/", now()).is_err());
        assert!(add(d.path(), "  ", "~/repos/x", now()).is_err());
    }

    #[test]
    fn task_script_works_on_the_mount_and_only_through_wl() {
        let t = add(
            tempfile::tempdir().unwrap().path(),
            "fix the build",
            "~/repos/efr32-boot",
            now(),
        )
        .unwrap();
        let s = task_script(&t, Path::new("/r"));
        assert!(s.contains("cd ~/.ai-mesh/wl-repos/efr32-boot ||"));
        assert!(s.contains("--model haiku --output-format json"));
        assert!(s.contains(&format!("mv /r/{0}.part /r/{0}.json", t.id)));
        assert!(s.contains("\"Bash(wl:*)\""));
        assert!(!s.contains("\"Bash\","));
        assert!(s.contains("fix the build"));
    }

    #[test]
    fn task_script_is_valid_bash() {
        let t = add(
            tempfile::tempdir().unwrap().path(),
            "it's \"quoted\" $HOME `x`",
            "~/repos/x",
            now(),
        )
        .unwrap();
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("t.sh");
        fs::write(&f, task_script(&t, d.path())).unwrap();
        let ok = std::process::Command::new("bash")
            .arg("-n")
            .arg(&f)
            .status()
            .unwrap();
        assert!(ok.success());
    }

    #[test]
    fn finish_reads_claude_json() {
        let mut t = add(tempfile::tempdir().unwrap().path(), "x", "~/repos/x", now()).unwrap();
        finish(
            &mut t,
            r#"{"result":"Built, 3 tests pass.","is_error":false,"total_cost_usd":0.02}"#,
            now(),
        );
        assert_eq!(
            (t.state, t.result.as_str()),
            (TaskState::Done, "Built, 3 tests pass.")
        );
        assert!((t.cost_usd - 0.02).abs() < 1e-9);
        finish(
            &mut t,
            r#"{"result":"NEEDS YOU: git push origin fix","is_error":false}"#,
            now(),
        );
        assert_eq!(t.state, TaskState::NeedsYou);
        finish(&mut t, "", now());
        assert_eq!(
            (t.state, t.result.as_str()),
            (TaskState::Failed, "It ended without a result.")
        );
    }
}

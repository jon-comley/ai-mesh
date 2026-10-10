//! Handing tasks to workers — the "team of agents" part.
//!
//! mac1 asks the coordinator which machines are free (the worker snapshot) and
//! this decides who takes what: one task per machine at a time, each task only
//! to a machine whose context (and, once measured, reading speed) can take it,
//! big review tasks to the biggest machine, and checks to a *different* model
//! from the one that raised the finding whenever such a machine exists.

use std::collections::HashSet;

/// One machine that can take work, as the worker snapshot describes it.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Worker {
    pub node_id: String,
    pub hostname: String,
    pub model: String,
    /// The model server's context size, in tokens.
    pub ctx_tokens: usize,
    pub busy: bool,
    /// Resting after a review step was paused for a home command.
    pub resting: bool,
    /// Measured prompt-reading speed (tokens a second), once known.
    #[serde(default)]
    pub prompt_tps: Option<f32>,
}

impl Worker {
    /// `model@hostname`, as reports name workers.
    pub fn label(&self) -> String {
        format!("{}@{}", self.model, self.hostname)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TaskKind {
    Review,
    Verify,
}

/// What the assigner needs to know about a waiting task.
#[derive(Debug, Clone, PartialEq)]
pub struct TaskNeed {
    pub id: u64,
    pub kind: TaskKind,
    /// Prompt size in tokens.
    pub tokens: usize,
    /// For a check: the model that raised the finding.
    pub avoid_model: Option<String>,
}

/// Tokens kept free for the answer.
pub const OUTPUT_RESERVE: usize = 4096;

/// How long a worker may spend just reading a prompt, once its speed is known.
/// Reviews run overnight and can take minutes; checks should stay short,
/// because the machines that do them also answer the lights.
pub fn max_read_secs(kind: TaskKind) -> f32 {
    match kind {
        TaskKind::Review => 900.0,
        TaskKind::Verify => 180.0,
    }
}

/// The largest prompt `w` should be given for a task of `kind`.
pub fn capacity(w: &Worker, kind: TaskKind) -> usize {
    let by_ctx = (w.ctx_tokens * 9 / 10).saturating_sub(OUTPUT_RESERVE);
    match w.prompt_tps {
        Some(tps) if tps > 0.0 => by_ctx.min((tps * max_read_secs(kind)) as usize),
        _ => by_ctx,
    }
}

/// The chunk budget for a run: the biggest review capacity among `workers`,
/// capped at `cap` (the configured ceiling, e.g. 100k, or less in the evening).
pub fn review_budget(workers: &[Worker], cap: usize) -> usize {
    workers
        .iter()
        .map(|w| capacity(w, TaskKind::Review))
        .max()
        .unwrap_or(0)
        .min(cap)
}

/// Pair waiting tasks with free workers. `occupied` holds node ids already
/// running one of this run's tasks. Returns `(task id, node id)` pairs; tasks
/// left out wait for the next round.
pub fn assign(
    pending: &[TaskNeed],
    workers: &[Worker],
    occupied: &HashSet<String>,
) -> Vec<(u64, String)> {
    let mut taken: HashSet<String> = occupied.clone();
    let mut out = Vec::new();
    for task in pending {
        let free = |w: &&Worker| !w.busy && !w.resting && !taken.contains(&w.node_id);
        let fits = |w: &&Worker| capacity(w, task.kind) >= task.tokens;
        let choice = match task.kind {
            TaskKind::Review => workers
                .iter()
                .filter(free)
                .filter(fits)
                .max_by_key(|w| (capacity(w, TaskKind::Review), w.node_id.clone())),
            TaskKind::Verify => {
                let other_model =
                    |w: &&Worker| task.avoid_model.as_deref() != Some(w.model.as_str());
                // Wait for a different model if one exists at all, even if it is
                // busy right now: a model checking its own finding mostly agrees
                // with itself.
                let a_different_model_exists = workers.iter().filter(fits).any(|w| other_model(&w));
                workers
                    .iter()
                    .filter(free)
                    .filter(fits)
                    .filter(|w| !a_different_model_exists || other_model(w))
                    // Smallest machine that fits, keeping big ones for reviews.
                    .min_by_key(|w| (capacity(w, TaskKind::Verify), w.node_id.clone()))
            }
        };
        if let Some(w) = choice {
            taken.insert(w.node_id.clone());
            out.push((task.id, w.node_id.clone()));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mac1() -> Worker {
        Worker {
            node_id: "mac1".into(),
            hostname: "mac1".into(),
            model: "qwen3-coder-30b".into(),
            ctx_tokens: 262_144,
            busy: false,
            resting: false,
            prompt_tps: None,
        }
    }

    fn beelink1() -> Worker {
        Worker {
            node_id: "beelink1".into(),
            hostname: "beelink1".into(),
            model: "qwen2.5:7b".into(),
            ctx_tokens: 32_768,
            busy: false,
            resting: false,
            prompt_tps: None,
        }
    }

    fn review(id: u64, tokens: usize) -> TaskNeed {
        TaskNeed {
            id,
            kind: TaskKind::Review,
            tokens,
            avoid_model: None,
        }
    }

    fn verify(id: u64, tokens: usize, avoid: &str) -> TaskNeed {
        TaskNeed {
            id,
            kind: TaskKind::Verify,
            tokens,
            avoid_model: Some(avoid.into()),
        }
    }

    #[test]
    fn big_review_goes_to_the_big_context_and_checks_to_the_other_model() {
        let got = assign(
            &[review(1, 90_000), verify(2, 8_000, "qwen3-coder-30b")],
            &[mac1(), beelink1()],
            &HashSet::new(),
        );
        assert_eq!(got, vec![(1, "mac1".into()), (2, "beelink1".into())]);
    }

    #[test]
    fn a_task_too_big_for_every_free_worker_waits() {
        let mut m = mac1();
        m.busy = true;
        let got = assign(&[review(1, 90_000)], &[m, beelink1()], &HashSet::new());
        assert!(got.is_empty());
    }

    #[test]
    fn one_task_per_machine_and_occupied_machines_are_skipped() {
        let occupied: HashSet<String> = ["beelink1".to_string()].into();
        let got = assign(
            &[review(1, 10_000), review(2, 10_000)],
            &[mac1(), beelink1()],
            &occupied,
        );
        assert_eq!(got, vec![(1, "mac1".into())]);
    }

    #[test]
    fn resting_and_busy_workers_get_nothing() {
        let mut b = beelink1();
        b.resting = true;
        let mut m = mac1();
        m.busy = true;
        assert!(assign(&[review(1, 100)], &[m, b], &HashSet::new()).is_empty());
    }

    #[test]
    fn a_check_waits_for_a_busy_different_model_rather_than_using_the_same_one() {
        let mut b = beelink1();
        b.busy = true;
        let got = assign(
            &[verify(1, 5_000, "qwen3-coder-30b")],
            &[mac1(), b],
            &HashSet::new(),
        );
        assert!(got.is_empty());
    }

    #[test]
    fn with_only_one_model_in_the_mesh_it_checks_its_own_findings() {
        let got = assign(
            &[verify(1, 5_000, "qwen3-coder-30b")],
            &[mac1()],
            &HashSet::new(),
        );
        assert_eq!(got, vec![(1, "mac1".into())]);
    }

    #[test]
    fn measured_speed_caps_capacity() {
        let mut b = beelink1();
        b.prompt_tps = Some(50.0); // 50 tok/s × 180 s = 9k for a check
        assert_eq!(capacity(&b, TaskKind::Verify), 9_000);
        assert_eq!(
            capacity(&beelink1(), TaskKind::Verify),
            32_768 * 9 / 10 - OUTPUT_RESERVE
        );
        let got = assign(&[verify(1, 12_000, "x")], &[b], &HashSet::new());
        assert!(got.is_empty());
    }

    #[test]
    fn review_budget_is_the_biggest_capacity_capped() {
        assert_eq!(review_budget(&[mac1(), beelink1()], 100_000), 100_000);
        assert_eq!(
            review_budget(&[beelink1()], 100_000),
            32_768 * 9 / 10 - OUTPUT_RESERVE
        );
        assert_eq!(review_budget(&[], 100_000), 0);
    }
}

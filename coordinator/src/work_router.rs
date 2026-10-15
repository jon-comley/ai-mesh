//! Home commands first, review work in the gaps.
//!
//! Every machine runs one generation at a time, first come first served, so
//! before this a light command queued behind a long review step on mac1 would
//! have hit the 150 s inference timeout. The coordinator is the only place
//! that sees every request on every machine, so it decides:
//!
//! * **Control** requests (home commands, voice, art, `/v1`) go to a machine
//!   with a control model that is *idle*. If none is idle but one is busy only
//!   with review work, that work is **paused** — cancelled on the machine and
//!   reported to mac1 as `Preempted`, so it goes back on mac1's queue — and the
//!   machine rests from work for [`REST_AFTER_PREEMPT`] so a back-and-forth
//!   voice conversation is not interrupted again.
//! * **Work** requests (review tasks from mac1) only ever go to a machine that
//!   is idle and not resting.
//!
//! Which models count as "control" and which as "work" is a coordinator
//! setting ([`ModelRoles`]); with nothing set, every model is both, which is
//! exactly how routing behaved before.
//!
//! The decisions are pure functions over [`WorkState`] so they can be tested
//! without a mesh; [`state`] is the one live instance.

use crate::registry::Registry;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::oneshot;

/// How long a machine is kept free of review work after a home command paused it.
pub const REST_AFTER_PREEMPT: Duration = Duration::from_secs(30);

/// Preferences namespace for the role lists.
pub const WORK_USER: &str = "__work__";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    Control,
    Work,
}

/// Which models may answer home commands and which may do review work.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ModelRoles {
    /// Empty: every model not listed only as work.
    pub control: Vec<String>,
    /// Empty: every model.
    pub work: Vec<String>,
}

impl ModelRoles {
    pub fn is_control(&self, model: &str) -> bool {
        if self.control.is_empty() {
            !self.work.iter().any(|m| m == model)
        } else {
            self.control.iter().any(|m| m == model)
        }
    }

    pub fn is_work(&self, model: &str) -> bool {
        self.work.is_empty() || self.work.iter().any(|m| m == model)
    }
}

fn parse_list(s: &str) -> Vec<String> {
    s.split(',')
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map(String::from)
        .collect()
}

/// Read the role lists: dashboard preference first, then `MESH_CONTROL_MODELS`
/// / `MESH_WORK_MODELS` (comma-separated), else empty.
pub fn load_roles(reg: &Registry) -> ModelRoles {
    let get = |key: &str, env: &str| {
        reg.get_preference(WORK_USER, key)
            .or_else(|| std::env::var(env).ok())
            .map(|v| parse_list(&v))
            .unwrap_or_default()
    };
    ModelRoles {
        control: get("control_models", "MESH_CONTROL_MODELS"),
        work: get("work_models", "MESH_WORK_MODELS"),
    }
}

pub fn save_roles(reg: &Registry, roles: &ModelRoles) {
    reg.set_preference(WORK_USER, "control_models", &roles.control.join(","));
    reg.set_preference(WORK_USER, "work_models", &roles.work.join(","));
}

/// A model that is Ready on a connected machine.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub node_id: String,
    pub hostname: String,
    pub model_name: String,
    pub size_mb: u64,
    pub ctx_size: Option<u32>,
}

struct InFlight {
    node_id: String,
    class: Class,
    /// Fired to pause a work request; the work task does the rest.
    pause: Option<oneshot::Sender<()>>,
}

#[derive(Default)]
pub struct WorkState {
    in_flight: HashMap<String, InFlight>,
    rest_until: HashMap<String, Instant>,
    pub roles: ModelRoles,
}

/// Where a control request should run, and which work requests to pause first.
#[derive(Debug, Clone, PartialEq)]
pub struct ControlPick {
    pub target: Candidate,
    /// `(request_id, node_id)` of work requests to cancel.
    pub preempt: Vec<(String, String)>,
}

impl WorkState {
    pub fn busy(&self, node_id: &str) -> bool {
        self.in_flight.values().any(|f| f.node_id == node_id)
    }

    fn busy_with(&self, node_id: &str, class: Class) -> bool {
        self.in_flight
            .values()
            .any(|f| f.node_id == node_id && f.class == class)
    }

    pub fn resting(&self, node_id: &str, now: Instant) -> bool {
        self.rest_until.get(node_id).is_some_and(|&t| t > now)
    }

    /// Record a request as running. A work request passes the sender that
    /// pauses it.
    pub fn begin(
        &mut self,
        request_id: &str,
        node_id: &str,
        class: Class,
        pause: Option<oneshot::Sender<()>>,
    ) {
        self.in_flight.insert(
            request_id.to_string(),
            InFlight {
                node_id: node_id.to_string(),
                class,
                pause,
            },
        );
    }

    pub fn end(&mut self, request_id: &str) {
        self.in_flight.remove(request_id);
    }

    /// Choose a machine for a control request. `requested` names a model the
    /// caller insists on; otherwise any control model will do.
    pub fn pick_control(
        &self,
        cands: &[Candidate],
        requested: Option<&str>,
    ) -> Option<ControlPick> {
        let usable: Vec<&Candidate> = cands
            .iter()
            .filter(|c| match requested {
                Some(m) => c.model_name == m,
                None => self.roles.is_control(&c.model_name),
            })
            .collect();
        let largest = |v: Vec<&Candidate>| -> Option<Candidate> {
            v.into_iter()
                .max_by(|a, b| {
                    a.size_mb
                        .cmp(&b.size_mb)
                        .then_with(|| b.node_id.cmp(&a.node_id))
                })
                .cloned()
        };
        // 1. An idle machine.
        let idle: Vec<&Candidate> = usable
            .iter()
            .copied()
            .filter(|c| !self.busy(&c.node_id))
            .collect();
        if let Some(target) = largest(idle) {
            return Some(ControlPick {
                target,
                preempt: Vec::new(),
            });
        }
        // 2. A machine busy only with review work: pause that work.
        let work_only: Vec<&Candidate> = usable
            .iter()
            .copied()
            .filter(|c| !self.busy_with(&c.node_id, Class::Control))
            .collect();
        if let Some(target) = largest(work_only) {
            let preempt = self
                .in_flight
                .iter()
                .filter(|(_, f)| f.node_id == target.node_id && f.class == Class::Work)
                .map(|(id, f)| (id.clone(), f.node_id.clone()))
                .collect();
            return Some(ControlPick { target, preempt });
        }
        // 3. Every machine is answering another home command: queue behind one.
        largest(usable).map(|target| ControlPick {
            target,
            preempt: Vec::new(),
        })
    }

    /// Pause the given work requests: fire their pause signals and rest their
    /// machines. Returns the ids actually paused.
    pub fn preempt(&mut self, preempt: &[(String, String)], now: Instant) -> Vec<(String, String)> {
        let mut done = Vec::new();
        for (request_id, node_id) in preempt {
            if let Some(mut f) = self.in_flight.remove(request_id) {
                if let Some(tx) = f.pause.take() {
                    let _ = tx.send(());
                }
                self.rest_until
                    .insert(node_id.clone(), now + REST_AFTER_PREEMPT);
                done.push((request_id.clone(), node_id.clone()));
            }
        }
        done
    }

    /// Choose a machine for a work request: idle, not resting, with a work
    /// model (the one asked for, if any). `Err` says why nothing can take it.
    pub fn pick_work(
        &self,
        cands: &[Candidate],
        node: Option<&str>,
        model: Option<&str>,
        now: Instant,
    ) -> Result<Candidate, String> {
        let matching: Vec<&Candidate> = cands
            .iter()
            .filter(|c| node.is_none_or(|n| c.node_id == n))
            .filter(|c| model.is_none_or(|m| c.model_name == m))
            .filter(|c| self.roles.is_work(&c.model_name))
            .collect();
        if matching.is_empty() {
            return Err(match node {
                Some(n) => format!("'{n}' has no work model ready"),
                None => "no machine has a work model ready".into(),
            });
        }
        let free: Vec<&Candidate> = matching
            .iter()
            .copied()
            .filter(|c| !self.busy(&c.node_id) && !self.resting(&c.node_id, now))
            .collect();
        free.into_iter()
            .max_by_key(|c| c.size_mb)
            .cloned()
            .ok_or_else(|| "busy or resting after a home command".to_string())
    }
}

/// The live router state.
pub fn state() -> &'static Mutex<WorkState> {
    static STATE: OnceLock<Mutex<WorkState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(WorkState::default()))
}

/// Removes a request from the router when dropped, however the request ends.
pub struct InFlightGuard(pub String);

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        if let Ok(mut s) = state().lock() {
            s.end(&self.0);
        }
    }
}

/// Every Ready model on a connected Compute node.
pub fn candidates(reg: &Registry, connected: &std::collections::HashSet<String>) -> Vec<Candidate> {
    reg.ready_model_targets(false)
        .into_iter()
        .filter(|c| connected.contains(&c.node_id))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(node: &str, model: &str, size: u64) -> Candidate {
        Candidate {
            node_id: node.into(),
            hostname: node.into(),
            model_name: model.into(),
            size_mb: size,
            ctx_size: None,
        }
    }

    fn mesh() -> Vec<Candidate> {
        vec![
            cand("mac1", "qwen3-coder-30b", 22_000),
            cand("beelink1", "qwen2.5:7b", 4_700),
        ]
    }

    #[test]
    fn roles_default_to_everything_being_both() {
        let r = ModelRoles::default();
        assert!(r.is_control("x") && r.is_work("x"));
    }

    #[test]
    fn listing_a_model_only_as_work_keeps_it_off_home_commands() {
        let r = ModelRoles {
            control: vec![],
            work: vec!["qwen3-coder-30b".into()],
        };
        assert!(!r.is_control("qwen3-coder-30b"));
        assert!(r.is_control("qwen2.5:7b"));
        assert!(!r.is_work("qwen2.5:7b"));
    }

    #[test]
    fn idle_machines_take_home_commands_largest_model_first() {
        let s = WorkState::default();
        let pick = s.pick_control(&mesh(), None).unwrap();
        assert_eq!(pick.target.node_id, "mac1");
        assert!(pick.preempt.is_empty());
    }

    #[test]
    fn a_home_command_goes_to_an_idle_control_machine_before_pausing_work() {
        let mut s = WorkState::default();
        let (tx, _rx) = oneshot::channel();
        s.begin("review-1", "mac1", Class::Work, Some(tx));
        let pick = s.pick_control(&mesh(), None).unwrap();
        assert_eq!(pick.target.node_id, "beelink1");
        assert!(pick.preempt.is_empty());
    }

    #[test]
    fn with_no_idle_machine_review_work_is_paused() {
        let mut s = WorkState::default();
        let (tx1, mut rx1) = oneshot::channel();
        let (tx2, _rx2) = oneshot::channel();
        s.begin("review-1", "mac1", Class::Work, Some(tx1));
        s.begin("check-1", "beelink1", Class::Work, Some(tx2));
        let pick = s.pick_control(&mesh(), None).unwrap();
        assert_eq!(pick.target.node_id, "mac1");
        assert_eq!(
            pick.preempt,
            vec![("review-1".to_string(), "mac1".to_string())]
        );

        let now = Instant::now();
        let done = s.preempt(&pick.preempt, now);
        assert_eq!(done.len(), 1);
        assert!(rx1.try_recv().is_ok(), "the work task is told to stop");
        assert!(s.resting("mac1", now + Duration::from_secs(1)));
        assert!(!s.resting("mac1", now + REST_AFTER_PREEMPT + Duration::from_secs(1)));
        assert!(!s.busy("mac1"));
    }

    #[test]
    fn a_machine_answering_a_home_command_is_never_preempted() {
        let mut s = WorkState::default();
        s.begin("intent-1", "mac1", Class::Control, None);
        s.begin("intent-2", "beelink1", Class::Control, None);
        let pick = s.pick_control(&mesh(), None).unwrap();
        assert!(pick.preempt.is_empty());
    }

    #[test]
    fn a_requested_model_is_honoured_even_if_it_is_work_only() {
        let mut s = WorkState::default();
        s.roles.work = vec!["qwen3-coder-30b".into()];
        assert_eq!(
            s.pick_control(&mesh(), None).unwrap().target.node_id,
            "beelink1"
        );
        assert_eq!(
            s.pick_control(&mesh(), Some("qwen3-coder-30b"))
                .unwrap()
                .target
                .node_id,
            "mac1"
        );
        assert!(s.pick_control(&mesh(), Some("nope")).is_none());
    }

    #[test]
    fn work_never_goes_to_a_busy_or_resting_machine() {
        let mut s = WorkState::default();
        let now = Instant::now();
        s.begin("intent-1", "mac1", Class::Control, None);
        assert_eq!(
            s.pick_work(&mesh(), None, None, now).unwrap().node_id,
            "beelink1"
        );
        assert!(s.pick_work(&mesh(), Some("mac1"), None, now).is_err());
        s.rest_until
            .insert("beelink1".into(), now + Duration::from_secs(10));
        assert!(s.pick_work(&mesh(), Some("beelink1"), None, now).is_err());
        s.end("intent-1");
        assert_eq!(
            s.pick_work(&mesh(), None, None, now).unwrap().node_id,
            "mac1"
        );
    }

    #[test]
    fn work_respects_the_work_list() {
        let mut s = WorkState::default();
        s.roles.work = vec!["qwen3-coder-30b".into()];
        let now = Instant::now();
        assert!(s.pick_work(&mesh(), Some("beelink1"), None, now).is_err());
        assert_eq!(
            s.pick_work(&mesh(), None, None, now).unwrap().node_id,
            "mac1"
        );
    }

    #[test]
    fn preempting_an_already_finished_request_is_a_no_op() {
        let mut s = WorkState::default();
        let done = s.preempt(&[("gone".into(), "mac1".into())], Instant::now());
        assert!(done.is_empty());
        assert!(!s.resting("mac1", Instant::now()));
    }

    #[test]
    fn parse_list_trims_and_drops_blanks() {
        assert_eq!(parse_list(" a, b ,,c "), vec!["a", "b", "c"]);
        assert!(parse_list("").is_empty());
    }
}

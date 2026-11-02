//! Agent watch: what each of Jon's workers is doing (plans/agent-watch.md).
//!
//! Everything here is pure: the server hands in file contents, commit times and
//! the clock, and gets back parsed logs, hub rows and each worker's state. That
//! keeps the parts that decide *what the page says* testable without git, ssh
//! or a clock, the same split `ebay` and `codereview` make.
//!
//! - [`config`] reads `agents.toml`.
//! - [`log`] reads a cloud agent's `agent-log.md`.
//! - [`hub`] reads the rows an agent is waiting on in `HUB.md`, and writes Jon's
//!   reply into one.
//! - [`status`] reads a local worker's status file (the mac1 orchestrator).
//! - [`state`] decides `ok`, `late`, `broken` and the rest.

pub mod config;
pub mod hub;
pub mod log;
pub mod state;
pub mod status;

//! The Schedule tab: every background job in one table, read from the
//! schedulers that run them (no AI). Agents come from their tiles; services
//! from systemd (here or over ssh) or Windows' service manager.

use std::time::Duration;

use agentwatch::config::{Service, ServiceKind};
use serde::Serialize;
use serde_json::{Value, json};

use super::App;
use super::run;

const LIMIT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Serialize)]
pub struct ServiceView {
    pub name: String,
    pub purpose: String,
    pub host: String,
    pub unit: String,
    /// running, waiting (a timer between runs), stopped, failed, unreachable.
    pub state: String,
    pub since: Option<String>,
    pub restarts: Option<u32>,
    /// A timer's next and last run.
    pub next: Option<String>,
    pub last: Option<String>,
    pub can_run: bool,
}

/// `systemctl show` for one unit, as a command for this machine or `host`.
fn show_cmd(unit: &str) -> String {
    let props = "ActiveState,SubState,ActiveEnterTimestamp,NRestarts,Result,NextElapseUSecRealtime,LastTriggerUSec";
    format!("systemctl show {unit} -p {props} --timestamp=pretty")
}

/// Reads `systemctl show` output into the row.
pub fn from_show(s: &Service, host: &str, out: &str) -> ServiceView {
    let get = |k: &str| {
        out.lines()
            .find_map(|l| l.strip_prefix(&format!("{k}=")))
            .map(str::trim)
            .filter(|v| !v.is_empty() && *v != "n/a" && *v != "0")
            .map(String::from)
    };
    let active = get("ActiveState").unwrap_or_default();
    let sub = get("SubState").unwrap_or_default();
    let timer = s.unit.ends_with(".timer");
    let state = match (active.as_str(), sub.as_str()) {
        ("failed", _) => "failed",
        ("active", "waiting") | ("active", "elapsed") if timer => "waiting",
        ("active", _) => "running",
        ("activating", _) => "running",
        _ => "stopped",
    };
    ServiceView {
        name: s.name.clone(),
        purpose: s.purpose.clone(),
        host: host.into(),
        unit: s.unit.clone(),
        state: state.into(),
        since: get("ActiveEnterTimestamp"),
        restarts: get("NRestarts").and_then(|n| n.parse().ok()),
        next: get("NextElapseUSecRealtime"),
        last: get("LastTriggerUSec"),
        can_run: timer,
    }
}

/// Reads `sc query` output (Windows) into the row.
pub fn from_sc(s: &Service, host: &str, out: &str) -> ServiceView {
    let state = if out.contains("RUNNING") {
        "running"
    } else if out.contains("STOPPED") {
        "stopped"
    } else {
        "unreachable"
    };
    ServiceView {
        name: s.name.clone(),
        purpose: s.purpose.clone(),
        host: host.into(),
        unit: s.unit.clone(),
        state: state.into(),
        since: None,
        restarts: None,
        next: None,
        last: None,
        can_run: false,
    }
}

fn local_cmd(script: &str) -> tokio::process::Command {
    let mut c = tokio::process::Command::new("sh");
    c.args(["-c", script]);
    c
}

fn here(s: &Service) -> String {
    s.host.clone().unwrap_or_else(|| "lenovo1".into())
}

pub async fn service(s: &Service) -> ServiceView {
    let host = here(s);
    let (script, windows) = match s.kind {
        ServiceKind::Systemd => (show_cmd(&s.unit), false),
        ServiceKind::Windows => (format!("sc query {}", s.unit), true),
    };
    let cmd = match &s.host {
        Some(h) => run::ssh(h, &script),
        None => local_cmd(&script),
    };
    match run::output(cmd, LIMIT).await {
        Ok(out) if windows => from_sc(s, &host, &out),
        Ok(out) => from_show(s, &host, &out),
        Err(_) => ServiceView {
            name: s.name.clone(),
            purpose: s.purpose.clone(),
            host,
            unit: s.unit.clone(),
            state: "unreachable".into(),
            since: None,
            restarts: None,
            next: None,
            last: None,
            can_run: false,
        },
    }
}

pub async fn all(app: &App) -> Value {
    let services = app
        .config
        .read()
        .unwrap_or_else(|p| p.into_inner())
        .services
        .clone();
    let rows = futures_join(services.iter().map(service)).await;
    json!({ "agents": app.views(), "services": rows })
}

async fn futures_join<F: std::future::Future<Output = ServiceView>>(
    futs: impl Iterator<Item = F>,
) -> Vec<ServiceView> {
    let handles: Vec<_> = futs.collect();
    let mut out = Vec::with_capacity(handles.len());
    for f in handles {
        out.push(f.await);
    }
    out
}

/// Restart (a service) or run now (a timer's service). Here through polkit
/// (systemd/agentwatch-units.rules), elsewhere with sudo over ssh.
pub async fn act(s: &Service, action: &str) -> run::Result<()> {
    let verb = match action {
        "restart" => "restart",
        "run" if s.unit.ends_with(".timer") => "start",
        _ => return Err(format!("{action} is not something {} can do", s.name)),
    };
    let unit = if verb == "start" {
        s.unit.trim_end_matches(".timer").to_string() + ".service"
    } else {
        s.unit.clone()
    };
    let script = match (s.kind, &s.host) {
        (ServiceKind::Windows, _) => format!("sc stop {unit} & sc start {unit}"),
        (_, Some(_)) => format!("sudo -n systemctl {verb} {unit}"),
        (_, None) => format!("systemctl {verb} {unit}"),
    };
    let cmd = match &s.host {
        Some(h) => run::ssh(h, &script),
        None => local_cmd(&script),
    };
    run::output(cmd, LIMIT).await.map(|_| ())
}

/// Its last 40 log lines.
pub async fn log(s: &Service) -> run::Result<String> {
    let script = match s.kind {
        ServiceKind::Windows => {
            return Ok("Its log is in C:\\Users\\jonno\\ai-mesh\\logs on beelink1.".into());
        }
        ServiceKind::Systemd => {
            let unit = s.unit.trim_end_matches(".timer");
            format!("journalctl -u {unit} -n 40 --no-pager -o short-iso")
        }
    };
    let cmd = match &s.host {
        Some(h) => run::ssh(h, &script),
        None => local_cmd(&script),
    };
    run::output(cmd, LIMIT).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn svc(unit: &str) -> Service {
        Service {
            name: "x".into(),
            purpose: "p".into(),
            host: None,
            unit: unit.into(),
            kind: ServiceKind::Systemd,
        }
    }

    #[test]
    fn reads_a_running_service() {
        let v = from_show(
            &svc("a.service"),
            "lenovo1",
            "ActiveState=active\nSubState=running\nActiveEnterTimestamp=Wed 2026-11-04 15:43:36 GMT\nNRestarts=2\n",
        );
        assert_eq!(
            (v.state.as_str(), v.restarts, v.can_run),
            ("running", Some(2), false)
        );
        assert_eq!(v.since.as_deref(), Some("Wed 2026-11-04 15:43:36 GMT"));
    }

    #[test]
    fn reads_a_waiting_timer_and_a_failure() {
        let t = from_show(
            &svc("p.timer"),
            "lenovo1",
            "ActiveState=active\nSubState=waiting\nNextElapseUSecRealtime=Wed 2026-11-04 18:00:00 GMT\nNRestarts=0\n",
        );
        assert_eq!(
            (t.state.as_str(), t.can_run, t.restarts),
            ("waiting", true, None)
        );
        assert!(t.next.is_some());
        assert_eq!(
            from_show(
                &svc("a.service"),
                "h",
                "ActiveState=failed\nSubState=failed\n"
            )
            .state,
            "failed"
        );
    }

    #[test]
    fn reads_windows_sc() {
        let s = Service {
            kind: ServiceKind::Windows,
            ..svc("ai-mesh-agent")
        };
        assert_eq!(
            from_sc(&s, "beelink1", "STATE : 4 RUNNING").state,
            "running"
        );
    }
}

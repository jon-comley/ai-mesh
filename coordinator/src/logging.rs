use crate::http::state::{DashboardState, ErrorEntry};
use std::sync::{Arc, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;

static DASHBOARD: OnceLock<Arc<DashboardState>> = OnceLock::new();

/// Call once after DashboardState is live to start routing captured errors to it.
pub fn bind(state: Arc<DashboardState>) {
    let _ = DASHBOARD.set(state);
}

/// Tracing layer that captures WARN/ERROR events into the dashboard error feed.
pub struct ErrorCaptureLayer;

impl<S: Subscriber> Layer<S> for ErrorCaptureLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let level = *event.metadata().level();
        if level > Level::WARN {
            return;
        }
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        let ts_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let entry = ErrorEntry {
            ts_ms,
            level: if level == Level::ERROR {
                "ERROR".into()
            } else {
                "WARN".into()
            },
            target: event.metadata().target().into(),
            message: visitor.message,
        };
        if let Some(dash) = DASHBOARD.get() {
            dash.push_error(entry);
        }
    }
}

#[derive(Default)]
struct MessageVisitor {
    message: String,
}

impl Visit for MessageVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_owned();
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        }
    }
}

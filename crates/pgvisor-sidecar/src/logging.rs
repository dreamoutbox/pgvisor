use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::RwLock;

use pgvisor_core::{LogLevel, NodeLogEntry};
use tracing::field::{Field, Visit};
use tracing::{Event, Level};
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

use crate::supervisor::MAX_LOG_ENTRIES;

/// A tracing layer that captures structured sidecar and PostgreSQL server logs
/// into a thread-safe circular buffer for node diagnostics inspection.
pub struct SidecarLogCaptureLayer {
    logs: Arc<RwLock<VecDeque<NodeLogEntry>>>,
}

impl SidecarLogCaptureLayer {
    pub fn new(logs: Arc<RwLock<VecDeque<NodeLogEntry>>>) -> Self {
        Self { logs }
    }
}

struct FieldVisitor {
    message: Option<String>,
    fields: Vec<(String, String)>,
}

impl Visit for FieldVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let s = format!("{value:?}");
            let s = if s.starts_with('"') && s.ends_with('"') && s.len() >= 2 {
                s[1..s.len() - 1].to_string()
            } else {
                s
            };
            self.message = Some(s);
        } else {
            self.fields.push((field.name().to_string(), format!("{value:?}")));
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = Some(value.to_string());
        } else {
            self.fields.push((field.name().to_string(), format!("\"{value}\"")));
        }
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.fields.push((field.name().to_string(), value.to_string()));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.fields.push((field.name().to_string(), value.to_string()));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.fields.push((field.name().to_string(), value.to_string()));
    }
}

impl<S> Layer<S> for SidecarLogCaptureLayer
where
    S: tracing::Subscriber,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        let target = meta.target();

        // Capture all pgvisor components (sidecar, supervisor, election, control, core)
        // and postgres server output while ignoring noise from third-party crates.
        if !target.starts_with("pgvisor") && target != "postgres" {
            return;
        }

        let level = match *meta.level() {
            Level::ERROR => LogLevel::Error,
            Level::WARN => LogLevel::Warn,
            _ => LogLevel::Info,
        };

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let mut visitor = FieldVisitor {
            message: None,
            fields: Vec::new(),
        };
        event.record(&mut visitor);

        let mut full_msg = String::with_capacity(128);
        if !target.is_empty() {
            full_msg.push_str(target);
            full_msg.push_str(": ");
        }
        if let Some(msg) = visitor.message {
            full_msg.push_str(&msg);
        }
        for (k, v) in visitor.fields {
            full_msg.push(' ');
            full_msg.push_str(&k);
            full_msg.push('=');
            full_msg.push_str(&v);
        }

        if let Ok(mut buf) = self.logs.write() {
            if buf.len() >= MAX_LOG_ENTRIES {
                buf.pop_front();
            }
            buf.push_back(NodeLogEntry {
                timestamp_ms: now,
                level,
                message: full_msg,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::layer::SubscriberExt;

    #[test]
    fn test_capture_sidecar_logs() {
        let logs = Arc::new(RwLock::new(VecDeque::new()));
        let layer = SidecarLogCaptureLayer::new(Arc::clone(&logs));
        let subscriber = tracing_subscriber::registry().with(layer);

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(
                data_dir = "/var/lib/postgresql/data/pgdata",
                port = 5432,
                superuser = "postgres",
                node_id = 1,
                role = "leader",
                "pgvisor-sidecar supervisor starting up"
            );
            tracing::info!(
                dir = "/var/lib/postgresql/data/pgdata",
                superuser = "postgres",
                "Running initdb to initialize new cluster"
            );
            tracing::warn!(target: "postgres", "2026-09-25 05:38:09.577 GMT [21] LOG: starting PostgreSQL 18.6");
        });

        let buf = logs.read().unwrap();
        assert_eq!(buf.len(), 3);
        assert!(buf[0].message.contains("pgvisor-sidecar supervisor starting up"));
        assert!(buf[0].message.contains("port=5432"));
        assert!(buf[0].message.contains("role=\"leader\""));
        assert_eq!(buf[0].level, LogLevel::Info);

        assert!(buf[1].message.contains("Running initdb to initialize new cluster"));
        assert_eq!(buf[1].level, LogLevel::Info);

        assert!(buf[2].message.starts_with("postgres: 2026-09-25 05:38:09.577 GMT [21] LOG: starting PostgreSQL 18.6"));
        assert_eq!(buf[2].level, LogLevel::Warn);
    }
}

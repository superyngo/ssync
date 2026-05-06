//! In-memory ring-buffer tracing layer for the TUI (AD-15, §17.2).
//!
//! When the TUI is active, `init_tracing` installs this layer alongside the
//! fmt layer (whose writer is swapped to `std::io::sink()`). Every tracing
//! event is captured as a `LogEntry` and pushed into a shared ring buffer
//! capped at 500 entries. The `L` key overlay reads from this buffer.

use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use tracing::{Level, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;

/// Maximum number of log entries retained in the ring buffer (§17.2).
const LOG_BUFFER_CAP: usize = 500;

/// A single captured tracing event, displayed in the log overlay.
#[derive(Debug, Clone)]
pub struct LogEntry {
    pub _when: SystemTime,
    pub level: String,
    pub target: String,
    pub text: String,
}

/// Shared handle to the ring buffer. Cheaply clonable; the layer and the
/// App each hold one.
#[derive(Clone)]
pub struct LogBufferHandle {
    inner: Arc<Mutex<VecDeque<LogEntry>>>,
}

use std::collections::VecDeque;

impl LogBufferHandle {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(VecDeque::with_capacity(LOG_BUFFER_CAP))),
        }
    }

    /// Push a new entry, evicting the oldest if at capacity.
    pub fn push(&self, entry: LogEntry) {
        let mut buf = self.inner.lock().unwrap();
        if buf.len() >= LOG_BUFFER_CAP {
            buf.pop_front();
        }
        buf.push_back(entry);
    }

    /// Snapshot the current buffer contents for rendering.
    pub fn snapshot(&self) -> Vec<LogEntry> {
        let buf = self.inner.lock().unwrap();
        buf.iter().cloned().collect()
    }

    /// Current number of entries.
    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().len()
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.inner.lock().unwrap().is_empty()
    }
}

/// A `tracing_subscriber::Layer` that captures events into a `LogBufferHandle`.
pub struct RingBufferLayer {
    handle: LogBufferHandle,
}

impl RingBufferLayer {
    pub fn new(handle: LogBufferHandle) -> Self {
        Self { handle }
    }
}

impl<S> tracing_subscriber::Layer<S> for RingBufferLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let level = match *event.metadata().level() {
            Level::ERROR => "ERROR",
            Level::WARN => "WARN",
            Level::INFO => "INFO",
            Level::DEBUG => "DEBUG",
            Level::TRACE => "TRACE",
        };

        let mut visitor = StringVisitor::new();
        event.record(&mut visitor);

        self.handle.push(LogEntry {
            _when: SystemTime::now(),
            level: level.to_string(),
            target: event.metadata().target().to_string(),
            text: visitor.0,
        });
    }
}

struct StringVisitor(String);

impl StringVisitor {
    fn new() -> Self {
        Self(String::new())
    }
}

impl tracing::field::Visit for StringVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if !self.0.is_empty() {
            self.0.push(' ');
        }
        if field.name() != "message" {
            self.0.push_str(field.name());
            self.0.push('=');
        }
        use std::fmt::Write;
        let _ = write!(self.0, "{value:?}");
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if !self.0.is_empty() {
            self.0.push(' ');
        }
        if field.name() != "message" {
            self.0.push_str(field.name());
            self.0.push('=');
        }
        self.0.push_str(value);
    }

    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        if !self.0.is_empty() {
            self.0.push(' ');
        }
        use std::fmt::Write;
        let _ = write!(self.0, "{}={value}", field.name());
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        if !self.0.is_empty() {
            self.0.push(' ');
        }
        use std::fmt::Write;
        let _ = write!(self.0, "{}={value}", field.name());
    }

    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        if !self.0.is_empty() {
            self.0.push(' ');
        }
        use std::fmt::Write;
        let _ = write!(self.0, "{}={value}", field.name());
    }

    fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
        if !self.0.is_empty() {
            self.0.push(' ');
        }
        use std::fmt::Write;
        let _ = write!(self.0, "{}={value}", field.name());
    }

    fn record_error(
        &mut self,
        field: &tracing::field::Field,
        value: &(dyn std::error::Error + 'static),
    ) {
        if !self.0.is_empty() {
            self.0.push(' ');
        }
        use std::fmt::Write;
        let _ = write!(self.0, "{}={value}", field.name());
    }
}

//! Async bridge between command-core operations and the TUI render loop
//! (per docs/tui_reconstruct_plan.md §18, AD-13).
//!
//! Operations run on tokio tasks. Per-host events flow through a bounded
//! `tokio::mpsc::channel(1024)` as `TuiEvent` values. The main loop drains
//! the channel non-blockingly each frame and updates `App` state.
//!
//! `AsyncBridge` implements `ProgressSink` so command-core can emit events
//! without knowing about the TUI.

use tokio::sync::mpsc::{Sender, UnboundedSender};
use tokio_util::sync::CancellationToken;

use crate::commands::report::{CheckReport, HostStatus, ProgressSink};

/// Events flowing from a running operation back to the main loop.
#[derive(Debug, Clone)]
pub enum TuiEvent {
    HostStarted(String),
    HostCompleted {
        host: String,
        status: HostStatus,
        detail: String,
        duration_ms: u64,
    },
    /// Operation finished cleanly — payload carries the final report.
    OperationFinished(CheckReport),
    /// Operation cancelled by user (Esc on progress popup).
    OperationCancelled,
    /// Top-level error (target resolution, pool setup, etc).
    OperationError(String),
}

/// Channel capacity — covers ~500 hosts × 2 events with headroom (§18.1).
pub const CHANNEL_CAPACITY: usize = 1024;

/// Handle to a currently-running operation (per §18.2).
///
/// The operation itself runs on a dedicated OS thread (see
/// `App::execute_check` for rationale); no `JoinHandle` is held — cancellation
/// is via `CancellationToken`, and termination is signalled exclusively
/// through the event channel.
pub struct RunningOp {
    pub cancel: CancellationToken,
    pub started_at: std::time::Instant,
    /// Hosts targeted at the start of the run (snapshot used by the progress
    /// popup to render rows for hosts that have not yet started).
    pub targets: Vec<String>,
    /// Per-host outcomes accumulated as events arrive.
    pub host_outcomes: Vec<(String, HostStatus, String, u64)>,
}

impl RunningOp {
    pub fn record_completed(&mut self, host: &str, status: HostStatus, detail: &str, ms: u64) {
        self.host_outcomes
            .push((host.to_string(), status, detail.to_string(), ms));
    }

    pub fn completed_count(&self) -> usize {
        self.host_outcomes.len()
    }
}

/// Sender-side of the bridge held by spawned operation tasks.
pub struct EventSender {
    tx: UnboundedSender<TuiEvent>,
}

impl EventSender {
    pub fn new(tx: UnboundedSender<TuiEvent>) -> Self {
        Self { tx }
    }
}

impl ProgressSink for EventSender {
    fn host_started(&self, host: &str) {
        let _ = self.tx.send(TuiEvent::HostStarted(host.to_string()));
    }

    fn host_completed(&self, host: &str, status: HostStatus, detail: &str, ms: u64) {
        // Truncate detail to 3 display lines (§18.1) before sending.
        let truncated = truncate_detail(detail, 3);
        let _ = self.tx.send(TuiEvent::HostCompleted {
            host: host.to_string(),
            status,
            detail: truncated,
            duration_ms: ms,
        });
    }
}

/// Bounded variant for callers that need backpressure. The current MVP uses
/// the unbounded sender for simplicity; bounded support is reserved for
/// future per-line streaming (Phase 8).
pub struct BoundedEventSender {
    pub tx: Sender<TuiEvent>,
}

fn truncate_detail(s: &str, max_lines: usize) -> String {
    let mut lines: Vec<&str> = s.lines().take(max_lines + 1).collect();
    let overflowed = lines.len() > max_lines;
    if overflowed {
        lines.truncate(max_lines);
    }
    let mut joined = lines.join(" ↵ ");
    if overflowed {
        joined.push_str(" …");
    }
    joined
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_detail_short_unchanged() {
        assert_eq!(truncate_detail("one line", 3), "one line");
    }

    #[test]
    fn truncate_detail_three_lines_joined() {
        assert_eq!(
            truncate_detail("a\nb\nc", 3),
            "a ↵ b ↵ c"
        );
    }

    #[test]
    fn truncate_detail_overflow_appends_ellipsis() {
        let out = truncate_detail("a\nb\nc\nd\ne", 3);
        assert!(out.starts_with("a ↵ b ↵ c"));
        assert!(out.ends_with(" …"));
    }
}

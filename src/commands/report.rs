//! Typed command-result and progress-sink contracts shared by command-core
//! functions (`check_core`, future `run_core`/`exec_core`/`sync_core`/
//! `checkout_core`) and their consumers (CLI wrapper printing to stdout,
//! TUI bridging to a `tokio::mpsc` channel).
//!
//! Per docs/tui_reconstruct_plan.md AD-14 and §7.5: putting these types
//! in `commands::report` keeps `*_core` functions independent of the
//! output layer; `output::report` is a thin downstream consumer.

use serde::Serialize;

/// Per-host outcome of a command operation.
///
/// Variants are command-agnostic so the same enum serves as a progress signal
/// for `check`, `run`, `exec`, and `sync`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
#[allow(dead_code)]
pub enum HostStatus {
    /// All probes succeeded / command exited 0.
    Online,
    /// Some probes succeeded, some failed (non-fatal).
    Partial,
    /// Host responded but reported offline / command exited non-zero.
    Offline,
    /// SshPool::setup could not establish a connection.
    Unreachable,
    /// Per-host timeout fired (`ctx.timeout`).
    TimedOut,
    /// Other transport / IO error during the run.
    Error,
    /// Host was intentionally skipped (e.g. shell mismatch in `exec`).
    Skipped,
}

/// Sink for per-host progress events emitted during a command-core run.
///
/// CLI wraps this with a printer impl; the TUI's AsyncBridge wraps it
/// with a channel sender impl. `*_core` functions should never call
/// `output::printer` directly.
pub trait ProgressSink: Send + Sync {
    fn host_started(&self, host: &str);
    fn host_completed(&self, host: &str, status: HostStatus, detail: &str, ms: u64);
}

/// Per-host typed result of a `check_core` run.
///
/// Carries a `serde_json::Value` for the dynamic metrics blob (the metrics
/// schema varies by host shell), but all top-level host attributes are
/// typed so consumers do not need to reach into `serde_json::Value` to
/// determine status, duration, or detail.
#[derive(Debug, Clone, Serialize)]
pub struct CheckHostResult {
    pub host: String,
    pub status: HostStatus,
    pub duration_ms: Option<u64>,
    /// Human-readable error / warning / summary detail.
    pub detail: String,
    pub metrics_succeeded: usize,
    pub metrics_failed: usize,
    /// Raw metrics blob (may be empty for unreachable / errored hosts).
    pub data: serde_json::Value,
    pub raw_stdout: String,
    pub raw_stderr: String,
}

/// Typed return value of `check_core`.
#[derive(Debug, Clone, Serialize)]
pub struct CheckReport {
    /// RFC 3339 timestamp captured at the start of the run.
    pub executed_at: String,
    /// Distinct metrics enabled across all matched [[check]] entries.
    pub enabled_metrics: Vec<String>,
    /// Names of all targeted hosts (reachable and unreachable).
    pub targets: Vec<String>,
    pub hosts: Vec<CheckHostResult>,
}

/// Top-level typed result enum returned by `*_core` functions.
///
/// MVP scope (per AD-14): only `Check` is implemented. `Run`, `Exec`,
/// `Sync`, and `Checkout` variants land alongside their `*_core` extraction
/// in Phases 4–6.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "command", rename_all = "lowercase")]
pub enum CommandReport {
    Check(CheckReport),
    Run(RunReport),
    Exec(ExecReport),
}

// ── Run ──────────────────────────────────────────────────────────────────────

/// Per-host result of a `run_core` invocation.
#[derive(Debug, Clone, Serialize)]
pub struct RunHostResult {
    pub host: String,
    pub status: HostStatus,
    pub duration_ms: Option<u64>,
    /// Human-readable summary (success: truncated stdout, failure: stderr).
    pub detail: String,
    pub stdout: String,
    pub stderr: String,
}

/// Typed return value of `run_core`.
#[derive(Debug, Clone, Serialize)]
pub struct RunReport {
    pub executed_at: String,
    pub command: String,
    pub targets: Vec<String>,
    pub hosts: Vec<RunHostResult>,
}

// ── Exec ─────────────────────────────────────────────────────────────────────

/// Per-host result of an `exec_core` invocation.
#[derive(Debug, Clone, Serialize)]
pub struct ExecHostResult {
    pub host: String,
    pub status: HostStatus,
    pub duration_ms: Option<u64>,
    /// Human-readable summary.
    pub detail: String,
    pub stdout: String,
    pub stderr: String,
}

/// Typed return value of `exec_core`.
#[derive(Debug, Clone, Serialize)]
pub struct ExecReport {
    pub executed_at: String,
    pub script: String,
    pub targets: Vec<String>,
    pub hosts: Vec<ExecHostResult>,
}

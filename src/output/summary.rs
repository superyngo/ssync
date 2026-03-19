use std::collections::{BTreeMap, BTreeSet};

/// Reason for skipping a file during sync.
#[derive(Debug)]
pub struct SkipReason {
    pub path: String,
    pub host: String,
    pub reason: String,
}

/// A single error entry with optional file-path context for deduplication.
#[derive(Debug)]
pub struct ErrorEntry {
    pub host: String,
    pub message: String,
    pub path: Option<String>,
}

/// Execution summary for a batch operation.
#[derive(Default)]
pub struct Summary {
    pub succeeded: usize,
    pub failed: usize,
    pub skipped: usize,
    pub errors: Vec<ErrorEntry>,
    pub skip_reasons: Vec<SkipReason>,
}

impl Summary {
    pub fn add_success(&mut self) {
        self.succeeded += 1;
    }

    pub fn add_failure(&mut self, host: &str, message: &str) {
        self.failed += 1;
        self.errors.push(ErrorEntry {
            host: host.to_string(),
            message: message.to_string(),
            path: None,
        });
    }

    #[allow(dead_code)]
    pub fn add_failure_with_path(&mut self, host: &str, message: &str, path: &str) {
        self.failed += 1;
        self.errors.push(ErrorEntry {
            host: host.to_string(),
            message: message.to_string(),
            path: Some(path.to_string()),
        });
    }

    pub fn add_skip(&mut self) {
        self.skipped += 1;
    }

    #[allow(dead_code)]
    pub fn add_skip_with_reason(&mut self, path: &str, host: &str, reason: &str) {
        self.skipped += 1;
        self.skip_reasons.push(SkipReason {
            path: path.to_string(),
            host: host.to_string(),
            reason: reason.to_string(),
        });
    }

    pub fn print(&self) {
        println!();
        println!("── Summary ──────────────────────────────");
        print!("  {} succeeded", self.succeeded);
        if self.failed > 0 {
            print!("  {} failed", self.failed);
        }
        if self.skipped > 0 {
            print!("  {} skipped", self.skipped);
        }
        println!();

        if !self.errors.is_empty() {
            println!("  Errors:");

            // Separate errors with file-path context from those without.
            let mut pathless_seen: BTreeSet<(String, String)> = BTreeSet::new();
            let mut pathless: Vec<(String, String)> = Vec::new();
            let mut by_host_msg: BTreeMap<(String, String), BTreeSet<String>> = BTreeMap::new();

            for entry in &self.errors {
                match &entry.path {
                    Some(path) => {
                        by_host_msg
                            .entry((entry.host.clone(), entry.message.clone()))
                            .or_default()
                            .insert(path.clone());
                    }
                    None => {
                        let key = (entry.host.clone(), entry.message.clone());
                        if pathless_seen.insert(key.clone()) {
                            pathless.push(key);
                        }
                    }
                }
            }

            // Print pathless errors (deduplicated)
            for (host, msg) in &pathless {
                println!("    {}: {}", host, msg);
            }

            // Cluster path-bearing errors: group (host, msg) pairs that
            // share the exact same set of affected files.
            let mut clusters: BTreeMap<Vec<String>, Vec<(String, String)>> = BTreeMap::new();
            for ((host, msg), paths) in &by_host_msg {
                let sorted_paths: Vec<String> = paths.iter().cloned().collect();
                clusters
                    .entry(sorted_paths)
                    .or_default()
                    .push((host.clone(), msg.clone()));
            }

            for (paths, host_msgs) in &clusters {
                println!("    [{}]", paths.join(", "));
                for (host, msg) in host_msgs {
                    println!("      {}: {}", host, msg);
                }
            }
        }

        if !self.skip_reasons.is_empty() {
            println!("  Skipped:");
            for sr in &self.skip_reasons {
                println!("    {} ({}): {}", sr.path, sr.host, sr.reason);
            }
        }
    }
}

/// Sync-specific execution summary with dual-perspective statistics.
///
/// File-level: tracks how many files were fully synced, partially synced, or failed.
/// Transfer-level: tracks individual host transfer outcomes (passed/synced/failed).
#[derive(Default)]
pub struct SyncSummary {
    // File-level counters
    pub files_synced: usize,
    pub files_partial: usize,
    pub files_failed: usize,
    pub files_skipped: usize,

    // Transfer-level counters
    pub transfers_passed: usize,
    pub transfers_synced: usize,
    pub transfers_failed: usize,

    // Transfer-level hostname tracking
    pub transfers_passed_hosts: Vec<String>,
    pub transfers_synced_hosts: Vec<String>,
    pub transfers_failed_hosts: Vec<String>,

    // Error/skip details (reuse existing types)
    pub errors: Vec<ErrorEntry>,
    pub skip_reasons: Vec<SkipReason>,
}

impl SyncSummary {
    /// Deduplicate a hostname list (preserving first-seen order) and join with ", ".
    pub fn format_hosts(hosts: &[String]) -> String {
        let mut seen = std::collections::HashSet::new();
        let mut order = Vec::new();
        for h in hosts {
            if seen.insert(h) {
                order.push(h.as_str());
            }
        }
        order.join(", ")
    }

    /// Record the result of syncing a single file across hosts.
    ///
    /// - `passed`: hosts already in sync (no transfer needed)
    /// - `synced`: hosts successfully transferred to
    /// - `failed`: hosts where transfer failed (with error messages)
    pub fn complete_file(
        &mut self,
        path: &str,
        passed: &[String],
        synced: &[String],
        failed: &[(String, String)],
    ) {
        self.transfers_passed += passed.len();
        self.transfers_synced += synced.len();
        self.transfers_failed += failed.len();

        self.transfers_passed_hosts.extend_from_slice(passed);
        self.transfers_synced_hosts.extend_from_slice(synced);

        for (host, msg) in failed {
            self.transfers_failed_hosts.push(host.clone());
            self.errors.push(ErrorEntry {
                host: host.clone(),
                message: msg.clone(),
                path: Some(path.to_string()),
            });
        }

        if failed.is_empty() {
            self.files_synced += 1;
        } else if !synced.is_empty() || !passed.is_empty() {
            self.files_partial += 1;
        } else {
            self.files_failed += 1;
        }
    }

    /// Record a file that is already fully in sync across all hosts (no transfers needed).
    pub fn file_in_sync(&mut self, passed_hosts: &[&str]) {
        self.transfers_passed += passed_hosts.len();
        self.transfers_passed_hosts.extend(passed_hosts.iter().map(|s| s.to_string()));
        self.files_synced += 1;
    }

    /// Record a host-level failure (unreachable, scp-unsupported) — not file-scoped.
    pub fn add_host_failure(&mut self, host: &str, message: &str) {
        self.transfers_failed += 1;
        self.transfers_failed_hosts.push(host.to_string());
        self.errors.push(ErrorEntry {
            host: host.to_string(),
            message: message.to_string(),
            path: None,
        });
    }

    /// Record a skipped file.
    pub fn add_skip_with_reason(&mut self, path: &str, host: &str, reason: &str) {
        self.files_skipped += 1;
        self.skip_reasons.push(SkipReason {
            path: path.to_string(),
            host: host.to_string(),
            reason: reason.to_string(),
        });
    }

    pub fn print(&self) {
        println!();
        println!("── Summary ──────────────────────────────");

        // File-level line
        let mut file_parts = Vec::new();
        if self.files_synced > 0 {
            file_parts.push(format!("{} synced", self.files_synced));
        }
        if self.files_partial > 0 {
            file_parts.push(format!("{} partial", self.files_partial));
        }
        if self.files_failed > 0 {
            file_parts.push(format!("{} failed", self.files_failed));
        }
        if self.files_skipped > 0 {
            file_parts.push(format!("{} skipped", self.files_skipped));
        }
        if !file_parts.is_empty() {
            println!("  Files:      {}", file_parts.join("  "));
        }

        // Transfer-level line
        let mut xfer_parts = Vec::new();
        if self.transfers_passed > 0 {
            let hosts = Self::format_hosts(&self.transfers_passed_hosts);
            xfer_parts.push(format!("{} passed({})", self.transfers_passed, hosts));
        }
        if self.transfers_synced > 0 {
            let hosts = Self::format_hosts(&self.transfers_synced_hosts);
            xfer_parts.push(format!("{} synced({})", self.transfers_synced, hosts));
        }
        if self.transfers_failed > 0 {
            let hosts = Self::format_hosts(&self.transfers_failed_hosts);
            xfer_parts.push(format!("{} failed({})", self.transfers_failed, hosts));
        }
        if !xfer_parts.is_empty() {
            println!("  Transfers:  {}", xfer_parts.join("  "));
        }

        if !self.errors.is_empty() {
            println!("  Errors:");

            // Separate errors with file-path context from those without.
            let mut pathless_seen: BTreeSet<(String, String)> = BTreeSet::new();
            let mut pathless: Vec<(String, String)> = Vec::new();
            let mut by_host_msg: BTreeMap<(String, String), BTreeSet<String>> = BTreeMap::new();

            for entry in &self.errors {
                match &entry.path {
                    Some(path) => {
                        by_host_msg
                            .entry((entry.host.clone(), entry.message.clone()))
                            .or_default()
                            .insert(path.clone());
                    }
                    None => {
                        let key = (entry.host.clone(), entry.message.clone());
                        if pathless_seen.insert(key.clone()) {
                            pathless.push(key);
                        }
                    }
                }
            }

            for (host, msg) in &pathless {
                println!("    {}: {}", host, msg);
            }

            let mut clusters: BTreeMap<Vec<String>, Vec<(String, String)>> = BTreeMap::new();
            for ((host, msg), paths) in &by_host_msg {
                let sorted_paths: Vec<String> = paths.iter().cloned().collect();
                clusters
                    .entry(sorted_paths)
                    .or_default()
                    .push((host.clone(), msg.clone()));
            }

            for (paths, host_msgs) in &clusters {
                println!("    [{}]", paths.join(", "));
                for (host, msg) in host_msgs {
                    println!("      {}: {}", host, msg);
                }
            }
        }

        if !self.skip_reasons.is_empty() {
            println!("  Skipped:");
            for sr in &self.skip_reasons {
                println!("    {} ({}): {}", sr.path, sr.host, sr.reason);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_skip_reason_recorded() {
        let mut s = Summary::default();
        s.add_skip_with_reason(
            "~/.bashrc",
            "host-a",
            "source 'host-a' does not have '~/.bashrc'",
        );
        assert_eq!(s.skipped, 1);
        assert_eq!(s.skip_reasons.len(), 1);
        assert_eq!(s.skip_reasons[0].path, "~/.bashrc");
        assert_eq!(s.skip_reasons[0].host, "host-a");
    }

    #[test]
    fn test_summary_counts() {
        let mut s = Summary::default();
        s.add_success();
        s.add_success();
        s.add_failure("host-x", "timeout");
        s.add_skip_with_reason("~/f", "h", "missing");
        assert_eq!(s.succeeded, 2);
        assert_eq!(s.failed, 1);
        assert_eq!(s.skipped, 1);
    }

    #[test]
    fn test_errors_dedup_pathless() {
        let mut s = Summary::default();
        s.add_failure("host-a", "connection refused");
        s.add_failure("host-a", "connection refused");
        s.add_failure("host-b", "timeout");
        // Pathless errors are deduplicated by (host, message)
        assert_eq!(s.failed, 3);
        assert_eq!(s.errors.len(), 3);
    }

    #[test]
    fn test_errors_with_path_grouping() {
        let mut s = Summary::default();
        // Same error on two hosts across multiple files
        s.add_failure_with_path("host-a", "scp failed", "~/.zshrc");
        s.add_failure_with_path("host-b", "scp failed", "~/.zshrc");
        s.add_failure_with_path("host-a", "scp failed", "~/.bashrc");
        s.add_failure_with_path("host-b", "scp failed", "~/.bashrc");
        assert_eq!(s.failed, 4);
        assert_eq!(s.errors.len(), 4);
        // All four share the same path set → should cluster into one group
        assert!(s.errors.iter().all(|e| e.path.is_some()));
    }

    // ── SyncSummary tests ──────────────────────────────

    #[test]
    fn test_sync_summary_file_in_sync() {
        let mut s = SyncSummary::default();
        s.file_in_sync(&["host-a", "host-b", "host-c"]);
        assert_eq!(s.files_synced, 1);
        assert_eq!(s.transfers_passed, 3);
        assert_eq!(s.transfers_synced, 0);
        assert_eq!(s.transfers_failed, 0);
    }

    #[test]
    fn test_sync_summary_complete_file_all_success() {
        let mut s = SyncSummary::default();
        s.complete_file(
            "~/.bashrc",
            &["host-a".to_string()],
            &["host-b".to_string(), "host-c".to_string()],
            &[],
        );
        assert_eq!(s.files_synced, 1);
        assert_eq!(s.files_partial, 0);
        assert_eq!(s.files_failed, 0);
        assert_eq!(s.transfers_passed, 1);
        assert_eq!(s.transfers_synced, 2);
        assert_eq!(s.transfers_failed, 0);
    }

    #[test]
    fn test_sync_summary_complete_file_partial() {
        let mut s = SyncSummary::default();
        s.complete_file(
            "~/.zshrc",
            &["host-a".to_string()],
            &["host-b".to_string()],
            &[("host-c".to_string(), "scp failed".to_string())],
        );
        assert_eq!(s.files_synced, 0);
        assert_eq!(s.files_partial, 1);
        assert_eq!(s.files_failed, 0);
        assert_eq!(s.transfers_passed, 1);
        assert_eq!(s.transfers_synced, 1);
        assert_eq!(s.transfers_failed, 1);
        assert_eq!(s.errors.len(), 1);
        assert_eq!(s.errors[0].host, "host-c");
        assert_eq!(s.errors[0].path, Some("~/.zshrc".to_string()));
    }

    #[test]
    fn test_sync_summary_complete_file_all_failed() {
        let mut s = SyncSummary::default();
        s.complete_file(
            "~/config",
            &[],
            &[],
            &[
                ("host-a".to_string(), "download failed".to_string()),
                ("host-b".to_string(), "download failed".to_string()),
            ],
        );
        assert_eq!(s.files_synced, 0);
        assert_eq!(s.files_partial, 0);
        assert_eq!(s.files_failed, 1);
        assert_eq!(s.transfers_failed, 2);
    }

    #[test]
    fn test_sync_summary_host_failure() {
        let mut s = SyncSummary::default();
        s.add_host_failure("host-x", "unreachable");
        assert_eq!(s.transfers_failed, 1);
        assert_eq!(s.errors.len(), 1);
        assert!(s.errors[0].path.is_none());
    }

    #[test]
    fn test_sync_summary_skip() {
        let mut s = SyncSummary::default();
        s.add_skip_with_reason("~/.vimrc", "host-a", "source missing");
        assert_eq!(s.files_skipped, 1);
        assert_eq!(s.skip_reasons.len(), 1);
    }

    #[test]
    fn test_sync_summary_mixed_scenario() {
        let mut s = SyncSummary::default();
        // Host failure
        s.add_host_failure("iphone12", "unreachable");
        // File 1: all in sync
        s.file_in_sync(&["macmini", "smbnfs", "realme", "debian"]);
        // File 2: partial success
        s.complete_file(
            "~/.gemini/oauth_creds.json",
            &[
                "smbnfs".to_string(),
                "realme".to_string(),
                "debian".to_string(),
            ],
            &[],
            &[("realmed".to_string(), "scp upload failed".to_string())],
        );
        // File 3: skipped
        s.add_skip_with_reason("~/.vimrc", "macmini", "source does not have file");

        assert_eq!(s.files_synced, 1);
        assert_eq!(s.files_partial, 1);
        assert_eq!(s.files_failed, 0);
        assert_eq!(s.files_skipped, 1);
        assert_eq!(s.transfers_passed, 4 + 3); // 4 from in_sync + 3 passed
        assert_eq!(s.transfers_synced, 0);
        assert_eq!(s.transfers_failed, 1 + 1); // 1 host failure + 1 file failure
    }

    #[test]
    fn test_sync_summary_tracks_passed_hosts() {
        let mut s = SyncSummary::default();
        s.file_in_sync(&["host-a", "host-b", "host-c"]);
        assert_eq!(s.transfers_passed_hosts, vec!["host-a", "host-b", "host-c"]);
    }

    #[test]
    fn test_sync_summary_tracks_synced_hosts() {
        let mut s = SyncSummary::default();
        s.complete_file(
            "~/.bashrc",
            &["host-a".to_string()],
            &["host-b".to_string(), "host-c".to_string()],
            &[],
        );
        assert_eq!(s.transfers_passed_hosts, vec!["host-a"]);
        assert_eq!(s.transfers_synced_hosts, vec!["host-b", "host-c"]);
    }

    #[test]
    fn test_sync_summary_tracks_failed_hosts() {
        let mut s = SyncSummary::default();
        s.complete_file(
            "~/config",
            &[],
            &[],
            &[
                ("host-a".to_string(), "download failed".to_string()),
                ("host-b".to_string(), "download failed".to_string()),
            ],
        );
        assert_eq!(s.transfers_failed_hosts, vec!["host-a", "host-b"]);
    }

    #[test]
    fn test_sync_summary_tracks_host_failure_hosts() {
        let mut s = SyncSummary::default();
        s.add_host_failure("host-x", "unreachable");
        assert_eq!(s.transfers_failed_hosts, vec!["host-x"]);
    }

    #[test]
    fn test_sync_summary_deduplicates_hosts_in_format() {
        let mut s = SyncSummary::default();
        s.complete_file("~/.bashrc", &[], &["host-a".to_string()], &[]);
        s.complete_file("~/.zshrc", &[], &["host-a".to_string()], &[]);
        assert_eq!(s.transfers_synced, 2);
        assert_eq!(SyncSummary::format_hosts(&s.transfers_synced_hosts), "host-a");
    }
}

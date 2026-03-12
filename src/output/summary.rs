/// Reason for skipping a file during sync.
#[derive(Debug)]
pub struct SkipReason {
    pub path: String,
    pub host: String,
    pub reason: String,
}

/// Execution summary for a batch operation.
#[derive(Default)]
pub struct Summary {
    pub succeeded: usize,
    pub failed: usize,
    pub skipped: usize,
    pub errors: Vec<(String, String)>, // (host, message)
    pub skip_reasons: Vec<SkipReason>,
}

impl Summary {
    pub fn add_success(&mut self) {
        self.succeeded += 1;
    }

    pub fn add_failure(&mut self, host: &str, message: &str) {
        self.failed += 1;
        self.errors.push((host.to_string(), message.to_string()));
    }

    pub fn add_skip(&mut self) {
        self.skipped += 1;
    }

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
            for (host, msg) in &self.errors {
                println!("    {}: {}", host, msg);
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
}

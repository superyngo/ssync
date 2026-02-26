/// Execution summary for a batch operation.
#[derive(Default)]
pub struct Summary {
    pub succeeded: usize,
    pub failed: usize,
    pub skipped: usize,
    pub errors: Vec<(String, String)>, // (host, message)
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
    }
}

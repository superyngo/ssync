use std::io::IsTerminal;

use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

pub struct SyncProgress {
    is_tty: bool,
    multi: MultiProgress,
    host_bar: Option<ProgressBar>,
    collect_bar: Option<ProgressBar>,
}

impl SyncProgress {
    pub fn new() -> Self {
        let is_tty = std::io::stderr().is_terminal();
        Self {
            is_tty,
            multi: MultiProgress::new(),
            host_bar: None,
            collect_bar: None,
        }
    }

    pub fn start_host_check(&mut self, total: usize) {
        if !self.is_tty {
            return;
        }
        let style = ProgressStyle::default_bar()
            .template(" {prefix:>12} {bar:30.cyan/dim} {pos}/{len} {msg}")
            .expect("valid template");
        let bar = self.multi.add(ProgressBar::new(total as u64));
        bar.set_style(style);
        bar.set_prefix("Hosts");
        self.host_bar = Some(bar);
    }

    #[allow(dead_code)]
    pub fn host_checked(&self, connected: usize, failed: usize) {
        if let Some(bar) = &self.host_bar {
            bar.inc(1);
            bar.set_message(format!("{connected} ok, {failed} failed"));
        }
    }

    pub fn finish_host_check(&mut self, connected: usize, failed: usize) {
        if let Some(bar) = self.host_bar.take() {
            bar.set_message(format!("{connected} ok, {failed} failed"));
            bar.finish();
        }
    }

    pub fn start_collect(&mut self, total: usize) {
        if !self.is_tty {
            return;
        }
        let style = ProgressStyle::default_bar()
            .template(" {prefix:>12} {bar:30.green/dim} {pos}/{len} {msg}")
            .expect("valid template");
        let bar = self.multi.add(ProgressBar::new(total as u64));
        bar.set_style(style);
        bar.set_prefix("Collecting");
        self.collect_bar = Some(bar);
    }

    pub fn host_collected(&self) {
        if let Some(bar) = &self.collect_bar {
            bar.inc(1);
        }
    }

    pub fn finish_collect(&mut self) {
        if let Some(bar) = self.collect_bar.take() {
            bar.finish();
        }
    }

    pub fn clear(&self) {
        if !self.is_tty {
            return;
        }
        let _ = self.multi.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_progress_non_tty_no_panic() {
        // Exercise all methods — should never panic regardless of TTY status.
        let mut progress = SyncProgress::new();
        // Force non-TTY mode to test the no-op paths.
        progress.is_tty = false;

        progress.start_host_check(5);
        progress.host_checked(1, 0);
        progress.host_checked(2, 1);
        progress.finish_host_check(2, 1);

        progress.start_collect(3);
        progress.host_collected();
        progress.host_collected();
        progress.finish_collect();

        progress.clear();
    }
}

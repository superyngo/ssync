use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Semaphore;

/// Dual-level concurrency limiter: global cap + per-host cap.
/// Acquire both permits before any SSH/SCP operation.
pub struct ConcurrencyLimiter {
    global: Arc<Semaphore>,
    per_host: HashMap<String, Arc<Semaphore>>,
}

impl ConcurrencyLimiter {
    /// Create a new limiter with global and per-host caps.
    /// `hosts` is the list of host names that will be used.
    pub fn new(global_limit: usize, per_host_limit: usize, hosts: &[String]) -> Self {
        let mut per_host = HashMap::new();
        for host in hosts {
            per_host.insert(host.clone(), Arc::new(Semaphore::new(per_host_limit)));
        }
        Self {
            global: Arc::new(Semaphore::new(global_limit)),
            per_host,
        }
    }

    /// Acquire both global and per-host permits.
    /// Order: global first, then per-host (deterministic to prevent deadlock).
    /// Returns a guard that releases both permits on drop.
    pub async fn acquire(&self, host: &str) -> ConcurrencyPermit {
        let global_permit = self.global.clone().acquire_owned().await.unwrap();
        let per_host_sem = self
            .per_host
            .get(host)
            .expect("host not registered in limiter");
        let per_host_permit = per_host_sem.clone().acquire_owned().await.unwrap();
        ConcurrencyPermit {
            _global: global_permit,
            _per_host: per_host_permit,
        }
    }

    /// Get a clone of the global semaphore (for use in spawned tasks).
    pub fn global_semaphore(&self) -> Arc<Semaphore> {
        self.global.clone()
    }

    /// Get a clone of a per-host semaphore (for use in spawned tasks).
    pub fn per_host_semaphore(&self, host: &str) -> Option<Arc<Semaphore>> {
        self.per_host.get(host).cloned()
    }
}

/// RAII guard that holds both global and per-host semaphore permits.
pub struct ConcurrencyPermit {
    _global: tokio::sync::OwnedSemaphorePermit,
    _per_host: tokio::sync::OwnedSemaphorePermit,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::Duration;

    #[tokio::test]
    async fn test_global_limit_respected() {
        let hosts = vec!["a".into(), "b".into(), "c".into()];
        let limiter = ConcurrencyLimiter::new(2, 10, &hosts);

        let _p1 = limiter.acquire("a").await;
        let _p2 = limiter.acquire("b").await;

        let result = tokio::time::timeout(Duration::from_millis(50), limiter.acquire("c")).await;
        assert!(
            result.is_err(),
            "Third acquire should block when global limit is 2"
        );
    }

    #[tokio::test]
    async fn test_per_host_limit_respected() {
        let hosts = vec!["a".into()];
        let limiter = ConcurrencyLimiter::new(10, 2, &hosts);

        let _p1 = limiter.acquire("a").await;
        let _p2 = limiter.acquire("a").await;

        let result = tokio::time::timeout(Duration::from_millis(50), limiter.acquire("a")).await;
        assert!(
            result.is_err(),
            "Third acquire on same host should block when per-host limit is 2"
        );
    }

    #[tokio::test]
    async fn test_permits_released_on_drop() {
        let hosts = vec!["a".into()];
        let limiter = ConcurrencyLimiter::new(1, 1, &hosts);

        {
            let _p = limiter.acquire("a").await;
        }
        // permit dropped — should be acquirable again
        let result = tokio::time::timeout(Duration::from_millis(50), limiter.acquire("a")).await;
        assert!(
            result.is_ok(),
            "Should acquire after previous permit dropped"
        );
    }
}

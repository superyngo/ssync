use anyhow::Result;
use serde_json::Value;

use super::probes;
use crate::config::schema::HostEntry;

/// Result of metric collection with success/failure tracking.
pub struct CollectionResult {
    pub data: Value,
    pub succeeded: usize,
    pub failed: usize,
    pub errors: Vec<String>,
    /// Raw stdout from the metrics batch SSH call (empty string if no metrics were collected).
    pub metrics_raw_stdout: String,
    /// Raw stderr from the metrics batch SSH call.
    pub metrics_raw_stderr: String,
}

/// Collect all enabled metrics from a remote host using batched SSH commands.
/// Uses 1 SSH call for metrics + 1 for paths (instead of N+M).
/// Requires a ControlMaster socket for connection reuse.
pub async fn collect_pooled(
    host: &HostEntry,
    enabled: &[String],
    check_paths: &[(String, String)], // (path, label)
    timeout_secs: u64,
    sessions: std::sync::Arc<crate::host::session_pool::RusshSessionPool>,
) -> Result<CollectionResult> {
    let mut result = serde_json::Map::new();
    result.insert("schema_version".to_string(), Value::Number(1.into()));

    let mut succeeded: usize = 0;
    let mut failed: usize = 0;
    let mut errors: Vec<String> = Vec::new();
    let mut metrics_raw_stdout = String::new();
    let mut metrics_raw_stderr = String::new();

    // Batch all metrics into a single SSH call
    if !enabled.is_empty() {
        let batch_cmd = probes::batch_command(host.shell, enabled);
        if !batch_cmd.is_empty() {
            match sessions
                .exec(&host.ssh_host, &batch_cmd, timeout_secs)
                .await
            {
                Ok(output) if output.success => {
                    metrics_raw_stdout = output.stdout;
                    metrics_raw_stderr = output.stderr;
                    let parsed =
                        super::parser::parse_batch(host.shell, enabled, &metrics_raw_stdout);
                    for metric in enabled {
                        if let Some(value) = parsed.get(metric) {
                            result.insert(metric.clone(), value.clone());
                            succeeded += 1;
                        } else {
                            let msg = format!("{}: no output in batch", metric);
                            failed += 1;
                            errors.push(msg);
                        }
                    }
                }
                Ok(output) => {
                    // Partial: try to parse what we got even if exit code non-zero
                    metrics_raw_stdout = output.stdout;
                    metrics_raw_stderr = output.stderr;
                    let parsed =
                        super::parser::parse_batch(host.shell, enabled, &metrics_raw_stdout);
                    for metric in enabled {
                        if let Some(value) = parsed.get(metric) {
                            result.insert(metric.clone(), value.clone());
                            succeeded += 1;
                        } else {
                            let msg = format!("{}: {}", metric, metrics_raw_stderr.trim());
                            failed += 1;
                            errors.push(msg);
                        }
                    }
                }
                Err(e) => {
                    let msg = format!("batch metrics: {}", e);
                    tracing::warn!(host = %host.name, error = %e, "Batch metric collection error");
                    failed += enabled.len();
                    errors.push(msg);
                }
            }
        }
    }

    // Batch all path checks into a single SSH call
    if !check_paths.is_empty() {
        let batch_cmd = probes::batch_path_command(host.shell, check_paths);
        if !batch_cmd.is_empty() {
            match sessions
                .exec(&host.ssh_host, &batch_cmd, timeout_secs)
                .await
            {
                Ok(output) => {
                    let parsed =
                        super::parser::parse_batch_paths(host.shell, check_paths, &output.stdout);
                    let mut paths_arr = Vec::new();
                    for (path, label) in check_paths {
                        if let Some(size_opt) = parsed.get(label.as_str()) {
                            if let Some(size) = size_opt {
                                let mut entry = serde_json::Map::new();
                                entry.insert("label".to_string(), Value::String(label.clone()));
                                entry.insert("path".to_string(), Value::String(path.clone()));
                                entry.insert(
                                    "size_bytes".to_string(),
                                    Value::Number((*size).into()),
                                );
                                paths_arr.push(Value::Object(entry));
                                succeeded += 1;
                            } else {
                                let msg = format!("path({}): MISSING", label);
                                failed += 1;
                                errors.push(msg);
                            }
                        } else {
                            let msg = format!("path({}): no output in batch", label);
                            failed += 1;
                            errors.push(msg);
                        }
                    }
                    result.insert("paths".to_string(), Value::Array(paths_arr));
                }
                Err(e) => {
                    let msg = format!("batch paths: {}", e);
                    failed += check_paths.len();
                    errors.push(msg);
                }
            }
        }
    }

    Ok(CollectionResult {
        data: Value::Object(result),
        succeeded,
        failed,
        errors,
        metrics_raw_stdout,
        metrics_raw_stderr,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_collection_result_defaults() {
        let cr = CollectionResult {
            data: serde_json::json!({"schema_version": 1}),
            succeeded: 0,
            failed: 0,
            errors: vec![],
            metrics_raw_stdout: String::new(),
            metrics_raw_stderr: String::new(),
        };
        assert_eq!(cr.succeeded, 0);
        assert_eq!(cr.failed, 0);
        assert!(cr.errors.is_empty());
    }
}

use anyhow::Result;
use serde_json::Value;

use crate::config::schema::HostEntry;
use crate::host::executor;
use super::probes;

/// Result of metric collection with success/failure tracking.
pub struct CollectionResult {
    pub data: Value,
    pub succeeded: usize,
    pub failed: usize,
    pub errors: Vec<String>,
}

/// Collect all enabled metrics from a remote host.
/// Returns a CollectionResult tracking per-metric success/failure.
pub async fn collect(
    host: &HostEntry,
    enabled: &[String],
    check_paths: &[(String, String)], // (path, label)
    timeout_secs: u64,
) -> Result<CollectionResult> {
    let mut result = serde_json::Map::new();
    result.insert("schema_version".to_string(), Value::Number(1.into()));

    let mut succeeded: usize = 0;
    let mut failed: usize = 0;
    let mut errors: Vec<String> = Vec::new();

    for metric in enabled {
        let command = probes::command_for(host.shell, metric);
        if command.is_empty() {
            continue;
        }

        match executor::run_remote(host, &command, timeout_secs).await {
            Ok(output) if output.success => {
                let parsed = super::parser::parse(host.shell, metric, &output.stdout);
                result.insert(metric.clone(), parsed);
                succeeded += 1;
            }
            Ok(output) => {
                let msg = format!("{}: {}", metric, output.stderr.trim());
                tracing::warn!(
                    host = %host.name,
                    metric = %metric,
                    stderr = %output.stderr.trim(),
                    "Metric collection failed"
                );
                failed += 1;
                errors.push(msg);
            }
            Err(e) => {
                let msg = format!("{}: {}", metric, e);
                tracing::warn!(host = %host.name, metric = %metric, error = %e, "Metric collection error");
                failed += 1;
                errors.push(msg);
            }
        }
    }

    // Collect path capacities
    if !check_paths.is_empty() {
        let mut paths_arr = Vec::new();
        for (path, label) in check_paths {
            let cmd = probes::path_size_command(host.shell, path);
            match executor::run_remote(host, &cmd, timeout_secs).await {
                Ok(output) if output.success => {
                    let size = super::parser::parse_path_size(host.shell, &output.stdout);
                    let mut entry = serde_json::Map::new();
                    entry.insert("label".to_string(), Value::String(label.clone()));
                    entry.insert("path".to_string(), Value::String(path.clone()));
                    entry.insert("size_bytes".to_string(), Value::Number(size.into()));
                    paths_arr.push(Value::Object(entry));
                    succeeded += 1;
                }
                Ok(output) => {
                    let msg = format!("path({}): {}", label, output.stderr.trim());
                    failed += 1;
                    errors.push(msg);
                }
                Err(e) => {
                    let msg = format!("path({}): {}", label, e);
                    failed += 1;
                    errors.push(msg);
                }
            }
        }
        result.insert("paths".to_string(), Value::Array(paths_arr));
    }

    Ok(CollectionResult {
        data: Value::Object(result),
        succeeded,
        failed,
        errors,
    })
}

use anyhow::{bail, Result};
use serde::Serialize;

use crate::commands::TargetMode;

#[derive(Serialize)]
pub struct OperationReport {
    pub executed_at: String,
    pub command: String,
    pub filter: FilterInfo,
    pub task: serde_json::Value,
    pub targets: Vec<String>,
    pub results: Vec<HostResult>,
    pub summary: ReportSummary,
}

#[derive(Serialize)]
pub struct FilterInfo {
    pub mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub values: Option<Vec<String>>,
}

impl FilterInfo {
    pub fn from_mode(mode: &TargetMode) -> Self {
        match mode {
            TargetMode::All => FilterInfo {
                mode: "all".to_string(),
                values: None,
            },
            TargetMode::Groups(g) => FilterInfo {
                mode: "groups".to_string(),
                values: Some(g.clone()),
            },
            TargetMode::Hosts(h) => FilterInfo {
                mode: "hosts".to_string(),
                values: Some(h.clone()),
            },
            TargetMode::Shell(s) => FilterInfo {
                mode: "shell".to_string(),
                values: Some(s.iter().map(|sh| sh.to_string()).collect()),
            },
        }
    }
}

#[derive(Serialize)]
pub struct HostResult {
    pub host: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(flatten)]
    pub output: serde_json::Value,
}

#[derive(Serialize, Default)]
pub struct ReportSummary {
    pub total: usize,
    pub success: usize,
    pub failed: usize,
    pub skipped: usize,
}

/// Write `report` to a file. Path semantics:
/// - `""` → auto-generate `ssync-{command}-{YYYYMMDD-HHmmss}.{fmt}` in CWD
/// - `*.json` → JSON
/// - `*.html` → HTML
/// - other extension → error
///
/// Format priority: path extension > `configured_default` > "json".
pub fn write_report(
    report: &OperationReport,
    out: &str,
    command: &str,
    configured_default: Option<&str>,
) -> Result<()> {
    use anyhow::Context;
    use std::path::Path;

    let auto_generated = out.is_empty();
    let default_ext = configured_default.unwrap_or("json");
    let auto_ext = if auto_generated { default_ext } else { "" };

    let path = if auto_generated {
        let ts = chrono::Local::now().format("%Y%m%d-%H%M%S");
        format!("ssync-{}-{}.{}", command, ts, auto_ext)
    } else {
        out.to_string()
    };

    let ext = Path::new(&path)
        .extension()
        .and_then(|e| e.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(default_ext)
        .to_lowercase();

    let content = match ext.as_str() {
        "json" => serde_json::to_string_pretty(report)?,
        "html" => render_html_report(report),
        other => {
            bail!("Unsupported output format '.{}'. Use .json or .html", other);
        }
    };

    if auto_generated {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .with_context(|| {
                format!("Failed to create report file '{}' (already exists?)", path)
            })?;
        f.write_all(content.as_bytes())
            .with_context(|| format!("Failed to write report to '{}'", path))?;
    } else {
        std::fs::write(&path, &content)
            .with_context(|| format!("Failed to write report to '{}'", path))?;
    }

    println!("Report written to {}", path);
    Ok(())
}

fn render_html_report(report: &OperationReport) -> String {
    let filter_str = match &report.filter.values {
        Some(vals) => format!("{}: {}", report.filter.mode, vals.join(", ")),
        None => report.filter.mode.clone(),
    };

    let rows = report
        .results
        .iter()
        .map(|r| {
            let duration = r
                .duration_ms
                .map(|ms| format!("{}ms", ms))
                .unwrap_or_else(|| "—".to_string());
            let status_class = match r.status.as_str() {
                "success" => "status-ok",
                "error" => "status-err",
                _ => "status-skip",
            };
            let output_html = render_output_html(&r.output);
            let output_raw = serde_json::to_string_pretty(&r.output)
                .unwrap_or_else(|e| format!("(serialization error: {})", e));
            format!(
                r#"<tr>
  <td>{host}</td>
  <td><span class="{cls}">{status}</span></td>
  <td>{duration}</td>
  <td class="output-cell">
    {output}
    <details>
      <summary>Raw output JSON</summary>
      <pre>{output_raw}</pre>
    </details>
  </td>
</tr>"#,
                host = html_escape(&r.host),
                cls = status_class,
                status = html_escape(&r.status),
                duration = duration,
                output = output_html,
                output_raw = html_escape(&output_raw),
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    let task_json = serde_json::to_string_pretty(&report.task)
        .unwrap_or_else(|e| format!("(serialization error: {})", e));
    let targets_json = serde_json::to_string_pretty(&report.targets)
        .unwrap_or_else(|e| format!("(serialization error: {})", e));

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>ssync {command} report</title>
<style>
  body {{ font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; margin: 2rem; background: #f8f9fa; color: #212529; }}
  h1 {{ font-size: 1.4rem; margin-bottom: 0.25rem; }}
  .meta {{ color: #6c757d; font-size: 0.9rem; margin-bottom: 1.5rem; }}
  .summary {{ display: flex; gap: 1rem; margin-bottom: 1.5rem; }}
  .badge {{ padding: 0.35rem 0.75rem; border-radius: 4px; font-weight: 600; font-size: 0.85rem; }}
  .badge-total {{ background: #e9ecef; }}
  .badge-ok {{ background: #d4edda; color: #155724; }}
  .badge-err {{ background: #f8d7da; color: #721c24; }}
  .badge-skip {{ background: #fff3cd; color: #856404; }}
  table {{ width: 100%; border-collapse: collapse; background: white; border-radius: 8px; overflow: hidden; box-shadow: 0 1px 3px rgba(0,0,0,0.1); }}
  th {{ background: #343a40; color: white; text-align: left; padding: 0.6rem 1rem; font-size: 0.85rem; }}
  td {{ padding: 0.6rem 1rem; border-bottom: 1px solid #dee2e6; vertical-align: top; font-size: 0.85rem; }}
  tr:last-child td {{ border-bottom: none; }}
  .status-ok {{ color: #28a745; font-weight: 600; }}
  .status-err {{ color: #dc3545; font-weight: 600; }}
  .status-skip {{ color: #ffc107; font-weight: 600; }}
  .output-cell {{ font-family: monospace; white-space: pre-wrap; word-break: break-all; max-width: 600px; }}
  details summary {{ cursor: pointer; color: #0066cc; }}
</style>
</head>
<body>
<h1>ssync {command} report</h1>
<div class="meta">
  <strong>Executed:</strong> {executed_at} &nbsp;|&nbsp;
  <strong>Filter:</strong> {filter}
</div>
<details>
  <summary>Task (JSON)</summary>
  <pre>{task_json}</pre>
</details>
<details>
  <summary>Targets (JSON)</summary>
  <pre>{targets_json}</pre>
</details>
<div class="summary">
  <span class="badge badge-total">Total: {total}</span>
  <span class="badge badge-ok">Success: {success}</span>
  <span class="badge badge-err">Failed: {failed}</span>
  <span class="badge badge-skip">Skipped: {skipped}</span>
</div>
<table>
<thead><tr><th>Host</th><th>Status</th><th>Duration</th><th>Output</th></tr></thead>
<tbody>
{rows}
</tbody>
</table>
</body>
</html>"#,
        command = html_escape(&report.command),
        executed_at = html_escape(&report.executed_at),
        filter = html_escape(&filter_str),
        task_json = html_escape(&task_json),
        targets_json = html_escape(&targets_json),
        total = report.summary.total,
        success = report.summary.success,
        failed = report.summary.failed,
        skipped = report.summary.skipped,
        rows = rows,
    )
}

fn render_output_html(output: &serde_json::Value) -> String {
    // check command: has "metrics" and "probe_outputs"
    if let Some(metrics) = output.get("metrics") {
        let mut html = String::from("<strong>Metrics:</strong><br>");
        if let Some(obj) = metrics.as_object() {
            for (k, v) in obj {
                html.push_str(&format!(
                    "{}: {}<br>",
                    html_escape(k),
                    html_escape(&v.to_string())
                ));
            }
        }
        if let Some(probes) = output.get("probe_outputs") {
            html.push_str("<details><summary>Raw probe output</summary><pre>");
            html.push_str(&html_escape(
                &serde_json::to_string_pretty(probes)
                    .unwrap_or_else(|e| format!("(serialization error: {})", e)),
            ));
            html.push_str("</pre></details>");
        }
        return html;
    }

    // sync command: has "files_synced" and "files_skipped"
    if let Some(synced) = output.get("files_synced") {
        let mut html = String::new();
        if let Some(arr) = synced.as_array() {
            if !arr.is_empty() {
                html.push_str("<strong>Synced:</strong><br>");
                for f in arr {
                    let fallback = f.to_string();
                    let path_str = f.as_str().unwrap_or(&fallback);
                    html.push_str(&format!("  {}<br>", html_escape(path_str)));
                }
            }
        }
        if let Some(skipped) = output.get("files_skipped") {
            if let Some(arr) = skipped.as_array() {
                if !arr.is_empty() {
                    html.push_str("<strong>Skipped (in-sync):</strong><br>");
                    for f in arr {
                        let fallback = f.to_string();
                        let path_str = f.as_str().unwrap_or(&fallback);
                        html.push_str(&format!("  {}<br>", html_escape(path_str)));
                    }
                }
            }
        }
        if let Some(stderr) = output.get("stderr") {
            let s = stderr.as_str().unwrap_or("");
            if !s.is_empty() {
                html.push_str(&format!(
                    "<strong>stderr:</strong><pre>{}</pre>",
                    html_escape(s)
                ));
            }
        }
        return html;
    }

    // checkout: has "snapshot"
    if let Some(snap) = output.get("snapshot") {
        let online = output
            .get("online")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let collected_at = output
            .get("collected_at")
            .and_then(|v| v.as_str())
            .unwrap_or("—");
        return format!(
            "Online: {} | Collected: {}<details><summary>Snapshot</summary><pre>{}</pre></details>",
            if online { "✓" } else { "✗" },
            html_escape(collected_at),
            html_escape(
                &serde_json::to_string_pretty(snap)
                    .unwrap_or_else(|e| format!("(serialization error: {})", e))
            ),
        );
    }

    // run/exec: stdout/stderr
    let stdout = output.get("stdout").and_then(|v| v.as_str()).unwrap_or("");
    let stderr = output.get("stderr").and_then(|v| v.as_str()).unwrap_or("");
    let mut html = String::new();
    if !stdout.is_empty() {
        html.push_str(&format!(
            "<strong>stdout:</strong><pre>{}</pre>",
            html_escape(stdout)
        ));
    }
    if !stderr.is_empty() {
        html.push_str(&format!(
            "<strong>stderr:</strong><pre>{}</pre>",
            html_escape(stderr)
        ));
    }
    html
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample_report(command: &str) -> OperationReport {
        OperationReport {
            executed_at: "2026-04-29T10:00:00Z".to_string(),
            command: command.to_string(),
            filter: FilterInfo {
                mode: "hosts".to_string(),
                values: Some(vec!["host1".to_string()]),
            },
            task: serde_json::json!({ "command": "echo hi" }),
            targets: vec!["host1".to_string()],
            results: vec![HostResult {
                host: "host1".to_string(),
                status: "success".to_string(),
                duration_ms: Some(42),
                output: serde_json::json!({ "stdout": "hi\n", "stderr": "" }),
            }],
            summary: ReportSummary {
                total: 1,
                success: 1,
                failed: 0,
                skipped: 0,
            },
        }
    }

    #[test]
    fn test_write_report_json_explicit_path() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("out.json").to_str().unwrap().to_string();
        let report = sample_report("run");
        write_report(&report, &path, "run", None).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(v["command"], "run");
        assert_eq!(v["summary"]["total"], 1);
        assert_eq!(v["filter"]["mode"], "hosts");
    }

    #[test]
    fn test_write_report_html_explicit_path() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("out.html").to_str().unwrap().to_string();
        let report = sample_report("run");
        write_report(&report, &path, "run", None).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("<!DOCTYPE html>"));
        assert!(content.contains("host1"));
        assert!(content.contains("success"));
    }

    #[test]
    fn test_write_report_auto_filename() {
        let dir = TempDir::new().unwrap();
        // Write to an explicit empty-named path inside the temp dir to avoid CWD races.
        let report = sample_report("check");
        let out_path = dir.path().join("ssync-check-test.json");
        write_report(&report, out_path.to_str().unwrap(), "check", None).unwrap();
        let content = std::fs::read_to_string(&out_path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(v["command"], "check");
    }

    #[test]
    fn test_write_report_unsupported_extension() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("out.csv").to_str().unwrap().to_string();
        let report = sample_report("run");
        let result = write_report(&report, &path, "run", None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Unsupported"));
    }

    #[test]
    fn test_write_report_configured_default_html() {
        let dir = TempDir::new().unwrap();
        let report = sample_report("check");
        // Use --out without extension → should use configured default "html"
        let out_path = dir.path().join("report");
        write_report(&report, out_path.to_str().unwrap(), "check", Some("html")).unwrap();
        // File should be written as HTML content since configured_default is "html"
        let content = std::fs::read_to_string(&out_path).unwrap();
        assert!(content.contains("<!DOCTYPE html>"));
    }

    #[test]
    fn test_write_report_path_extension_overrides_configured_default() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("out.json").to_str().unwrap().to_string();
        let report = sample_report("run");
        // configured_default is "html" but path extension is .json → should produce JSON
        write_report(&report, &path, "run", Some("html")).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(v["command"], "run");
    }

    #[test]
    fn test_filter_info_all_has_no_values() {
        let fi = FilterInfo {
            mode: "all".to_string(),
            values: None,
        };
        let json = serde_json::to_string(&fi).unwrap();
        assert!(!json.contains("values"));
    }
}

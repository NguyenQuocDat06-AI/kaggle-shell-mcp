//! Async subprocess bridge to `python -m kaggle_mcp.bridge <action>` for Kaggle SDK ops.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::state::find_repo_root;

#[derive(Debug, Clone)]
pub struct PythonBridge {
    pub python_exe: String,
    pub workdir: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
struct BridgeResponse {
    ok: bool,
    #[serde(default)]
    data: Value,
    #[serde(default)]
    error: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct QuotaBucket {
    pub used_hours: f64,
    pub reserved_hours: f64,
    pub total_hours: f64,
    pub remaining_hours: f64,
    pub used_pct: f64,
    pub over_quota: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct QuotaSummary {
    pub username: String,
    pub gpu: QuotaBucket,
    pub tpu: QuotaBucket,
    pub quota_refresh_time: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct HubItem {
    pub category: String,
    #[serde(rename = "ref")]
    pub item_ref: String,
    pub title: String,
    pub info: String,
    pub metric: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct HubDetail {
    pub category: String,
    #[serde(rename = "ref")]
    pub item_ref: String,
    pub text: String,
    pub stage_kind: Option<String>,
    pub stage_value: String,
    pub can_download: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct RefreshEndpointRes {
    pub stopped: bool,
    pub updated: bool,
    pub message: String,
    pub tunnel_url: String,
    pub ts_url: String,
    pub base_url: String,
}

impl PythonBridge {
    pub fn discover(override_python: Option<String>) -> Self {
        let repo = find_repo_root();
        let python_exe = if let Some(p) = override_python.filter(|s| !s.trim().is_empty()) {
            p
        } else if let Some(ref root) = repo {
            let win = root.join(".venv").join("Scripts").join("python.exe");
            let posix = root.join(".venv").join("bin").join("python");
            if win.is_file() {
                win.to_string_lossy().to_string()
            } else if posix.is_file() {
                posix.to_string_lossy().to_string()
            } else {
                "python".to_string()
            }
        } else {
            "python".to_string()
        };
        Self {
            python_exe,
            workdir: repo,
        }
    }

    pub async fn call(&self, action: &str, payload: Value) -> Result<Value> {
        let mut cmd = Command::new(&self.python_exe);
        cmd.arg("-m").arg("kaggle_mcp.bridge").arg(action);
        cmd.env("PYTHONUTF8", "1").env("PYTHONIOENCODING", "utf-8");
        if let Some(ref wd) = self.workdir {
            if Path::new(wd).join("src").is_dir() {
                cmd.current_dir(wd);
            }
        }
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd
            .spawn()
            .with_context(|| format!("Failed to spawn '{}' for bridge '{}'", self.python_exe, action))?;

        if let Some(mut stdin) = child.stdin.take() {
            let bytes = serde_json::to_vec(&payload)?;
            let _ = stdin.write_all(&bytes).await;
            let _ = stdin.shutdown().await;
        }

        let output = child.wait_with_output().await?;
        let stdout_str = String::from_utf8_lossy(&output.stdout);
        let stderr_str = String::from_utf8_lossy(&output.stderr);

        let last_line = stdout_str
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("");

        if last_line.is_empty() {
            bail!(
                "Bridge '{}' returned no JSON (exit={:?}): {}",
                action,
                output.status.code(),
                stderr_str.trim()
            );
        }

        let resp: BridgeResponse = serde_json::from_str(last_line)
            .with_context(|| format!("Invalid JSON from bridge '{}': {}", action, last_line))?;

        if !resp.ok {
            bail!(
                "{}",
                if resp.error.is_empty() {
                    format!("Bridge '{}' failed", action)
                } else {
                    resp.error
                }
            );
        }
        Ok(resp.data)
    }
}

//! Local STATE_DIR persistence: sessions.json, profiles.json, activity.jsonl, mcp_flow.jsonl.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ActiveSession {
    pub session_id: String,
    pub worker_token: String,
    pub kernel_ref: String,
    pub state_dataset_ref: String,
    pub tunnel_url: String,
    pub ts_url: String,
    pub created_at: f64,
    pub idle_timeout_min: i64,
    pub slot: String,
    pub mode: String,
    pub accelerator: String,
    pub version_number: Option<i64>,
    pub competition: String,
    pub enable_internet: bool,
}

impl ActiveSession {
    pub fn base_url(&self) -> String {
        let raw = if !self.ts_url.trim().is_empty() {
            &self.ts_url
        } else {
            &self.tunnel_url
        };
        raw.trim().trim_end_matches('/').to_string()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct BootProfile {
    pub name: String,
    pub accelerator: String,
    pub slots: String,
    pub datasets: Vec<String>,
    pub models: Vec<String>,
    pub competition: String,
    pub idle_timeout_min: u32,
    pub chain_previous_output: bool,
}

impl Default for BootProfile {
    fn default() -> Self {
        Self {
            name: String::new(),
            accelerator: "NvidiaTeslaT4".to_string(),
            slots: "1".to_string(),
            datasets: Vec::new(),
            models: Vec::new(),
            competition: String::new(),
            idle_timeout_min: 0,
            chain_previous_output: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ActivityEvent {
    pub ts: f64,
    pub slot: String,
    pub caller: String,
    pub kind: String,
    pub summary: String,
    pub detail: String,
    pub exit_code: Option<i64>,
    pub duration_sec: Option<f64>,
    pub seq: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct McpFlowEvent {
    pub ts: f64,
    pub span_id: String,
    pub phase: String,
    pub caller: String,
    pub tool: String,
    pub slot: String,
    pub summary: String,
    pub duration_sec: Option<f64>,
    pub error: String,
}

#[derive(Debug, Deserialize)]
struct SessionsWrapper {
    #[serde(default)]
    slots: BTreeMap<String, ActiveSession>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ProfilesWrapper {
    #[serde(default)]
    profiles: Vec<BootProfile>,
}

pub fn find_repo_root() -> Option<PathBuf> {
    let mut seeds = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        seeds.push(cwd);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(p) = exe.parent() {
            seeds.push(p.to_path_buf());
        }
    }
    for seed in seeds {
        for anc in seed.ancestors() {
            if anc.join("pyproject.toml").is_file() && anc.join("src").join("kaggle_mcp").is_dir() {
                return Some(anc.to_path_buf());
            }
        }
    }
    None
}

pub fn resolve_state_dir(override_dir: Option<PathBuf>) -> PathBuf {
    if let Some(p) = override_dir {
        return p;
    }
    if let Ok(custom) = std::env::var("KAGGLE_MCP_HOME") {
        let trimmed = custom.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    if let Some(repo) = find_repo_root() {
        return repo.join(".kaggle_mcp");
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".kaggle_mcp")
}

pub fn normalize_slot(raw: &str) -> String {
    let s = raw.trim().to_ascii_lowercase();
    if s.is_empty() {
        "1".to_string()
    } else {
        s
    }
}

pub fn load_all_sessions(state_dir: &Path) -> BTreeMap<String, ActiveSession> {
    let path = state_dir.join("sessions.json");
    let Ok(txt) = fs::read_to_string(&path) else {
        return BTreeMap::new();
    };
    let mut out = BTreeMap::new();
    if let Ok(wrapper) = serde_json::from_str::<SessionsWrapper>(&txt) {
        if !wrapper.slots.is_empty() {
            for (k, mut v) in wrapper.slots {
                if !v.session_id.is_empty() {
                    let norm = normalize_slot(&k);
                    v.slot = norm.clone();
                    if v.mode.is_empty() {
                        v.mode = "live".to_string();
                    }
                    out.insert(norm, v);
                }
            }
            return out;
        }
    }
    if let Ok(flat) = serde_json::from_str::<BTreeMap<String, ActiveSession>>(&txt) {
        for (k, mut v) in flat {
            if !v.session_id.is_empty() {
                let norm = normalize_slot(&k);
                v.slot = norm.clone();
                if v.mode.is_empty() {
                    v.mode = "live".to_string();
                }
                out.insert(norm, v);
            }
        }
    }
    out
}

pub fn load_profiles(state_dir: &Path) -> Vec<BootProfile> {
    let path = state_dir.join("profiles.json");
    let Ok(txt) = fs::read_to_string(&path) else {
        return Vec::new();
    };
    let items: Vec<BootProfile> = if let Ok(wrap) = serde_json::from_str::<ProfilesWrapper>(&txt) {
        wrap.profiles
    } else if let Ok(list) = serde_json::from_str::<Vec<BootProfile>>(&txt) {
        list
    } else {
        return Vec::new();
    };
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for mut p in items {
        p.name = p.name.trim().to_string();
        if p.name.is_empty() {
            continue;
        }
        let key = p.name.to_ascii_lowercase();
        if seen.insert(key) {
            if p.accelerator.is_empty() {
                p.accelerator = "NvidiaTeslaT4".to_string();
            }
            if p.slots.is_empty() {
                p.slots = "1".to_string();
            }
            out.push(p);
        }
    }
    out
}

fn write_profiles_atomic(state_dir: &Path, items: &[BootProfile]) -> Result<()> {
    fs::create_dir_all(state_dir).with_context(|| format!("create {:?}", state_dir))?;
    let path = state_dir.join("profiles.json");
    let tmp = state_dir.join(format!(".profiles.json.{}.tmp", std::process::id()));
    let body = serde_json::to_string_pretty(&ProfilesWrapper {
        profiles: items.to_vec(),
    })?;
    fs::write(&tmp, body)?;
    fs::rename(&tmp, &path)?;
    Ok(())
}

pub fn save_profile(state_dir: &Path, mut profile: BootProfile) -> Result<Vec<BootProfile>> {
    profile.name = profile.name.trim().to_string();
    anyhow::ensure!(!profile.name.is_empty(), "Profile name cannot be empty");
    if profile.accelerator.trim().is_empty() {
        profile.accelerator = "NvidiaTeslaT4".to_string();
    }
    if profile.slots.trim().is_empty() {
        profile.slots = "1".to_string();
    }
    let key = profile.name.to_ascii_lowercase();
    let mut current: Vec<BootProfile> = load_profiles(state_dir)
        .into_iter()
        .filter(|p| p.name.to_ascii_lowercase() != key)
        .collect();
    current.push(profile);
    current.sort_by_key(|a| a.name.to_ascii_lowercase());
    write_profiles_atomic(state_dir, &current)?;
    Ok(current)
}

pub fn delete_profile(state_dir: &Path, name: &str) -> Result<Vec<BootProfile>> {
    let key = name.trim().to_ascii_lowercase();
    let current = load_profiles(state_dir);
    let orig_len = current.len();
    let updated: Vec<BootProfile> = current
        .into_iter()
        .filter(|p| p.name.to_ascii_lowercase() != key)
        .collect();
    if updated.len() != orig_len {
        write_profiles_atomic(state_dir, &updated)?;
    }
    Ok(updated)
}

pub fn read_local_activities(state_dir: &Path, slot: &str, since_ts: f64) -> Vec<ActivityEvent> {
    let path = state_dir.join("activity.jsonl");
    let Ok(txt) = fs::read_to_string(&path) else {
        return Vec::new();
    };
    let lines: Vec<&str> = txt.lines().collect();
    let start = lines.len().saturating_sub(240);
    let mut out = Vec::new();
    for ln in &lines[start..] {
        let trimmed = ln.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(ev) = serde_json::from_str::<ActivityEvent>(trimmed) else {
            continue;
        };
        if ev.ts <= since_ts {
            continue;
        }
        if slot != "all" && ev.slot != slot && ev.slot != "all" {
            continue;
        }
        out.push(ev);
    }
    if out.len() > 80 {
        out.drain(0..out.len() - 80);
    }
    out
}

pub fn mcp_flow_mtime_ns(state_dir: &Path) -> u128 {
    let path = state_dir.join("mcp_flow.jsonl");
    fs::metadata(&path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

pub fn read_mcp_flow_events(state_dir: &Path, since_ts: f64) -> Vec<McpFlowEvent> {
    let path = state_dir.join("mcp_flow.jsonl");
    let Ok(txt) = fs::read_to_string(&path) else {
        return Vec::new();
    };
    let lines: Vec<&str> = txt.lines().collect();
    let start = lines.len().saturating_sub(100);
    let mut out = Vec::new();
    for ln in &lines[start..] {
        let trimmed = ln.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(ev) = serde_json::from_str::<McpFlowEvent>(trimmed) else {
            continue;
        };
        if ev.ts <= since_ts {
            continue;
        }
        out.push(ev);
    }
    if out.len() > 40 {
        out.drain(0..out.len() - 40);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kgsh_state_{}_{}", tag, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn test_sessions_load_and_profiles_crud() {
        let dir = temp_dir("crud");
        let sess_json = r#"{
          "slots": {
            "1": {
              "session_id": "abc",
              "worker_token": "tok",
              "tunnel_url": "https://demo.trycloudflare.com/",
              "slot": "1",
              "accelerator": "NvidiaTeslaT4"
            }
          }
        }"#;
        fs::write(dir.join("sessions.json"), sess_json).unwrap();
        let loaded = load_all_sessions(&dir);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded["1"].base_url(), "https://demo.trycloudflare.com");

        let p = BootProfile {
            name: "t4-run".into(),
            idle_timeout_min: 30,
            ..Default::default()
        };
        let list = save_profile(&dir, p).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(load_profiles(&dir)[0].idle_timeout_min, 30);

        let after = delete_profile(&dir, "T4-RUN").unwrap();
        assert!(after.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_read_mcp_flow_events() {
        let dir = temp_dir("flow");
        let line = r#"{"ts": 1000.5, "span_id": "sp1", "phase": "start", "tool": "term_exec", "slot": "1", "summary": "ls"}"#;
        fs::write(dir.join("mcp_flow.jsonl"), format!("{line}\n")).unwrap();
        assert!(mcp_flow_mtime_ns(&dir) > 0);
        let evs = read_mcp_flow_events(&dir, 900.0);
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].tool, "term_exec");
        let _ = fs::remove_dir_all(&dir);
    }
}

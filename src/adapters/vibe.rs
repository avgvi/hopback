use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::Value;

use super::{mtime_nanos, nanos_to_secs, parse_iso8601_secs, Adapter};
use crate::config::AgentId;
use crate::model::{join_content, truncate_title, Session, ASSISTANT_PREFIX, USER_PREFIX};

const TITLE_MAX: usize = 80;

pub struct VibeAdapter {
    sessions_dir: PathBuf,
}

impl VibeAdapter {
    pub fn new() -> VibeAdapter {
        VibeAdapter {
            sessions_dir: default_sessions_dir(),
        }
    }

    pub fn with_sessions_dir(dir: PathBuf) -> VibeAdapter {
        VibeAdapter { sessions_dir: dir }
    }
}

impl Default for VibeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

pub fn default_sessions_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home)
        .join(".vibe")
        .join("logs")
        .join("session")
}

fn content_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => {
            let mut out = Vec::new();
            for part in parts {
                if let Some(t) = part.get("text").and_then(|v| v.as_str()) {
                    if !t.is_empty() {
                        out.push(t);
                    }
                }
            }
            out.join(" ")
        }
        _ => String::new(),
    }
}

pub fn parse_session(
    meta_bytes: &[u8],
    messages_bytes: &[u8],
    dir_name: &str,
    mtime_ns: i64,
) -> Option<Session> {
    let meta: Value = serde_json::from_slice(meta_bytes).ok()?;

    let session_id = meta
        .get("session_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(dir_name)
        .to_string();

    let directory = meta
        .get("environment")
        .and_then(|e| e.get("working_directory"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let yolo = meta
        .get("config")
        .and_then(|c| c.get("auto_approve"))
        .and_then(|v| v.as_bool())
        .or_else(|| meta.get("auto_approve").and_then(|v| v.as_bool()))
        .unwrap_or(false);

    let timestamp = meta
        .get("start_time")
        .and_then(|v| v.as_str())
        .and_then(parse_iso8601_secs)
        .unwrap_or_else(|| nanos_to_secs(mtime_ns));

    let meta_title = meta
        .get("title")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.to_string());

    let mut messages: Vec<String> = Vec::new();
    let mut first_user_message = String::new();
    let mut turn_count: u32 = 0;

    for line in messages_bytes.split(|&b| b == b'\n') {
        if line.iter().all(|b| b.is_ascii_whitespace()) {
            continue;
        }
        let Ok(data) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        let role = data.get("role").and_then(|v| v.as_str()).unwrap_or("");
        let prefix = match role {
            "user" => USER_PREFIX,
            "assistant" => ASSISTANT_PREFIX,
            _ => continue,
        };
        let Some(content) = data.get("content") else {
            continue;
        };
        let text = content_text(content);
        if text.is_empty() {
            continue;
        }
        messages.push(format!("{prefix}{text}"));
        turn_count += 1;
        if role == "user" && first_user_message.is_empty() {
            first_user_message = text;
        }
    }

    if first_user_message.is_empty() && meta_title.is_none() {
        return None;
    }

    let title = match &meta_title {
        Some(t) => truncate_title(t, TITLE_MAX, false),
        None => truncate_title(&first_user_message, TITLE_MAX, false),
    };

    Some(Session {
        id: session_id,
        agent: AgentId::Vibe,
        title,
        directory,
        timestamp,
        content: join_content(&messages),
        message_count: turn_count,
        mtime: mtime_ns,
        yolo,
        source_path: None,
    })
}

fn session_id_for_dir(session_dir: &Path) -> String {
    let dir_name = session_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();
    if let Ok(bytes) = std::fs::read(session_dir.join("meta.json")) {
        if let Ok(meta) = serde_json::from_slice::<Value>(&bytes) {
            if let Some(id) = meta.get("session_id").and_then(|v| v.as_str()) {
                if !id.is_empty() {
                    return id.to_string();
                }
            }
        }
    }
    dir_name
}

impl Adapter for VibeAdapter {
    fn id(&self) -> AgentId {
        AgentId::Vibe
    }

    fn supports_yolo(&self) -> bool {
        true
    }

    fn detects_yolo(&self) -> bool {
        true
    }

    fn is_available(&self) -> bool {
        self.sessions_dir.is_dir()
    }

    fn scan(&self) -> Result<HashMap<String, (PathBuf, i64)>> {
        let mut out = HashMap::new();
        let Ok(entries) = std::fs::read_dir(&self.sessions_dir) else {
            return Ok(out);
        };
        for e in entries.flatten() {
            let dir = e.path();
            if !dir.is_dir() {
                continue;
            }
            let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !name.starts_with("session_") {
                continue;
            }

            let meta_path = dir.join("meta.json");
            let Ok(mtime) = mtime_nanos(&meta_path) else {
                continue;
            };
            let id = session_id_for_dir(&dir);
            if !id.is_empty() {
                out.insert(id, (dir, mtime));
            }
        }
        Ok(out)
    }

    fn parse(&self, path: &Path) -> Result<Option<Session>> {
        let meta_bytes = match std::fs::read(path.join("meta.json")) {
            Ok(b) => b,
            Err(_) => return Ok(None),
        };
        let messages_bytes = std::fs::read(path.join("messages.jsonl")).unwrap_or_default();
        let mtime = mtime_nanos(&path.join("meta.json")).unwrap_or(0);
        let dir_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let mut session = parse_session(&meta_bytes, &messages_bytes, dir_name, mtime);
        if let Some(s) = &mut session {
            s.source_path = Some(path.to_string_lossy().to_string());
        }
        Ok(session)
    }

    fn resume_command(&self, session: &Session, yolo: bool) -> Vec<String> {
        let mut cmd = vec!["vibe".to_string()];
        if yolo {
            cmd.push("--agent".to_string());
            cmd.push("auto-approve".to_string());
        }
        cmd.push("--resume".to_string());
        cmd.push(session.id.clone());
        cmd
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const META: &str = r#"{"session_id":"vibe-123","start_time":"2025-12-20T10:00:00Z","environment":{"working_directory":"/home/u/proj"},"config":{"auto_approve":false}}"#;
    const MESSAGES: &str = r#"{"role":"system","content":"you are helpful"}
{"role":"user","content":"refactor the auth module please"}
{"role":"assistant","content":"On it."}
{"role":"user","content":[{"type":"text","text":"and add tests"}]}
"#;

    #[test]
    fn parses_basic_session() {
        let s = parse_session(
            META.as_bytes(),
            MESSAGES.as_bytes(),
            "session_dir",
            9_000_000_000,
        )
        .unwrap();
        assert_eq!(s.id, "vibe-123");
        assert_eq!(s.agent, AgentId::Vibe);
        assert_eq!(s.directory, "/home/u/proj");

        assert_eq!(
            s.timestamp,
            parse_iso8601_secs("2025-12-20T10:00:00Z").unwrap()
        );
        assert_eq!(s.title, "refactor the auth module please");

        assert_eq!(s.message_count, 3);
        assert!(s.content.contains("» refactor the auth module"));
        assert!(s.content.contains("  On it."));
        assert!(s.content.contains("» and add tests"));
        assert!(!s.content.contains("you are helpful"));
        assert!(!s.yolo);
    }

    #[test]
    fn yolo_from_config_auto_approve() {
        let meta = r#"{"session_id":"x","config":{"auto_approve":true}}"#;
        let msgs = r#"{"role":"user","content":"go fast please"}"#;
        let s = parse_session(meta.as_bytes(), msgs.as_bytes(), "d", 0).unwrap();
        assert!(s.yolo);
    }

    #[test]
    fn yolo_from_legacy_root_auto_approve() {
        let meta = r#"{"session_id":"x","auto_approve":true}"#;
        let msgs = r#"{"role":"user","content":"go fast please"}"#;
        let s = parse_session(meta.as_bytes(), msgs.as_bytes(), "d", 0).unwrap();
        assert!(s.yolo);
    }

    #[test]
    fn meta_title_overrides_first_message() {
        let meta = r#"{"session_id":"x","title":"My Named Session"}"#;
        let s = parse_session(meta.as_bytes(), MESSAGES.as_bytes(), "d", 0).unwrap();
        assert_eq!(s.title, "My Named Session");
    }

    #[test]
    fn fallback_id_is_dir_name() {
        let meta = r#"{}"#;
        let msgs = r#"{"role":"user","content":"a real question here"}"#;
        let s = parse_session(meta.as_bytes(), msgs.as_bytes(), "session_20251220_abc", 0).unwrap();
        assert_eq!(s.id, "session_20251220_abc");
    }

    #[test]
    fn timestamp_falls_back_to_mtime() {
        let meta = r#"{"session_id":"x"}"#;
        let msgs = r#"{"role":"user","content":"a real question here"}"#;
        let s = parse_session(meta.as_bytes(), msgs.as_bytes(), "d", 7_000_000_000).unwrap();
        assert_eq!(s.timestamp, 7);
    }

    #[test]
    fn no_user_and_no_title_returns_none() {
        let meta = r#"{"session_id":"x"}"#;
        let msgs = r#"{"role":"assistant","content":"hi"}"#;
        assert!(parse_session(meta.as_bytes(), msgs.as_bytes(), "d", 0).is_none());
    }

    #[test]
    fn corrupt_meta_returns_none() {
        assert!(parse_session(b"{bad json", MESSAGES.as_bytes(), "d", 0).is_none());
    }

    #[test]
    fn resume_command_with_and_without_yolo() {
        let a = VibeAdapter::with_sessions_dir(PathBuf::from("/nope"));
        let s = parse_session(META.as_bytes(), MESSAGES.as_bytes(), "d", 0).unwrap();
        assert_eq!(
            a.resume_command(&s, false),
            vec!["vibe", "--resume", "vibe-123"]
        );
        assert_eq!(
            a.resume_command(&s, true),
            vec!["vibe", "--agent", "auto-approve", "--resume", "vibe-123"]
        );
        assert!(!a.supports_fork());
    }
}

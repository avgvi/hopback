use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::Value;

use super::{mtime_nanos, nanos_to_secs, parse_iso8601_secs, Adapter};
use crate::config::AgentId;
use crate::model::{join_content, truncate_title, Session, ASSISTANT_PREFIX, USER_PREFIX};

const TITLE_MAX: usize = 100;

pub struct ClaudeAdapter {
    sessions_dir: PathBuf,
}

impl ClaudeAdapter {
    pub fn new() -> ClaudeAdapter {
        ClaudeAdapter {
            sessions_dir: crate::adapters::claude::default_sessions_dir(),
        }
    }

    pub fn with_sessions_dir(dir: PathBuf) -> ClaudeAdapter {
        ClaudeAdapter { sessions_dir: dir }
    }

    fn index_title(&self, project_dir: &Path, session_id: &str) -> String {
        let index_path = project_dir.join("sessions-index.json");
        let Ok(bytes) = std::fs::read(&index_path) else {
            return String::new();
        };
        let Ok(data) = serde_json::from_slice::<Value>(&bytes) else {
            return String::new();
        };
        let Some(entries) = data.get("entries").and_then(|e| e.as_array()) else {
            return String::new();
        };
        for entry in entries {
            let sid = entry
                .get("sessionId")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if sid != session_id {
                continue;
            }
            let summary = entry.get("summary").and_then(|v| v.as_str()).unwrap_or("");
            if !summary.trim().is_empty() {
                return summary.trim().to_string();
            }
        }
        String::new()
    }
}

impl Default for ClaudeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

pub fn default_sessions_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".claude").join("projects")
}

fn part_text(part: &Value) -> Option<&str> {
    if let Some(s) = part.as_str() {
        return Some(s);
    }
    if part.get("type").and_then(|t| t.as_str()) == Some("text") {
        return part.get("text").and_then(|t| t.as_str());
    }
    None
}

pub fn parse_jsonl(bytes: &[u8], id: &str, mtime_ns: i64, index_title: &str) -> Option<Session> {
    let mut first_user_message = String::new();
    let mut directory = String::new();
    let mut messages: Vec<String> = Vec::new();
    let mut turn_count: u32 = 0;

    let mut conversation_secs: Option<i64> = None;

    for line in bytes.split(|&b| b == b'\n') {
        if line.iter().all(|b| b.is_ascii_whitespace()) {
            continue;
        }

        let Ok(data) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        let msg_type = data.get("type").and_then(|t| t.as_str()).unwrap_or("");

        if msg_type == "user" && directory.is_empty() {
            if let Some(cwd) = data.get("cwd").and_then(|c| c.as_str()) {
                directory = cwd.to_string();
            }
        }
        if msg_type == "user" && conversation_secs.is_none() {
            conversation_secs = data
                .get("timestamp")
                .and_then(|t| t.as_str())
                .and_then(parse_iso8601_secs);
        }

        if msg_type == "user" {
            let content = data.get("message").and_then(|m| m.get("content"));
            let is_meta = data
                .get("isMeta")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let mut is_human_input = false;

            match content {
                Some(Value::String(s)) => {
                    is_human_input = true;
                    let is_command = s.starts_with("<command") || s.starts_with("<local-command");
                    if !is_meta && !is_command {
                        messages.push(format!("{USER_PREFIX}{s}"));
                        if first_user_message.is_empty() && s.chars().count() > 10 {
                            first_user_message = s.clone();
                        }
                    }
                }
                Some(Value::Array(parts)) => {
                    if let Some(first) = parts.first() {
                        if first.get("type").and_then(|t| t.as_str()) == Some("text") {
                            is_human_input = true;
                        }
                    }
                    for part in parts {
                        if let Some(text) = part_text(part) {
                            messages.push(format!("{USER_PREFIX}{text}"));
                            if first_user_message.is_empty() {
                                first_user_message = text.to_string();
                            }
                        }
                    }
                }
                _ => {}
            }

            if is_human_input {
                turn_count += 1;
            }
        }

        if msg_type == "assistant" {
            let content = data.get("message").and_then(|m| m.get("content"));
            let mut has_text = false;
            match content {
                Some(Value::String(s)) if !s.is_empty() => {
                    messages.push(format!("{ASSISTANT_PREFIX}{s}"));
                    has_text = true;
                }
                Some(Value::Array(parts)) => {
                    for part in parts {
                        if let Some(text) = part_text(part) {
                            if !text.is_empty() {
                                messages.push(format!("{ASSISTANT_PREFIX}{text}"));
                                has_text = true;
                            }
                        }
                    }
                }
                _ => {}
            }
            if has_text {
                turn_count += 1;
            }
        }
    }

    if first_user_message.is_empty() || messages.is_empty() {
        return None;
    }

    let title_source = if !index_title.trim().is_empty() {
        index_title
    } else {
        &first_user_message
    };

    Some(Session {
        id: id.to_string(),
        agent: AgentId::Claude,
        title: truncate_title(title_source, TITLE_MAX, true),
        directory,
        timestamp: conversation_secs.unwrap_or_else(|| nanos_to_secs(mtime_ns)),
        content: join_content(&messages),
        message_count: turn_count,
        mtime: mtime_ns,
        yolo: false,
        source_path: None,
    })
}

impl Adapter for ClaudeAdapter {
    fn id(&self) -> AgentId {
        AgentId::Claude
    }

    fn supports_yolo(&self) -> bool {
        true
    }

    fn supports_fork(&self) -> bool {
        true
    }

    fn is_available(&self) -> bool {
        self.sessions_dir.is_dir()
    }

    fn scan(&self) -> Result<HashMap<String, (PathBuf, i64)>> {
        let mut out = HashMap::new();
        let Ok(projects) = std::fs::read_dir(&self.sessions_dir) else {
            return Ok(out);
        };
        for project in projects.flatten() {
            let pdir = project.path();
            if !pdir.is_dir() {
                continue;
            }
            let Ok(files) = std::fs::read_dir(&pdir) else {
                continue;
            };
            for f in files.flatten() {
                let path = f.path();
                let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };

                if !name.ends_with(".jsonl") || name.starts_with("agent-") {
                    continue;
                }
                let Ok(mtime) = mtime_nanos(&path) else {
                    continue;
                };
                let id = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_string();
                if !id.is_empty() {
                    out.insert(id, (path, mtime));
                }
            }
        }
        Ok(out)
    }

    fn parse(&self, path: &Path) -> Result<Option<Session>> {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(_) => return Ok(None),
        };
        let id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        let mtime = mtime_nanos(path).unwrap_or(0);
        let index_title = path
            .parent()
            .map(|p| self.index_title(p, &id))
            .unwrap_or_default();
        let mut session = parse_jsonl(&bytes, &id, mtime, &index_title);
        if let Some(s) = &mut session {
            s.source_path = Some(path.to_string_lossy().to_string());
        }
        Ok(session)
    }

    fn resume_command(&self, session: &Session, yolo: bool) -> Vec<String> {
        let mut cmd = vec!["claude".to_string()];
        if yolo {
            cmd.push("--dangerously-skip-permissions".to_string());
        }
        cmd.push("--resume".to_string());
        cmd.push(session.id.clone());
        cmd
    }

    fn fork_command(
        &self,
        seed: &Session,
        new_id: Option<&str>,
        name: Option<&str>,
        yolo: bool,
    ) -> Option<Vec<String>> {
        let mut cmd = vec!["claude".to_string()];
        if yolo {
            cmd.push("--dangerously-skip-permissions".to_string());
        }
        cmd.push("--resume".to_string());
        cmd.push(seed.id.clone());
        cmd.push("--fork-session".to_string());
        if let Some(id) = new_id {
            cmd.push("--session-id".to_string());
            cmd.push(id.to_string());
        }
        if let Some(n) = name {
            cmd.push("--name".to_string());
            cmd.push(n.to_string());
        }
        Some(cmd)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"{"type":"queue-operation","operation":"enqueue"}
{"type":"user","cwd":"/home/u/proj","message":{"role":"user","content":"Help me fix the authentication bug please"},"uuid":"1"}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Sure, let me look."},{"type":"tool_use","name":"Read","id":"t1"}]}}
{"type":"user","isMeta":true,"message":{"content":"<system meta>"}}
{"type":"user","message":{"content":"<command>/clear</command>"}}
{"type":"user","message":{"content":[{"type":"tool_result","content":"file dump..."}]}}
{"type":"assistant","message":{"content":"Found it on line 42."}}
{"type":"user","message":{"content":"Thanks!"}}
"#;

    #[test]
    fn parses_real_shape_into_correct_session() {
        let s = parse_jsonl(FIXTURE.as_bytes(), "sess-1", 5_000_000_000, "").unwrap();
        assert_eq!(s.id, "sess-1");
        assert_eq!(s.agent, AgentId::Claude);
        assert_eq!(s.directory, "/home/u/proj");
        assert_eq!(s.timestamp, 5);
        assert_eq!(s.mtime, 5_000_000_000);

        assert_eq!(s.title, "Help me fix the authentication bug please");

        assert_eq!(s.message_count, 6);

        assert!(s
            .content
            .contains("» Help me fix the authentication bug please"));
        assert!(s.content.contains("  Sure, let me look."));
        assert!(s.content.contains("  Found it on line 42."));
        assert!(s.content.contains("» Thanks!"));
        assert!(!s.content.contains("file dump"));
        assert!(!s.content.contains("system meta"));
        assert!(!s.content.contains("/clear"));
    }

    #[test]
    fn index_title_overrides_first_user_message() {
        let s = parse_jsonl(FIXTURE.as_bytes(), "sess-1", 0, "Renamed Title").unwrap();
        assert_eq!(s.title, "Renamed Title");
    }

    #[test]
    fn timestamp_uses_first_user_line_not_mtime() {
        let f = "{\"type\":\"user\",\"timestamp\":\"2026-06-12T06:40:46.538Z\",\"cwd\":\"/w\",\"message\":{\"content\":\"a real question here\"}}\n{\"type\":\"assistant\",\"timestamp\":\"2026-06-12T06:41:00.000Z\",\"message\":{\"content\":\"ok\"}}";
        let s = parse_jsonl(f.as_bytes(), "x", 9_999_000_000_000, "").unwrap();
        let expected = super::super::parse_iso8601_secs("2026-06-12T06:40:46.538Z").unwrap();
        assert_eq!(s.timestamp, expected);
        assert_ne!(s.timestamp, super::super::nanos_to_secs(9_999_000_000_000));

        assert_eq!(s.mtime, 9_999_000_000_000);
    }

    #[test]
    fn timestamp_falls_back_to_mtime_when_absent() {
        let s = parse_jsonl(FIXTURE.as_bytes(), "sess-1", 5_000_000_000, "").unwrap();
        assert_eq!(s.timestamp, 5);
    }

    #[test]
    fn no_user_message_returns_none() {
        let only_assistant = r#"{"type":"assistant","message":{"content":"hi there"}}"#;
        assert!(parse_jsonl(only_assistant.as_bytes(), "x", 0, "").is_none());
    }

    #[test]
    fn short_first_user_then_longer_counts_and_titles() {
        let f = "{\"type\":\"user\",\"message\":{\"content\":\"hi\"}}\n{\"type\":\"user\",\"message\":{\"content\":\"this is a longer prompt\"}}\n{\"type\":\"assistant\",\"message\":{\"content\":\"ok\"}}";
        let s = parse_jsonl(f.as_bytes(), "x", 0, "").unwrap();
        assert_eq!(s.title, "this is a longer prompt");

        assert_eq!(s.message_count, 3);
    }

    #[test]
    fn malformed_lines_skipped() {
        let f = "not json\n{\"type\":\"user\",\"message\":{\"content\":\"a real question here\"}}\n{bad\n{\"type\":\"assistant\",\"message\":{\"content\":\"reply\"}}";
        let s = parse_jsonl(f.as_bytes(), "x", 0, "").unwrap();
        assert_eq!(s.message_count, 2);
        assert_eq!(s.title, "a real question here");
    }

    #[test]
    fn empty_returns_none() {
        assert!(parse_jsonl(b"", "x", 0, "").is_none());
        assert!(parse_jsonl(b"\n\n  \n", "x", 0, "").is_none());
    }

    #[test]
    fn resume_and_fork_commands() {
        let a = ClaudeAdapter::with_sessions_dir(PathBuf::from("/nope"));
        let s = parse_jsonl(FIXTURE.as_bytes(), "sess-1", 0, "").unwrap();
        assert_eq!(
            a.resume_command(&s, false),
            vec!["claude", "--resume", "sess-1"]
        );
        assert_eq!(
            a.resume_command(&s, true),
            vec![
                "claude",
                "--dangerously-skip-permissions",
                "--resume",
                "sess-1"
            ]
        );
        assert!(a.supports_fork());
        assert_eq!(
            a.fork_command(&s, Some("new-uuid"), Some("mybranch"), false)
                .unwrap(),
            vec![
                "claude",
                "--resume",
                "sess-1",
                "--fork-session",
                "--session-id",
                "new-uuid",
                "--name",
                "mybranch"
            ]
        );
    }
}

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::Value;

use super::{mtime_nanos, nanos_to_secs, Adapter};
use crate::config::AgentId;
use crate::model::{join_content, truncate_title, Session, ASSISTANT_PREFIX, USER_PREFIX};

const TITLE_MAX: usize = 100;

pub struct CopilotAdapter {
    sessions_dir: PathBuf,
}

impl CopilotAdapter {
    pub fn new() -> CopilotAdapter {
        CopilotAdapter {
            sessions_dir: default_sessions_dir(),
        }
    }

    pub fn with_sessions_dir(dir: PathBuf) -> CopilotAdapter {
        CopilotAdapter { sessions_dir: dir }
    }
}

impl Default for CopilotAdapter {
    fn default() -> Self {
        Self::new()
    }
}

pub fn default_sessions_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".copilot").join("session-state")
}

fn collect_jsonl(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let path = e.path();
        if path.is_dir() {
            collect_jsonl(&path, out);
        } else if path.extension().and_then(|x| x.to_str()) == Some("jsonl") {
            out.push(path);
        }
    }
}

fn fallback_id(path: &Path, root: &Path) -> String {
    if let Some(parent) = path.parent() {
        if parent != root {
            if let Some(name) = parent.file_name().and_then(|n| n.to_str()) {
                return name.to_string();
            }
        }
    }
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string()
}

fn dir_from_folder_trust(message: &str) -> String {
    let Some(rest) = message.split_once("Folder ").map(|(_, r)| r) else {
        return String::new();
    };
    let candidate = rest.split_whitespace().next().unwrap_or("");
    if candidate.starts_with('/') {
        candidate.to_string()
    } else {
        String::new()
    }
}

pub fn parse_jsonl(bytes: &[u8], fallback: &str, mtime_ns: i64) -> Option<Session> {
    let mut session_id = String::new();
    let mut directory = String::new();
    let mut title_override = String::new();
    let mut first_user_message = String::new();
    let mut messages: Vec<String> = Vec::new();
    let mut turn_count: u32 = 0;

    for line in bytes.split(|&b| b == b'\n') {
        if line.iter().all(|b| b.is_ascii_whitespace()) {
            continue;
        }
        let Ok(data) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        let msg_type = data.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let payload = data.get("data");

        match msg_type {
            "session.start" => {
                if let Some(p) = payload {
                    if let Some(id) = p.get("sessionId").and_then(|v| v.as_str()) {
                        if !id.is_empty() {
                            session_id = id.to_string();
                        }
                    }
                    if directory.is_empty() {
                        if let Some(cwd) = p
                            .get("context")
                            .and_then(|c| c.get("cwd"))
                            .and_then(|v| v.as_str())
                        {
                            directory = cwd.to_string();
                        }
                    }
                }
            }
            "session.info" if directory.is_empty() => {
                if let Some(p) = payload {
                    let info_type = p.get("infoType").and_then(|v| v.as_str()).unwrap_or("");
                    if info_type == "folder_trust" {
                        if let Some(msg) = p.get("message").and_then(|v| v.as_str()) {
                            let d = dir_from_folder_trust(msg);
                            if !d.is_empty() {
                                directory = d;
                            }
                        }
                    }
                }
            }
            "session.title_changed" => {
                if let Some(t) = payload
                    .and_then(|p| p.get("title"))
                    .and_then(|v| v.as_str())
                {
                    if !t.trim().is_empty() {
                        title_override = t.to_string();
                    }
                }
            }
            "user.message" => {
                if let Some(c) = payload
                    .and_then(|p| p.get("content"))
                    .and_then(|v| v.as_str())
                {
                    if !c.is_empty() {
                        messages.push(format!("{USER_PREFIX}{c}"));
                        turn_count += 1;
                        if first_user_message.is_empty() {
                            first_user_message = c.to_string();
                        }
                    }
                }
            }
            "assistant.message" => {
                if let Some(c) = payload
                    .and_then(|p| p.get("content"))
                    .and_then(|v| v.as_str())
                {
                    if !c.is_empty() {
                        messages.push(format!("{ASSISTANT_PREFIX}{c}"));
                        turn_count += 1;
                    }
                }
            }
            _ => {}
        }
    }

    if first_user_message.is_empty() {
        return None;
    }
    if session_id.is_empty() {
        session_id = fallback.to_string();
    }

    let title_source = if !title_override.trim().is_empty() {
        title_override.as_str()
    } else {
        &first_user_message
    };

    Some(Session {
        id: session_id,
        agent: AgentId::CopilotCli,
        title: truncate_title(title_source, TITLE_MAX, false),
        directory,
        timestamp: nanos_to_secs(mtime_ns),
        content: join_content(&messages),
        message_count: turn_count,
        mtime: mtime_ns,
        yolo: false,
        source_path: None,
    })
}

fn session_id_for_file(path: &Path, root: &Path) -> String {
    if let Ok(bytes) = std::fs::read(path) {
        for line in bytes.split(|&b| b == b'\n') {
            if line.iter().all(|b| b.is_ascii_whitespace()) {
                continue;
            }
            if let Ok(data) = serde_json::from_slice::<Value>(line) {
                if data.get("type").and_then(|t| t.as_str()) == Some("session.start") {
                    if let Some(id) = data
                        .get("data")
                        .and_then(|p| p.get("sessionId"))
                        .and_then(|v| v.as_str())
                    {
                        if !id.is_empty() {
                            return id.to_string();
                        }
                    }
                    break;
                }
            }
        }
    }
    fallback_id(path, root)
}

impl Adapter for CopilotAdapter {
    fn id(&self) -> AgentId {
        AgentId::CopilotCli
    }

    fn supports_yolo(&self) -> bool {
        true
    }

    fn is_available(&self) -> bool {
        self.sessions_dir.is_dir()
    }

    fn scan(&self) -> Result<HashMap<String, (PathBuf, i64)>> {
        let mut files = Vec::new();
        collect_jsonl(&self.sessions_dir, &mut files);
        let mut out = HashMap::new();
        for path in files {
            let Ok(mtime) = mtime_nanos(&path) else {
                continue;
            };
            let id = session_id_for_file(&path, &self.sessions_dir);
            if !id.is_empty() {
                out.insert(id, (path, mtime));
            }
        }
        Ok(out)
    }

    fn parse(&self, path: &Path) -> Result<Option<Session>> {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(_) => return Ok(None),
        };
        let mtime = mtime_nanos(path).unwrap_or(0);
        let fb = fallback_id(path, &self.sessions_dir);
        let mut session = parse_jsonl(&bytes, &fb, mtime);
        if let Some(s) = &mut session {
            s.source_path = Some(path.to_string_lossy().to_string());
        }
        Ok(session)
    }

    fn resume_command(&self, session: &Session, yolo: bool) -> Vec<String> {
        let mut cmd = vec!["copilot".to_string()];
        if yolo {
            cmd.push("--yolo".to_string());
        }
        cmd.push("--resume".to_string());
        cmd.push(session.id.clone());
        cmd
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"{"type":"session.start","data":{"sessionId":"sess-abc","context":{"cwd":"/home/u/proj"}}}
{"type":"user.message","data":{"content":"help me refactor the database layer"}}
{"type":"assistant.message","data":{"content":"Sure, starting now."}}
{"type":"user.message","data":{"content":"also add tests"}}
"#;

    #[test]
    fn parses_basic_session() {
        let s = parse_jsonl(FIXTURE.as_bytes(), "fb", 5_000_000_000).unwrap();
        assert_eq!(s.id, "sess-abc");
        assert_eq!(s.agent, AgentId::CopilotCli);
        assert_eq!(s.directory, "/home/u/proj");
        assert_eq!(s.timestamp, 5);
        assert_eq!(s.title, "help me refactor the database layer");
        assert_eq!(s.message_count, 3);
        assert!(s.content.contains("» help me refactor"));
        assert!(s.content.contains("  Sure, starting now."));
        assert!(!s.yolo);
    }

    #[test]
    fn title_changed_last_wins() {
        let f = "{\"type\":\"user.message\",\"data\":{\"content\":\"a long enough first prompt\"}}\n{\"type\":\"session.title_changed\",\"data\":{\"title\":\"First Title\"}}\n{\"type\":\"session.title_changed\",\"data\":{\"title\":\"Final Title\"}}";
        let s = parse_jsonl(f.as_bytes(), "fb", 0).unwrap();
        assert_eq!(s.title, "Final Title");
    }

    #[test]
    fn directory_from_folder_trust_when_no_cwd() {
        let f = "{\"type\":\"session.info\",\"data\":{\"infoType\":\"folder_trust\",\"message\":\"Folder /work/api has been trusted by the user\"}}\n{\"type\":\"user.message\",\"data\":{\"content\":\"a real question here\"}}";
        let s = parse_jsonl(f.as_bytes(), "fb", 0).unwrap();
        assert_eq!(s.directory, "/work/api");
    }

    #[test]
    fn fallback_id_used_when_no_session_start() {
        let f = "{\"type\":\"user.message\",\"data\":{\"content\":\"a real question here\"}}";
        let s = parse_jsonl(f.as_bytes(), "the-fallback", 0).unwrap();
        assert_eq!(s.id, "the-fallback");
    }

    #[test]
    fn no_user_message_returns_none() {
        let f = "{\"type\":\"session.start\",\"data\":{\"sessionId\":\"x\"}}\n{\"type\":\"assistant.message\",\"data\":{\"content\":\"hi\"}}";
        assert!(parse_jsonl(f.as_bytes(), "fb", 0).is_none());
    }

    #[test]
    fn malformed_lines_skipped() {
        let f = "not json\n{\"type\":\"user.message\",\"data\":{\"content\":\"a real question here\"}}\n{bad";
        let s = parse_jsonl(f.as_bytes(), "fb", 0).unwrap();
        assert_eq!(s.message_count, 1);
    }

    #[test]
    fn resume_command_with_and_without_yolo() {
        let a = CopilotAdapter::with_sessions_dir(PathBuf::from("/nope"));
        let s = parse_jsonl(FIXTURE.as_bytes(), "fb", 0).unwrap();
        assert_eq!(
            a.resume_command(&s, false),
            vec!["copilot", "--resume", "sess-abc"]
        );
        assert_eq!(
            a.resume_command(&s, true),
            vec!["copilot", "--yolo", "--resume", "sess-abc"]
        );
        assert!(!a.supports_fork());
        assert!(a.fork_command(&s, None, None, false).is_none());
    }

    #[test]
    fn fallback_id_prefers_parent_dir_when_nested() {
        let root = Path::new("/root/session-state");

        assert_eq!(
            fallback_id(Path::new("/root/session-state/abc-123/events.jsonl"), root),
            "abc-123"
        );

        assert_eq!(
            fallback_id(Path::new("/root/session-state/session-xyz.jsonl"), root),
            "session-xyz"
        );
    }
}

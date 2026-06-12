use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::Value;

use super::{mtime_nanos, nanos_to_secs, parse_iso8601_secs, Adapter};
use crate::config::AgentId;
use crate::model::{join_content, truncate_title, Session, ASSISTANT_PREFIX, USER_PREFIX};

const TITLE_MAX: usize = 80;

pub struct CodexAdapter {
    sessions_dir: PathBuf,
    session_index_file: PathBuf,
}

impl CodexAdapter {
    pub fn new() -> CodexAdapter {
        let dir = default_sessions_dir();
        let index = dir
            .parent()
            .map(|p| p.join("session_index.jsonl"))
            .unwrap_or_else(|| PathBuf::from("session_index.jsonl"));
        CodexAdapter {
            sessions_dir: dir,
            session_index_file: index,
        }
    }

    pub fn with_paths(sessions_dir: PathBuf, session_index_file: PathBuf) -> CodexAdapter {
        CodexAdapter {
            sessions_dir,
            session_index_file,
        }
    }

    fn thread_name(&self, session_id: &str) -> String {
        let Ok(bytes) = std::fs::read(&self.session_index_file) else {
            return String::new();
        };
        for line in bytes.split(|&b| b == b'\n') {
            if line.iter().all(|b| b.is_ascii_whitespace()) {
                continue;
            }
            let Ok(data) = serde_json::from_slice::<Value>(line) else {
                continue;
            };
            let id = data.get("id").and_then(|v| v.as_str()).unwrap_or("");
            if id != session_id {
                continue;
            }
            let name = data
                .get("thread_name")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if !name.trim().is_empty() {
                return name.trim().to_string();
            }
        }
        String::new()
    }
}

impl Default for CodexAdapter {
    fn default() -> Self {
        Self::new()
    }
}

pub fn default_sessions_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".codex").join("sessions")
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

fn id_from_stem(stem: &str) -> String {
    match stem.split_once('-') {
        Some((_, rest)) => rest.to_string(),
        None => stem.to_string(),
    }
}

pub fn parse_jsonl(
    bytes: &[u8],
    fallback_id: &str,
    mtime_ns: i64,
    thread_name: &str,
) -> Option<Session> {
    let mut session_id = String::new();
    let mut directory = String::new();
    let mut messages: Vec<String> = Vec::new();
    let mut user_prompts: Vec<String> = Vec::new();
    let mut turn_count: u32 = 0;
    let mut yolo = false;

    let mut conversation_secs: Option<i64> = None;

    for line in bytes.split(|&b| b == b'\n') {
        if line.iter().all(|b| b.is_ascii_whitespace()) {
            continue;
        }
        let Ok(data) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        let msg_type = data.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let payload = data.get("payload");

        match msg_type {
            "session_meta" => {
                if let Some(p) = payload {
                    if let Some(id) = p.get("id").and_then(|v| v.as_str()) {
                        session_id = id.to_string();
                    }
                    if let Some(cwd) = p.get("cwd").and_then(|v| v.as_str()) {
                        directory = cwd.to_string();
                    }
                    if conversation_secs.is_none() {
                        conversation_secs = p
                            .get("timestamp")
                            .and_then(|v| v.as_str())
                            .and_then(parse_iso8601_secs);
                    }
                }
            }
            "turn_context" => {
                if let Some(p) = payload {
                    let approval = p
                        .get("approval_policy")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");

                    let sandbox = p.get("sandbox_policy");
                    let danger_sandbox = sandbox
                        .map(|s| {
                            let mode = s.get("mode").and_then(|v| v.as_str()).unwrap_or("");
                            let typ = s.get("type").and_then(|v| v.as_str()).unwrap_or("");
                            mode == "danger-full-access" || typ == "danger-full-access"
                        })
                        .unwrap_or(false);
                    if approval == "never" || danger_sandbox {
                        yolo = true;
                    }
                }
            }
            "response_item" => {
                if let Some(p) = payload {
                    let role = p.get("role").and_then(|v| v.as_str()).unwrap_or("");
                    if role == "user" || role == "assistant" {
                        let prefix = if role == "user" {
                            USER_PREFIX
                        } else {
                            ASSISTANT_PREFIX
                        };
                        let mut has_text = false;
                        if let Some(parts) = p.get("content").and_then(|c| c.as_array()) {
                            for part in parts {
                                let text = part
                                    .get("text")
                                    .and_then(|v| v.as_str())
                                    .filter(|s| !s.is_empty())
                                    .or_else(|| part.get("input_text").and_then(|v| v.as_str()))
                                    .unwrap_or("");
                                if !text.is_empty()
                                    && !text.trim_start().starts_with("<environment_context>")
                                {
                                    messages.push(format!("{prefix}{text}"));
                                    has_text = true;
                                }
                            }
                        }
                        if has_text {
                            turn_count += 1;
                        }
                    }
                }
            }
            "event_msg" => {
                if let Some(p) = payload {
                    let event_type = p.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    match event_type {
                        "user_message" => {
                            if let Some(msg) = p.get("message").and_then(|v| v.as_str()) {
                                if !msg.is_empty() {
                                    messages.push(format!("{USER_PREFIX}{msg}"));
                                    user_prompts.push(msg.to_string());
                                }
                            }
                        }
                        "agent_reasoning" => {
                            if let Some(text) = p.get("text").and_then(|v| v.as_str()) {
                                if !text.is_empty() {
                                    messages.push(format!("{ASSISTANT_PREFIX}{text}"));
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    if session_id.is_empty() {
        session_id = fallback_id.to_string();
    }

    if user_prompts.is_empty() {
        return None;
    }

    let title_source = if !thread_name.trim().is_empty() {
        thread_name
    } else {
        &user_prompts[0]
    };

    Some(Session {
        id: session_id,
        agent: AgentId::Codex,
        title: truncate_title(title_source, TITLE_MAX, false),
        directory,
        timestamp: conversation_secs.unwrap_or_else(|| nanos_to_secs(mtime_ns)),
        content: join_content(&messages),
        message_count: turn_count,
        mtime: mtime_ns,
        yolo,
        source_path: None,
    })
}

fn session_id_for_file(path: &Path) -> String {
    if let Ok(bytes) = std::fs::read(path) {
        for line in bytes.split(|&b| b == b'\n') {
            if line.iter().all(|b| b.is_ascii_whitespace()) {
                continue;
            }
            if let Ok(data) = serde_json::from_slice::<Value>(line) {
                if data.get("type").and_then(|t| t.as_str()) == Some("session_meta") {
                    if let Some(id) = data
                        .get("payload")
                        .and_then(|p| p.get("id"))
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
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    id_from_stem(stem)
}

impl Adapter for CodexAdapter {
    fn id(&self) -> AgentId {
        AgentId::Codex
    }

    fn supports_yolo(&self) -> bool {
        true
    }

    fn detects_yolo(&self) -> bool {
        true
    }

    fn supports_fork(&self) -> bool {
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
            let id = session_id_for_file(&path);
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
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let fallback_id = id_from_stem(stem);
        let mtime = mtime_nanos(path).unwrap_or(0);

        let real_id = session_id_for_file(path);
        let thread = self.thread_name(&real_id);
        let mut session = parse_jsonl(&bytes, &fallback_id, mtime, &thread);
        if let Some(s) = &mut session {
            s.source_path = Some(path.to_string_lossy().to_string());
        }
        Ok(session)
    }

    fn resume_command(&self, session: &Session, yolo: bool) -> Vec<String> {
        let mut cmd = vec!["codex".to_string()];
        if yolo {
            cmd.push("--dangerously-bypass-approvals-and-sandbox".to_string());
        }
        cmd.push("resume".to_string());
        cmd.push(session.id.clone());
        cmd
    }

    fn fork_command(
        &self,
        seed: &Session,
        _new_id: Option<&str>,
        _name: Option<&str>,
        _yolo: bool,
    ) -> Option<Vec<String>> {
        Some(vec![
            "codex".to_string(),
            "fork".to_string(),
            seed.id.clone(),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"{"timestamp":"2026-03-06T02:23:26Z","type":"session_meta","payload":{"id":"019cc0f4-uuid","cwd":"/home/u/codeproj"}}
{"type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"<environment_context>ignore me</environment_context>"}]}}
{"type":"event_msg","payload":{"type":"user_message","message":"refactor the database layer"}}
{"type":"response_item","payload":{"role":"assistant","content":[{"type":"output_text","text":"Sure, starting now."}]}}
{"type":"event_msg","payload":{"type":"agent_reasoning","text":"The user wants a refactor."}}
{"type":"response_item","payload":{"role":"user","content":[{"input_text":"also add tests"}]}}
"#;

    #[test]
    fn parses_codex_session_correctly() {
        let s = parse_jsonl(FIXTURE.as_bytes(), "fallback", 3_000_000_000, "").unwrap();
        assert_eq!(s.id, "019cc0f4-uuid");
        assert_eq!(s.agent, AgentId::Codex);
        assert_eq!(s.directory, "/home/u/codeproj");
        assert_eq!(s.timestamp, 3);
        assert!(!s.yolo);

        assert_eq!(s.title, "refactor the database layer");

        assert_eq!(s.message_count, 2);

        assert!(s.content.contains("» refactor the database layer"));
        assert!(s.content.contains("  Sure, starting now."));
        assert!(s.content.contains("  The user wants a refactor."));
        assert!(s.content.contains("» also add tests"));
        assert!(!s.content.contains("environment_context"));
    }

    #[test]
    fn thread_name_overrides_title() {
        let s = parse_jsonl(FIXTURE.as_bytes(), "fb", 0, "DB Refactor Thread").unwrap();
        assert_eq!(s.title, "DB Refactor Thread");
    }

    #[test]
    fn timestamp_uses_session_meta_payload_not_mtime() {
        let f = "{\"timestamp\":\"2026-03-06T02:23:26.428Z\",\"type\":\"session_meta\",\"payload\":{\"timestamp\":\"2026-03-06T02:23:01.572Z\",\"id\":\"x\",\"cwd\":\"/w\"}}\n{\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"go\"}}";
        let s = parse_jsonl(f.as_bytes(), "fb", 7_000_000_000, "").unwrap();
        let expected = super::super::parse_iso8601_secs("2026-03-06T02:23:01.572Z").unwrap();
        assert_eq!(s.timestamp, expected);
        assert_eq!(s.mtime, 7_000_000_000);
    }

    #[test]
    fn timestamp_falls_back_to_mtime_when_payload_has_none() {
        let s = parse_jsonl(FIXTURE.as_bytes(), "fb", 3_000_000_000, "").unwrap();
        assert_eq!(s.timestamp, 3);
    }

    #[test]
    fn yolo_detected_from_approval_policy_never() {
        let f = r#"{"type":"session_meta","payload":{"id":"x","cwd":"/w"}}
{"type":"turn_context","payload":{"approval_policy":"never","sandbox_policy":{"type":"workspace-write"}}}
{"type":"event_msg","payload":{"type":"user_message","message":"go fast"}}
"#;
        let s = parse_jsonl(f.as_bytes(), "fb", 0, "").unwrap();
        assert!(s.yolo);
    }

    #[test]
    fn yolo_detected_from_danger_full_access_sandbox() {
        for key in ["mode", "type"] {
            let f = format!(
                "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"x\",\"cwd\":\"/w\"}}}}\n{{\"type\":\"turn_context\",\"payload\":{{\"approval_policy\":\"on-request\",\"sandbox_policy\":{{\"{key}\":\"danger-full-access\"}}}}}}\n{{\"type\":\"event_msg\",\"payload\":{{\"type\":\"user_message\",\"message\":\"go\"}}}}"
            );
            let s = parse_jsonl(f.as_bytes(), "fb", 0, "").unwrap();
            assert!(s.yolo, "yolo should be detected via sandbox_policy.{key}");
        }
    }

    #[test]
    fn no_user_prompt_returns_none() {
        let f = r#"{"type":"session_meta","payload":{"id":"x","cwd":"/w"}}
{"type":"response_item","payload":{"role":"assistant","content":[{"text":"hi"}]}}
"#;
        assert!(parse_jsonl(f.as_bytes(), "fb", 0, "").is_none());
    }

    #[test]
    fn id_falls_back_to_filename_stem() {
        assert_eq!(
            id_from_stem("rollout-2026-03-05T18-23-01-019cc0f4-uuid"),
            "2026-03-05T18-23-01-019cc0f4-uuid"
        );
        assert_eq!(id_from_stem("nodash"), "nodash");

        let f = r#"{"type":"event_msg","payload":{"type":"user_message","message":"hello there friend"}}"#;
        let s = parse_jsonl(f.as_bytes(), "the-fallback-id", 0, "").unwrap();
        assert_eq!(s.id, "the-fallback-id");
    }

    #[test]
    fn resume_and_fork_commands() {
        let a = CodexAdapter::with_paths(PathBuf::from("/nope"), PathBuf::from("/nope.jsonl"));
        let s = parse_jsonl(FIXTURE.as_bytes(), "fb", 0, "").unwrap();
        assert_eq!(
            a.resume_command(&s, false),
            vec!["codex", "resume", "019cc0f4-uuid"]
        );
        assert_eq!(
            a.resume_command(&s, true),
            vec![
                "codex",
                "--dangerously-bypass-approvals-and-sandbox",
                "resume",
                "019cc0f4-uuid"
            ]
        );
        assert_eq!(
            a.fork_command(&s, None, None, false).unwrap(),
            vec!["codex", "fork", "019cc0f4-uuid"]
        );
    }
}

// Codex rollout files: the subagent filter, the id-from-filename rule, the
// response_item prompt fallback and the session_index handling.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::Result;
use serde_json::Value;

use super::{mtime_nanos, nanos_to_secs, parse_iso8601_secs, Adapter};
use crate::config::AgentId;
use crate::model::{join_content, truncate_title, Session, ASSISTANT_PREFIX, USER_PREFIX};

const TITLE_MAX: usize = 80;

/// One `session_index.jsonl` entry: the thread name Codex shows and when it
/// was last renamed (nanoseconds, 0 when unknown).
type ThreadIndex = HashMap<String, (String, i64)>;

pub struct CodexAdapter {
    sessions_dir: PathBuf,
    session_index_file: PathBuf,
    thread_index: OnceLock<ThreadIndex>,
}

impl CodexAdapter {
    pub fn new() -> CodexAdapter {
        let dir = default_sessions_dir();
        let index = dir
            .parent()
            .map(|p| p.join("session_index.jsonl"))
            .unwrap_or_else(|| PathBuf::from("session_index.jsonl"));
        CodexAdapter::with_paths(dir, index)
    }

    pub fn with_paths(sessions_dir: PathBuf, session_index_file: PathBuf) -> CodexAdapter {
        CodexAdapter {
            sessions_dir,
            session_index_file,
            thread_index: OnceLock::new(),
        }
    }

    /// Thread names from `session_index.jsonl`, read once per adapter. Codex
    /// appends a line on every rename, so the last entry for an id wins.
    fn thread_index(&self) -> &ThreadIndex {
        self.thread_index
            .get_or_init(|| load_thread_index(&self.session_index_file))
    }

    fn thread_name(&self, session_id: &str) -> String {
        self.thread_index()
            .get(session_id)
            .map(|(name, _)| name.clone())
            .unwrap_or_default()
    }
}

fn load_thread_index(path: &Path) -> ThreadIndex {
    let mut out = ThreadIndex::new();
    let Ok(bytes) = std::fs::read(path) else {
        return out;
    };
    for line in bytes.split(|&b| b == b'\n') {
        if line.iter().all(|b| b.is_ascii_whitespace()) {
            continue;
        }
        let Ok(data) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        let id = data.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let name = data
            .get("thread_name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if id.is_empty() || name.is_empty() {
            continue;
        }
        let renamed = data
            .get("updated_at")
            .and_then(|v| v.as_str())
            .and_then(parse_iso8601_secs)
            .map(|secs| secs.saturating_mul(1_000_000_000))
            .unwrap_or(0);
        out.insert(id.to_string(), (name.to_string(), renamed));
    }
    out
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

/// Current rollouts are named `rollout-<local time>-<uuid>.jsonl`; the uuid is
/// the id `codex resume` takes.
fn uuid_from_stem(stem: &str) -> Option<&str> {
    let start = stem.len().checked_sub(36)?;
    let candidate = stem.get(start..)?;
    let ok = candidate.char_indices().all(|(i, c)| match i {
        8 | 13 | 18 | 23 => c == '-',
        _ => c.is_ascii_hexdigit(),
    });
    ok.then_some(candidate)
}

/// Payload of the rollout's own `session_meta`, which is its first record.
/// Only the head of the file is read. Later `session_meta` records can be
/// copies from a fork's history and must not be used.
fn first_session_meta(path: &Path) -> Option<Value> {
    let file = std::fs::File::open(path).ok()?;
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    for _ in 0..8 {
        line.clear();
        if reader.read_until(b'\n', &mut line).ok()? == 0 {
            return None;
        }
        if line.iter().all(|b| b.is_ascii_whitespace()) {
            continue;
        }
        let Ok(mut data) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        if data.get("type").and_then(|t| t.as_str()) == Some("session_meta") {
            return data.get_mut("payload").map(Value::take);
        }
    }
    None
}

/// Reviews, guardian checks and spawned threads. `codex resume` does not list
/// them, and they are noise next to the session that started them.
fn is_subagent_meta(meta: &Value) -> bool {
    meta.pointer("/source/subagent").is_some()
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
    let mut response_user_prompts: Vec<String> = Vec::new();
    let mut item_prompts: Vec<String> = Vec::new();
    let mut seen_meta = false;
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
                    if seen_meta {
                        continue;
                    }
                    seen_meta = true;
                    if is_subagent_meta(p) {
                        return None;
                    }
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
                    // Only what `--dangerously-bypass-approvals-and-sandbox`
                    // gives: no approvals and no sandbox. `codex exec` runs
                    // with approvals off inside a sandbox, which is not that.
                    // The latest turn decides.
                    yolo = approval == "never" && danger_sandbox;
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
                                if !text.is_empty() && !is_injected_part(text) {
                                    // Current Codex records the prompt only as a
                                    // `message` response item; older builds also
                                    // wrote an `event_msg`/`user_message`.
                                    let is_message = p
                                        .get("type")
                                        .and_then(|v| v.as_str())
                                        .is_none_or(|t| t == "message");
                                    if role == "user" && is_message {
                                        response_user_prompts.push(text.to_string());
                                    }
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
                        // The prompt as the user saw it, without the context
                        // and image labels that the model input carries.
                        "item_completed" => {
                            let item = p.get("item");
                            let is_user = item.and_then(|i| i.get("type")).and_then(|v| v.as_str())
                                == Some("UserMessage");
                            if is_user {
                                let text = item
                                    .and_then(|i| i.get("content"))
                                    .and_then(|c| c.as_array())
                                    .map(|parts| {
                                        parts
                                            .iter()
                                            .filter_map(|part| {
                                                part.get("text").and_then(|v| v.as_str())
                                            })
                                            .collect::<Vec<_>>()
                                            .join("\n")
                                    })
                                    .unwrap_or_default();
                                if !text.trim().is_empty() {
                                    item_prompts.push(text);
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
        user_prompts = response_user_prompts;
    }
    if user_prompts.is_empty() {
        return None;
    }

    let title_source = if !thread_name.trim().is_empty() {
        thread_name
    } else if let Some(prompt) = item_prompts.first() {
        prompt
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

/// Text parts that Codex adds to the model input around the user's prompt:
/// the environment block and the labels that bracket each attached image.
fn is_injected_part(text: &str) -> bool {
    let t = text.trim_start();
    t.starts_with("<environment_context>") || t.starts_with("<image") || t.starts_with("</image>")
}

fn session_id_for_file(path: &Path, meta: Option<&Value>) -> String {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    if let Some(uuid) = uuid_from_stem(stem) {
        return uuid.to_string();
    }
    if let Some(id) = meta
        .and_then(|m| m.get("id"))
        .and_then(|v| v.as_str())
        .filter(|id| !id.is_empty())
    {
        return id.to_string();
    }
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
            let Ok(mut mtime) = mtime_nanos(&path) else {
                continue;
            };
            let meta = first_session_meta(&path);
            if meta.as_ref().is_some_and(is_subagent_meta) {
                continue;
            }
            let id = session_id_for_file(&path, meta.as_ref());
            if id.is_empty() {
                continue;
            }
            // A rename only touches session_index.jsonl, so let it count as a
            // change to the session.
            if let Some((_, renamed)) = self.thread_index().get(&id) {
                mtime = mtime.max(*renamed);
            }
            out.insert(id, (path, mtime));
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

        let meta = first_session_meta(path);
        let real_id = session_id_for_file(path, meta.as_ref());
        let thread = self.thread_name(&real_id);
        let mut session = parse_jsonl(&bytes, &fallback_id, mtime, &thread);
        if let Some(s) = &mut session {
            s.id = real_id;
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
    fn approvals_off_inside_a_sandbox_is_not_yolo() {
        // What `codex exec` records: no approvals, but sandboxed.
        for sandbox in ["workspace-write", "read-only"] {
            let f = format!(
                "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"x\",\"cwd\":\"/w\"}}}}\n{{\"type\":\"turn_context\",\"payload\":{{\"approval_policy\":\"never\",\"sandbox_policy\":{{\"type\":\"{sandbox}\"}}}}}}\n{{\"type\":\"event_msg\",\"payload\":{{\"type\":\"user_message\",\"message\":\"go fast\"}}}}"
            );
            let s = parse_jsonl(f.as_bytes(), "fb", 0, "").unwrap();
            assert!(!s.yolo, "{sandbox} with approvals off is not a bypass");
        }
    }

    #[test]
    fn the_latest_turn_decides_yolo() {
        let f = r#"{"type":"session_meta","payload":{"id":"x","cwd":"/w"}}
{"type":"turn_context","payload":{"approval_policy":"never","sandbox_policy":{"type":"danger-full-access"}}}
{"type":"event_msg","payload":{"type":"user_message","message":"go"}}
{"type":"turn_context","payload":{"approval_policy":"on-request","sandbox_policy":{"type":"workspace-write"}}}
"#;
        let s = parse_jsonl(f.as_bytes(), "fb", 0, "").unwrap();
        assert!(!s.yolo);
    }

    #[test]
    fn yolo_detected_from_no_approvals_and_no_sandbox() {
        for key in ["mode", "type"] {
            let f = format!(
                "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"x\",\"cwd\":\"/w\"}}}}\n{{\"type\":\"turn_context\",\"payload\":{{\"approval_policy\":\"never\",\"sandbox_policy\":{{\"{key}\":\"danger-full-access\"}}}}}}\n{{\"type\":\"event_msg\",\"payload\":{{\"type\":\"user_message\",\"message\":\"go\"}}}}"
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

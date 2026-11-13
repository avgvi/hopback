//! Kiro CLI keeps its sessions under `~/.kiro/sessions`, in two layouts:
//!
//! - `cli/<uuid>.json` with the session's metadata, beside `cli/<uuid>.jsonl`
//!   with one `Prompt` or `AssistantMessage` record per line.
//! - `<workspace hash>/sess_<uuid>/session.json` beside `messages.jsonl`,
//!   written by the newer engine, with one `{id, timestamp, payload}` record
//!   per line.
//!
//! Both were read from files written by Kiro CLI 2.22. A session's id is its
//! file or folder name, which is what `kiro-cli chat --resume-id` takes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::Value;

use super::{mtime_nanos, nanos_to_secs, parse_iso8601_secs, Adapter};
use crate::config::AgentId;
use crate::model::{join_content, truncate_title, Session, ASSISTANT_PREFIX, USER_PREFIX};

const TITLE_MAX: usize = 100;

/// The folder that holds the first layout.
const CLI_DIR: &str = "cli";

/// Folder-name prefix of a session in the second layout.
const SESSION_DIR_PREFIX: &str = "sess_";

pub struct KiroAdapter {
    sessions_dir: PathBuf,
}

impl KiroAdapter {
    pub fn new() -> KiroAdapter {
        KiroAdapter {
            sessions_dir: default_sessions_dir(),
        }
    }

    pub fn with_sessions_dir(dir: PathBuf) -> KiroAdapter {
        KiroAdapter { sessions_dir: dir }
    }
}

impl Default for KiroAdapter {
    fn default() -> Self {
        Self::new()
    }
}

pub fn default_sessions_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".kiro").join("sessions")
}

fn str_field<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(|v| v.as_str()).unwrap_or("")
}

fn time_field(value: &Value, key: &str) -> Option<i64> {
    parse_iso8601_secs(str_field(value, key))
}

fn json_lines(bytes: &[u8]) -> impl Iterator<Item = Value> + '_ {
    bytes
        .split(|&b| b == b'\n')
        .filter(|line| !line.iter().all(|b| b.is_ascii_whitespace()))
        .filter_map(|line| serde_json::from_slice::<Value>(line).ok())
}

/// What a transcript adds to the metadata: its readable messages and the
/// first prompt.
#[derive(Default)]
struct Transcript {
    messages: Vec<String>,
    first_user: String,
}

impl Transcript {
    fn push(&mut self, from_user: bool, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        if from_user && self.first_user.is_empty() {
            self.first_user = text.to_string();
        }
        let prefix = if from_user {
            USER_PREFIX
        } else {
            ASSISTANT_PREFIX
        };
        self.messages.push(format!("{prefix}{text}"));
    }
}

/// The text parts of a first-layout record, with a short marker for each tool
/// call. Thinking, tool results and images are left out.
fn cli_content_text(content: Option<&Value>) -> String {
    let Some(parts) = content.and_then(|c| c.as_array()) else {
        return String::new();
    };
    let mut out: Vec<String> = Vec::new();
    for part in parts {
        let data = part.get("data");
        match str_field(part, "kind") {
            "text" => {
                if let Some(text) = data.and_then(|d| d.as_str()) {
                    let text = text.trim();
                    if !text.is_empty() {
                        out.push(text.to_string());
                    }
                }
            }
            "toolUse" => {
                let name = data
                    .and_then(|d| d.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("tool");
                out.push(format!("[calling {name}]"));
            }
            _ => {}
        }
    }
    out.join(" ")
}

fn cli_transcript(bytes: &[u8]) -> Transcript {
    let mut transcript = Transcript::default();
    for record in json_lines(bytes) {
        let from_user = match str_field(&record, "kind") {
            "Prompt" => true,
            "AssistantMessage" => false,
            _ => continue,
        };
        let content = record.get("data").and_then(|d| d.get("content"));
        transcript.push(from_user, &cli_content_text(content));
    }
    transcript
}

/// A payload's `content` is a string in the files seen so far; accept a list
/// of text parts too.
fn payload_text(payload: &Value) -> String {
    match payload.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| part.get("text").and_then(|v| v.as_str()))
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

fn dir_transcript(bytes: &[u8]) -> Transcript {
    let mut transcript = Transcript::default();
    for record in json_lines(bytes) {
        let Some(payload) = record.get("payload") else {
            continue;
        };
        match str_field(payload, "type") {
            "user" => transcript.push(true, &payload_text(payload)),
            // An assistant record is either what it said or its reasoning.
            "assistant" if str_field(payload, "operationType") != "Reasoning" => {
                transcript.push(false, &payload_text(payload));
            }
            _ => {}
        }
    }
    transcript
}

struct Meta {
    title: String,
    directory: String,
    timestamp: Option<i64>,
}

fn build(id: &str, meta: Meta, transcript: Transcript, mtime_ns: i64) -> Option<Session> {
    // No prompt: Kiro creates the files as soon as a session opens.
    if id.is_empty() || transcript.first_user.is_empty() {
        return None;
    }
    let title_source = if meta.title.trim().is_empty() {
        transcript.first_user.as_str()
    } else {
        meta.title.trim()
    };
    Some(Session {
        id: id.to_string(),
        agent: AgentId::Kiro,
        title: truncate_title(title_source, TITLE_MAX, false),
        directory: meta.directory,
        timestamp: meta.timestamp.unwrap_or_else(|| nanos_to_secs(mtime_ns)),
        content: join_content(&transcript.messages),
        message_count: transcript.messages.len() as u32,
        mtime: mtime_ns,
        yolo: false,
        source_path: None,
    })
}

/// A first-layout session: `<id>.json` and `<id>.jsonl`.
pub fn parse_cli_session(
    meta_bytes: &[u8],
    transcript_bytes: &[u8],
    id: &str,
    mtime_ns: i64,
) -> Option<Session> {
    let meta: Value = serde_json::from_slice(meta_bytes).ok()?;
    let meta = Meta {
        title: str_field(&meta, "title").to_string(),
        directory: str_field(&meta, "cwd").to_string(),
        timestamp: time_field(&meta, "updated_at").or_else(|| time_field(&meta, "created_at")),
    };
    build(id, meta, cli_transcript(transcript_bytes), mtime_ns)
}

/// A second-layout session: `session.json` and `messages.jsonl` in a folder
/// named after the session.
pub fn parse_dir_session(
    meta_bytes: &[u8],
    transcript_bytes: &[u8],
    id: &str,
    mtime_ns: i64,
) -> Option<Session> {
    let meta: Value = serde_json::from_slice(meta_bytes).ok()?;
    let first_path = |key: &str| {
        meta.get(key)
            .and_then(|v| v.as_array())
            .and_then(|paths| paths.first())
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
    };
    let directory = first_path("workspacePaths")
        .or_else(|| first_path("rootPaths"))
        .unwrap_or("")
        .to_string();
    let meta = Meta {
        title: str_field(&meta, "title").to_string(),
        directory,
        timestamp: time_field(&meta, "lastModifiedAt").or_else(|| time_field(&meta, "createdAt")),
    };
    build(id, meta, dir_transcript(transcript_bytes), mtime_ns)
}

/// The newer of the two files that make up a session. The metadata file must
/// exist; a missing transcript counts as never written.
fn pair_mtime(meta: &Path, transcript: &Path) -> Option<i64> {
    let meta = mtime_nanos(meta).ok()?;
    Some(meta.max(mtime_nanos(transcript).unwrap_or(0)))
}

fn file_name(path: &Path) -> &str {
    path.file_name().and_then(|n| n.to_str()).unwrap_or("")
}

impl KiroAdapter {
    fn scan_cli_files(&self, out: &mut HashMap<String, (PathBuf, i64)>) {
        let Ok(entries) = std::fs::read_dir(self.sessions_dir.join(CLI_DIR)) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") || !path.is_file() {
                continue;
            }
            let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if id.is_empty() {
                continue;
            }
            if let Some(mtime) = pair_mtime(&path, &path.with_extension("jsonl")) {
                out.insert(id.to_string(), (path.clone(), mtime));
            }
        }
    }

    fn scan_session_dirs(&self, out: &mut HashMap<String, (PathBuf, i64)>) {
        let Ok(workspaces) = std::fs::read_dir(&self.sessions_dir) else {
            return;
        };
        for workspace in workspaces.flatten() {
            let workspace = workspace.path();
            if !workspace.is_dir() || file_name(&workspace) == CLI_DIR {
                continue;
            }
            let Ok(entries) = std::fs::read_dir(&workspace) else {
                continue;
            };
            for entry in entries.flatten() {
                let dir = entry.path();
                let id = file_name(&dir).to_string();
                if !id.starts_with(SESSION_DIR_PREFIX) || !dir.is_dir() {
                    continue;
                }
                let mtime = pair_mtime(&dir.join("session.json"), &dir.join("messages.jsonl"));
                if let Some(mtime) = mtime {
                    out.insert(id, (dir, mtime));
                }
            }
        }
    }
}

impl Adapter for KiroAdapter {
    fn id(&self) -> AgentId {
        AgentId::Kiro
    }

    fn supports_yolo(&self) -> bool {
        true
    }

    fn is_available(&self) -> bool {
        self.sessions_dir.is_dir()
    }

    fn scan(&self) -> Result<HashMap<String, (PathBuf, i64)>> {
        let mut out = HashMap::new();
        self.scan_cli_files(&mut out);
        self.scan_session_dirs(&mut out);
        Ok(out)
    }

    /// `path` is what `scan` returned: a session folder in the second layout,
    /// or the `.json` file in the first.
    fn parse(&self, path: &Path) -> Result<Option<Session>> {
        let mut session = if path.is_dir() {
            let meta_path = path.join("session.json");
            let transcript_path = path.join("messages.jsonl");
            let Ok(meta) = std::fs::read(&meta_path) else {
                return Ok(None);
            };
            let transcript = std::fs::read(&transcript_path).unwrap_or_default();
            let mtime = pair_mtime(&meta_path, &transcript_path).unwrap_or(0);
            parse_dir_session(&meta, &transcript, file_name(path), mtime)
        } else {
            let transcript_path = path.with_extension("jsonl");
            let Ok(meta) = std::fs::read(path) else {
                return Ok(None);
            };
            let transcript = std::fs::read(&transcript_path).unwrap_or_default();
            let mtime = pair_mtime(path, &transcript_path).unwrap_or(0);
            let id = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            parse_cli_session(&meta, &transcript, id, mtime)
        };
        if let Some(s) = &mut session {
            s.source_path = Some(path.to_string_lossy().to_string());
        }
        Ok(session)
    }

    /// `kiro-cli chat --resume-id <id>`, as `kiro-cli chat --help` documents
    /// it in Kiro CLI 2.22. `--trust-all-tools` is its skip-permissions flag.
    fn resume_command(&self, session: &Session, yolo: bool) -> Vec<String> {
        let mut cmd = vec!["kiro-cli".to_string(), "chat".to_string()];
        if yolo {
            cmd.push("--trust-all-tools".to_string());
        }
        cmd.push("--resume-id".to_string());
        cmd.push(session.id.clone());
        cmd
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_content_keeps_text_and_names_tool_calls() {
        let content: Value = serde_json::from_str(
            r#"[{"kind":"thinking","data":{"text":"private","signature":"s"}},
                {"kind":"text","data":" Reading the config. "},
                {"kind":"toolUse","data":{"toolUseId":"t1","name":"fs_read","input":{"path":"secret"}}},
                {"kind":"toolResult","data":{"toolUseId":"t1","content":"tool output"}}]"#,
        )
        .unwrap();
        assert_eq!(
            cli_content_text(Some(&content)),
            "Reading the config. [calling fs_read]"
        );
        assert_eq!(cli_content_text(None), "");
    }

    #[test]
    fn payload_text_reads_strings_and_part_lists() {
        let text: Value = serde_json::from_str(r#"{"content":"plain"}"#).unwrap();
        assert_eq!(payload_text(&text), "plain");
        let parts: Value =
            serde_json::from_str(r#"{"content":[{"text":"one"},{"text":"two"}]}"#).unwrap();
        assert_eq!(payload_text(&parts), "one two");
        let none: Value = serde_json::from_str(r#"{"type":"turn_start"}"#).unwrap();
        assert_eq!(payload_text(&none), "");
    }

    #[test]
    fn a_session_without_a_prompt_is_dropped() {
        let meta = br#"{"session_id":"a","cwd":"/work","title":"Named anyway"}"#;
        assert!(parse_cli_session(meta, b"", "a", 1).is_none());
        let only_assistant = br#"{"version":"v1","kind":"AssistantMessage","data":{"content":[{"kind":"text","data":"hello"}]}}"#;
        assert!(parse_cli_session(meta, only_assistant, "a", 1).is_none());
    }
}

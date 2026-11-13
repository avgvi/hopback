// Gemini CLI sessions: `~/.gemini/tmp/<project>/chats/session-*.jsonl`, plus
// the single-document `session-*.json` files older builds wrote.
//
// The record kinds, the replay order and the "ignored prompt" rule follow the
// CLI's own loader (`chatRecordingService.ts` and the session selector in
// @google/gemini-cli 0.67.0).

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::Value;

use super::{mtime_nanos, nanos_to_secs, parse_iso8601_secs, Adapter};
use crate::config::AgentId;
use crate::model::{join_content, truncate_title, Session, ASSISTANT_PREFIX, USER_PREFIX};

const TITLE_MAX: usize = 80;
const SESSION_FILE_PREFIX: &str = "session-";
const PROJECT_ROOT_FILE: &str = ".project_root";
const PROJECTS_FILE: &str = "projects.json";

/// How many leading records `scan` reads to find a log's metadata record. The
/// CLI writes it first.
const HEAD_RECORDS: usize = 8;

pub struct GeminiAdapter {
    root: PathBuf,
}

impl GeminiAdapter {
    pub fn new() -> GeminiAdapter {
        GeminiAdapter::with_root(default_root())
    }

    /// `root` is the `.gemini` directory, the one that holds `tmp/` and
    /// `projects.json`.
    pub fn with_root(root: PathBuf) -> GeminiAdapter {
        GeminiAdapter { root }
    }

    fn tmp_dir(&self) -> PathBuf {
        self.root.join("tmp")
    }

    /// The project root a `tmp/<project>` directory belongs to. The session
    /// file does not record it. `gemini --resume` only finds a session when it
    /// is started there.
    fn project_directory(&self, project_dir: &Path) -> String {
        if let Ok(marker) = std::fs::read_to_string(project_dir.join(PROJECT_ROOT_FILE)) {
            let marker = marker.trim();
            if !marker.is_empty() {
                return marker.to_string();
            }
        }
        let Some(slug) = project_dir.file_name().and_then(|n| n.to_str()) else {
            return String::new();
        };
        registry_path_for_slug(&self.root.join(PROJECTS_FILE), slug).unwrap_or_default()
    }
}

impl Default for GeminiAdapter {
    fn default() -> Self {
        Self::new()
    }
}

/// `~/.gemini`. Like the CLI, a non-empty `GEMINI_CLI_HOME` stands in for the
/// home directory.
pub fn default_root() -> PathBuf {
    root_from(
        std::env::var("GEMINI_CLI_HOME").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
    )
}

fn root_from(cli_home: Option<&str>, home: Option<&str>) -> PathBuf {
    let base = cli_home.filter(|h| !h.is_empty()).or(home).unwrap_or(".");
    PathBuf::from(base).join(".gemini")
}

/// Reverse lookup in `projects.json`: `{"projects": {"<project root>": "<slug>"}}`.
fn registry_path_for_slug(registry: &Path, slug: &str) -> Option<String> {
    let bytes = std::fs::read(registry).ok()?;
    let data = serde_json::from_slice::<Value>(&bytes).ok()?;
    data.get("projects")?
        .as_object()?
        .iter()
        .find(|(_, v)| v.as_str() == Some(slug))
        .map(|(path, _)| path.clone())
}

fn is_session_file(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    name.starts_with(SESSION_FILE_PREFIX) && (name.ends_with(".jsonl") || name.ends_with(".json"))
}

fn is_jsonl(path: &Path) -> bool {
    path.extension().and_then(|x| x.to_str()) == Some("jsonl")
}

/// Session files of every project. Only files directly in `chats/` count:
/// subagent logs live one level down, in `chats/<parent session id>/`.
fn collect_session_files(tmp_dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(projects) = std::fs::read_dir(tmp_dir) else {
        return out;
    };
    for project in projects.flatten() {
        let Ok(files) = std::fs::read_dir(project.path().join("chats")) else {
            continue;
        };
        for file in files.flatten() {
            let path = file.path();
            if is_session_file(&path) && path.is_file() {
                out.push(path);
            }
        }
    }
    out
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    User,
    Assistant,
    /// `info`, `error` and `warning` records the UI shows but the model never saw.
    Other,
}

struct Message {
    role: Role,
    /// Text of `content`, which is what the model received.
    text: String,
    /// Text of `displayContent`: what the user typed, when the CLI expanded
    /// the prompt (file references, custom commands) before sending it.
    shown: String,
}

impl Message {
    fn from_record(record: &Value) -> Message {
        let role = match record.get("type").and_then(|t| t.as_str()) {
            Some("user") => Role::User,
            Some("gemini") => Role::Assistant,
            _ => Role::Other,
        };
        Message {
            role,
            text: record.get("content").map(parts_text).unwrap_or_default(),
            shown: record
                .get("displayContent")
                .map(parts_text)
                .unwrap_or_default(),
        }
    }
}

/// Text of a `PartListUnion`: a string, a part, or a list of either. Function
/// calls, function responses, inline data and thought parts carry no text for
/// the index.
fn parts_text(value: &Value) -> String {
    let mut out = String::new();
    push_parts_text(value, &mut out);
    out
}

fn push_parts_text(value: &Value, out: &mut String) {
    match value {
        Value::String(s) => out.push_str(s),
        Value::Array(parts) => {
            for part in parts {
                push_parts_text(part, out);
            }
        }
        Value::Object(part) => {
            if part.get("thought").and_then(|t| t.as_bool()) == Some(true) {
                return;
            }
            if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                out.push_str(text);
            }
        }
        _ => {}
    }
}

/// User records the CLI does not treat as a prompt: slash commands, the `?`
/// shortcut and the context blocks it injects itself.
fn is_ignored_user_content(text: &str) -> bool {
    let text = text.trim();
    text.is_empty()
        || text.starts_with('/')
        || text.starts_with('?')
        || text.starts_with("<session_context>")
        || text.starts_with("<hook_context>")
}

fn is_message_record(record: &Value) -> bool {
    record.get("id").is_some_and(Value::is_string) && record.get("$patch").is_none()
}

/// JavaScript truthiness, which is how the CLI tests the metadata it needs.
fn is_truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(_) => true,
    }
}

/// Session metadata. Each field holds the last value written for it, of
/// whatever JSON type.
#[derive(Default)]
struct Metadata {
    session_id: Option<Value>,
    project_hash: Option<Value>,
    start_time: Option<Value>,
    last_updated: Option<Value>,
    kind: Option<Value>,
    summary: Option<Value>,
}

impl Metadata {
    fn merge(&mut self, fields: &serde_json::Map<String, Value>) {
        for (key, value) in fields {
            let slot = match key.as_str() {
                "sessionId" => &mut self.session_id,
                "projectHash" => &mut self.project_hash,
                "startTime" => &mut self.start_time,
                "lastUpdated" => &mut self.last_updated,
                "kind" => &mut self.kind,
                "summary" => &mut self.summary,
                _ => continue,
            };
            *slot = Some(value.clone());
        }
    }

    fn into_conversation(self, messages: Vec<Message>) -> Option<Conversation> {
        let secs = |v: &Option<Value>| {
            v.as_ref()
                .and_then(|v| v.as_str())
                .and_then(|s| parse_iso8601_secs(s.trim()))
        };
        let session_id = self.session_id.as_ref()?.as_str()?.to_string();
        if session_id.is_empty() {
            return None;
        }
        Some(Conversation {
            session_id,
            start_secs: secs(&self.start_time),
            updated_secs: secs(&self.last_updated),
            summary: self
                .summary
                .as_ref()
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            is_subagent: is_subagent_kind(self.kind.as_ref()),
            messages,
        })
    }
}

fn is_subagent_kind(kind: Option<&Value>) -> bool {
    kind.and_then(|k| k.as_str()) == Some("subagent")
}

/// State rebuilt by replaying an append-only session log. Messages are keyed
/// by id and keep the position of their first record.
#[derive(Default)]
struct Replay {
    meta: Metadata,
    order: Vec<String>,
    messages: HashMap<String, Message>,
}

impl Replay {
    /// Applies one log record. The kinds are tested in the CLI's order.
    fn apply(&mut self, record: &Value) {
        let Some(fields) = record.as_object() else {
            return;
        };
        if let Some(id) = fields.get("$rewindTo").and_then(|v| v.as_str()) {
            self.rewind_to(id);
        } else if let Some(patch) = fields
            .get("$patch")
            .filter(|p| p.is_object() || p.is_array())
        {
            self.patch(patch);
        } else if is_message_record(record) {
            self.upsert(record);
        } else if let Some(set) = fields.get("$set").and_then(|s| s.as_object()) {
            if let Some(messages) = set.get("messages").and_then(|m| m.as_array()) {
                self.order.clear();
                self.messages.clear();
                self.upsert_all(messages);
            }
            self.meta.merge(set);
        } else if fields.get("sessionId").is_some_and(Value::is_string)
            && fields.get("projectHash").is_some_and(Value::is_string)
        {
            self.meta.merge(fields);
            if let Some(messages) = fields.get("messages").and_then(|m| m.as_array()) {
                self.upsert_all(messages);
            }
        }
    }

    fn upsert(&mut self, record: &Value) {
        let Some(id) = record.get("id").and_then(|v| v.as_str()) else {
            return;
        };
        if !self.messages.contains_key(id) {
            self.order.push(id.to_string());
        }
        self.messages
            .insert(id.to_string(), Message::from_record(record));
    }

    fn upsert_all(&mut self, records: &[Value]) {
        for record in records {
            if is_message_record(record) {
                self.upsert(record);
            }
        }
    }

    /// Drops the named message and everything after it. An id the log does
    /// not know clears the conversation.
    fn rewind_to(&mut self, id: &str) {
        let keep = self.order.iter().position(|m| m == id).unwrap_or(0);
        for removed in self.order.drain(keep..) {
            self.messages.remove(&removed);
        }
    }

    fn patch(&mut self, patch: &Value) {
        self.patch_message(patch);
        if let Some(updates) = patch.get("updates").and_then(|u| u.as_array()) {
            for update in updates {
                self.patch_message(update);
            }
        }
        if let Some(ids) = patch.get("removeIds").and_then(|r| r.as_array()) {
            for id in ids.iter().filter_map(|id| id.as_str()) {
                if self.messages.remove(id).is_some() {
                    self.order.retain(|m| m != id);
                }
            }
        }
        if let Some(ids) = patch.get("orderIds").and_then(|o| o.as_array()) {
            self.reorder(ids);
        }
    }

    /// `{id, content?, toolCalls?}`. Tool results are not indexed, so only a
    /// new `content` matters here.
    fn patch_message(&mut self, patch: &Value) {
        let Some(id) = patch.get("id").and_then(|v| v.as_str()) else {
            return;
        };
        let Some(message) = self.messages.get_mut(id) else {
            return;
        };
        if let Some(content) = patch.get("content") {
            message.text = parts_text(content);
        }
    }

    /// Listed messages move to the end in the given order. The others keep
    /// their place before them.
    fn reorder(&mut self, ids: &[Value]) {
        let mut moved: Vec<String> = Vec::new();
        for id in ids.iter().filter_map(|id| id.as_str()) {
            if self.messages.contains_key(id) && !moved.iter().any(|m| m == id) {
                moved.push(id.to_string());
            }
        }
        self.order.retain(|m| !moved.contains(m));
        self.order.extend(moved);
    }

    fn finish(mut self) -> Option<Conversation> {
        if !is_truthy(self.meta.session_id.as_ref()) || !is_truthy(self.meta.project_hash.as_ref())
        {
            return None;
        }
        let messages = self
            .order
            .iter()
            .filter_map(|id| self.messages.remove(id))
            .collect();
        self.meta.into_conversation(messages)
    }
}

struct Conversation {
    session_id: String,
    start_secs: Option<i64>,
    updated_secs: Option<i64>,
    summary: String,
    is_subagent: bool,
    messages: Vec<Message>,
}

fn json_lines(bytes: &[u8]) -> impl Iterator<Item = Value> + '_ {
    bytes
        .split(|&b| b == b'\n')
        .filter(|line| !line.iter().all(|b| b.is_ascii_whitespace()))
        .filter_map(|line| serde_json::from_slice::<Value>(line).ok())
}

/// Loads a session file the way the CLI does: replay it as a log, and if that
/// yields no session id and project hash, read it as one legacy document.
fn load_conversation(bytes: &[u8]) -> Option<Conversation> {
    let mut replay = Replay::default();
    for record in json_lines(bytes) {
        replay.apply(&record);
    }
    replay.finish().or_else(|| load_legacy(bytes))
}

/// `{"sessionId", "projectHash", "startTime", "lastUpdated", "messages": [...]}`.
fn load_legacy(bytes: &[u8]) -> Option<Conversation> {
    let data = serde_json::from_slice::<Value>(bytes).ok()?;
    let fields = data.as_object()?;
    let mut meta = Metadata::default();
    meta.merge(fields);
    let messages = fields
        .get("messages")
        .and_then(|m| m.as_array())
        .map(|records| {
            records
                .iter()
                .filter(|r| r.is_object())
                .map(Message::from_record)
                .collect()
        })
        .unwrap_or_default();
    meta.into_conversation(messages)
}

/// What `scan` needs from a session file.
struct Identity {
    session_id: String,
    is_subagent: bool,
}

/// Session id and kind from the metadata record at the head of a log, without
/// reading the rest of the file.
fn head_identity(path: &Path) -> Option<Identity> {
    let file = std::fs::File::open(path).ok()?;
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    let mut records = 0;
    while records < HEAD_RECORDS {
        line.clear();
        if reader.read_until(b'\n', &mut line).ok()? == 0 {
            return None;
        }
        if line.iter().all(|b| b.is_ascii_whitespace()) {
            continue;
        }
        records += 1;
        let Ok(data) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        let id = data.get("sessionId").and_then(|v| v.as_str()).unwrap_or("");
        if !id.is_empty() && data.get("projectHash").is_some_and(Value::is_string) {
            return Some(Identity {
                session_id: id.to_string(),
                is_subagent: is_subagent_kind(data.get("kind")),
            });
        }
    }
    None
}

fn full_identity(path: &Path) -> Option<Identity> {
    let conversation = load_conversation(&std::fs::read(path).ok()?)?;
    Some(Identity {
        session_id: conversation.session_id,
        is_subagent: conversation.is_subagent,
    })
}

fn identity(path: &Path) -> Option<Identity> {
    if is_jsonl(path) {
        if let Some(found) = head_identity(path) {
            return Some(found);
        }
    }
    full_identity(path)
}

/// Orders the files that hold one session id. Resuming a legacy `.json`
/// session leaves it in place and continues in a `.jsonl` beside it. Like the
/// CLI, the copy updated last wins.
fn freshness(path: &Path, mtime: i64) -> (i64, bool, i64) {
    let updated = std::fs::read(path)
        .ok()
        .and_then(|bytes| load_conversation(&bytes))
        .and_then(|c| c.updated_secs.or(c.start_secs))
        .unwrap_or(0);
    (updated, is_jsonl(path), mtime)
}

/// Control characters would garble the list row. Whitespace is left for
/// `truncate_title` to collapse.
fn clean_title(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || c.is_whitespace())
        .collect()
}

fn build_session(conversation: Conversation, directory: String, mtime_ns: i64) -> Option<Session> {
    if conversation.is_subagent {
        return None;
    }

    let mut messages: Vec<String> = Vec::new();
    let mut first_prompt: Option<String> = None;
    let mut message_count: u32 = 0;

    for message in &conversation.messages {
        match message.role {
            Role::User => {
                if is_ignored_user_content(&message.text) {
                    continue;
                }
                let typed = message.shown.trim();
                let text = if typed.is_empty() {
                    message.text.trim()
                } else {
                    typed
                };
                messages.push(format!("{USER_PREFIX}{text}"));
                message_count += 1;
                if first_prompt.is_none() {
                    first_prompt = Some(text.to_string());
                }
            }
            Role::Assistant => {
                let text = message.text.trim();
                if text.is_empty() {
                    continue;
                }
                messages.push(format!("{ASSISTANT_PREFIX}{text}"));
                message_count += 1;
            }
            Role::Other => {}
        }
    }

    // The CLI's own list also leaves out sessions nobody prompted.
    let first_prompt = first_prompt?;

    let summary = clean_title(&conversation.summary);
    let title_source = if summary.trim().is_empty() {
        clean_title(&first_prompt)
    } else {
        summary
    };

    Some(Session {
        id: conversation.session_id,
        agent: AgentId::Gemini,
        title: truncate_title(&title_source, TITLE_MAX, false),
        directory,
        timestamp: conversation
            .start_secs
            .or(conversation.updated_secs)
            .unwrap_or_else(|| nanos_to_secs(mtime_ns)),
        content: join_content(&messages),
        message_count,
        mtime: mtime_ns,
        yolo: false,
        source_path: None,
    })
}

impl Adapter for GeminiAdapter {
    fn id(&self) -> AgentId {
        AgentId::Gemini
    }

    fn supports_yolo(&self) -> bool {
        true
    }

    fn is_available(&self) -> bool {
        self.tmp_dir().is_dir()
    }

    fn scan(&self) -> Result<HashMap<String, (PathBuf, i64)>> {
        let mut out: HashMap<String, (PathBuf, i64)> = HashMap::new();
        for path in collect_session_files(&self.tmp_dir()) {
            let Ok(mtime) = mtime_nanos(&path) else {
                continue;
            };
            let Some(found) = identity(&path) else {
                continue;
            };
            if found.is_subagent {
                continue;
            }
            if let Some((other, other_mtime)) = out.get(&found.session_id) {
                if freshness(other, *other_mtime) >= freshness(&path, mtime) {
                    continue;
                }
            }
            out.insert(found.session_id, (path, mtime));
        }
        Ok(out)
    }

    fn parse(&self, path: &Path) -> Result<Option<Session>> {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(_) => return Ok(None),
        };
        let Some(conversation) = load_conversation(&bytes) else {
            return Ok(None);
        };
        let mtime = mtime_nanos(path).unwrap_or(0);
        let directory = path
            .parent()
            .and_then(|chats| chats.parent())
            .map(|project| self.project_directory(project))
            .unwrap_or_default();

        let mut session = build_session(conversation, directory, mtime);
        if let Some(s) = &mut session {
            s.source_path = Some(path.to_string_lossy().to_string());
        }
        Ok(session)
    }

    fn resume_command(&self, session: &Session, yolo: bool) -> Vec<String> {
        let mut cmd = vec!["gemini".to_string()];
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

    fn load(log: &str) -> Conversation {
        load_conversation(log.as_bytes()).expect("a session")
    }

    fn texts(conversation: &Conversation) -> Vec<&str> {
        conversation
            .messages
            .iter()
            .map(|m| m.text.as_str())
            .collect()
    }

    const META: &str = r#"{"sessionId":"11111111-2222-4333-8444-555555555555","projectHash":"00","startTime":"2026-07-17T10:00:00.000Z","lastUpdated":"2026-07-17T10:00:00.000Z"}"#;

    #[test]
    fn root_honours_gemini_cli_home_like_the_cli() {
        assert_eq!(
            root_from(None, Some("/home/u")),
            PathBuf::from("/home/u/.gemini")
        );
        assert_eq!(
            root_from(Some("/alt"), Some("/home/u")),
            PathBuf::from("/alt/.gemini")
        );
        assert_eq!(
            root_from(Some(""), Some("/home/u")),
            PathBuf::from("/home/u/.gemini")
        );
        assert_eq!(root_from(None, None), PathBuf::from("./.gemini"));
    }

    #[test]
    fn a_repeated_id_replaces_the_message_in_place() {
        let c = load(&format!(
            "{META}\n{}\n{}\n{}\n",
            r#"{"id":"a","type":"user","content":"one"}"#,
            r#"{"id":"b","type":"gemini","content":"two"}"#,
            r#"{"id":"a","type":"user","content":"one again"}"#,
        ));
        assert_eq!(texts(&c), vec!["one again", "two"]);
    }

    #[test]
    fn rewind_drops_the_message_and_what_follows() {
        let log = format!(
            "{META}\n{}\n{}\n{}\n",
            r#"{"id":"a","type":"user","content":"one"}"#,
            r#"{"id":"b","type":"gemini","content":"two"}"#,
            r#"{"id":"c","type":"user","content":"three"}"#,
        );
        let c = load(&format!("{log}{}\n", r#"{"$rewindTo":"b"}"#));
        assert_eq!(texts(&c), vec!["one"]);

        let c = load(&format!("{log}{}\n", r#"{"$rewindTo":"unknown"}"#));
        assert!(c.messages.is_empty(), "an unknown id clears the log");
    }

    #[test]
    fn patch_updates_removes_and_reorders() {
        let c = load(&format!(
            "{META}\n{}\n{}\n{}\n{}\n{}\n",
            r#"{"id":"a","type":"user","content":"one"}"#,
            r#"{"id":"b","type":"gemini","content":"two"}"#,
            r#"{"id":"c","type":"user","content":"three"}"#,
            r#"{"id":"d","type":"gemini","content":"four"}"#,
            r#"{"$patch":{"id":"a","content":[{"text":"ONE"}],"updates":[{"id":"b","content":"TWO"},{"id":"zz","content":"x"}],"removeIds":["c"],"orderIds":["d","a","missing"]}}"#,
        ));
        assert_eq!(texts(&c), vec!["TWO", "four", "ONE"]);
    }

    #[test]
    fn set_messages_replaces_the_list_and_set_merges_metadata() {
        let c = load(&format!(
            "{META}\n{}\n{}\n{}\n",
            r#"{"id":"a","type":"user","content":"one"}"#,
            r#"{"$set":{"messages":[{"id":"x","type":"user","content":"fresh"}],"summary":"Named","lastUpdated":"2026-07-17T10:00:09.000Z"}}"#,
            r#"{"$set":{"kind":"subagent"}}"#,
        ));
        assert_eq!(texts(&c), vec!["fresh"]);
        assert_eq!(c.summary, "Named");
        assert!(c.is_subagent);
        assert_eq!(c.updated_secs, Some(c.start_secs.unwrap() + 9));
    }

    #[test]
    fn a_log_without_id_and_hash_is_read_as_a_legacy_document() {
        let pretty = "{\n  \"sessionId\": \"abc\",\n  \"messages\": [\n    {\"id\": \"a\", \"type\": \"user\", \"content\": \"hello\"}\n  ]\n}\n";
        let c = load(pretty);
        assert_eq!(c.session_id, "abc");
        assert_eq!(texts(&c), vec!["hello"]);

        assert!(load_conversation(b"not json\n{\"id\":\"a\",\"type\":\"user\"}\n").is_none());
        assert!(load_conversation(b"").is_none());
    }

    #[test]
    fn parts_text_keeps_only_text() {
        let v: Value = serde_json::from_str(
            r#"["a",{"text":"b"},{"functionCall":{"name":"x"}},{"thought":true,"text":"hidden"},[{"text":"c"}],{"inlineData":{"mimeType":"image/png","data":"AAAA"}}]"#,
        )
        .unwrap();
        assert_eq!(parts_text(&v), "abc");
        assert_eq!(parts_text(&Value::Null), "");
    }

    #[test]
    fn ignored_user_content_matches_the_cli_rule() {
        for ignored in [
            "",
            "  \n",
            "/help",
            "?",
            " <session_context>\nx",
            "<hook_context>y</hook_context>",
        ] {
            assert!(is_ignored_user_content(ignored), "{ignored:?}");
        }
        assert!(!is_ignored_user_content("fix /etc/hosts"));
    }

    #[test]
    fn resume_command_shapes() {
        let a = GeminiAdapter::with_root(PathBuf::from("/nope"));
        assert!(!a.is_available());
        assert!(a.supports_yolo());
        assert!(!a.supports_fork());
        let s = build_session(
            load(&format!(
                "{META}\n{}\n",
                r#"{"id":"a","type":"user","content":"go"}"#
            )),
            "/work/app".into(),
            0,
        )
        .unwrap();
        assert_eq!(
            a.resume_command(&s, false),
            vec!["gemini", "--resume", "11111111-2222-4333-8444-555555555555"]
        );
        assert_eq!(
            a.resume_command(&s, true),
            vec![
                "gemini",
                "--yolo",
                "--resume",
                "11111111-2222-4333-8444-555555555555"
            ]
        );
    }
}

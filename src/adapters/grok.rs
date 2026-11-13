// Grok Build keeps one directory per session:
// `<root>/sessions/<percent-encoded cwd>/<session id>/`, holding
// `summary.json` (metadata) beside `updates.jsonl` (an ACP update stream).
// Message chunks are merged by promptIndex and promptId. No Grok data was at
// hand when this was written, so nothing here was checked against real files.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::Value;

use super::{mtime_nanos, nanos_to_secs, parse_iso8601_secs, Adapter};
use crate::config::AgentId;
use crate::model::{join_content, truncate_title, Session, ASSISTANT_PREFIX, USER_PREFIX};

const TITLE_MAX: usize = 80;
const SUMMARY_FILE: &str = "summary.json";
const UPDATES_FILE: &str = "updates.jsonl";

/// Numeric timestamps above this are milliseconds, the rest are seconds.
const MILLIS_THRESHOLD: i64 = 100_000_000_000;

pub struct GrokAdapter {
    sessions_dir: PathBuf,
}

impl GrokAdapter {
    pub fn new() -> GrokAdapter {
        GrokAdapter::with_root(default_root())
    }

    /// `root` is the Grok home directory, the one that holds `sessions/`.
    pub fn with_root(root: PathBuf) -> GrokAdapter {
        GrokAdapter {
            sessions_dir: root.join("sessions"),
        }
    }
}

impl Default for GrokAdapter {
    fn default() -> Self {
        Self::new()
    }
}

/// `$GROK_HOME` when it is set and not blank, else `~/.grok`.
pub fn default_root() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    let grok_home = std::env::var("GROK_HOME").ok();
    root_from(grok_home.as_deref(), Path::new(&home))
}

fn root_from(grok_home: Option<&str>, home: &Path) -> PathBuf {
    match grok_home {
        Some(value) if !value.trim().is_empty() => {
            if value == "~" {
                home.to_path_buf()
            } else if let Some(rest) = value.strip_prefix("~/") {
                home.join(rest)
            } else {
                PathBuf::from(value)
            }
        }
        _ => home.join(".grok"),
    }
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect()
}

fn file_name(path: &Path) -> &str {
    path.file_name().and_then(|n| n.to_str()).unwrap_or("")
}

fn read_summary(session_dir: &Path) -> Option<Value> {
    let bytes = std::fs::read(session_dir.join(SUMMARY_FILE)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Newest of `updates.jsonl` and `summary.json`. Grok rewrites the summary
/// (title, kind, hidden) without touching the update stream.
fn session_mtime(session_dir: &Path) -> Option<i64> {
    let updates = mtime_nanos(&session_dir.join(UPDATES_FILE)).ok()?;
    let summary = mtime_nanos(&session_dir.join(SUMMARY_FILE)).unwrap_or(0);
    Some(updates.max(summary))
}

/// Grok's `Summary::is_hidden`: an explicit `hidden` wins, otherwise subagent
/// kinds are hidden. Forks set `parent_session_id` too, so that field is not a
/// subagent marker.
fn is_hidden(summary: &Value) -> bool {
    summary
        .get("hidden")
        .and_then(Value::as_bool)
        .unwrap_or_else(|| {
            summary
                .get("session_kind")
                .and_then(Value::as_str)
                .is_some_and(|kind| kind.starts_with("subagent"))
        })
}

/// The id `grok --resume` takes: `info.id`, else the session directory name.
/// `scan` and `parse` must agree on it.
fn session_id(summary: &Value, dir_name: &str) -> String {
    summary
        .pointer("/info/id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .unwrap_or(dir_name)
        .to_string()
}

fn hex_value(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// `%XX` becomes one byte and anything else is kept. If the result is not
/// UTF-8 the input comes back unchanged.
fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_value(bytes[i + 1]), hex_value(bytes[i + 2])) {
                out.push(hi * 16 + lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| value.to_string())
}

/// The workspace directory is named after the percent-encoded cwd.
fn directory_from_workspace(workspace: &str) -> String {
    let decoded = percent_decode(workspace);
    if Path::new(&decoded).is_absolute() {
        decoded
    } else {
        String::new()
    }
}

fn content_text(content: &Value) -> String {
    if let Some(text) = content.as_str() {
        return text.to_string();
    }
    if let Some(text) = content.get("text").and_then(Value::as_str) {
        return text.to_string();
    }
    content
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Unix seconds from an ISO 8601 string or from a number in seconds or
/// milliseconds.
fn timestamp_secs(value: Option<&Value>) -> Option<i64> {
    let value = value?;
    if let Some(text) = value.as_str() {
        return parse_iso8601_secs(text.trim());
    }
    let number = value
        .as_i64()
        .or_else(|| value.as_f64().map(|f| f as i64))?;
    if number <= 0 {
        return None;
    }
    Some(if number > MILLIS_THRESHOLD {
        number / 1000
    } else {
        number
    })
}

struct Message {
    user: bool,
    text: String,
}

/// The conversation rebuilt from the update stream. Text arrives in chunks,
/// so a message stays "pending" while the following chunks belong to it.
#[derive(Default)]
struct Transcript {
    messages: Vec<Message>,
    /// Position in `messages` of each user prompt, which is what a rewind
    /// marker's target indexes.
    user_positions: Vec<usize>,
    /// The promptIndex and position of the user message still being streamed.
    pending_user: Option<(Option<u64>, usize)>,
    /// The promptId and position of the agent message still being streamed.
    pending_agent: Option<(String, usize)>,
    seen_prompt_index: bool,
    last_activity: Option<i64>,
}

impl Transcript {
    fn apply(&mut self, record: &Value) {
        if let Some(ts) = timestamp_secs(record.get("timestamp")) {
            if self.last_activity.is_none_or(|current| ts > current) {
                self.last_activity = Some(ts);
            }
        }
        let params = record.get("params").unwrap_or(&Value::Null);
        let update = params.get("update").unwrap_or(&Value::Null);
        let kind = update
            .get("sessionUpdate")
            .and_then(Value::as_str)
            .unwrap_or("");
        match kind {
            "user_message_chunk" => self.user_chunk(update),
            "agent_message_chunk" => self.agent_chunk(params, update),
            "rewind_marker" => self.rewind(update),
            // Tool calls, plans and thoughts end a user message but not an
            // agent message, which carries on under the same promptId.
            _ => self.pending_user = None,
        }
    }

    fn user_chunk(&mut self, update: &Value) {
        self.pending_agent = None;
        let content = update.get("content").unwrap_or(&Value::Null);
        // A shell command typed at the prompt, not a message to the model.
        if content.pointer("/_meta/bashCommand").is_some() {
            self.pending_user = None;
            return;
        }
        let text = content_text(content);
        if text.is_empty() {
            return;
        }
        let prompt_index = update.pointer("/_meta/promptIndex").and_then(Value::as_u64);
        if prompt_index.is_some() {
            self.seen_prompt_index = true;
        }
        // Once prompts carry an index, chunks without one are not prompts.
        if self.seen_prompt_index && prompt_index.is_none() {
            self.pending_user = None;
            return;
        }
        if let Some((pending_index, position)) = self.pending_user {
            if pending_index == prompt_index {
                if let Some(message) = self.messages.get_mut(position).filter(|m| m.user) {
                    message.text.push_str(&text);
                    return;
                }
            }
        }
        let position = self.messages.len();
        self.user_positions.push(position);
        self.messages.push(Message { user: true, text });
        self.pending_user = Some((prompt_index, position));
    }

    fn agent_chunk(&mut self, params: &Value, update: &Value) {
        self.pending_user = None;
        let text = content_text(update.get("content").unwrap_or(&Value::Null));
        if text.trim().is_empty() {
            return;
        }
        let prompt_id = params
            .pointer("/_meta/promptId")
            .and_then(Value::as_str)
            .unwrap_or("");
        if let Some((pending_id, position)) = &self.pending_agent {
            if pending_id == prompt_id {
                if let Some(message) = self.messages.get_mut(*position).filter(|m| !m.user) {
                    message.text.push_str(&text);
                    return;
                }
            }
        }
        let position = self.messages.len();
        self.messages.push(Message { user: false, text });
        self.pending_agent = Some((prompt_id.to_string(), position));
    }

    /// Drops the target prompt and everything after it; Grok keeps the
    /// abandoned records in the file.
    fn rewind(&mut self, update: &Value) {
        let target = update
            .get("target_prompt_index")
            .or_else(|| update.get("targetPromptIndex"))
            .and_then(Value::as_u64)
            .and_then(|index| usize::try_from(index).ok());
        if let Some(target) = target {
            if let Some(position) = self.user_positions.get(target).copied() {
                self.messages.truncate(position);
                self.user_positions.truncate(target);
            }
        }
        self.pending_user = None;
        self.pending_agent = None;
    }
}

/// Builds a session from the two files of one session directory. `dir_name`
/// is the session directory's name and `workspace` its parent's name.
/// Returns `None` for an unreadable summary, a hidden session, or a session
/// without a user prompt.
pub fn parse_session(
    summary_bytes: &[u8],
    updates_bytes: &[u8],
    dir_name: &str,
    workspace: &str,
    mtime_ns: i64,
) -> Option<Session> {
    let summary: Value = serde_json::from_slice(summary_bytes).ok()?;
    if is_hidden(&summary) {
        return None;
    }

    let mut transcript = Transcript::default();
    for line in updates_bytes.split(|&b| b == b'\n') {
        if line.iter().all(|b| b.is_ascii_whitespace()) {
            continue;
        }
        let Ok(record) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        transcript.apply(&record);
    }

    let first_user = transcript
        .messages
        .iter()
        .find(|m| m.user && !m.text.trim().is_empty())
        .map(|m| m.text.trim().to_string())?;

    let title_source = ["generated_title", "session_summary"]
        .into_iter()
        .find_map(|field| {
            summary
                .get(field)
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
        })
        .unwrap_or(&first_user);

    let directory = summary
        .pointer("/info/cwd")
        .and_then(Value::as_str)
        .filter(|cwd| !cwd.trim().is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| directory_from_workspace(workspace));

    let timestamp = transcript
        .last_activity
        .or_else(|| {
            ["updated_at", "last_active_at", "created_at"]
                .into_iter()
                .find_map(|field| timestamp_secs(summary.get(field)))
        })
        .unwrap_or_else(|| nanos_to_secs(mtime_ns));

    let messages: Vec<String> = transcript
        .messages
        .iter()
        .filter(|m| !m.text.trim().is_empty())
        .map(|m| {
            let prefix = if m.user {
                USER_PREFIX
            } else {
                ASSISTANT_PREFIX
            };
            format!("{prefix}{}", m.text.trim())
        })
        .collect();

    Some(Session {
        id: session_id(&summary, dir_name),
        agent: AgentId::Grok,
        title: truncate_title(title_source, TITLE_MAX, false),
        directory,
        timestamp,
        content: join_content(&messages),
        message_count: messages.len() as u32,
        mtime: mtime_ns,
        yolo: false,
        source_path: None,
    })
}

impl Adapter for GrokAdapter {
    fn id(&self) -> AgentId {
        AgentId::Grok
    }

    fn supports_yolo(&self) -> bool {
        true
    }

    fn is_available(&self) -> bool {
        self.sessions_dir.is_dir()
    }

    /// Maps each session id to its `updates.jsonl`. Sessions Grok hides are
    /// left out so they are never indexed.
    fn scan(&self) -> Result<HashMap<String, (PathBuf, i64)>> {
        let mut out: HashMap<String, (PathBuf, i64)> = HashMap::new();
        for workspace in subdirs(&self.sessions_dir) {
            for session_dir in subdirs(&workspace) {
                let dir_name = file_name(&session_dir);
                if dir_name.is_empty() {
                    continue;
                }
                let updates = session_dir.join(UPDATES_FILE);
                if !updates.is_file() {
                    continue;
                }
                let Some(mtime) = session_mtime(&session_dir) else {
                    continue;
                };
                let id = match read_summary(&session_dir) {
                    Some(summary) if is_hidden(&summary) => continue,
                    Some(summary) => session_id(&summary, dir_name),
                    // A missing or half-written summary: `parse` returns
                    // nothing for it, but listing the directory name keeps a
                    // session that is already indexed from being deleted.
                    None => dir_name.to_string(),
                };
                // The same id under two workspaces: the newest copy wins.
                let newer = match out.get(&id) {
                    Some((path, known)) => (mtime, &updates) > (*known, path),
                    None => true,
                };
                if newer {
                    out.insert(id, (updates, mtime));
                }
            }
        }
        Ok(out)
    }

    /// `path` is the `updates.jsonl` that `scan` returned.
    fn parse(&self, path: &Path) -> Result<Option<Session>> {
        let Some(session_dir) = path.parent() else {
            return Ok(None);
        };
        let Ok(summary_bytes) = std::fs::read(session_dir.join(SUMMARY_FILE)) else {
            return Ok(None);
        };
        let Ok(updates_bytes) = std::fs::read(path) else {
            return Ok(None);
        };
        let workspace = session_dir.parent().map(file_name).unwrap_or("");
        let mtime = session_mtime(session_dir).unwrap_or(0);
        let mut session = parse_session(
            &summary_bytes,
            &updates_bytes,
            file_name(session_dir),
            workspace,
            mtime,
        );
        if let Some(s) = &mut session {
            s.source_path = Some(path.to_string_lossy().to_string());
        }
        Ok(session)
    }

    fn resume_command(&self, session: &Session, yolo: bool) -> Vec<String> {
        let mut cmd = vec!["grok".to_string()];
        if yolo {
            cmd.push("--always-approve".to_string());
        }
        cmd.push("--resume".to_string());
        cmd.push(session.id.clone());
        cmd
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn percent_decode_decodes_escapes_and_keeps_the_rest() {
        assert_eq!(percent_decode("%2Fwork%2Fproject"), "/work/project");
        assert_eq!(percent_decode("%2fwork%20dir"), "/work dir");
        assert_eq!(percent_decode("plain"), "plain");
        assert_eq!(percent_decode("%2Fcaf%C3%A9"), "/café");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz%2"), "%zz%2");
        assert_eq!(percent_decode("%+f"), "%+f");
        assert_eq!(percent_decode("%é%2F"), "%é/");
    }

    #[test]
    fn percent_decode_returns_the_input_when_the_bytes_are_not_utf8() {
        assert_eq!(percent_decode("%2Fwork%FF"), "%2Fwork%FF");
    }

    #[test]
    fn workspace_directory_must_decode_to_an_absolute_path() {
        assert_eq!(directory_from_workspace("%2Fwork%2Fapp"), "/work/app");
        assert_eq!(directory_from_workspace("relative%2Fapp"), "");
        assert_eq!(directory_from_workspace(""), "");
    }

    #[test]
    fn root_prefers_grok_home_and_expands_a_tilde() {
        let home = Path::new("/home/u");
        assert_eq!(root_from(None, home), PathBuf::from("/home/u/.grok"));
        assert_eq!(root_from(Some("  "), home), PathBuf::from("/home/u/.grok"));
        assert_eq!(
            root_from(Some("/opt/grok"), home),
            PathBuf::from("/opt/grok")
        );
        assert_eq!(root_from(Some("~"), home), PathBuf::from("/home/u"));
        assert_eq!(root_from(Some("~/g"), home), PathBuf::from("/home/u/g"));
    }

    #[test]
    fn timestamps_accept_strings_seconds_and_milliseconds() {
        let secs = parse_iso8601_secs("2026-07-17T10:00:00Z").unwrap();
        assert_eq!(
            timestamp_secs(Some(&json!("2026-07-17T10:00:00Z"))),
            Some(secs)
        );
        assert_eq!(timestamp_secs(Some(&json!(secs))), Some(secs));
        assert_eq!(timestamp_secs(Some(&json!(secs * 1000 + 250))), Some(secs));
        assert_eq!(timestamp_secs(Some(&json!(secs as f64 + 0.5))), Some(secs));
        assert_eq!(timestamp_secs(Some(&json!(0))), None);
        assert_eq!(timestamp_secs(Some(&json!("soon"))), None);
        assert_eq!(timestamp_secs(Some(&json!(null))), None);
        assert_eq!(timestamp_secs(None), None);
    }

    #[test]
    fn hidden_rule_matches_groks_own() {
        assert!(!is_hidden(&json!({})));
        assert!(!is_hidden(
            &json!({"session_kind": "fork", "parent_session_id": "p"})
        ));
        assert!(is_hidden(&json!({"session_kind": "subagent_fork"})));
        assert!(is_hidden(&json!({"session_kind": "fork", "hidden": true})));
        assert!(!is_hidden(
            &json!({"session_kind": "subagent", "hidden": false})
        ));
    }
}

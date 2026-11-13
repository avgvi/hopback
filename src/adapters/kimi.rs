// Kimi Code sessions: discovery (the current layout and the older
// `session-meta` one), the wire.jsonl transcript reconstruction (steps, undo,
// clear, compaction, skill, plugin and shell commands) and the title and
// directory fallbacks.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader};
use std::path::{Component, Path, PathBuf};

use anyhow::Result;
use serde_json::Value;

use super::{mtime_nanos, nanos_to_secs, parse_iso8601_secs, Adapter};
use crate::config::AgentId;
use crate::model::{join_content, truncate_title, Session, ASSISTANT_PREFIX, USER_PREFIX};

const TITLE_MAX: usize = 80;
const FALLBACK_TITLE: &str = "Kimi session";

/// Kimi Code keeps one directory per session:
/// `<root>/sessions/wd_<slug>_<hash>/session_<uuid>/` with `state.json` and
/// the main agent's transcript in `agents/main/wire.jsonl`. The session id is
/// the directory name, `session_` prefix included.
pub struct KimiAdapter {
    root: PathBuf,
}

impl KimiAdapter {
    pub fn new() -> KimiAdapter {
        KimiAdapter::with_root(default_root())
    }

    /// `root` is the Kimi Code home, the directory that holds `sessions/` and
    /// `session_index.jsonl`.
    pub fn with_root(root: PathBuf) -> KimiAdapter {
        KimiAdapter { root }
    }

    fn sessions_dir(&self) -> PathBuf {
        self.root.join("sessions")
    }

    fn session_index_file(&self) -> PathBuf {
        self.root.join("session_index.jsonl")
    }

    /// Working directory recorded for this session in `session_index.jsonl`.
    /// Only used when `state.json` names no directory. Later lines win, a
    /// `deleted` line removes the id, and an entry counts only when its
    /// `sessionDir` is this session's own directory inside the sessions root.
    fn indexed_work_dir(&self, state_file: &Path, session_id: &str) -> Option<String> {
        let session_dir = normalized_absolute_path(session_dir_of(state_file)?)?;
        let sessions_root = normalized_absolute_path(&self.sessions_dir())?;
        let bytes = std::fs::read(self.session_index_file()).ok()?;

        let mut found: Option<(PathBuf, String)> = None;
        for line in bytes.split(|&b| b == b'\n') {
            if line.iter().all(|b| b.is_ascii_whitespace()) {
                continue;
            }
            let Ok(entry) = serde_json::from_slice::<Value>(line) else {
                continue;
            };
            if str_at(&entry, &["sessionId"]) != session_id {
                continue;
            }
            if entry.get("deleted").and_then(Value::as_bool) == Some(true) {
                found = None;
                continue;
            }
            let Some(indexed_dir) = entry.get("sessionDir").and_then(Value::as_str) else {
                continue;
            };
            let Some(work_dir) = entry.get("workDir").and_then(Value::as_str) else {
                continue;
            };
            let Some(indexed_dir) =
                validated_index_session_dir(&sessions_root, session_id, Path::new(indexed_dir))
            else {
                continue;
            };
            found = Some((indexed_dir, work_dir.to_string()));
        }

        let (indexed_dir, work_dir) = found?;
        (!work_dir.trim().is_empty() && indexed_dir == session_dir).then_some(work_dir)
    }
}

impl Default for KimiAdapter {
    fn default() -> Self {
        Self::new()
    }
}

/// `$KIMI_CODE_HOME` (tilde-expanded, ignored when blank), else `~/.kimi-code`.
pub fn default_root() -> PathBuf {
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into()));
    match std::env::var("KIMI_CODE_HOME") {
        Ok(value) if !value.trim().is_empty() => expand_tilde(&value, &home),
        _ => home.join(".kimi-code"),
    }
}

fn expand_tilde(path: &str, home: &Path) -> PathBuf {
    if path == "~" {
        return home.to_path_buf();
    }
    match path.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None => PathBuf::from(path),
    }
}

/// Every `state.json` below `dir`. Symlinked directories are not followed.
fn collect_state_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if file_type.is_dir() {
            collect_state_files(&path, out);
        } else if entry.file_name() == "state.json" {
            out.push(path);
        }
    }
}

/// Older builds wrote `<session dir>/session-meta/state.json`.
fn is_legacy_state_file(state_file: &Path) -> bool {
    state_file
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        == Some("session-meta")
}

fn session_dir_of(state_file: &Path) -> Option<&Path> {
    let metadata_dir = state_file.parent()?;
    if is_legacy_state_file(state_file) {
        metadata_dir.parent()
    } else {
        Some(metadata_dir)
    }
}

fn session_id_of(state_file: &Path) -> Option<String> {
    session_dir_of(state_file)?
        .file_name()?
        .to_str()
        .filter(|name| !name.is_empty())
        .map(ToString::to_string)
}

fn wire_file_of(state_file: &Path) -> PathBuf {
    session_dir_of(state_file)
        .unwrap_or_else(|| Path::new(""))
        .join("agents")
        .join("main")
        .join("wire.jsonl")
}

/// Newest of `state.json` and the main wire file, so a new message is seen
/// even when Kimi has not rewritten the state yet.
fn session_mtime(state_file: &Path) -> Option<i64> {
    let state = mtime_nanos(state_file).ok()?;
    let wire = mtime_nanos(&wire_file_of(state_file)).unwrap_or(0);
    Some(state.max(wire))
}

fn normalized_absolute_path(path: &Path) -> Option<PathBuf> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
        }
    }
    Some(normalized)
}

fn validated_index_session_dir(
    sessions_root: &Path,
    session_id: &str,
    session_dir: &Path,
) -> Option<PathBuf> {
    if !session_dir.is_absolute() {
        return None;
    }
    let session_dir = normalized_absolute_path(session_dir)?;
    if !session_dir.starts_with(sessions_root)
        || session_dir.file_name().and_then(|name| name.to_str()) != Some(session_id)
    {
        return None;
    }
    Some(session_dir)
}

fn str_at<'a>(value: &'a Value, path: &[&str]) -> &'a str {
    let mut current = value;
    for key in path {
        match current.get(*key) {
            Some(next) => current = next,
            None => return "",
        }
    }
    current.as_str().unwrap_or("")
}

fn non_blank_string(value: &Value, key: &str) -> Option<String> {
    let text = str_at(value, &[key]);
    (!text.trim().is_empty()).then(|| text.to_string())
}

fn state_title(state: &Value) -> Option<String> {
    // Current states carry a boolean `isCustomTitle`; `customTitle` is only
    // meaningful in the older schema.
    if state.get("isCustomTitle").is_some_and(Value::is_boolean) {
        return non_blank_string(state, "title");
    }
    non_blank_string(state, "customTitle").or_else(|| non_blank_string(state, "title"))
}

/// `updatedAt` / `createdAt`: milliseconds since the epoch, or an ISO string
/// in older states.
fn state_timestamp_secs(state: &Value, key: &str) -> Option<i64> {
    let value = state.get(key)?;
    if let Some(ms) = value.as_i64().or_else(|| value.as_f64().map(|v| v as i64)) {
        return (ms > 0).then_some(ms / 1000);
    }
    value.as_str().and_then(parse_iso8601_secs)
}

fn content_texts(content: &Value) -> Vec<String> {
    match content {
        Value::String(text) if !text.is_empty() => vec![text.clone()],
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| {
                part.get("text")
                    .and_then(Value::as_str)
                    .or_else(|| part.get("input_text").and_then(Value::as_str))
                    .or_else(|| part.as_str())
            })
            .filter(|text| !text.is_empty())
            .map(ToString::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

/// One block of the reconstructed transcript: a message, an assistant step or
/// a compaction summary.
#[derive(Debug, Default)]
struct TranscriptEntry {
    rendered: Vec<String>,
    user_turn_text: Option<String>,
    undo_anchor: bool,
    compaction_summary: bool,
}

/// Replays `wire.jsonl` records into the conversation as it stands now.
struct Transcript {
    entries: Vec<TranscriptEntry>,
    /// Entries before this index were cleared from the context; they stay
    /// searchable but a later undo cannot reach them.
    clear_floor: usize,
    open_steps: HashMap<String, usize>,
    pending_tool_results: HashSet<String>,
    /// Messages that arrived while tool calls were running. They are placed
    /// after the step that issued the calls.
    deferred: Vec<TranscriptEntry>,
    /// False once an undo or clear means `state.lastPrompt` may name a prompt
    /// that is no longer part of the conversation.
    allow_last_prompt_fallback: bool,
}

impl Transcript {
    fn new() -> Transcript {
        Transcript {
            entries: Vec::new(),
            clear_floor: 0,
            open_steps: HashMap::new(),
            pending_tool_results: HashSet::new(),
            deferred: Vec::new(),
            allow_last_prompt_fallback: true,
        }
    }

    fn read(wire_file: &Path) -> Transcript {
        let mut transcript = Transcript::new();
        let Ok(file) = std::fs::File::open(wire_file) else {
            return transcript;
        };
        let mut reader = BufReader::new(file);
        let mut line = Vec::new();
        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            if line.iter().all(|b| b.is_ascii_whitespace()) {
                continue;
            }
            let Ok(record) = serde_json::from_slice::<Value>(&line) else {
                continue;
            };
            transcript.apply(&record);
        }
        transcript
    }

    fn apply(&mut self, record: &Value) {
        match str_at(record, &["type"]) {
            "context.append_message" => {
                let message = record.get("message").unwrap_or(&Value::Null);
                if let Some(entry) = message_entry(message) {
                    if entry.user_turn_text.is_some() {
                        self.allow_last_prompt_fallback = true;
                    }
                    if self.pending_tool_results.is_empty() {
                        self.entries.push(entry);
                    } else {
                        self.deferred.push(entry);
                    }
                }
            }
            "context.append_loop_event" => {
                self.apply_loop_event(record.get("event").unwrap_or(&Value::Null));
            }
            "context.apply_compaction" => {
                self.entries.push(compaction_entry(record));
                self.reset_in_flight();
            }
            "context.undo" => {
                let count = record
                    .get("count")
                    .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)))
                    .unwrap_or(0);
                self.undo(count);
                self.reset_in_flight();
                if count > 0 {
                    self.allow_last_prompt_fallback = false;
                }
            }
            "context.clear" => {
                self.clear_floor = self.entries.len();
                self.reset_in_flight();
                self.allow_last_prompt_fallback = false;
            }
            _ => {}
        }
    }

    fn apply_loop_event(&mut self, event: &Value) {
        match str_at(event, &["type"]) {
            "step.begin" => {
                self.pending_tool_results.clear();
                self.entries.append(&mut self.deferred);
                let step_id = str_at(event, &["uuid"]);
                if !step_id.is_empty() {
                    self.entries.push(TranscriptEntry::default());
                    self.open_steps
                        .insert(step_id.to_string(), self.entries.len() - 1);
                }
            }
            "content.part" => {
                // `think` parts are the model's private reasoning; only text
                // parts are indexed.
                let part = event.get("part").unwrap_or(&Value::Null);
                let text = str_at(part, &["text"]);
                if str_at(part, &["type"]) != "text" || text.is_empty() {
                    return;
                }
                let Some(index) = self.open_steps.get(str_at(event, &["stepUuid"])).copied() else {
                    return;
                };
                if let Some(entry) = self.entries.get_mut(index) {
                    append_assistant_text(entry, text);
                }
            }
            "tool.call" => {
                let tool_call_id = str_at(event, &["toolCallId"]);
                if !tool_call_id.is_empty()
                    && self.open_steps.contains_key(str_at(event, &["stepUuid"]))
                {
                    self.pending_tool_results.insert(tool_call_id.to_string());
                }
            }
            "tool.result" => {
                if self
                    .pending_tool_results
                    .remove(str_at(event, &["toolCallId"]))
                    && self.pending_tool_results.is_empty()
                {
                    self.entries.append(&mut self.deferred);
                }
            }
            "step.end" => {
                self.open_steps.remove(str_at(event, &["uuid"]));
                if self.pending_tool_results.is_empty() {
                    self.entries.append(&mut self.deferred);
                }
            }
            _ => {}
        }
    }

    fn reset_in_flight(&mut self) {
        self.open_steps.clear();
        self.pending_tool_results.clear();
        self.deferred.clear();
    }

    /// Drops entries from the end until `count` user prompts are gone. Stops
    /// at a compaction summary and never crosses the last clear.
    fn undo(&mut self, count: i64) {
        if count <= 0 {
            return;
        }
        let mut removed_prompts = 0i64;
        while self.entries.len() > self.clear_floor {
            if self.entries.last().is_some_and(|e| e.compaction_summary) {
                break;
            }
            let Some(entry) = self.entries.pop() else {
                break;
            };
            if entry.undo_anchor {
                removed_prompts += 1;
                if removed_prompts >= count {
                    break;
                }
            }
        }
    }

    fn user_turn_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| entry.user_turn_text.is_some())
            .count()
    }
}

fn append_assistant_text(entry: &mut TranscriptEntry, text: &str) {
    match entry.rendered.last_mut() {
        Some(rendered) => rendered.push_str(text),
        None => entry.rendered.push(format!("{ASSISTANT_PREFIX}{text}")),
    }
}

fn compaction_entry(record: &Value) -> TranscriptEntry {
    let summary = record.get("summary").unwrap_or(&Value::Null);
    let text = summary
        .as_str()
        .map(ToString::to_string)
        .or_else(|| {
            record
                .get("contextSummary")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
        .or_else(|| {
            let texts = content_texts(summary.get("content").unwrap_or(&Value::Null));
            (!texts.is_empty()).then(|| texts.join(""))
        })
        .unwrap_or_default();
    TranscriptEntry {
        rendered: if text.is_empty() {
            Vec::new()
        } else {
            vec![format!("{USER_PREFIX}{text}")]
        },
        compaction_summary: true,
        ..TranscriptEntry::default()
    }
}

/// Injected reminders and the captured output of `!` shell commands are not
/// part of what the user or the assistant said.
fn message_is_searchable(message: &Value) -> bool {
    let kind = str_at(message, &["origin", "kind"]);
    kind != "injection"
        && !(kind == "shell_command" && str_at(message, &["origin", "phase"]) == "output")
}

/// A prompt the user typed: plain input, a slash command, or a `!` command.
/// Task notifications and skills the model activated are not turns.
fn message_is_user_turn(message: &Value) -> bool {
    message_is_undo_anchor(message)
        || (str_at(message, &["role"]) == "user"
            && str_at(message, &["origin", "kind"]) == "shell_command"
            && str_at(message, &["origin", "phase"]) == "input")
}

fn message_is_undo_anchor(message: &Value) -> bool {
    if str_at(message, &["role"]) != "user" {
        return false;
    }
    match str_at(message, &["origin", "kind"]) {
        "" | "user" => true,
        "skill_activation" | "plugin_command" => {
            str_at(message, &["origin", "trigger"]) == "user-slash"
        }
        _ => false,
    }
}

fn message_texts(message: &Value) -> Vec<String> {
    let content = message.get("content").unwrap_or(&Value::Null);
    match str_at(message, &["origin", "kind"]) {
        "skill_activation" => {
            skill_activation_text(message).map_or_else(|| content_texts(content), |t| vec![t])
        }
        "plugin_command" => {
            plugin_command_text(message).map_or_else(|| content_texts(content), |t| vec![t])
        }
        "shell_command" if str_at(message, &["origin", "phase"]) == "input" => {
            content_texts(content)
                .into_iter()
                .map(|text| shell_input(&text).unwrap_or(text))
                .filter(|text| !text.is_empty())
                .collect()
        }
        _ => content_texts(content),
    }
}

/// The message body of a skill activation is the expanded skill prompt; show
/// what the user typed instead.
fn skill_activation_text(message: &Value) -> Option<String> {
    let skill_name = str_at(message, &["origin", "skillName"]);
    if skill_name.is_empty() {
        return None;
    }
    if str_at(message, &["origin", "trigger"]) == "user-slash" {
        Some(slash_command(
            format!("/{skill_name}"),
            str_at(message, &["origin", "skillArgs"]),
        ))
    } else {
        Some(format!("Activated skill: {skill_name}"))
    }
}

fn plugin_command_text(message: &Value) -> Option<String> {
    let plugin_id = str_at(message, &["origin", "pluginId"]);
    let command_name = str_at(message, &["origin", "commandName"]);
    if plugin_id.is_empty() || command_name.is_empty() {
        return None;
    }
    Some(slash_command(
        format!("/{plugin_id}:{command_name}"),
        str_at(message, &["origin", "commandArgs"]),
    ))
}

fn slash_command(command: String, args: &str) -> String {
    let args = args.trim();
    if args.is_empty() {
        command
    } else {
        format!("{command} {args}")
    }
}

fn shell_input(text: &str) -> Option<String> {
    const OPEN: &str = "<bash-input>";
    const CLOSE: &str = "</bash-input>";
    let start = text.find(OPEN)? + OPEN.len();
    let end = start + text[start..].find(CLOSE)?;
    Some(
        text[start..end]
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&amp;", "&")
            .trim()
            .to_string(),
    )
}

fn message_entry(message: &Value) -> Option<TranscriptEntry> {
    if !message_is_searchable(message) {
        return None;
    }
    let prefix = match str_at(message, &["role"]) {
        "user" => USER_PREFIX,
        "assistant" => ASSISTANT_PREFIX,
        _ => return None,
    };
    let texts = message_texts(message);
    let user_turn_text = if message_is_user_turn(message) {
        texts.first().cloned()
    } else {
        None
    };
    Some(TranscriptEntry {
        rendered: texts.iter().map(|text| format!("{prefix}{text}")).collect(),
        user_turn_text,
        undo_anchor: message_is_undo_anchor(message),
        compaction_summary: str_at(message, &["origin", "kind"]) == "compaction_summary",
    })
}

impl Adapter for KimiAdapter {
    fn id(&self) -> AgentId {
        AgentId::Kimi
    }

    fn supports_yolo(&self) -> bool {
        true
    }

    fn is_available(&self) -> bool {
        self.sessions_dir().is_dir()
    }

    /// Lists every session directory, keyed by its name. Whether a session has
    /// a prompt is only known after replaying its wire file, so that check is
    /// left to `parse`.
    fn scan(&self) -> Result<HashMap<String, (PathBuf, i64)>> {
        let mut state_files = Vec::new();
        collect_state_files(&self.sessions_dir(), &mut state_files);
        state_files.sort();

        let mut out: HashMap<String, (PathBuf, i64)> = HashMap::new();
        for state_file in state_files {
            let Some(id) = session_id_of(&state_file) else {
                continue;
            };
            // When both layouts exist for one id, the current one wins.
            if out.get(&id).is_some_and(|(existing, _)| {
                !is_legacy_state_file(existing) && is_legacy_state_file(&state_file)
            }) {
                continue;
            }
            let Some(mtime) = session_mtime(&state_file) else {
                continue;
            };
            out.insert(id, (state_file, mtime));
        }
        Ok(out)
    }

    /// `path` is the session's `state.json`. Returns `Ok(None)` for a session
    /// that never got a prompt and for a state file that is not valid JSON
    /// (typically one being rewritten).
    fn parse(&self, path: &Path) -> Result<Option<Session>> {
        let Ok(bytes) = std::fs::read(path) else {
            return Ok(None);
        };
        let Ok(state) = serde_json::from_slice::<Value>(&bytes) else {
            return Ok(None);
        };
        let session_id = match session_id_of(path) {
            Some(id) => id,
            None => str_at(&state, &["id"]).to_string(),
        };
        if session_id.is_empty() {
            return Ok(None);
        }

        let mut transcript = Transcript::read(&wire_file_of(path));
        let last_prompt = str_at(&state, &["lastPrompt"]);
        let last_prompt_usable =
            transcript.allow_last_prompt_fallback && !last_prompt.trim().is_empty();
        // A session whose prompt never reached the wire file (or whose wire
        // file is gone) is still worth listing under its last prompt.
        if transcript.user_turn_count() == 0 && last_prompt_usable {
            transcript.entries.push(TranscriptEntry {
                rendered: vec![format!("{USER_PREFIX}{last_prompt}")],
                user_turn_text: Some(last_prompt.to_string()),
                undo_anchor: true,
                compaction_summary: false,
            });
        }
        if transcript.user_turn_count() == 0 {
            return Ok(None);
        }

        let first_user_text = transcript
            .entries
            .iter()
            .find_map(|entry| entry.user_turn_text.as_deref())
            .unwrap_or("");
        let title_source = state_title(&state)
            .or_else(|| last_prompt_usable.then(|| last_prompt.to_string()))
            .or_else(|| (!first_user_text.is_empty()).then(|| first_user_text.to_string()))
            .unwrap_or_else(|| FALLBACK_TITLE.to_string());

        let directory = non_blank_string(&state, "cwd")
            .or_else(|| non_blank_string(&state, "workDir"))
            .or_else(|| non_blank_string(state.get("custom").unwrap_or(&Value::Null), "cwd"))
            .or_else(|| self.indexed_work_dir(path, &session_id))
            .unwrap_or_default();

        let mtime = session_mtime(path).unwrap_or(0);
        let timestamp = state_timestamp_secs(&state, "updatedAt")
            .or_else(|| state_timestamp_secs(&state, "createdAt"))
            .unwrap_or_else(|| nanos_to_secs(mtime_nanos(path).unwrap_or(mtime)));

        let message_count = transcript
            .entries
            .iter()
            .filter(|entry| !entry.rendered.is_empty())
            .count() as u32;
        let messages: Vec<String> = transcript
            .entries
            .into_iter()
            .flat_map(|entry| entry.rendered)
            .collect();

        Ok(Some(Session {
            id: session_id,
            agent: AgentId::Kimi,
            title: truncate_title(&title_source, TITLE_MAX, false),
            directory,
            timestamp,
            content: join_content(&messages),
            message_count,
            mtime,
            yolo: false,
            source_path: Some(path.to_string_lossy().to_string()),
        }))
    }

    fn resume_command(&self, session: &Session, yolo: bool) -> Vec<String> {
        let mut cmd = vec!["kimi".to_string()];
        if yolo {
            cmd.push("--yolo".to_string());
        }
        cmd.push("--session".to_string());
        cmd.push(session.id.clone());
        cmd
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn user(text: &str) -> Value {
        json!({"type": "context.append_message",
            "message": {"role": "user", "content": text, "origin": {"kind": "user"}}})
    }

    fn replay(records: &[Value]) -> Transcript {
        let mut transcript = Transcript::new();
        for record in records {
            transcript.apply(record);
        }
        transcript
    }

    fn rendered(transcript: &Transcript) -> Vec<&str> {
        transcript
            .entries
            .iter()
            .flat_map(|entry| entry.rendered.iter().map(String::as_str))
            .collect()
    }

    #[test]
    fn undo_stops_at_the_clear_floor() {
        let t = replay(&[
            user("kept"),
            json!({"type": "context.clear"}),
            user("dropped"),
            json!({"type": "context.undo", "count": 5}),
        ]);
        assert_eq!(rendered(&t), vec!["» kept"]);
        assert!(!t.allow_last_prompt_fallback);
    }

    #[test]
    fn a_new_prompt_after_an_undo_allows_the_last_prompt_again() {
        let t = replay(&[
            user("first"),
            json!({"type": "context.undo", "count": 1}),
            user("second"),
        ]);
        assert_eq!(rendered(&t), vec!["» second"]);
        assert!(t.allow_last_prompt_fallback);
    }

    #[test]
    fn shell_input_is_unwrapped_and_unescaped() {
        assert_eq!(
            shell_input(
                "<bash-input>\nls &lt;dir&gt; &amp;&amp; echo &quot;hi&quot;\n</bash-input>"
            )
            .as_deref(),
            Some("ls <dir> && echo \"hi\"")
        );
        assert_eq!(shell_input("no wrapper"), None);
    }

    #[test]
    fn modern_title_ignores_a_stale_legacy_custom_title() {
        let state = json!({"title": "Modern", "isCustomTitle": false, "customTitle": "Stale"});
        assert_eq!(state_title(&state).as_deref(), Some("Modern"));
        let legacy = json!({"title": "Generated", "customTitle": "Pinned"});
        assert_eq!(state_title(&legacy).as_deref(), Some("Pinned"));
    }

    #[test]
    fn state_timestamp_accepts_millis_and_iso_strings() {
        let state = json!({"a": 1784110807000i64, "b": "2026-07-15T10:00:00Z", "c": 0, "d": ""});
        assert_eq!(state_timestamp_secs(&state, "a"), Some(1_784_110_807));
        assert_eq!(
            state_timestamp_secs(&state, "b"),
            parse_iso8601_secs("2026-07-15T10:00:00Z")
        );
        assert_eq!(state_timestamp_secs(&state, "c"), None);
        assert_eq!(state_timestamp_secs(&state, "d"), None);
        assert_eq!(state_timestamp_secs(&state, "missing"), None);
    }

    #[test]
    fn tilde_in_the_home_override_is_expanded() {
        let home = Path::new("/home/u");
        assert_eq!(expand_tilde("~", home), PathBuf::from("/home/u"));
        assert_eq!(expand_tilde("~/kimi", home), PathBuf::from("/home/u/kimi"));
        assert_eq!(expand_tilde("/opt/kimi", home), PathBuf::from("/opt/kimi"));
    }

    #[test]
    fn index_entries_outside_the_sessions_root_are_rejected() {
        let root = Path::new("/k/sessions");
        assert_eq!(
            validated_index_session_dir(root, "s1", Path::new("/k/sessions/wd/s1")),
            Some(PathBuf::from("/k/sessions/wd/s1"))
        );
        assert_eq!(
            validated_index_session_dir(root, "s1", Path::new("/k/sessions/wd/../../other/s1")),
            None
        );
        assert_eq!(
            validated_index_session_dir(root, "s1", Path::new("/k/sessions/wd/s2")),
            None
        );
        assert_eq!(
            validated_index_session_dir(root, "s1", Path::new("sessions/wd/s1")),
            None
        );
    }
}

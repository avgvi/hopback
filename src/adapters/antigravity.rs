// Antigravity conversations: the discovery rule, the SQLite reader with its
// protobuf wire decoding, the JSONL transcript fallback and the workspace map.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::Value;

use super::{mtime_nanos, nanos_to_secs, parse_iso8601_secs, Adapter};
use crate::config::AgentId;
use crate::model::{join_content, truncate_title, Session, ASSISTANT_PREFIX, USER_PREFIX};

const TITLE_MAX: usize = 80;

const STEP_USER_INPUT: i64 = 14;
const STEP_PLANNER_RESPONSE: i64 = 15;
const STATUS_DONE: i64 = 3;

/// Antigravity CLI (`agy`) keeps one SQLite database per conversation in
/// `conversations/<id>.db` and a generated JSONL transcript of the same
/// conversation in `brain/<id>/.system_generated/logs/`.
pub struct AntigravityAdapter {
    root: PathBuf,
}

impl AntigravityAdapter {
    pub fn new() -> AntigravityAdapter {
        AntigravityAdapter::with_root(default_root())
    }

    pub fn with_root(root: PathBuf) -> AntigravityAdapter {
        AntigravityAdapter { root }
    }

    fn transcript_for_id(&self, id: &str) -> Option<PathBuf> {
        preferred_transcript(&logs_dir(&self.root.join("brain").join(id)))
    }

    /// The two files the workspace map is built from.
    fn workspace_mtime(&self) -> i64 {
        file_mtime(&self.root.join("history.jsonl")).max(file_mtime(
            &self.root.join("cache").join("last_conversations.json"),
        ))
    }

    /// A database session also changes when its transcript does, because the
    /// transcript supplies the messages whenever the database yields none.
    fn database_session_mtime(&self, db: &Path, id: &str) -> i64 {
        let transcript = self
            .transcript_for_id(id)
            .map(|path| file_mtime(&path))
            .unwrap_or(0);
        sqlite_mtime(db).max(transcript)
    }

    /// Conversation id to workspace path. `cache/last_conversations.json`
    /// keeps one conversation per workspace; `history.jsonl` lines that name a
    /// conversation override it.
    fn workspace_map(&self) -> HashMap<String, String> {
        let mut map = self.last_conversations_map();
        let Ok(bytes) = std::fs::read(self.root.join("history.jsonl")) else {
            return map;
        };
        for line in bytes.split(|&b| b == b'\n') {
            let Ok(value) = serde_json::from_slice::<Value>(line) else {
                continue;
            };
            let id = str_field(&value, "conversationId");
            let workspace = str_field(&value, "workspace");
            if !id.is_empty() && !workspace.is_empty() {
                map.insert(id.to_string(), workspace.to_string());
            }
        }
        map
    }

    fn last_conversations_map(&self) -> HashMap<String, String> {
        let path = self.root.join("cache").join("last_conversations.json");
        let Some(value) = std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        else {
            return HashMap::new();
        };
        let Some(object) = value.as_object() else {
            return HashMap::new();
        };
        object
            .iter()
            .filter_map(|(workspace, value)| {
                let id = value.as_str().or_else(|| {
                    value
                        .get("conversationId")
                        .or_else(|| value.get("conversation_id"))
                        .and_then(Value::as_str)
                })?;
                Some((id.to_string(), workspace.to_string()))
            })
            .collect()
    }

    fn workspace_for(&self, id: &str) -> Option<String> {
        self.workspace_map().remove(id)
    }

    fn parse_transcript_session(&self, path: &Path) -> Result<Option<Session>> {
        let Some(id) = transcript_session_id(path) else {
            return Ok(None);
        };
        let Some(conversation) = read_transcript(path)? else {
            return Ok(None);
        };
        let directory = self.workspace_for(&id).unwrap_or_default();
        let mtime = file_mtime(path).max(self.workspace_mtime());
        Ok(Some(
            conversation.into_session(id, directory, path, path, mtime),
        ))
    }

    fn parse_database_session(&self, path: &Path) -> Result<Option<Session>> {
        let Some(id) = path
            .file_stem()
            .and_then(|name| name.to_str())
            .filter(|id| !id.is_empty())
        else {
            return Ok(None);
        };
        let transcript = self.transcript_for_id(id);

        // Steps that are not DONE are skipped in the database but kept in the
        // transcript, so a conversation whose only prompt was cleared is still
        // found through the transcript.
        let (directory, conversation, parsed_file) = match read_database(path) {
            Ok((directory, Some(conversation))) => (directory, conversation, path.to_path_buf()),
            Ok((directory, None)) => {
                let Some(transcript) = transcript else {
                    return Ok(None);
                };
                let Some(conversation) = read_transcript(&transcript)? else {
                    return Ok(None);
                };
                (directory, conversation, transcript)
            }
            Err(err) => {
                let fallback = transcript.and_then(|transcript| {
                    let conversation = read_transcript(&transcript).ok().flatten()?;
                    Some((conversation, transcript))
                });
                let Some((conversation, transcript)) = fallback else {
                    return Err(err).with_context(|| format!("reading {}", path.display()));
                };
                (None, conversation, transcript)
            }
        };

        let directory = directory
            .or_else(|| self.workspace_for(id))
            .unwrap_or_default();
        let mtime = self.database_session_mtime(path, id);
        Ok(Some(conversation.into_session(
            id.to_string(),
            directory,
            path,
            &parsed_file,
            mtime,
        )))
    }
}

impl Default for AntigravityAdapter {
    fn default() -> Self {
        Self::new()
    }
}

pub fn default_root() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".gemini").join("antigravity-cli")
}

impl Adapter for AntigravityAdapter {
    fn id(&self) -> AgentId {
        AgentId::Antigravity
    }

    fn supports_yolo(&self) -> bool {
        true
    }

    fn is_available(&self) -> bool {
        self.root.join("conversations").is_dir() || self.root.join("brain").is_dir()
    }

    /// Ids are the database file stem, or the `brain/` directory name for a
    /// conversation that only has a transcript. `parse` uses the same rule.
    fn scan(&self) -> Result<HashMap<String, (PathBuf, i64)>> {
        let mut out = HashMap::new();

        if let Ok(entries) = std::fs::read_dir(self.root.join("brain")) {
            let workspace_mtime = self.workspace_mtime();
            for entry in entries.flatten() {
                let dir = entry.path();
                if !dir.is_dir() {
                    continue;
                }
                let Some(id) = dir
                    .file_name()
                    .and_then(|name| name.to_str())
                    .filter(|id| !id.is_empty())
                else {
                    continue;
                };
                let Some(transcript) = preferred_transcript(&logs_dir(&dir)) else {
                    continue;
                };
                // A transcript-only session takes its directory from the
                // workspace map, so a change to the map is a change to it.
                let mtime = file_mtime(&transcript).max(workspace_mtime);
                out.insert(id.to_string(), (transcript, mtime));
            }
        }

        if let Ok(entries) = std::fs::read_dir(self.root.join("conversations")) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !is_sqlite_database(&path) {
                    continue;
                }
                let Some(id) = path
                    .file_stem()
                    .and_then(|name| name.to_str())
                    .filter(|id| !id.is_empty())
                else {
                    continue;
                };
                let mtime = self.database_session_mtime(&path, id);
                out.insert(id.to_string(), (path.clone(), mtime));
            }
        }

        Ok(out)
    }

    fn parse(&self, path: &Path) -> Result<Option<Session>> {
        if is_sqlite_database(path) {
            self.parse_database_session(path)
        } else {
            self.parse_transcript_session(path)
        }
    }

    fn resume_command(&self, session: &Session, yolo: bool) -> Vec<String> {
        let mut command = vec!["agy".to_string()];
        if yolo {
            command.push("--dangerously-skip-permissions".to_string());
        }
        command.push("--conversation".to_string());
        command.push(session.id.clone());
        command
    }
}

/// The user and planner messages of one conversation, from either source.
#[derive(Default)]
struct Conversation {
    messages: Vec<String>,
    first_user: String,
    message_count: u32,
    last_activity: Option<i64>,
}

impl Conversation {
    fn push(&mut self, user: bool, text: &str, timestamp: Option<i64>) {
        if user && self.first_user.is_empty() {
            self.first_user = text.to_string();
        }
        if let Some(timestamp) = timestamp {
            if self.last_activity.is_none_or(|current| timestamp > current) {
                self.last_activity = Some(timestamp);
            }
        }
        let prefix = if user { USER_PREFIX } else { ASSISTANT_PREFIX };
        self.messages.push(format!("{prefix}{text}"));
        self.message_count += 1;
    }

    /// A conversation without a user prompt is not a session.
    fn with_prompt(self) -> Option<Conversation> {
        (!self.first_user.is_empty()).then_some(self)
    }

    fn into_session(
        self,
        id: String,
        directory: String,
        source: &Path,
        parsed_file: &Path,
        mtime: i64,
    ) -> Session {
        let timestamp = self
            .last_activity
            .unwrap_or_else(|| nanos_to_secs(file_mtime(parsed_file)));
        Session {
            id,
            agent: AgentId::Antigravity,
            title: truncate_title(&self.first_user, TITLE_MAX, false),
            directory,
            timestamp,
            content: join_content(&self.messages),
            message_count: self.message_count,
            mtime,
            yolo: false,
            source_path: Some(source.to_string_lossy().to_string()),
        }
    }
}

fn str_field<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

fn file_mtime(path: &Path) -> i64 {
    mtime_nanos(path).unwrap_or(0)
}

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

fn sqlite_mtime(path: &Path) -> i64 {
    file_mtime(path).max(file_mtime(&sidecar(path, "-wal")))
}

fn is_sqlite_database(path: &Path) -> bool {
    path.extension().and_then(|extension| extension.to_str()) == Some("db")
}

fn logs_dir(conversation_dir: &Path) -> PathBuf {
    conversation_dir.join(".system_generated").join("logs")
}

/// `transcript_full.jsonl` when it is at least as large as `transcript.jsonl`,
/// otherwise whichever of the two exists.
fn preferred_transcript(logs: &Path) -> Option<PathBuf> {
    let regular = logs.join("transcript.jsonl");
    let full = logs.join("transcript_full.jsonl");
    let size = |path: &Path| path.metadata().ok().map(|metadata| metadata.len());
    match (size(&regular), size(&full)) {
        (Some(regular_size), Some(full_size)) if full_size >= regular_size && full_size > 0 => {
            Some(full)
        }
        (Some(_), _) => Some(regular),
        (_, Some(full_size)) if full_size > 0 => Some(full),
        _ => None,
    }
}

/// `brain/<id>/.system_generated/logs/<file>` to `<id>`.
fn transcript_session_id(path: &Path) -> Option<String> {
    path.parent()?
        .parent()?
        .parent()?
        .file_name()?
        .to_str()
        .filter(|id| !id.is_empty())
        .map(ToString::to_string)
}

/// The transcript has no status filter: cleared and errored steps count.
fn read_transcript(path: &Path) -> Result<Option<Conversation>> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let mut conversation = Conversation::default();

    for line in bytes.split(|&b| b == b'\n') {
        let Ok(step) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        let source = str_field(&step, "source");
        let step_type = str_field(&step, "type");
        if matches!(step_type, "CONVERSATION_HISTORY" | "EPHEMERAL_MESSAGE") {
            continue;
        }
        let is_user = matches!(source, "USER_EXPLICIT" | "USER");
        let is_assistant = source == "MODEL" && step_type == "PLANNER_RESPONSE";
        if !is_user && !is_assistant {
            continue;
        }
        let content = str_field(&step, "content");
        let text = if is_user {
            unwrap_user_request(content)
        } else if content.trim().is_empty() {
            str_field(&step, "thinking").trim()
        } else {
            content.trim()
        };
        if text.is_empty() {
            continue;
        }
        let timestamp = parse_iso8601_secs(str_field(&step, "created_at"));
        conversation.push(is_user, text, timestamp);
    }

    Ok(conversation.with_prompt())
}

/// The prompt without the metadata blocks Antigravity wraps around it.
fn unwrap_user_request(content: &str) -> &str {
    if let Some((_, after)) = content.split_once("<USER_REQUEST>") {
        if let Some((request, _)) = after.split_once("</USER_REQUEST>") {
            return request.trim();
        }
    }
    let end = ["<ADDITIONAL_METADATA>", "<USER_SETTINGS_CHANGE>"]
        .into_iter()
        .filter_map(|marker| content.find(marker))
        .min()
        .unwrap_or(content.len());
    content[..end].trim()
}

/// Opens a conversation database without writing next to it. A plain
/// read-only open of a WAL database creates `-shm` and `-wal` files when they
/// are missing; immutable mode reads the main file only, so use it unless a
/// non-empty WAL holds steps that were never checkpointed.
fn open_read_only(path: &Path) -> rusqlite::Result<Connection> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let pending_wal = std::fs::metadata(sidecar(path, "-wal"))
        .map(|metadata| metadata.len() > 0)
        .unwrap_or(false);

    if !pending_wal {
        let encoded = path
            .to_string_lossy()
            .replace('%', "%25")
            .replace('?', "%3f")
            .replace('#', "%23");
        let uri = format!("file:{encoded}?immutable=1");
        if let Ok(conn) = Connection::open_with_flags(uri, flags | OpenFlags::SQLITE_OPEN_URI) {
            let readable = conn
                .query_row("SELECT COUNT(*) FROM sqlite_master", [], |row| {
                    row.get::<_, i64>(0)
                })
                .is_ok();
            if readable {
                return Ok(conn);
            }
        }
    }

    Connection::open_with_flags(path, flags)
}

/// The workspace directory from the metadata blob, and the conversation when
/// it has a prompt. The directory is returned either way so that a session
/// read from the transcript still gets it.
fn read_database(path: &Path) -> rusqlite::Result<(Option<String>, Option<Conversation>)> {
    let conn = open_read_only(path)?;

    let metadata = conn
        .query_row(
            "SELECT data FROM trajectory_metadata_blob WHERE id = 'main' LIMIT 1",
            [],
            |row| row.get::<_, Option<Vec<u8>>>(0),
        )
        .optional()?
        .flatten();
    let directory = metadata.as_deref().and_then(workspace_directory);

    let mut statement =
        conn.prepare("SELECT step_type, status, metadata, step_payload FROM steps ORDER BY idx")?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, Option<Vec<u8>>>(2)?,
            row.get::<_, Option<Vec<u8>>>(3)?,
        ))
    })?;

    let mut conversation = Conversation::default();
    for row in rows {
        let (column_step_type, column_status, metadata, payload) = row?;
        let Some(payload) = payload else {
            continue;
        };
        let fields = ProtoMessage::new(&payload);
        let step_type = fields
            .first_i64(1)
            .filter(|step_type| *step_type != 0)
            .unwrap_or(column_step_type);
        let status = fields
            .first_i64(4)
            .filter(|status| *status != 0)
            .unwrap_or(column_status);
        if status != 0 && status != STATUS_DONE {
            continue;
        }
        let Some((user, text)) = step_text(fields, step_type) else {
            continue;
        };
        let text = if user {
            unwrap_user_request(&text)
        } else {
            text.trim()
        };
        if text.is_empty() {
            continue;
        }
        let timestamp = fields
            .first_message(5)
            .and_then(|step_metadata| step_metadata.first_message(1))
            .and_then(proto_timestamp)
            .or_else(|| {
                ProtoMessage::new(metadata.as_deref()?)
                    .first_message(1)
                    .and_then(proto_timestamp)
            });
        conversation.push(user, text, timestamp);
    }

    Ok((directory, conversation.with_prompt()))
}

/// User text is `#19.#2` (or `#19.#8`), the planner's visible text `#20.#1`
/// (or `#20.#8`). Tool steps, errors and checkpoints carry neither.
fn step_text(fields: ProtoMessage<'_>, step_type: i64) -> Option<(bool, String)> {
    match step_type {
        STEP_USER_INPUT => fields
            .first_message(19)
            .and_then(|message| message.first_string(2).or_else(|| message.first_string(8)))
            .map(|text| (true, text)),
        STEP_PLANNER_RESPONSE => fields
            .first_message(20)
            .and_then(|message| message.first_string(1).or_else(|| message.first_string(8)))
            .map(|text| (false, text)),
        _ => None,
    }
}

/// `{1: seconds, 2: nanos}` to Unix seconds.
fn proto_timestamp(message: ProtoMessage<'_>) -> Option<i64> {
    let seconds = message.first_i64(1)?;
    let nanos = message.first_i64(2).unwrap_or(0);
    (0..1_000_000_000).contains(&nanos).then_some(seconds)
}

/// The workspace is `#1.#1` (or `#1.#2`, or top-level `#7`), normally a
/// `file://` URI.
fn workspace_directory(data: &[u8]) -> Option<String> {
    let fields = ProtoMessage::new(data);
    let value = fields
        .first_message(1)
        .and_then(|message| message.first_string(1).or_else(|| message.first_string(2)))
        .or_else(|| fields.first_string(7))?;
    if value.starts_with("file:") {
        return file_uri_to_path(&value);
    }
    Some(value)
}

/// `file:///work/my%20app` to `/work/my app`. Only local URIs convert.
fn file_uri_to_path(uri: &str) -> Option<String> {
    let rest = uri.strip_prefix("file:")?;
    let path = match rest.strip_prefix("//") {
        Some(after_slashes) => {
            let slash = after_slashes.find('/')?;
            let host = &after_slashes[..slash];
            if !host.is_empty() && !host.eq_ignore_ascii_case("localhost") {
                return None;
            }
            &after_slashes[slash..]
        }
        None => rest,
    };
    if !path.starts_with('/') {
        return None;
    }
    let end = path.find(['?', '#']).unwrap_or(path.len());
    Some(percent_decode(&path[..end]))
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let hex = |index: usize| {
        bytes
            .get(index)
            .and_then(|byte| (*byte as char).to_digit(16))
    };
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if let (Some(high), Some(low)) = (hex(index + 1), hex(index + 2)) {
                output.push((high * 16 + low) as u8);
                index += 3;
                continue;
            }
        }
        output.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&output).into_owned()
}

// Antigravity stores each step as a protobuf blob but does not publish
// descriptors. These bounded wire readers extract only the fields needed
// here: step kind and status, user or planner text, timestamps and the
// workspace URI.
#[derive(Clone, Copy)]
struct ProtoMessage<'a> {
    data: &'a [u8],
}

impl<'a> ProtoMessage<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data }
    }

    fn fields(self) -> ProtoFields<'a> {
        ProtoFields {
            data: self.data,
            position: 0,
        }
    }

    fn first_i64(self, number: u64) -> Option<i64> {
        self.fields().find_map(|field| match field {
            ProtoField::Varint(field_number, value) if field_number == number => {
                i64::try_from(value).ok()
            }
            _ => None,
        })
    }

    fn first_string(self, number: u64) -> Option<String> {
        self.fields().find_map(|field| match field {
            ProtoField::LengthDelimited(field_number, value) if field_number == number => {
                std::str::from_utf8(value)
                    .ok()
                    .filter(|value| !value.is_empty())
                    .map(ToString::to_string)
            }
            _ => None,
        })
    }

    fn first_message(self, number: u64) -> Option<ProtoMessage<'a>> {
        self.fields().find_map(|field| match field {
            ProtoField::LengthDelimited(field_number, value) if field_number == number => {
                Some(ProtoMessage::new(value))
            }
            _ => None,
        })
    }
}

struct ProtoFields<'a> {
    data: &'a [u8],
    position: usize,
}

enum ProtoField<'a> {
    Varint(u64, u64),
    LengthDelimited(u64, &'a [u8]),
    Fixed32,
    Fixed64,
}

impl<'a> Iterator for ProtoFields<'a> {
    type Item = ProtoField<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let key = read_proto_varint(self.data, &mut self.position)?;
        let field_number = key >> 3;
        if field_number == 0 {
            return None;
        }
        match key & 7 {
            0 => read_proto_varint(self.data, &mut self.position)
                .map(|value| ProtoField::Varint(field_number, value)),
            1 => {
                self.position = self.position.checked_add(8)?;
                (self.position <= self.data.len()).then_some(ProtoField::Fixed64)
            }
            2 => {
                let length =
                    usize::try_from(read_proto_varint(self.data, &mut self.position)?).ok()?;
                let end = self.position.checked_add(length)?;
                if end > self.data.len() {
                    return None;
                }
                let value = &self.data[self.position..end];
                self.position = end;
                Some(ProtoField::LengthDelimited(field_number, value))
            }
            5 => {
                self.position = self.position.checked_add(4)?;
                (self.position <= self.data.len()).then_some(ProtoField::Fixed32)
            }
            _ => None,
        }
    }
}

fn read_proto_varint(data: &[u8], position: &mut usize) -> Option<u64> {
    let mut value = 0u64;
    let mut shift = 0u32;
    while *position < data.len() && shift < 64 {
        let byte = data[*position];
        *position += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte < 0x80 {
            return Some(value);
        }
        shift += 7;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_reads_multi_byte_values_and_stops_on_truncation() {
        let mut position = 0;
        assert_eq!(
            read_proto_varint(&[0xa0, 0xfa, 0xe7, 0xd2, 0x06], &mut position),
            Some(1_784_282_400)
        );
        assert_eq!(position, 5);

        let mut position = 0;
        assert_eq!(read_proto_varint(&[0x80, 0x80], &mut position), None);
    }

    #[test]
    fn lookups_return_the_first_field_of_the_right_wire_type() {
        // #1 varint 14, #19 { #2 "hi" }, #1 varint 15, then a length that
        // overruns the buffer.
        let data = [
            0x08, 0x0e, 0x9a, 0x01, 0x04, 0x12, 0x02, b'h', b'i', 0x08, 0x0f, 0x12, 0x7f,
        ];
        let message = ProtoMessage::new(&data);
        assert_eq!(message.first_i64(1), Some(14));
        assert_eq!(
            message.first_message(19).and_then(|m| m.first_string(2)),
            Some("hi".to_string())
        );
        assert_eq!(message.first_string(1), None);
        assert_eq!(message.first_i64(2), None);
    }

    #[test]
    fn fixed_width_fields_are_skipped() {
        // #1 fixed64, #2 fixed32, #3 varint 7.
        let mut data = vec![0x09];
        data.extend([0; 8]);
        data.push(0x15);
        data.extend([0; 4]);
        data.extend([0x18, 0x07]);
        assert_eq!(ProtoMessage::new(&data).first_i64(3), Some(7));
    }

    #[test]
    fn timestamp_rejects_out_of_range_nanos() {
        // { #1 = 5, #2 = 1_000_000_000 }
        let bad = [0x08, 0x05, 0x10, 0x80, 0x94, 0xeb, 0xdc, 0x03];
        assert_eq!(proto_timestamp(ProtoMessage::new(&bad)), None);
        assert_eq!(proto_timestamp(ProtoMessage::new(&[0x08, 0x05])), Some(5));
    }

    #[test]
    fn file_uris_become_paths() {
        assert_eq!(
            file_uri_to_path("file:///work/app").as_deref(),
            Some("/work/app")
        );
        assert_eq!(
            file_uri_to_path("file://localhost/work/app").as_deref(),
            Some("/work/app")
        );
        assert_eq!(
            file_uri_to_path("file:///work/my%20app/caf%C3%A9").as_deref(),
            Some("/work/my app/café")
        );
        assert_eq!(
            file_uri_to_path("file:///work/100%/a%2").as_deref(),
            Some("/work/100%/a%2")
        );
        assert_eq!(file_uri_to_path("file://remote-host/work/app"), None);
        assert_eq!(file_uri_to_path("file:relative"), None);
    }

    #[test]
    fn unwrap_keeps_only_the_request() {
        assert_eq!(
            unwrap_user_request(
                "<USER_REQUEST>\nFix it\n</USER_REQUEST>\n<ADDITIONAL_METADATA>x</ADDITIONAL_METADATA>"
            ),
            "Fix it"
        );
        assert_eq!(
            unwrap_user_request("Fix it\n<USER_SETTINGS_CHANGE>x</USER_SETTINGS_CHANGE>"),
            "Fix it"
        );
        assert_eq!(unwrap_user_request("  plain prompt  "), "plain prompt");
        assert_eq!(
            unwrap_user_request("<ADDITIONAL_METADATA>x</ADDITIONAL_METADATA>"),
            ""
        );
    }
}

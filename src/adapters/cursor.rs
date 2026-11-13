// Cursor CLI (`agent`, formerly `cursor-agent`) keeps every chat in its own
// SQLite file, `~/.cursor/chats/<workspace>/<chat id>/store.db`, with a `meta`
// and a `blobs` table.
//
// Shapes reported from real stores are handled as well: hex-encoded values,
// `<user_query>` unwrapping and the `<user_info>` workspace fallback.
// No real store was available to check any of this against.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;

use super::{mtime_nanos, nanos_to_secs, parse_iso8601_secs, Adapter};
use crate::config::AgentId;
use crate::model::{join_content, truncate_title, Session, ASSISTANT_PREFIX, USER_PREFIX};

const TITLE_MAX: usize = 80;

/// Numeric timestamps above this are milliseconds, the rest are seconds.
const MILLIS_THRESHOLD: i64 = 100_000_000_000;

const STORE_FILE: &str = "store.db";

/// How many layers of string or hex wrapping a stored value may have around
/// its JSON document.
const MAX_WRAPPING: u8 = 3;

pub struct CursorAdapter {
    chats_dir: PathBuf,
}

impl CursorAdapter {
    pub fn new() -> CursorAdapter {
        CursorAdapter::with_chats_dir(default_chats_dir())
    }

    pub fn with_chats_dir(chats_dir: PathBuf) -> CursorAdapter {
        CursorAdapter { chats_dir }
    }
}

impl Default for CursorAdapter {
    fn default() -> Self {
        Self::new()
    }
}

pub fn default_chats_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".cursor").join("chats")
}

fn wal_path(path: &Path) -> PathBuf {
    let mut wal = path.as_os_str().to_owned();
    wal.push("-wal");
    PathBuf::from(wal)
}

/// Cursor writes through a WAL, so a chat can change while `store.db` keeps
/// its old mtime. The newer of the two files says when the chat last changed.
fn store_mtime(path: &Path) -> Result<i64> {
    let db = mtime_nanos(path)?;
    Ok(mtime_nanos(&wal_path(path)).map_or(db, |wal| wal.max(db)))
}

/// The chat id is the name of the directory that holds `store.db`.
fn session_id_for_store(path: &Path) -> Option<String> {
    path.parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

fn collect_stores(dir: &Path, out: &mut HashMap<String, (PathBuf, i64)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            collect_stores(&path, out);
            continue;
        }
        if entry.file_name() != STORE_FILE || !path.is_file() {
            continue;
        }
        let Some(id) = session_id_for_store(&path) else {
            continue;
        };
        let Ok(mtime) = store_mtime(&path) else {
            continue;
        };
        // The same chat id under two workspaces: keep the newer store, and
        // break ties by path so the choice does not depend on listing order.
        let keep_existing = out
            .get(&id)
            .is_some_and(|(known_path, known)| (*known, &path) > (mtime, known_path));
        if !keep_existing {
            out.insert(id, (path, mtime));
        }
    }
}

fn open_read_only(path: &Path) -> Result<Connection> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;

    // A plain read-only connection to a WAL database creates `-shm` and `-wal`
    // files beside it. Immutable mode reads the main file only and leaves the
    // agent's directory as it was, so use it unless a non-empty WAL holds
    // changes that were never checkpointed.
    let pending_wal = std::fs::metadata(wal_path(path))
        .map(|m| m.len() > 0)
        .unwrap_or(false);

    if !pending_wal {
        let encoded = path
            .to_string_lossy()
            .replace('%', "%25")
            .replace('?', "%3f")
            .replace('#', "%23");
        let uri = format!("file:{encoded}?immutable=1");
        if let Ok(conn) = Connection::open_with_flags(uri, flags | OpenFlags::SQLITE_OPEN_URI) {
            if readable(&conn) {
                return Ok(conn);
            }
        }
    }

    let conn = Connection::open_with_flags(path, flags)
        .with_context(|| format!("opening Cursor store at {}", path.display()))?;
    if !readable(&conn) {
        anyhow::bail!("Cursor store at {} is not readable", path.display());
    }
    Ok(conn)
}

fn readable(conn: &Connection) -> bool {
    conn.query_row("SELECT COUNT(*) FROM sqlite_master", [], |r| {
        r.get::<_, i64>(0)
    })
    .is_ok()
}

/// Calls `on_value` with every non-null value of `table`, oldest row first.
/// The value column is `value` (`key`/`value` layout) or `data` (`id`/`data`
/// layout); a table with neither, or no such table, has no values.
fn for_each_value(
    conn: &Connection,
    table: &str,
    on_value: &mut dyn FnMut(&[u8]),
) -> rusqlite::Result<()> {
    let mut columns: Vec<String> = Vec::new();
    let mut info = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let names = info.query_map([], |row| row.get::<_, String>(1))?;
    for name in names {
        columns.push(name?);
    }
    let Some(column) = ["value", "data"]
        .into_iter()
        .find(|want| columns.iter().any(|have| have.eq_ignore_ascii_case(want)))
    else {
        return Ok(());
    };

    let select = format!("SELECT {column} FROM {table} WHERE {column} IS NOT NULL");
    // Insertion order approximates conversation order. A table declared
    // WITHOUT ROWID has no rowid to sort by.
    let mut statement = match conn.prepare(&format!("{select} ORDER BY rowid")) {
        Ok(statement) => statement,
        Err(_) => conn.prepare(&select)?,
    };
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        match row.get_ref(0)? {
            ValueRef::Text(bytes) | ValueRef::Blob(bytes) => on_value(bytes),
            ValueRef::Integer(n) => on_value(n.to_string().as_bytes()),
            ValueRef::Real(n) => on_value(n.to_string().as_bytes()),
            ValueRef::Null => {}
        }
    }
    Ok(())
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Bytes of a value made only of hex digit pairs, such as `7b22...` for `{"...`.
fn decode_hex(bytes: &[u8]) -> Option<Vec<u8>> {
    let digits = bytes.trim_ascii();
    let pairs = digits.chunks_exact(2);
    if digits.is_empty() || !pairs.remainder().is_empty() {
        return None;
    }
    pairs
        .map(|pair| Some(hex_digit(pair[0])? << 4 | hex_digit(pair[1])?))
        .collect()
}

/// The JSON document in a stored value. Values vary across releases: plain
/// JSON, JSON written as hex digits, either of those inside a JSON string, or
/// JSON with a binary prefix or suffix. Anything else is not JSON.
fn decode_json(bytes: &[u8]) -> Option<Value> {
    decode_json_layer(bytes, 0)
}

fn decode_json_layer(bytes: &[u8], depth: u8) -> Option<Value> {
    if depth > MAX_WRAPPING {
        return None;
    }
    if let Ok(value) = serde_json::from_slice::<Value>(bytes) {
        if let Value::String(inner) = &value {
            if let Some(unwrapped) = decode_json_layer(inner.as_bytes(), depth + 1) {
                return Some(unwrapped);
            }
        }
        return Some(value);
    }
    if let Some(value) = decode_hex(bytes).and_then(|raw| decode_json_layer(&raw, depth + 1)) {
        return Some(value);
    }
    let start = bytes.iter().position(|byte| *byte == b'{')?;
    let end = bytes.iter().rposition(|byte| *byte == b'}')?;
    if start >= end {
        return None;
    }
    serde_json::from_slice(&bytes[start..=end]).ok()
}

struct Message {
    user: bool,
    text: String,
    timestamp: Option<i64>,
}

#[derive(Default)]
struct Chat {
    title: String,
    directory: String,
    created: Option<i64>,
    /// Workspace path found in a prompt's `<user_info>` block.
    workspace_hint: String,
    messages: Vec<Message>,
}

fn first_text<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .filter_map(|key| value.get(key).and_then(Value::as_str))
        .map(str::trim)
        .find(|text| !text.is_empty())
}

/// Unix seconds from a string timestamp or a number in seconds or
/// milliseconds.
fn timestamp_secs(value: Option<&Value>) -> Option<i64> {
    let value = value?;
    if let Some(text) = value.as_str() {
        return parse_iso8601_secs(text.trim());
    }
    let number = value
        .as_i64()
        .or_else(|| value.as_f64().map(|number| number as i64))?;
    if number <= 0 {
        None
    } else if number > MILLIS_THRESHOLD {
        Some(number / 1000)
    } else {
        Some(number)
    }
}

/// The first non-empty value wins for each field.
fn apply_metadata(value: &Value, chat: &mut Chat, with_directory: bool) {
    if chat.title.is_empty() {
        if let Some(title) = first_text(value, &["name", "title"]) {
            chat.title = title.to_string();
        }
    }
    if with_directory && chat.directory.is_empty() {
        if let Some(directory) = first_text(value, &["cwd", "workspace"]) {
            chat.directory = directory.to_string();
        }
    }
    if chat.created.is_none() {
        chat.created = timestamp_secs(value.get("createdAt"));
    }
}

fn message_text(value: &Value) -> String {
    if let Some(text) = value.get("text").and_then(Value::as_str) {
        if !text.trim().is_empty() {
            return text.to_string();
        }
    }
    match value.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| match part {
                Value::String(text) => Some(text.as_str()),
                _ => part
                    .get("text")
                    .or_else(|| part.get("data"))
                    .and_then(Value::as_str),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// The text between each `open` and the `close` that follows it.
fn tagged_sections<'a>(text: &'a str, open: &str, close: &str) -> Vec<&'a str> {
    let mut sections = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find(open) {
        let body = &rest[start + open.len()..];
        let Some(end) = body.find(close) else {
            break;
        };
        sections.push(&body[..end]);
        rest = &body[end + close.len()..];
    }
    sections
}

fn without_sections(text: &str, open: &str, close: &str) -> String {
    let mut kept = String::new();
    let mut rest = text;
    while let Some(start) = rest.find(open) {
        let body = &rest[start + open.len()..];
        let Some(end) = body.find(close) else {
            break;
        };
        kept.push_str(&rest[..start]);
        rest = &body[end + close.len()..];
    }
    kept.push_str(rest);
    kept
}

/// What the user typed. Cursor is reported to store a prompt wrapped in
/// `<user_query>` next to a `<user_info>` block describing the machine; when
/// those tags are present only the prompt is kept.
fn user_prompt_text(text: &str) -> String {
    let queries: Vec<&str> = tagged_sections(text, "<user_query>", "</user_query>")
        .into_iter()
        .map(str::trim)
        .filter(|query| !query.is_empty())
        .collect();
    if !queries.is_empty() {
        return queries.join("\n");
    }
    without_sections(text, "<user_info>", "</user_info>")
        .trim()
        .to_string()
}

/// The `Workspace Path:` line of a `<user_info>` block, when it holds an
/// absolute path.
fn workspace_from_user_info(text: &str) -> Option<String> {
    tagged_sections(text, "<user_info>", "</user_info>")
        .into_iter()
        .flat_map(str::lines)
        .filter_map(|line| line.trim().strip_prefix("Workspace Path:"))
        .map(str::trim)
        .find(|path| Path::new(path).is_absolute())
        .map(str::to_string)
}

fn collect_records(value: &Value, chat: &mut Chat) {
    if let Some(items) = value.as_array() {
        for item in items {
            collect_records(item, chat);
        }
        return;
    }

    let role = value.get("role").and_then(Value::as_str);
    // On a message, `name` is the speaker or the tool, not the chat's title.
    if role.is_none() {
        apply_metadata(value, chat, false);
    }
    for key in ["messages", "conversation"] {
        if let Some(items) = value.get(key).and_then(Value::as_array) {
            for item in items {
                collect_records(item, chat);
            }
        }
    }

    let user = match (role, value.get("type")) {
        (Some("user"), _) => true,
        (Some("assistant"), _) => false,
        (_, Some(Value::Number(kind))) if kind.as_i64() == Some(1) => true,
        (_, Some(Value::Number(kind))) if kind.as_i64() == Some(2) => false,
        (_, Some(Value::String(kind))) if kind == "user" => true,
        (_, Some(Value::String(kind))) if kind == "ai" || kind == "assistant" => false,
        _ => return,
    };

    let raw = message_text(value);
    let text = if user {
        if chat.workspace_hint.is_empty() {
            if let Some(workspace) = workspace_from_user_info(&raw) {
                chat.workspace_hint = workspace;
            }
        }
        user_prompt_text(&raw)
    } else {
        raw.trim().to_string()
    };
    if text.is_empty() {
        return;
    }
    chat.messages.push(Message {
        user,
        text,
        timestamp: timestamp_secs(value.get("timestamp")),
    });
}

/// Older stores hold a prompt as plain text followed by `$` and an id, for
/// example `hello there$027f8b2f-d09c-4a69-98b0-b53f0118605d`.
fn plain_user_message(bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(bytes).ok()?;
    let (content, id) = text.trim_matches('\0').trim().rsplit_once('$')?;
    let id = id.trim_matches(|c: char| c.is_control() || c.is_whitespace());
    let content = content.trim_matches(|c: char| c.is_control()).trim();
    if content.is_empty() || id.len() < 8 || !id.contains('-') {
        return None;
    }
    Some(content.to_string())
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let pair = bytes
            .get(i + 1..i + 3)
            .filter(|_| bytes[i] == b'%')
            .and_then(|pair| Some(hex_digit(pair[0])? << 4 | hex_digit(pair[1])?));
        match pair {
            Some(byte) => {
                out.push(byte);
                i += 3;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8(out).unwrap_or_else(|_| value.to_string())
}

/// The workspace directory, when its name is the percent-encoded path. A
/// hashed name decodes to something that is not an absolute path.
fn directory_from_store_path(path: &Path) -> Option<String> {
    let encoded = path
        .parent()
        .and_then(Path::parent)
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())?;
    let decoded = percent_decode(encoded);
    Path::new(&decoded).is_absolute().then_some(decoded)
}

/// Reads one chat. `Ok(None)` means the store holds no user prompt; an error
/// means it could not be read (not a database, locked, half-written).
pub fn parse_store(path: &Path) -> Result<Option<Session>> {
    let Some(id) = session_id_for_store(path) else {
        return Ok(None);
    };
    let mtime = store_mtime(path)
        .with_context(|| format!("reading mtime of Cursor store at {}", path.display()))?;
    let conn = open_read_only(path)?;

    let mut chat = Chat::default();
    for_each_value(&conn, "meta", &mut |bytes| {
        if let Some(value) = decode_json(bytes) {
            apply_metadata(&value, &mut chat, true);
        }
    })?;
    for_each_value(&conn, "blobs", &mut |bytes| {
        if let Some(value) = decode_json(bytes) {
            collect_records(&value, &mut chat);
        } else if let Some(text) = plain_user_message(bytes) {
            chat.messages.push(Message {
                user: true,
                text,
                timestamp: None,
            });
        }
        // Anything else is a binary record this adapter cannot read.
    })?;

    let Some(first_prompt) = chat.messages.iter().find(|m| m.user).map(|m| &m.text) else {
        return Ok(None);
    };
    let title_source = if chat.title.is_empty() {
        first_prompt
    } else {
        &chat.title
    };
    let title = truncate_title(title_source, TITLE_MAX, false);

    let directory = if !chat.directory.is_empty() {
        chat.directory
    } else if let Some(decoded) = directory_from_store_path(path) {
        decoded
    } else {
        chat.workspace_hint
    };

    let timestamp = chat
        .messages
        .iter()
        .filter_map(|m| m.timestamp)
        .max()
        .or(chat.created)
        .unwrap_or_else(|| nanos_to_secs(mtime));

    let lines: Vec<String> = chat
        .messages
        .iter()
        .map(|m| {
            let prefix = if m.user {
                USER_PREFIX
            } else {
                ASSISTANT_PREFIX
            };
            format!("{prefix}{}", m.text)
        })
        .collect();

    Ok(Some(Session {
        id,
        agent: AgentId::Cursor,
        title,
        directory,
        timestamp,
        content: join_content(&lines),
        message_count: lines.len() as u32,
        mtime,
        yolo: false,
        source_path: Some(path.to_string_lossy().to_string()),
    }))
}

impl Adapter for CursorAdapter {
    fn id(&self) -> AgentId {
        AgentId::Cursor
    }

    fn supports_yolo(&self) -> bool {
        true
    }

    fn is_available(&self) -> bool {
        self.chats_dir.is_dir()
    }

    fn scan(&self) -> Result<HashMap<String, (PathBuf, i64)>> {
        let mut out = HashMap::new();
        collect_stores(&self.chats_dir, &mut out);
        Ok(out)
    }

    fn parse(&self, path: &Path) -> Result<Option<Session>> {
        parse_store(path)
    }

    fn resume_command(&self, session: &Session, yolo: bool) -> Vec<String> {
        let mut command = vec!["agent".to_string()];
        if yolo {
            command.push("--yolo".to_string());
        }
        command.push("--resume".to_string());
        command.push(session.id.clone());
        command
    }
}

// The Pi coding agent's sessions: store discovery (the env and settings.json
// overrides, and the fixed scan depth that leaves out nested subagent
// transcripts), the row handling and the lenient row decoders.

use std::borrow::Cow;
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::fmt;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::de::{self, Deserialize, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde_json::Value;

use super::{mtime_nanos, nanos_to_secs, parse_iso8601_secs, Adapter};
use crate::config::AgentId;
use crate::model::{join_content, truncate_title, Session, ASSISTANT_PREFIX, USER_PREFIX};

const TITLE_MAX: usize = 80;

/// Title of a session that has indexed text but no user prompt and no name.
const UNTITLED: &str = "(no messages)";

/// Read buffer for transcripts. Pi rows can be megabytes long (tool output,
/// compaction summaries), so a larger buffer means fewer `read` syscalls.
const READ_BUFFER_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone)]
pub struct PiAdapter {
    sessions_dir: PathBuf,
    /// Directory depth of transcripts: 2 for the default store, grouped by
    /// working directory, and 1 for a custom store. Pi lists only that level,
    /// so subagent sessions that extensions nest deeper stay out.
    depth: usize,
}

impl PiAdapter {
    /// The store Pi itself would use: `PI_CODING_AGENT_SESSION_DIR`, else
    /// `sessionDir` in the agent dir's `settings.json`, else
    /// `<agent dir>/sessions`. The agent dir is `PI_CODING_AGENT_DIR` or
    /// `~/.pi/agent`.
    pub fn new() -> PiAdapter {
        let home = home_dir();
        let agent_dir = std::env::var("PI_CODING_AGENT_DIR")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(|value| expand_tilde(&value, &home))
            .unwrap_or_else(|| home.join(".pi").join("agent"));
        let session_dir_env = std::env::var("PI_CODING_AGENT_SESSION_DIR").ok();
        let (sessions_dir, custom) =
            resolve_sessions_dir(session_dir_env.as_deref(), &agent_dir, &home);
        if custom {
            PiAdapter::with_custom_dir(sessions_dir)
        } else {
            PiAdapter::with_sessions_dir(sessions_dir)
        }
    }

    /// The default store layout: `<sessions_dir>/<workspace>/<file>.jsonl`.
    pub fn with_sessions_dir(sessions_dir: PathBuf) -> PiAdapter {
        PiAdapter {
            sessions_dir,
            depth: 2,
        }
    }

    /// A custom store, with transcripts directly in `sessions_dir`.
    pub fn with_custom_dir(sessions_dir: PathBuf) -> PiAdapter {
        PiAdapter {
            sessions_dir,
            depth: 1,
        }
    }
}

impl Default for PiAdapter {
    fn default() -> Self {
        Self::new()
    }
}

/// Pi's session store, and whether it is a custom (flat) one.
/// `session_dir_env` is the value of `PI_CODING_AGENT_SESSION_DIR`, if set.
pub fn resolve_sessions_dir(
    session_dir_env: Option<&str>,
    agent_dir: &Path,
    home: &Path,
) -> (PathBuf, bool) {
    if let Some(dir) = session_dir_env.filter(|value| !value.trim().is_empty()) {
        return (expand_tilde(dir, home), true);
    }

    let settings = std::fs::read(agent_dir.join("settings.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
    if let Some(dir) = settings
        .as_ref()
        .and_then(|settings| settings.get("sessionDir"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    {
        return (expand_tilde(dir, home), true);
    }

    (agent_dir.join("sessions"), false)
}

fn home_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into()))
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

/// `.jsonl` files exactly `depth` levels below `dir`.
fn collect_jsonl_at_depth(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if depth > 1 {
            if path.is_dir() {
                collect_jsonl_at_depth(&path, depth - 1, out);
            }
        } else if path.extension().and_then(|x| x.to_str()) == Some("jsonl") && path.is_file() {
            out.push(path);
        }
    }
}

/// Pi names transcripts `<timestamp>_<sessionId>.jsonl`.
fn id_from_stem(path: &Path) -> String {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    match stem.rsplit_once('_') {
        Some((_, id)) => id.to_string(),
        None => stem.to_string(),
    }
}

/// The `id` of the `session` header, which must be the first row that is
/// valid JSON. Only the head of the file is read.
fn header_id(path: &Path) -> Option<String> {
    let mut reader = BufReader::new(File::open(path).ok()?);
    let mut line = Vec::new();
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line).ok()? == 0 {
            return None;
        }
        if line.iter().all(|b| b.is_ascii_whitespace()) {
            continue;
        }
        let Ok(row) = serde_json::from_slice::<PiRow>(&line) else {
            continue;
        };
        if row.kind.as_deref() != Some("session") {
            return None;
        }
        return row.id.filter(|id| !id.is_empty());
    }
}

/// The session id, used both as the scan key and as `Session::id`: the header
/// id, which is what `pi --session` matches, else the file name's suffix.
///
/// The scan key and the parsed id must be equal, so the header is always
/// read, even when the file name already looks like an id.
fn session_id_for_file(path: &Path) -> String {
    header_id(path).unwrap_or_else(|| id_from_stem(path))
}

/// Session state accumulated row by row while reading a transcript.
///
/// Entries form a tree through `id` / `parentId`. The tree is ignored and
/// every row is indexed in file order, so text on abandoned branches stays
/// searchable.
#[derive(Default)]
struct PiTranscript {
    directory: String,
    session_name: Option<String>,
    first_user_message: String,
    messages: Vec<String>,
    message_count: u32,
    header_secs: Option<i64>,
    last_activity_ms: Option<i64>,
}

impl PiTranscript {
    fn add_row(&mut self, row: PiRow) {
        match row.kind.as_deref().unwrap_or_default() {
            "session" => {
                if self.directory.is_empty() {
                    self.directory = row.cwd.unwrap_or_default();
                }
                if self.header_secs.is_none() {
                    self.header_secs = row.timestamp.as_deref().and_then(parse_iso8601_secs);
                }
            }
            "session_info" => {
                // The last row wins, and a blank name clears an earlier one.
                let name = row.name.unwrap_or_default();
                let name = name.trim();
                self.session_name = (!name.is_empty()).then(|| name.to_string());
            }
            "message" => {
                let Some(message) = row.message else {
                    return;
                };
                let role = message.role.as_deref().unwrap_or_default();
                let is_user = role == "user";
                let is_assistant = role == "assistant";
                let is_visible_custom = is_custom_role(role) && message.display == Some(true);
                if !is_user && !is_assistant && !is_visible_custom {
                    return;
                }
                if is_user || is_assistant {
                    let timestamp_ms = message.timestamp_ms.or_else(|| {
                        row.timestamp
                            .as_deref()
                            .and_then(parse_iso8601_secs)
                            .map(|secs| secs.saturating_mul(1000))
                    });
                    if let Some(ms) = timestamp_ms {
                        if self.last_activity_ms.is_none_or(|current| ms > current) {
                            self.last_activity_ms = Some(ms);
                        }
                    }
                    if !message.content.0.is_empty() {
                        self.message_count += 1;
                    }
                }

                let prefix = if is_user {
                    USER_PREFIX
                } else {
                    ASSISTANT_PREFIX
                };
                for text in message.content.0 {
                    if is_user && self.first_user_message.is_empty() {
                        self.first_user_message = text.clone();
                    }
                    self.messages.push(format!("{prefix}{text}"));
                }
            }
            "custom_message" if row.display == Some(true) => {
                for text in row.content.0 {
                    self.messages.push(format!("{ASSISTANT_PREFIX}{text}"));
                }
            }
            "compaction" | "branch_summary" => {
                let summary = row.summary.unwrap_or_default();
                if !summary.trim().is_empty() {
                    self.messages.push(format!("{ASSISTANT_PREFIX}{summary}"));
                }
            }
            _ => {}
        }
    }

    fn into_session(self, id: String, path: &Path, mtime_ns: i64) -> Option<Session> {
        if self.messages.is_empty() {
            return None;
        }
        let title_source = match &self.session_name {
            Some(name) => name.as_str(),
            None if self.first_user_message.is_empty() => UNTITLED,
            None => self.first_user_message.as_str(),
        };
        Some(Session {
            id,
            agent: AgentId::Pi,
            title: truncate_title(title_source, TITLE_MAX, false),
            directory: self.directory,
            timestamp: self
                .last_activity_ms
                .map(|ms| ms.div_euclid(1000))
                .or(self.header_secs)
                .unwrap_or_else(|| nanos_to_secs(mtime_ns)),
            content: join_content(&self.messages),
            message_count: self.message_count,
            mtime: mtime_ns,
            yolo: false,
            source_path: Some(path.to_string_lossy().to_string()),
        })
    }
}

/// Message roles that extensions use for messages they may show in the
/// transcript. Only rows with `display: true` are indexed.
fn is_custom_role(role: &str) -> bool {
    matches!(role, "custom" | "hookMessage")
}

/// Whether a message role's content is ever indexed. Content for any other
/// role (tool results, bash output, ...) is skipped without being decoded.
fn role_content_is_indexed(role: &str) -> bool {
    matches!(role, "user" | "assistant") || is_custom_role(role)
}

impl Adapter for PiAdapter {
    fn id(&self) -> AgentId {
        AgentId::Pi
    }

    fn is_available(&self) -> bool {
        self.sessions_dir.is_dir()
    }

    fn scan(&self) -> Result<HashMap<String, (PathBuf, i64)>> {
        let mut files = Vec::new();
        collect_jsonl_at_depth(&self.sessions_dir, self.depth, &mut files);
        let mut out: HashMap<String, (PathBuf, i64)> = HashMap::new();
        for path in files {
            let Ok(mtime) = mtime_nanos(&path) else {
                continue;
            };
            let id = session_id_for_file(&path);
            if id.is_empty() {
                continue;
            }
            match out.entry(id) {
                Entry::Vacant(slot) => {
                    slot.insert((path, mtime));
                }
                // Two files with one header id (a copied transcript): keep
                // the newer one, whatever order the directory lists them in.
                Entry::Occupied(mut slot) => {
                    let (kept_path, kept_mtime) = slot.get();
                    if (mtime, &path) > (*kept_mtime, kept_path) {
                        slot.insert((path, mtime));
                    }
                }
            }
        }
        Ok(out)
    }

    /// Each row is decoded into [`PiRow`], which keeps only what the index
    /// needs. Tool results, tool-call arguments, thinking blocks and
    /// extension state make up most of a Pi transcript's bytes; they are
    /// syntax-checked but never materialized. Rows that are not valid JSON,
    /// such as a last row Pi is still writing, are skipped.
    fn parse(&self, path: &Path) -> Result<Option<Session>> {
        let Ok(file) = File::open(path) else {
            return Ok(None);
        };
        let mtime = mtime_nanos(path).unwrap_or(0);
        let mut reader = BufReader::with_capacity(READ_BUFFER_BYTES, file);
        let mut line = Vec::new();
        let mut transcript = PiTranscript::default();

        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            if line.iter().all(|b| b.is_ascii_whitespace()) {
                continue;
            }
            // `PiRow` accepts any JSON shape, so an error here means the row
            // is not valid JSON.
            if let Ok(row) = serde_json::from_slice::<PiRow>(&line) {
                transcript.add_row(row);
            }
        }

        let id = session_id_for_file(path);
        if id.is_empty() {
            return Ok(None);
        }
        Ok(transcript.into_session(id, path, mtime))
    }

    fn resume_command(&self, session: &Session, _yolo: bool) -> Vec<String> {
        // `pi --session <path|id>` takes the session id directly and looks it
        // up in the same store this adapter indexes, so no path is needed.
        // Pi has no permission prompts, hence no yolo variant.
        vec![
            "pi".to_string(),
            "--session".to_string(),
            session.id.clone(),
        ]
    }
}

// ---------------------------------------------------------------------------
// Row decoding
//
// Decoding every row into `serde_json::Value` would spend most of a refresh
// allocating tool output that the index then throws away. The types below
// decode only the fields the adapter reads and let serde skip everything else.
//
// Every decoder is lenient: a field with an unexpected JSON type decodes as
// absent rather than failing the row, and a row that is not an object decodes
// as an empty row. A row fails to decode only when it is not valid JSON.
// ---------------------------------------------------------------------------

/// Implements `Visitor` methods for JSON types a decoder does not use. The
/// value is consumed (containers are drained with `IgnoredAny`) and the
/// decoder returns its default.
macro_rules! ignore_json_types {
    ($de:lifetime; $($kind:ident),+ $(,)?) => {
        $(ignore_json_types!(@one $de $kind);)+
    };
    (@one $de:lifetime bool) => {
        fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
            Ok(Default::default())
        }
    };
    (@one $de:lifetime i64) => {
        fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
            Ok(Default::default())
        }
    };
    (@one $de:lifetime u64) => {
        fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
            Ok(Default::default())
        }
    };
    (@one $de:lifetime f64) => {
        fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
            Ok(Default::default())
        }
    };
    (@one $de:lifetime str) => {
        fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
            Ok(Default::default())
        }
    };
    (@one $de:lifetime unit) => {
        fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(Default::default())
        }
    };
    (@one $de:lifetime seq) => {
        fn visit_seq<A: SeqAccess<$de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            while seq.next_element::<IgnoredAny>()?.is_some() {}
            Ok(Default::default())
        }
    };
    (@one $de:lifetime map) => {
        fn visit_map<A: MapAccess<$de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
            while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
            Ok(Default::default())
        }
    };
}

/// Implements `Deserialize` for a type through a unit visitor struct, always
/// using `deserialize_any` so the visitor sees the actual JSON type.
macro_rules! deserialize_any_with {
    ($target:ty, $visitor:ident) => {
        impl<'de> Deserialize<'de> for $target {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                deserializer.deserialize_any($visitor)
            }
        }
    };
}

/// Object key, borrowed from the input when it has no escapes.
struct Key<'de>(Cow<'de, str>);

struct KeyVisitor;

impl<'de> Visitor<'de> for KeyVisitor {
    type Value = Key<'de>;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("an object key")
    }

    fn visit_borrowed_str<E: de::Error>(self, value: &'de str) -> Result<Self::Value, E> {
        Ok(Key(Cow::Borrowed(value)))
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
        Ok(Key(Cow::Owned(value.to_owned())))
    }
}

impl<'de> Deserialize<'de> for Key<'de> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_str(KeyVisitor)
    }
}

/// A JSON string, or `None` for any other JSON type.
#[derive(Default)]
struct LenientString(Option<String>);

struct LenientStringVisitor;

impl<'de> Visitor<'de> for LenientStringVisitor {
    type Value = LenientString;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("any JSON value")
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
        Ok(LenientString(Some(value.to_owned())))
    }

    fn visit_string<E: de::Error>(self, value: String) -> Result<Self::Value, E> {
        Ok(LenientString(Some(value)))
    }

    ignore_json_types!('de; bool, i64, u64, f64, unit, seq, map);
}

deserialize_any_with!(LenientString, LenientStringVisitor);

/// A JSON boolean, or `None` for any other JSON type.
#[derive(Default)]
struct LenientBool(Option<bool>);

struct LenientBoolVisitor;

impl<'de> Visitor<'de> for LenientBoolVisitor {
    type Value = LenientBool;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("any JSON value")
    }

    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Self::Value, E> {
        Ok(LenientBool(Some(value)))
    }

    ignore_json_types!('de; i64, u64, f64, str, unit, seq, map);
}

deserialize_any_with!(LenientBool, LenientBoolVisitor);

/// A millisecond Unix timestamp from a JSON number, or `None` for any other
/// JSON type. Fractional values are truncated.
#[derive(Default)]
struct LenientMillis(Option<i64>);

struct LenientMillisVisitor;

impl<'de> Visitor<'de> for LenientMillisVisitor {
    type Value = LenientMillis;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("any JSON value")
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
        Ok(LenientMillis(Some(value)))
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
        Ok(LenientMillis(Some(
            i64::try_from(value).unwrap_or(i64::MAX),
        )))
    }

    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Self::Value, E> {
        Ok(LenientMillis(Some(value as i64)))
    }

    ignore_json_types!('de; bool, str, unit, seq, map);
}

deserialize_any_with!(LenientMillis, LenientMillisVisitor);

/// Non-empty text from Pi message content: either a plain string or the
/// `text` of every `{"type": "text"}` part. Other parts (tool calls, thinking,
/// images) are skipped.
#[derive(Default)]
struct PiContent(Vec<String>);

struct PiContentVisitor;

impl<'de> Visitor<'de> for PiContentVisitor {
    type Value = PiContent;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("any JSON value")
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
        self.visit_string(value.to_owned())
    }

    fn visit_string<E: de::Error>(self, value: String) -> Result<Self::Value, E> {
        Ok(PiContent(
            (!value.is_empty()).then_some(value).into_iter().collect(),
        ))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut texts = Vec::new();
        while let Some(part) = seq.next_element::<PiContentPart>()? {
            if part.kind.as_deref() != Some("text") {
                continue;
            }
            if let Some(text) = part.text.filter(|text| !text.is_empty()) {
                texts.push(text);
            }
        }
        Ok(PiContent(texts))
    }

    ignore_json_types!('de; bool, i64, u64, f64, unit, map);
}

deserialize_any_with!(PiContent, PiContentVisitor);

/// One element of an array-shaped content field.
#[derive(Default)]
struct PiContentPart {
    kind: Option<String>,
    text: Option<String>,
}

struct PiContentPartVisitor;

impl<'de> Visitor<'de> for PiContentPartVisitor {
    type Value = PiContentPart;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("any JSON value")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut part = PiContentPart::default();
        while let Some(key) = map.next_key::<Key>()? {
            match key.0.as_ref() {
                "type" => part.kind = map.next_value::<LenientString>()?.0,
                "text" => part.text = map.next_value::<LenientString>()?.0,
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(part)
    }

    ignore_json_types!('de; bool, i64, u64, f64, str, unit, seq);
}

deserialize_any_with!(PiContentPart, PiContentPartVisitor);

/// The nested `message` object of a `{"type": "message"}` row.
#[derive(Default)]
struct PiMessage {
    role: Option<String>,
    content: PiContent,
    timestamp_ms: Option<i64>,
    display: Option<bool>,
}

struct PiMessageVisitor;

impl<'de> Visitor<'de> for PiMessageVisitor {
    type Value = PiMessage;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("any JSON value")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut message = PiMessage::default();
        while let Some(key) = map.next_key::<Key>()? {
            match key.0.as_ref() {
                "role" => message.role = map.next_value::<LenientString>()?.0,
                // Pi writes `role` before `content`, so tool results and other
                // unindexed roles skip their (often huge) content here. If
                // `content` ever comes first it is decoded and ignored later.
                "content" if message.role.as_deref().is_none_or(role_content_is_indexed) => {
                    message.content = map.next_value()?;
                }
                "timestamp" => message.timestamp_ms = map.next_value::<LenientMillis>()?.0,
                "display" => message.display = map.next_value::<LenientBool>()?.0,
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(message)
    }

    ignore_json_types!('de; bool, i64, u64, f64, str, unit, seq);
}

deserialize_any_with!(PiMessage, PiMessageVisitor);

/// One transcript row, reduced to the fields the adapter reads.
#[derive(Default)]
struct PiRow {
    kind: Option<String>,
    id: Option<String>,
    cwd: Option<String>,
    timestamp: Option<String>,
    name: Option<String>,
    summary: Option<String>,
    display: Option<bool>,
    content: PiContent,
    message: Option<PiMessage>,
}

struct PiRowVisitor;

impl<'de> Visitor<'de> for PiRowVisitor {
    type Value = PiRow;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("any JSON value")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut row = PiRow::default();
        while let Some(key) = map.next_key::<Key>()? {
            // `type` is the first key Pi writes, so row kinds that never use
            // `content` or `message` skip those fields without decoding them.
            let kind = row.kind.as_deref();
            match key.0.as_ref() {
                "type" => row.kind = map.next_value::<LenientString>()?.0,
                "id" => row.id = map.next_value::<LenientString>()?.0,
                "cwd" => row.cwd = map.next_value::<LenientString>()?.0,
                "timestamp" => row.timestamp = map.next_value::<LenientString>()?.0,
                "name" => row.name = map.next_value::<LenientString>()?.0,
                "summary" => row.summary = map.next_value::<LenientString>()?.0,
                "display" => row.display = map.next_value::<LenientBool>()?.0,
                "content" if kind.is_none_or(|kind| kind == "custom_message") => {
                    row.content = map.next_value()?;
                }
                "message" if kind.is_none_or(|kind| kind == "message") => {
                    row.message = Some(map.next_value()?);
                }
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(row)
    }

    ignore_json_types!('de; bool, i64, u64, f64, str, unit, seq);
}

deserialize_any_with!(PiRow, PiRowVisitor);

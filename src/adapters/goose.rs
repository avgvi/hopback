//! Goose (Block) keeps every session in one SQLite database,
//! `~/.local/share/goose/sessions/sessions.db`, with a `sessions` table and a
//! `messages` table whose `content_json` column holds the parts of a message.
//! Written against the schema Goose 1.50 creates (schema version 16); columns
//! that older versions lack are optional here.

use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use anyhow::Result;
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;

use super::{mtime_nanos, parse_iso8601_secs, Adapter, IncrementalResult, KnownSessions};
use crate::config::AgentId;
use crate::model::{
    join_content, truncate_title, Session, SessionKey, ASSISTANT_PREFIX, USER_PREFIX,
};

const TITLE_MAX: usize = 100;

const MILLIS_THRESHOLD: i64 = 100_000_000_000;

/// Where the Goose desktop app keeps its command-line program on macOS. The
/// app does not put it on `PATH`.
const MACOS_BUNDLED_CLI: &str = "/Applications/Goose.app/Contents/Resources/bin/goose";

pub struct GooseAdapter {
    db_path: PathBuf,
}

impl GooseAdapter {
    pub fn new() -> GooseAdapter {
        GooseAdapter {
            db_path: default_db_path(),
        }
    }

    pub fn with_db_path(db_path: PathBuf) -> GooseAdapter {
        GooseAdapter { db_path }
    }
}

impl Default for GooseAdapter {
    fn default() -> Self {
        Self::new()
    }
}

/// Goose follows the XDG layout on macOS as well as Linux.
pub fn db_path_from(xdg_data_home: Option<&str>, home: &str) -> PathBuf {
    let data = match xdg_data_home {
        Some(v) if !v.is_empty() && Path::new(v).is_absolute() => PathBuf::from(v),
        _ => PathBuf::from(home).join(".local").join("share"),
    };
    data.join("goose").join("sessions").join("sessions.db")
}

pub fn default_db_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    db_path_from(std::env::var("XDG_DATA_HOME").ok().as_deref(), &home)
}

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = OsString::from(path.as_os_str());
    name.push(suffix);
    PathBuf::from(name)
}

/// When the database last changed: the newer of the database file and its
/// `-wal` file. Goose runs SQLite in WAL mode.
fn db_change_mtime(db_path: &Path) -> Result<i64> {
    let db = mtime_nanos(db_path)?;
    let wal = mtime_nanos(&sidecar(db_path, "-wal")).unwrap_or(0);
    Ok(db.max(wal))
}

/// Opens the database without writing next to it. A plain read-only open of a
/// WAL database creates `-shm` and `-wal` files when they are missing;
/// immutable mode reads the main file only, so use it unless a non-empty WAL
/// holds messages that were never checkpointed.
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

fn columns(conn: &Connection, table: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    let Ok(mut stmt) = conn.prepare(&format!("PRAGMA table_info({table})")) else {
        return out;
    };
    if let Ok(rows) = stmt.query_map([], |row| row.get::<_, String>(1)) {
        out.extend(rows.flatten());
    }
    out
}

/// Goose writes `created_at` and `updated_at` as `YYYY-MM-DD HH:MM:SS` in UTC.
/// Accept a number of seconds or milliseconds too.
fn timestamp_secs(value: ValueRef<'_>) -> Option<i64> {
    match value {
        ValueRef::Integer(n) if n > MILLIS_THRESHOLD => Some(n / 1000),
        ValueRef::Integer(n) if n > 0 => Some(n),
        ValueRef::Real(f) if f > 0.0 => Some(f as i64),
        ValueRef::Text(bytes) => {
            let text = std::str::from_utf8(bytes).ok()?.trim();
            parse_iso8601_secs(text).or_else(|| text.parse::<i64>().ok().filter(|n| *n > 0))
        }
        _ => None,
    }
}

fn text_of(value: ValueRef<'_>) -> String {
    match value {
        ValueRef::Text(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        _ => String::new(),
    }
}

/// Subagent and hidden sessions are Goose's own bookkeeping. The check ignores
/// case and separators so `sub_agent`, `subagent` and `SubAgent` all match.
fn is_internal_type(session_type: &str) -> bool {
    let letters: String = session_type
        .chars()
        .filter(|c| c.is_ascii_alphabetic())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    letters == "subagent" || letters == "hidden"
}

/// Goose adds messages the person never sees, such as the turn context it
/// sends with each prompt, and marks them `"userVisible": false`.
fn is_user_visible(metadata_json: &str) -> bool {
    serde_json::from_str::<Value>(metadata_json)
        .ok()
        .and_then(|m| m.get("userVisible").and_then(|v| v.as_bool()))
        .unwrap_or(true)
}

/// The readable text of a message: its text parts, and a short marker for
/// each tool call. Tool results, thinking and images are left out.
fn content_text(content_json: &str) -> String {
    let Ok(parts) = serde_json::from_str::<Value>(content_json) else {
        return String::new();
    };
    let Some(parts) = parts.as_array() else {
        return String::new();
    };
    let mut out: Vec<String> = Vec::new();
    for part in parts {
        match part.get("type").and_then(|v| v.as_str()).unwrap_or("") {
            "text" => {
                if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                    let text = text.trim();
                    if !text.is_empty() {
                        out.push(text.to_string());
                    }
                }
            }
            "toolRequest" => {
                let name = part
                    .get("toolCall")
                    .and_then(|call| call.get("value"))
                    .and_then(|value| value.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("tool");
                out.push(format!("[calling {name}]"));
            }
            _ => {}
        }
    }
    out.join(" ")
}

struct Row {
    id: String,
    name: String,
    description: String,
    directory: String,
    timestamp: i64,
}

/// `None` means the database could not be read at all, which is different
/// from a readable database that holds no sessions.
fn read_db_sessions(db_path: &Path, db_mtime: i64) -> Option<Vec<Session>> {
    let conn = open_read_only(db_path).ok()?;
    let mut sessions = read_from_conn(&conn, db_mtime)?;
    for session in &mut sessions {
        session.source_path = Some(db_path.to_string_lossy().to_string());
    }
    Some(sessions)
}

pub fn load_db_sessions(db_path: &Path, db_mtime: i64) -> Vec<Session> {
    read_db_sessions(db_path, db_mtime).unwrap_or_default()
}

fn read_from_conn(conn: &Connection, db_mtime: i64) -> Option<Vec<Session>> {
    let session_cols = columns(conn, "sessions");
    if !session_cols.contains("id") || !session_cols.contains("working_dir") {
        return None;
    }
    let optional = |name: &str, fallback: &str| {
        if session_cols.contains(name) {
            name.to_string()
        } else {
            fallback.to_string()
        }
    };
    let sql = format!(
        "SELECT id, {}, {}, working_dir, {}, {}, {}, {} FROM sessions",
        optional("name", "''"),
        optional("description", "''"),
        optional("updated_at", "NULL"),
        optional("created_at", "NULL"),
        optional("session_type", "'user'"),
        optional("archived_at", "NULL"),
    );

    let mut rows: Vec<Row> = Vec::new();
    {
        let mut stmt = conn.prepare(&sql).ok()?;
        let mut query = stmt.query([]).ok()?;
        while let Ok(Some(row)) = query.next() {
            let id = text_of(row.get_ref(0).ok()?);
            if id.is_empty() {
                continue;
            }
            if is_internal_type(&text_of(row.get_ref(6).ok()?)) {
                continue;
            }
            // An archived session is one the person put away in Goose.
            if !matches!(row.get_ref(7).ok()?, ValueRef::Null) {
                continue;
            }
            let timestamp = timestamp_secs(row.get_ref(4).ok()?)
                .or_else(|| timestamp_secs(row.get_ref(5).ok()?))
                .unwrap_or(db_mtime / 1_000_000_000);
            rows.push(Row {
                id,
                name: text_of(row.get_ref(1).ok()?),
                description: text_of(row.get_ref(2).ok()?),
                directory: text_of(row.get_ref(3).ok()?),
                timestamp,
            });
        }
    }

    let message_cols = columns(conn, "messages");
    let metadata = if message_cols.contains("metadata_json") {
        "metadata_json"
    } else {
        "NULL"
    };
    let order = if message_cols.contains("created_timestamp") {
        "created_timestamp, id"
    } else {
        "id"
    };
    let message_sql = format!(
        "SELECT role, content_json, {metadata} FROM messages WHERE session_id = ?1 ORDER BY {order}"
    );
    let mut message_stmt = conn.prepare(&message_sql).ok();

    let mut sessions = Vec::new();
    for row in rows {
        let mut messages: Vec<String> = Vec::new();
        let mut first_user = String::new();

        if let Some(stmt) = message_stmt.as_mut() {
            if let Ok(mut query) = stmt.query([&row.id]) {
                while let Ok(Some(m)) = query.next() {
                    let role = m.get_ref(0).map(text_of).unwrap_or_default();
                    let prefix = match role.as_str() {
                        "user" => USER_PREFIX,
                        "assistant" => ASSISTANT_PREFIX,
                        _ => continue,
                    };
                    let metadata = m.get_ref(2).map(text_of).unwrap_or_default();
                    if !metadata.is_empty() && !is_user_visible(&metadata) {
                        continue;
                    }
                    let text = content_text(&m.get_ref(1).map(text_of).unwrap_or_default());
                    if text.is_empty() {
                        continue;
                    }
                    if role == "user" && first_user.is_empty() {
                        first_user = text.clone();
                    }
                    messages.push(format!("{prefix}{text}"));
                }
            }
        }

        // No prompt the person typed: nothing to resume.
        if first_user.is_empty() {
            continue;
        }

        let title_source = [row.name.trim(), row.description.trim()]
            .into_iter()
            .find(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or(first_user);

        sessions.push(Session {
            id: row.id,
            agent: AgentId::Goose,
            title: truncate_title(&title_source, TITLE_MAX, false),
            directory: row.directory,
            timestamp: row.timestamp,
            content: join_content(&messages),
            message_count: messages.len() as u32,
            mtime: db_mtime,
            yolo: false,
            source_path: None,
        });
    }

    Some(sessions)
}

/// `goose` when it is on `PATH`; otherwise the copy inside the desktop app,
/// when that exists; otherwise `goose`, so the error names the command.
fn program_from(path_var: Option<&OsStr>, bundled: &Path) -> String {
    let on_path = path_var
        .map(|paths| std::env::split_paths(paths).any(|dir| dir.join("goose").is_file()))
        .unwrap_or(false);
    if !on_path && bundled.is_file() {
        return bundled.to_string_lossy().to_string();
    }
    "goose".to_string()
}

fn program() -> String {
    program_from(
        std::env::var_os("PATH").as_deref(),
        Path::new(MACOS_BUNDLED_CLI),
    )
}

fn resume_args(session: &Session) -> Vec<String> {
    vec![
        "session".to_string(),
        "--resume".to_string(),
        "--session-id".to_string(),
        session.id.clone(),
    ]
}

impl Adapter for GooseAdapter {
    fn id(&self) -> AgentId {
        AgentId::Goose
    }

    fn mtime_is_per_session(&self) -> bool {
        false
    }

    fn supports_fork(&self) -> bool {
        true
    }

    fn is_available(&self) -> bool {
        self.db_path.is_file()
    }

    fn scan(&self) -> Result<HashMap<String, (PathBuf, i64)>> {
        let mut out = HashMap::new();
        let Ok(mtime) = db_change_mtime(&self.db_path) else {
            return Ok(out);
        };
        for s in load_db_sessions(&self.db_path, mtime) {
            out.insert(s.id, (self.db_path.clone(), mtime));
        }
        Ok(out)
    }

    fn parse(&self, _path: &Path) -> Result<Option<Session>> {
        Ok(None)
    }

    fn find_incremental(
        &self,
        known: &KnownSessions,
        on_session: &mut dyn FnMut(&Session),
    ) -> Result<IncrementalResult> {
        let my_agent = self.id();

        if !self.is_available() {
            let deleted = known
                .iter()
                .filter(|(key, _)| key.agent == my_agent)
                .map(|(key, _)| key.id.clone())
                .collect();
            return Ok(IncrementalResult {
                upserts: Vec::new(),
                deleted,
            });
        }

        let mtime = db_change_mtime(&self.db_path).unwrap_or(0);
        let Some(sessions) = read_db_sessions(&self.db_path, mtime) else {
            // The file is there but cannot be read right now. Keep what is
            // indexed instead of reporting every known session as deleted.
            return Ok(IncrementalResult {
                upserts: Vec::new(),
                deleted: Vec::new(),
            });
        };

        let mut upserts = Vec::new();
        let mut seen = HashSet::new();
        for session in sessions {
            seen.insert(session.id.clone());
            let changed = match known.get(&SessionKey::from(&session)) {
                None => true,
                Some(known_mtime) => session.mtime > *known_mtime,
            };
            if changed {
                on_session(&session);
                upserts.push(session);
            }
        }

        let deleted = known
            .iter()
            .filter(|(key, _)| key.agent == my_agent && !seen.contains(&key.id))
            .map(|(key, _)| key.id.clone())
            .collect();

        Ok(IncrementalResult { upserts, deleted })
    }

    /// `goose session --resume --session-id <id>`, as `goose session --help`
    /// documents it in Goose 1.50.
    fn resume_command(&self, session: &Session, _yolo: bool) -> Vec<String> {
        let mut cmd = vec![program()];
        cmd.extend(resume_args(session));
        cmd
    }

    /// `--fork` copies the messages into a new session. Goose names the copy
    /// itself: with `--resume`, `--name` picks the session to open.
    fn fork_command(
        &self,
        seed: &Session,
        _new_id: Option<&str>,
        _name: Option<&str>,
        _yolo: bool,
    ) -> Option<Vec<String>> {
        let mut cmd = vec![program()];
        cmd.extend(resume_args(seed));
        cmd.push("--fork".to_string());
        Some(cmd)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_path_follows_xdg_data_home() {
        assert_eq!(
            db_path_from(None, "/home/u"),
            PathBuf::from("/home/u/.local/share/goose/sessions/sessions.db")
        );
        assert_eq!(
            db_path_from(Some("/data"), "/home/u"),
            PathBuf::from("/data/goose/sessions/sessions.db")
        );
        assert_eq!(
            db_path_from(Some("relative"), "/home/u"),
            PathBuf::from("/home/u/.local/share/goose/sessions/sessions.db"),
            "a relative XDG_DATA_HOME is ignored"
        );
    }

    #[test]
    fn content_text_keeps_text_and_names_tool_calls() {
        let json = r#"[{"type":"text","text":" look at the parser "},
            {"type":"thinking","thinking":"private","signature":"x"},
            {"type":"toolRequest","id":"t1","toolCall":{"status":"success","value":{"name":"developer__shell","arguments":{"command":"secret"}}}},
            {"type":"toolResponse","id":"t1","toolResult":{"status":"success","value":[{"type":"text","text":"tool output"}]}}]"#;
        assert_eq!(
            content_text(json),
            "look at the parser [calling developer__shell]"
        );
        assert_eq!(content_text("not json"), "");
        assert_eq!(content_text("{}"), "");
    }

    #[test]
    fn hidden_messages_are_recognised() {
        assert!(is_user_visible(
            r#"{"userVisible":true,"agentVisible":true}"#
        ));
        assert!(!is_user_visible(
            r#"{"userVisible":false,"agentVisible":true,"turnContext":true}"#
        ));
        assert!(is_user_visible("{}"));
        assert!(is_user_visible("broken"));
    }

    #[test]
    fn internal_session_types_are_recognised() {
        for internal in ["sub_agent", "subagent", "SubAgent", "hidden"] {
            assert!(is_internal_type(internal), "{internal}");
        }
        for kept in ["user", "scheduled", "terminal", ""] {
            assert!(!is_internal_type(kept), "{kept}");
        }
    }

    #[test]
    fn timestamps_accept_text_seconds_and_millis() {
        assert_eq!(
            timestamp_secs(ValueRef::Text(b"1970-01-02 00:00:00")),
            Some(86_400)
        );
        assert_eq!(
            timestamp_secs(ValueRef::Integer(1_700_000_000)),
            Some(1_700_000_000)
        );
        assert_eq!(
            timestamp_secs(ValueRef::Integer(1_700_000_000_000)),
            Some(1_700_000_000)
        );
        assert_eq!(timestamp_secs(ValueRef::Null), None);
        assert_eq!(timestamp_secs(ValueRef::Text(b"soon")), None);
    }

    #[test]
    fn program_prefers_path_then_the_app_bundle() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let bundled = tmp.path().join("Goose.app").join("goose");
        std::fs::create_dir_all(bundled.parent().unwrap()).unwrap();
        let path_var = std::env::join_paths([bin.clone()]).unwrap();

        // Neither exists: the plain name, so the error names the command.
        assert_eq!(program_from(Some(&path_var), &bundled), "goose");

        std::fs::write(&bundled, "").unwrap();
        assert_eq!(
            program_from(Some(&path_var), &bundled),
            bundled.to_string_lossy()
        );
        assert_eq!(program_from(None, &bundled), bundled.to_string_lossy());

        std::fs::write(bin.join("goose"), "").unwrap();
        assert_eq!(program_from(Some(&path_var), &bundled), "goose");
    }
}

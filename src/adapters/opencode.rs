//! OpenCode keeps every session in one SQLite database. Two table sets exist:
//! OpenCode 1.x wrote `session`, `message` and `part`; OpenCode 2.x writes
//! `session_v2` and `session_message`, with the text of a turn stored inline
//! in the message row. This adapter reads whichever the database has.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::Result;
use rusqlite::{params_from_iter, Connection, OpenFlags, OptionalExtension};
use serde_json::Value;

use super::{mtime_nanos, Adapter, IncrementalResult, KnownSessions};
use crate::config::AgentId;
use crate::model::{
    join_content, truncate_title, Session, SessionKey, ASSISTANT_PREFIX, USER_PREFIX,
};

const TITLE_MAX: usize = 100;

/// OpenCode 2 tracks its import of a 1.x database under this `kv` key.
const V1_MIGRATION_STATE_KEY: &str = "migration.v1-v2";

pub struct OpenCodeAdapter {
    db_path: PathBuf,
}

impl OpenCodeAdapter {
    pub fn new() -> OpenCodeAdapter {
        OpenCodeAdapter {
            db_path: default_db_path(),
        }
    }

    pub fn with_db_path(db_path: PathBuf) -> OpenCodeAdapter {
        OpenCodeAdapter { db_path }
    }
}

impl Default for OpenCodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

pub fn default_db_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home)
        .join(".local")
        .join("share")
        .join("opencode")
        .join("opencode.db")
}

fn ms_to_secs(ms: i64) -> i64 {
    ms / 1000
}

/// When the database last changed: the newer of the database file and its
/// `-wal` file. OpenCode runs SQLite in WAL mode, so a new message can sit in
/// the WAL for a while before a checkpoint touches the main file.
fn db_change_mtime(db_path: &Path) -> Result<i64> {
    let db = mtime_nanos(db_path)?;
    let mut wal = OsString::from(db_path.as_os_str());
    wal.push("-wal");
    let wal = mtime_nanos(Path::new(&wal)).unwrap_or(0);
    Ok(db.max(wal))
}

pub fn load_db_sessions(db_path: &Path, db_mtime: i64) -> Vec<Session> {
    read_db_sessions(db_path, db_mtime).unwrap_or_default()
}

/// `None` means the database could not be read at all, which is different
/// from a readable database that holds no sessions.
fn read_db_sessions(db_path: &Path, db_mtime: i64) -> Option<Vec<Session>> {
    // Read-only, and not `immutable`: OpenCode may be writing, and an
    // immutable connection would ignore what is still in the WAL.
    let conn = Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    let mut sessions = read_from_conn(&conn, db_mtime)?;
    for session in &mut sessions {
        session.source_path = Some(db_path.to_string_lossy().to_string());
    }
    Some(sessions)
}

#[cfg(test)]
fn load_from_conn(conn: &Connection, db_mtime: i64) -> Vec<Session> {
    read_from_conn(conn, db_mtime).unwrap_or_default()
}

fn read_from_conn(conn: &Connection, db_mtime: i64) -> Option<Vec<Session>> {
    let layout = DbLayout::detect(conn)?;
    let mut sessions = Vec::new();
    for &source in layout.sources() {
        let rows = session_rows(conn, &layout, source)?;
        if rows.is_empty() {
            continue;
        }
        // Each source renders with its own messages: a session OpenCode 2
        // copied keeps its 1.x message ids, so the two cannot share one map.
        let mut conversations = match source {
            DbSchema::V1 => load_v1_conversations(conn),
            DbSchema::V2 => load_v2_conversations(conn),
        };
        for row in rows {
            let messages = conversations.remove(&row.id).unwrap_or_default();
            if let Some(session) = build_session(row, &messages, db_mtime) {
                sessions.push(session);
            }
        }
    }
    Some(sessions)
}

/// Table set that holds one OpenCode session and its conversation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DbSchema {
    /// OpenCode 1.x: `session`, `message` and `part`.
    V1,
    /// OpenCode 2.x: `session_v2` plus one `session_message` row per turn.
    V2,
}

impl DbSchema {
    fn session_table(self) -> &'static str {
        match self {
            DbSchema::V1 => "session",
            DbSchema::V2 => "session_v2",
        }
    }
}

/// Where an OpenCode database currently keeps its sessions.
///
/// A fresh OpenCode 2 database only has `session_v2`. When OpenCode 2 opens a
/// 1.x database it adds `session_v2` and copies the 1.x sessions in the
/// background, newest id first, saving its progress in
/// `kv['migration.v1-v2']`. It leaves the 1.x tables behind afterwards.
#[derive(Debug, PartialEq, Eq)]
enum DbLayout {
    V1,
    V2,
    /// OpenCode 2 has not finished copying 1.x sessions. Sessions with an id
    /// at or above `cursor` are already in `session_v2`; older ones still live
    /// only in the 1.x tables. No cursor means nothing was copied yet.
    Migrating {
        cursor: Option<String>,
    },
}

impl DbLayout {
    /// `None` when there is no session table or SQLite cannot tell.
    fn detect(conn: &Connection) -> Option<DbLayout> {
        let mut stmt = conn
            .prepare(
                "SELECT name FROM sqlite_master
                 WHERE type = 'table' AND name IN ('session', 'session_v2', 'kv')",
            )
            .ok()?;
        let tables = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .ok()?
            .collect::<rusqlite::Result<HashSet<String>>>()
            .ok()?;
        match (tables.contains("session_v2"), tables.contains("session")) {
            (false, false) => None,
            (false, true) => Some(DbLayout::V1),
            (true, false) => Some(DbLayout::V2),
            (true, true) if !tables.contains("kv") => Some(DbLayout::Migrating { cursor: None }),
            (true, true) => DbLayout::detect_migration(conn),
        }
    }

    /// Reads the migration state the way OpenCode does: a missing or unknown
    /// value means the copy has not started.
    fn detect_migration(conn: &Connection) -> Option<DbLayout> {
        let state = conn
            .query_row(
                "SELECT value FROM kv WHERE key = ?1",
                [V1_MIGRATION_STATE_KEY],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .ok()?
            .and_then(|value| serde_json::from_str::<Value>(&value).ok());
        let field = |key: &str| {
            state
                .as_ref()
                .and_then(|state| state.get(key))
                .and_then(Value::as_str)
                .map(String::from)
        };
        Some(match field("phase").as_deref() {
            // After the copy OpenCode deletes sessions only from `session_v2`,
            // so reading the 1.x leftovers would bring deleted sessions back.
            Some("completed") => DbLayout::V2,
            Some("sessions") => DbLayout::Migrating {
                cursor: field("cursor"),
            },
            _ => DbLayout::Migrating { cursor: None },
        })
    }

    fn sources(&self) -> &'static [DbSchema] {
        match self {
            DbLayout::V1 => &[DbSchema::V1],
            DbLayout::V2 => &[DbSchema::V2],
            DbLayout::Migrating { .. } => &[DbSchema::V2, DbSchema::V1],
        }
    }
}

struct SessionRow {
    id: String,
    directory: String,
    title: String,
    time_created: i64,
    time_updated: i64,
}

/// Reads the sessions of one source. Subagent child sessions (`parent_id` set)
/// are left out, as in OpenCode's own session list; forks are top-level
/// sessions and stay. `None` means the table could not be queried.
fn session_rows(conn: &Connection, layout: &DbLayout, source: DbSchema) -> Option<Vec<SessionRow>> {
    let table = source.session_table();
    let mut filters: Vec<&str> = Vec::new();
    let mut params: Vec<Option<String>> = Vec::new();
    // Early 1.x databases have no `parent_id` column.
    if table_columns(conn, table).contains("parent_id") {
        filters.push("parent_id IS NULL");
    }
    if let (DbLayout::Migrating { cursor }, DbSchema::V1) = (layout, source) {
        // Only the 1.x sessions OpenCode has not copied yet. The `NOT IN`
        // guard covers ids that were already present in `session_v2`.
        filters.push("(?1 IS NULL OR id < ?1)");
        filters.push("id NOT IN (SELECT id FROM session_v2)");
        params.push(cursor.clone());
    }
    let filter = if filters.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", filters.join(" AND "))
    };
    let query = format!(
        "SELECT id, directory, title, time_created, time_updated FROM {table}{filter} ORDER BY id"
    );

    let mut stmt = conn.prepare(&query).ok()?;
    let rows = stmt
        .query_map(params_from_iter(params.iter()), |row| {
            Ok(SessionRow {
                id: row.get(0)?,
                directory: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                title: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                time_created: row.get::<_, Option<i64>>(3)?.unwrap_or(0),
                time_updated: row.get::<_, Option<i64>>(4)?.unwrap_or(0),
            })
        })
        .ok()?;
    Some(rows.flatten().filter(|row| !row.id.is_empty()).collect())
}

fn table_columns(conn: &Connection, table: &str) -> HashSet<String> {
    let Ok(mut stmt) = conn.prepare(&format!("PRAGMA table_info({table})")) else {
        return HashSet::new();
    };
    let Ok(rows) = stmt.query_map([], |row| row.get::<_, String>(1)) else {
        return HashSet::new();
    };
    rows.flatten().collect()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    User,
    Assistant,
}

impl Role {
    fn parse(role: &str) -> Option<Role> {
        match role {
            "user" => Some(Role::User),
            "assistant" => Some(Role::Assistant),
            _ => None,
        }
    }
}

/// The user and assistant messages that have text, in order, per session id.
type Conversations = HashMap<String, Vec<(Role, String)>>;

/// OpenCode 1.x: the role is in `message.data`, the text in `part` rows of
/// type `text`. Unreadable rows are skipped.
fn load_v1_conversations(conn: &Connection) -> Conversations {
    let mut texts: HashMap<String, Vec<String>> = HashMap::new();
    if let Ok(mut stmt) =
        conn.prepare("SELECT message_id, data FROM part ORDER BY time_created, id")
    {
        if let Ok(rows) = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        }) {
            for (message_id, data) in rows.flatten() {
                let Ok(part) = serde_json::from_str::<Value>(&data) else {
                    continue;
                };
                if part.get("type").and_then(Value::as_str) != Some("text") {
                    continue;
                }
                match part.get("text").and_then(Value::as_str) {
                    Some(text) if !text.is_empty() => {
                        texts.entry(message_id).or_default().push(text.to_string());
                    }
                    _ => {}
                }
            }
        }
    }

    let mut conversations = Conversations::new();
    if let Ok(mut stmt) =
        conn.prepare("SELECT id, session_id, data FROM message ORDER BY time_created, id")
    {
        if let Ok(rows) = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        }) {
            for (message_id, session_id, data) in rows.flatten() {
                let role = serde_json::from_str::<Value>(&data)
                    .ok()
                    .and_then(|v| v.get("role").and_then(Value::as_str).and_then(Role::parse));
                let (Some(role), Some(chunks)) = (role, texts.remove(&message_id)) else {
                    continue;
                };
                conversations
                    .entry(session_id)
                    .or_default()
                    .push((role, chunks.join("\n")));
            }
        }
    }
    conversations
}

/// OpenCode 2.x: one `session_message` row per turn, ordered by `seq`. Only
/// user prompts and assistant text are read; the `synthetic`, `system`,
/// `compaction`, `idle` and `model-switched` rows are not conversation.
fn load_v2_conversations(conn: &Connection) -> Conversations {
    let mut conversations = Conversations::new();
    let Ok(mut stmt) = conn.prepare(
        "SELECT session_id, type, data FROM session_message
         WHERE type IN ('user', 'assistant') ORDER BY session_id, seq",
    ) else {
        return conversations;
    };
    let Ok(rows) = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    }) else {
        return conversations;
    };
    for (session_id, kind, data) in rows.flatten() {
        let Some(role) = Role::parse(&kind) else {
            continue;
        };
        let Ok(data) = serde_json::from_str::<Value>(&data) else {
            continue;
        };
        let text = v2_message_text(role, &data);
        if text.is_empty() {
            continue;
        }
        conversations
            .entry(session_id)
            .or_default()
            .push((role, text));
    }
    conversations
}

/// A user row keeps the prompt in `text`. An assistant row keeps an ordered
/// `content` array; its `text` items are the visible reply, and its
/// `reasoning` and `tool` items are skipped.
fn v2_message_text(role: Role, data: &Value) -> String {
    let chunks: Vec<&str> = match role {
        Role::User => data
            .get("text")
            .and_then(Value::as_str)
            .into_iter()
            .collect(),
        Role::Assistant => data
            .get("content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|item| item.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|item| item.get("text").and_then(Value::as_str))
            .collect(),
    };
    chunks
        .into_iter()
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// `None` for a session with neither a title nor a user prompt: there is
/// nothing to show for it.
fn build_session(row: SessionRow, messages: &[(Role, String)], db_mtime: i64) -> Option<Session> {
    let title_source = if !row.title.trim().is_empty() {
        row.title.as_str()
    } else {
        messages
            .iter()
            .find(|(role, _)| *role == Role::User)
            .map(|(_, text)| text.as_str())?
    };
    let title = truncate_title(title_source, TITLE_MAX, false);

    let rendered: Vec<String> = messages
        .iter()
        .map(|(role, text)| {
            let prefix = match role {
                Role::User => USER_PREFIX,
                Role::Assistant => ASSISTANT_PREFIX,
            };
            format!("{prefix}{text}")
        })
        .collect();

    Some(Session {
        id: row.id,
        agent: AgentId::OpenCode,
        title,
        directory: row.directory,
        timestamp: ms_to_secs(row.time_created.max(row.time_updated)),
        content: join_content(&rendered),
        message_count: rendered.len() as u32,
        mtime: db_mtime,
        yolo: false,
        source_path: None,
    })
}

impl Adapter for OpenCodeAdapter {
    fn id(&self) -> AgentId {
        AgentId::OpenCode
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

    /// `opencode [directory] --session <id>` opens the TUI on that session.
    /// OpenCode 1.x and 2.x both accept this form.
    fn resume_command(&self, session: &Session, _yolo: bool) -> Vec<String> {
        let mut cmd = vec!["opencode".to_string()];
        if !session.directory.is_empty() {
            cmd.push(session.directory.clone());
        }
        cmd.push("--session".to_string());
        cmd.push(session.id.clone());
        cmd
    }

    fn fork_command(
        &self,
        seed: &Session,
        _new_id: Option<&str>,
        name: Option<&str>,
        _yolo: bool,
    ) -> Option<Vec<String>> {
        let mut cmd = vec![
            "opencode".to_string(),
            "run".to_string(),
            "--session".to_string(),
            seed.id.clone(),
            "--fork".to_string(),
        ];
        if let Some(n) = name {
            cmd.push("--title".to_string());
            cmd.push(n.to_string());
        }
        Some(cmd)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE session (id TEXT PRIMARY KEY, directory TEXT, title TEXT,
                                   time_created INTEGER, time_updated INTEGER);
             CREATE TABLE message (id TEXT PRIMARY KEY, session_id TEXT,
                                   time_created INTEGER, data TEXT);
             CREATE TABLE part (id TEXT PRIMARY KEY, message_id TEXT, session_id TEXT,
                                time_created INTEGER, data TEXT);",
        )
        .unwrap();
        conn
    }

    #[test]
    fn loads_session_with_parts() {
        let conn = make_db();
        conn.execute(
            "INSERT INTO session VALUES ('ses_1','/work/proj','Quick check-in',1776554457825,1776562587506)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO message VALUES ('m1','ses_1',1,'{\"role\":\"user\"}')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO message VALUES ('m2','ses_1',2,'{\"role\":\"assistant\"}')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO part VALUES ('p1','m1','ses_1',1,'{\"type\":\"text\",\"text\":\"are you here\"}')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO part VALUES ('p2','m2','ses_1',2,'{\"type\":\"text\",\"text\":\"yes, here\"}')",
            [],
        )
        .unwrap();

        let sessions = load_from_conn(&conn, 555);
        assert_eq!(sessions.len(), 1);
        let s = &sessions[0];
        assert_eq!(s.id, "ses_1");
        assert_eq!(s.agent, AgentId::OpenCode);
        assert_eq!(s.directory, "/work/proj");
        assert_eq!(s.title, "Quick check-in");
        assert_eq!(s.timestamp, ms_to_secs(1776562587506));
        assert_eq!(s.message_count, 2);
        assert_eq!(s.mtime, 555);
        assert!(s.content.contains("» are you here"));
        assert!(s.content.contains("  yes, here"));
    }

    #[test]
    fn non_text_parts_ignored() {
        let conn = make_db();
        conn.execute("INSERT INTO session VALUES ('s','/w','T',1000,1000)", [])
            .unwrap();
        conn.execute(
            "INSERT INTO message VALUES ('m','s',1,'{\"role\":\"user\"}')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO part VALUES ('p1','m','s',1,'{\"type\":\"tool\",\"text\":\"ignored\"}')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO part VALUES ('p2','m','s',2,'{\"type\":\"text\",\"text\":\"kept\"}')",
            [],
        )
        .unwrap();
        let sessions = load_from_conn(&conn, 1);
        assert!(sessions[0].content.contains("kept"));
        assert!(!sessions[0].content.contains("ignored"));
    }

    #[test]
    fn ms_to_secs_divides() {
        assert_eq!(ms_to_secs(1776562587506), 1776562587);
    }

    #[test]
    fn resume_and_fork_commands() {
        let a = OpenCodeAdapter::with_db_path(PathBuf::from("/nope.db"));
        let s = Session {
            id: "ses_1".into(),
            agent: AgentId::OpenCode,
            title: "t".into(),
            directory: "/work/proj".into(),
            timestamp: 0,
            content: String::new(),
            message_count: 1,
            mtime: 0,
            yolo: false,
            source_path: None,
        };
        assert_eq!(
            a.resume_command(&s, false),
            vec!["opencode", "/work/proj", "--session", "ses_1"]
        );
        assert!(a.supports_fork());
        assert_eq!(
            a.fork_command(&s, None, None, false).unwrap(),
            vec!["opencode", "run", "--session", "ses_1", "--fork"]
        );
        assert_eq!(
            a.fork_command(&s, None, Some("my branch"), false).unwrap(),
            vec![
                "opencode",
                "run",
                "--session",
                "ses_1",
                "--fork",
                "--title",
                "my branch"
            ]
        );
    }
}

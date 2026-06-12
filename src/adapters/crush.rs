use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;

use super::{mtime_nanos, Adapter, IncrementalResult, KnownSessions};
use crate::config::AgentId;
use crate::model::{
    join_content, truncate_title, Session, SessionKey, ASSISTANT_PREFIX, USER_PREFIX,
};

const TITLE_MAX: usize = 100;

const MILLIS_THRESHOLD: i64 = 100_000_000_000;

const TOOL_RESULT_MAX: usize = 500;

pub struct CrushAdapter {
    projects_file: PathBuf,
}

impl CrushAdapter {
    pub fn new() -> CrushAdapter {
        CrushAdapter {
            projects_file: default_projects_file(),
        }
    }

    pub fn with_projects_file(projects_file: PathBuf) -> CrushAdapter {
        CrushAdapter { projects_file }
    }

    fn project_dbs(&self) -> Vec<(String, PathBuf)> {
        let Ok(bytes) = std::fs::read(&self.projects_file) else {
            return Vec::new();
        };
        let Ok(json) = serde_json::from_slice::<Value>(&bytes) else {
            return Vec::new();
        };
        let Some(projects) = json.get("projects").and_then(|p| p.as_array()) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for p in projects {
            let path = p.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let data_dir = p.get("data_dir").and_then(|v| v.as_str()).unwrap_or("");
            if path.is_empty() || data_dir.is_empty() {
                continue;
            }
            let db = PathBuf::from(data_dir).join("crush.db");
            if db.is_file() {
                out.push((path.to_string(), db));
            }
        }
        out
    }
}

impl Default for CrushAdapter {
    fn default() -> Self {
        Self::new()
    }
}

pub fn default_projects_file() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home)
        .join(".local")
        .join("share")
        .join("crush")
        .join("projects.json")
}

fn to_unix_secs(raw: i64) -> i64 {
    if raw > MILLIS_THRESHOLD {
        raw / 1000
    } else {
        raw
    }
}

fn parts_text(parts_json: &str) -> String {
    let Ok(parts) = serde_json::from_str::<Value>(parts_json) else {
        return String::new();
    };
    let Some(arr) = parts.as_array() else {
        return String::new();
    };
    let mut out: Vec<String> = Vec::new();
    for part in arr {
        let kind = part.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let data = part.get("data");
        match kind {
            "text" => {
                if let Some(t) = data.and_then(|d| d.get("text")).and_then(|v| v.as_str()) {
                    if !t.is_empty() {
                        out.push(t.to_string());
                    }
                }
            }
            "tool_call" => {
                let name = data
                    .and_then(|d| d.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("tool");
                out.push(format!("[calling {name}]"));
            }
            "tool_result" => {
                let name = data
                    .and_then(|d| d.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("tool");
                let content = data
                    .and_then(|d| d.get("content"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if content.len() < TOOL_RESULT_MAX {
                    let snippet: String = content.chars().take(200).collect();
                    out.push(format!("[{name}]: {snippet}"));
                }
            }
            _ => {}
        }
    }
    out.join(" ")
}

pub fn load_db_sessions(db_path: &Path, directory: &str, db_mtime: i64) -> Vec<Session> {
    let conn = match Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    load_from_conn(&conn, directory, db_mtime)
}

fn load_from_conn(conn: &Connection, directory: &str, db_mtime: i64) -> Vec<Session> {
    let mut sessions = Vec::new();

    let mut stmt = match conn.prepare(
        "SELECT id, title, message_count, updated_at, created_at
         FROM sessions WHERE message_count > 0",
    ) {
        Ok(s) => s,
        Err(_) => return sessions,
    };

    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?.unwrap_or_default(),
            row.get::<_, i64>(2)?,
            row.get::<_, Option<i64>>(3)?,
            row.get::<_, Option<i64>>(4)?,
        ))
    });
    let rows = match rows {
        Ok(r) => r,
        Err(_) => return sessions,
    };

    for row in rows.flatten() {
        let (id, title, message_count, updated_at, created_at) = row;
        if id.is_empty() {
            continue;
        }
        let raw_ts = updated_at.or(created_at).unwrap_or(0);
        let timestamp = to_unix_secs(raw_ts);

        let mut messages: Vec<String> = Vec::new();
        let mut first_user = String::new();
        if let Ok(mut mstmt) = conn
            .prepare("SELECT role, parts FROM messages WHERE session_id = ?1 ORDER BY created_at")
        {
            if let Ok(mrows) = mstmt.query_map([&id], |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?,
                    r.get::<_, Option<String>>(1)?,
                ))
            }) {
                for m in mrows.flatten() {
                    let (role, parts) = m;
                    let Some(role) = role else { continue };
                    let prefix = match role.as_str() {
                        "user" => USER_PREFIX,
                        "assistant" => ASSISTANT_PREFIX,
                        _ => continue,
                    };
                    let text = parts.map(|p| parts_text(&p)).unwrap_or_default();
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

        let title_source = if !title.trim().is_empty() {
            title
        } else if !first_user.is_empty() {
            first_user
        } else {
            continue;
        };

        sessions.push(Session {
            id,
            agent: AgentId::Crush,
            title: truncate_title(&title_source, TITLE_MAX, false),
            directory: directory.to_string(),
            timestamp,
            content: join_content(&messages),
            message_count: message_count.max(0) as u32,
            mtime: db_mtime,
            yolo: false,
            source_path: None,
        });
    }

    sessions
}

impl Adapter for CrushAdapter {
    fn id(&self) -> AgentId {
        AgentId::Crush
    }

    fn supports_yolo(&self) -> bool {
        true
    }

    fn is_available(&self) -> bool {
        self.projects_file.is_file()
    }

    fn scan(&self) -> Result<HashMap<String, (PathBuf, i64)>> {
        let mut out = HashMap::new();
        for (dir, db) in self.project_dbs() {
            let Ok(mtime) = mtime_nanos(&db) else {
                continue;
            };
            for s in load_db_sessions(&db, &dir, mtime) {
                out.insert(s.id, (db.clone(), mtime));
            }
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
        let mut upserts = Vec::new();
        let mut seen_ids = std::collections::HashSet::new();

        for (dir, db) in self.project_dbs() {
            let Ok(mtime) = mtime_nanos(&db) else {
                continue;
            };
            for mut session in load_db_sessions(&db, &dir, mtime) {
                session.source_path = Some(db.to_string_lossy().to_string());
                seen_ids.insert(session.id.clone());
                let changed = match known.get(&SessionKey::from(&session)) {
                    None => true,
                    Some(known_mtime) => session.mtime > *known_mtime,
                };
                if changed {
                    on_session(&session);
                    upserts.push(session);
                }
            }
        }

        let deleted = known
            .iter()
            .filter(|(key, _)| key.agent == my_agent && !seen_ids.contains(&key.id))
            .map(|(key, _)| key.id.clone())
            .collect();

        Ok(IncrementalResult { upserts, deleted })
    }

    fn resume_command(&self, session: &Session, yolo: bool) -> Vec<String> {
        let mut cmd = vec!["crush".to_string()];
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

    fn make_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (id TEXT PRIMARY KEY, title TEXT, message_count INTEGER,
                                    updated_at INTEGER, created_at INTEGER);
             CREATE TABLE messages (id TEXT PRIMARY KEY, session_id TEXT, role TEXT,
                                    parts TEXT, created_at INTEGER);",
        )
        .unwrap();
        conn
    }

    #[test]
    fn parts_text_extracts_text_and_markers() {
        let json = r#"[{"type":"text","data":{"text":"hello world"}},
                       {"type":"tool_call","data":{"name":"Read"}},
                       {"type":"tool_result","data":{"name":"Read","content":"short output"}}]"#;
        let t = parts_text(json);
        assert!(t.contains("hello world"));
        assert!(t.contains("[calling Read]"));
        assert!(t.contains("[Read]: short output"));
    }

    #[test]
    fn parts_text_drops_large_tool_results() {
        let big = "x".repeat(600);
        let json = format!(
            "[{{\"type\":\"tool_result\",\"data\":{{\"name\":\"Big\",\"content\":\"{big}\"}}}}]"
        );
        assert_eq!(parts_text(&json), "");
    }

    #[test]
    fn to_unix_secs_detects_millis() {
        assert_eq!(to_unix_secs(1_700_000_000), 1_700_000_000);
        assert_eq!(to_unix_secs(1_700_000_000_000), 1_700_000_000);
    }

    #[test]
    fn loads_sessions_from_conn() {
        let conn = make_db();
        conn.execute(
            "INSERT INTO sessions VALUES ('s1', 'Fix auth', 2, 1700000000000, 1699000000000)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO messages VALUES ('m1','s1','user','[{\"type\":\"text\",\"data\":{\"text\":\"fix the auth bug\"}}]', 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO messages VALUES ('m2','s1','assistant','[{\"type\":\"text\",\"data\":{\"text\":\"on it\"}}]', 2)",
            [],
        )
        .unwrap();

        let sessions = load_from_conn(&conn, "/work/proj", 12345);
        assert_eq!(sessions.len(), 1);
        let s = &sessions[0];
        assert_eq!(s.id, "s1");
        assert_eq!(s.agent, AgentId::Crush);
        assert_eq!(s.title, "Fix auth");
        assert_eq!(s.directory, "/work/proj");
        assert_eq!(s.timestamp, 1_700_000_000);
        assert_eq!(s.message_count, 2);
        assert_eq!(s.mtime, 12345);
        assert!(s.content.contains("» fix the auth bug"));
        assert!(s.content.contains("  on it"));
        assert!(!s.yolo);
    }

    #[test]
    fn title_falls_back_to_first_user_message() {
        let conn = make_db();
        conn.execute("INSERT INTO sessions VALUES ('s1', '', 1, 100, 100)", [])
            .unwrap();
        conn.execute(
            "INSERT INTO messages VALUES ('m1','s1','user','[{\"type\":\"text\",\"data\":{\"text\":\"the first thing I asked\"}}]', 1)",
            [],
        )
        .unwrap();
        let sessions = load_from_conn(&conn, "/w", 1);
        assert_eq!(sessions[0].title, "the first thing I asked");
    }

    #[test]
    fn sessions_with_zero_message_count_skipped() {
        let conn = make_db();
        conn.execute(
            "INSERT INTO sessions VALUES ('empty', 'x', 0, 100, 100)",
            [],
        )
        .unwrap();
        assert!(load_from_conn(&conn, "/w", 1).is_empty());
    }

    #[test]
    fn resume_command_with_and_without_yolo() {
        let a = CrushAdapter::with_projects_file(PathBuf::from("/nope.json"));
        let s = Session {
            id: "s1".into(),
            agent: AgentId::Crush,
            title: "t".into(),
            directory: "/w".into(),
            timestamp: 0,
            content: String::new(),
            message_count: 1,
            mtime: 0,
            yolo: false,
            source_path: None,
        };
        assert_eq!(
            a.resume_command(&s, false),
            vec!["crush", "--session", "s1"]
        );
        assert_eq!(
            a.resume_command(&s, true),
            vec!["crush", "--yolo", "--session", "s1"]
        );
        assert!(!a.supports_fork());
    }
}

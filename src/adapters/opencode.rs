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

pub fn load_db_sessions(db_path: &Path, db_mtime: i64) -> Vec<Session> {
    let conn = match Connection::open_with_flags(
        db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let mut sessions = load_from_conn(&conn, db_mtime);
    for session in &mut sessions {
        session.source_path = Some(db_path.to_string_lossy().to_string());
    }
    sessions
}

fn load_from_conn(conn: &Connection, db_mtime: i64) -> Vec<Session> {
    let mut sessions = Vec::new();

    let mut stmt = match conn
        .prepare("SELECT id, directory, title, time_created, time_updated FROM session")
    {
        Ok(s) => s,
        Err(_) => return sessions,
    };
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<i64>>(3)?,
            row.get::<_, Option<i64>>(4)?,
        ))
    });
    let rows = match rows {
        Ok(r) => r,
        Err(_) => return sessions,
    };

    for row in rows.flatten() {
        let (id, directory, title, time_created, time_updated) = row;
        if id.is_empty() {
            continue;
        }
        let raw_ms = time_created.unwrap_or(0).max(time_updated.unwrap_or(0));
        let timestamp = ms_to_secs(raw_ms);

        let mut messages: Vec<String> = Vec::new();
        let mut first_user = String::new();
        let mut turn_count: u32 = 0;

        if let Ok(mut mstmt) = conn
            .prepare("SELECT id, data FROM message WHERE session_id = ?1 ORDER BY time_created, id")
        {
            if let Ok(mrows) = mstmt.query_map([&id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            }) {
                for m in mrows.flatten() {
                    let (msg_id, data) = m;
                    let role = serde_json::from_str::<Value>(&data)
                        .ok()
                        .and_then(|v| v.get("role").and_then(|r| r.as_str()).map(String::from))
                        .unwrap_or_default();
                    let prefix = match role.as_str() {
                        "user" => USER_PREFIX,
                        "assistant" => ASSISTANT_PREFIX,
                        _ => continue,
                    };

                    let mut text = String::new();
                    if let Ok(mut pstmt) = conn.prepare(
                        "SELECT data FROM part WHERE message_id = ?1 ORDER BY time_created, id",
                    ) {
                        if let Ok(prows) = pstmt.query_map([&msg_id], |r| r.get::<_, String>(0)) {
                            let mut chunks = Vec::new();
                            for pdata in prows.flatten() {
                                if let Ok(pv) = serde_json::from_str::<Value>(&pdata) {
                                    if pv.get("type").and_then(|t| t.as_str()) == Some("text") {
                                        if let Some(t) = pv.get("text").and_then(|t| t.as_str()) {
                                            if !t.is_empty() {
                                                chunks.push(t.to_string());
                                            }
                                        }
                                    }
                                }
                            }
                            text = chunks.join("\n");
                        }
                    }

                    if text.is_empty() {
                        continue;
                    }
                    if role == "user" && first_user.is_empty() {
                        first_user = text.clone();
                    }
                    messages.push(format!("{prefix}{text}"));
                    turn_count += 1;
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
            agent: AgentId::OpenCode,
            title: truncate_title(&title_source, TITLE_MAX, false),
            directory,
            timestamp,
            content: join_content(&messages),
            message_count: turn_count,
            mtime: db_mtime,
            yolo: false,
            source_path: None,
        });
    }

    sessions
}

impl Adapter for OpenCodeAdapter {
    fn id(&self) -> AgentId {
        AgentId::OpenCode
    }

    fn supports_fork(&self) -> bool {
        true
    }

    fn is_available(&self) -> bool {
        self.db_path.is_file()
    }

    fn scan(&self) -> Result<HashMap<String, (PathBuf, i64)>> {
        let mut out = HashMap::new();
        let Ok(mtime) = mtime_nanos(&self.db_path) else {
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

        let mtime = mtime_nanos(&self.db_path).unwrap_or(0);
        let mut upserts = Vec::new();
        let mut seen = std::collections::HashSet::new();

        for session in load_db_sessions(&self.db_path, mtime) {
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

    fn resume_command(&self, session: &Session, _yolo: bool) -> Vec<String> {
        vec![
            "opencode".to_string(),
            session.directory.clone(),
            "--session".to_string(),
            session.id.clone(),
        ]
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

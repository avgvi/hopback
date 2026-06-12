use std::collections::HashMap;

use anyhow::Result;
use rusqlite::{params, Connection};

use super::Store;
use crate::adapters::KnownSessions;
use crate::config::AgentId;
use crate::model::{Session, SessionKey};

const SESSION_COLS: &str = "s.id, s.agent, COALESCE(o.title, s.title) AS title, \
    s.directory, s.timestamp, s.content, s.message_count, s.mtime, s.yolo, s.source_path";
const VISIBLE_WHERE: &str = "COALESCE(o.hidden, 0) = 0";
const HIDDEN_WHERE: &str = "COALESCE(o.hidden, 0) = 1";

fn row_to_session(row: &rusqlite::Row) -> rusqlite::Result<Session> {
    let agent_str: String = row.get("agent")?;
    let agent = AgentId::from_id(&agent_str).unwrap_or(AgentId::Claude);
    Ok(Session {
        id: row.get("id")?,
        agent,
        title: row.get("title")?,
        directory: row.get("directory")?,
        timestamp: row.get("timestamp")?,
        content: row.get("content")?,
        message_count: row.get::<_, i64>("message_count")? as u32,
        mtime: row.get("mtime")?,
        yolo: row.get::<_, i64>("yolo")? != 0,
        source_path: row.get("source_path")?,
    })
}

pub(super) fn upsert_one(conn: &Connection, s: &Session) -> Result<()> {
    conn.execute(
        "INSERT INTO sessions
            (agent, id, title, directory, timestamp, content, message_count, mtime, yolo, source_path)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT(agent, id) DO UPDATE SET
            title=excluded.title, directory=excluded.directory,
            timestamp=excluded.timestamp, content=excluded.content,
            message_count=excluded.message_count, mtime=excluded.mtime, yolo=excluded.yolo,
            source_path=excluded.source_path",
        params![
            s.agent.as_str(),
            s.id,
            s.title,
            s.directory,
            s.timestamp,
            s.content,
            s.message_count as i64,
            s.mtime,
            s.yolo as i64,
            s.source_path,
        ],
    )?;

    conn.execute(
        "DELETE FROM sessions_fts WHERE agent = ?1 AND id = ?2",
        params![s.agent.as_str(), s.id],
    )?;
    conn.execute(
        "INSERT INTO sessions_fts (title, content, agent, id) VALUES (?1, ?2, ?3, ?4)",
        params![s.title, s.content, s.agent.as_str(), s.id],
    )?;
    Ok(())
}

pub(super) fn delete_one(conn: &Connection, agent: AgentId, id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM sessions WHERE agent = ?1 AND id = ?2",
        params![agent.as_str(), id],
    )?;
    conn.execute(
        "DELETE FROM sessions_fts WHERE agent = ?1 AND id = ?2",
        params![agent.as_str(), id],
    )?;
    Ok(())
}

impl Store {
    pub fn upsert_sessions(&mut self, sessions: &[Session]) -> Result<()> {
        let tx = self.conn.transaction()?;
        for s in sessions {
            upsert_one(&tx, s)?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn delete_sessions(&mut self, agent: AgentId, ids: &[String]) -> Result<()> {
        let tx = self.conn.transaction()?;
        for id in ids {
            delete_one(&tx, agent, id)?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn apply_incremental(
        &mut self,
        agent: AgentId,
        upserts: &[Session],
        deleted: &[String],
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        for id in deleted {
            delete_one(&tx, agent, id)?;
        }
        for s in upserts {
            upsert_one(&tx, s)?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn get_known_sessions(&self) -> Result<KnownSessions> {
        let mut stmt = self.conn.prepare("SELECT agent, id, mtime FROM sessions")?;
        let rows = stmt.query_map([], |row| {
            let agent_str: String = row.get(0)?;
            let agent = AgentId::from_id(&agent_str).unwrap_or(AgentId::Claude);
            Ok((
                SessionKey::new(agent, row.get::<_, String>(1)?),
                row.get::<_, i64>(2)?,
            ))
        })?;
        let mut out = HashMap::new();
        for r in rows {
            let (id, val) = r?;
            out.insert(id, val);
        }
        Ok(out)
    }

    pub fn get_all_sessions(&self) -> Result<Vec<Session>> {
        let sql = format!(
            "SELECT {SESSION_COLS} FROM sessions s
             LEFT JOIN session_overrides o ON o.agent = s.agent AND o.session_id = s.id
             WHERE {VISIBLE_WHERE}
             ORDER BY s.timestamp DESC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], row_to_session)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn get_session(&self, key: &SessionKey) -> Result<Option<Session>> {
        let sql = format!(
            "SELECT {SESSION_COLS} FROM sessions s
             LEFT JOIN session_overrides o ON o.agent = s.agent AND o.session_id = s.id
             WHERE s.agent = ?1 AND s.id = ?2 AND {VISIBLE_WHERE}"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query_map(params![key.agent.as_str(), key.id], row_to_session)?;
        match rows.next() {
            Some(r) => Ok(Some(r?)),
            None => Ok(None),
        }
    }

    pub fn find_sessions_by_raw_id(&self, id: &str) -> Result<Vec<Session>> {
        let sql = format!(
            "SELECT {SESSION_COLS} FROM sessions s
             LEFT JOIN session_overrides o ON o.agent = s.agent AND o.session_id = s.id
             WHERE s.id = ?1 AND {VISIBLE_WHERE}
             ORDER BY s.timestamp DESC, s.agent"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![id], row_to_session)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn session_count(&self) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM sessions s
             LEFT JOIN session_overrides o ON o.agent = s.agent AND o.session_id = s.id
             WHERE COALESCE(o.hidden, 0) = 0",
            [],
            |r| r.get(0),
        )?)
    }

    pub fn get_hidden_sessions(&self) -> Result<Vec<Session>> {
        let sql = format!(
            "SELECT {SESSION_COLS} FROM sessions s
             INNER JOIN session_overrides o ON o.agent = s.agent AND o.session_id = s.id
             WHERE {HIDDEN_WHERE}
             ORDER BY s.timestamp DESC, s.agent"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], row_to_session)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn clear_sessions(&mut self) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM sessions_fts", [])?;
        tx.execute("DELETE FROM sessions", [])?;
        tx.execute("DELETE FROM dir_mtimes", [])?;
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(agent: AgentId, id: &str) -> SessionKey {
        SessionKey::new(agent, id)
    }

    fn sess(id: &str, agent: AgentId, ts: i64, mtime: i64) -> Session {
        Session {
            id: id.into(),
            agent,
            title: format!("title {id}"),
            directory: "/work".into(),
            timestamp: ts,
            content: format!("» hello {id}\n\n  reply {id}"),
            message_count: 2,
            mtime,
            yolo: false,
            source_path: Some(format!("/tmp/{id}.jsonl")),
        }
    }

    #[test]
    fn insert_reopen_read_back_identically() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let path = dir.path().join("aiz.db");

        let a = sess("a", AgentId::Claude, 100, 1000);
        let b = sess("b", AgentId::Codex, 200, 2000);
        {
            let mut s = Store::open(&path).unwrap();
            s.upsert_sessions(&[a.clone(), b.clone()]).unwrap();
        }

        let s = Store::open(&path).unwrap();
        let all = s.get_all_sessions().unwrap();

        assert_eq!(all, vec![b.clone(), a.clone()]);
        assert_eq!(s.get_session(&key(AgentId::Claude, "a")).unwrap(), Some(a));
        assert_eq!(
            s.get_session(&key(AgentId::Claude, "missing")).unwrap(),
            None
        );
    }

    #[test]
    fn known_sessions_round_trip() {
        let mut s = Store::open_in_memory().unwrap();
        s.upsert_sessions(&[
            sess("a", AgentId::Claude, 1, 111),
            sess("b", AgentId::Codex, 2, 222),
        ])
        .unwrap();
        let known = s.get_known_sessions().unwrap();
        assert_eq!(known.get(&key(AgentId::Claude, "a")), Some(&111));
        assert_eq!(known.get(&key(AgentId::Codex, "b")), Some(&222));
    }

    #[test]
    fn same_raw_id_can_exist_for_multiple_agents() {
        let mut s = Store::open_in_memory().unwrap();
        let claude = sess("same", AgentId::Claude, 1, 10);
        let codex = sess("same", AgentId::Codex, 2, 20);
        s.upsert_sessions(&[claude.clone(), codex.clone()]).unwrap();

        assert_eq!(s.session_count().unwrap(), 2);
        assert_eq!(
            s.get_session(&key(AgentId::Claude, "same")).unwrap(),
            Some(claude)
        );
        assert_eq!(
            s.get_session(&key(AgentId::Codex, "same")).unwrap(),
            Some(codex)
        );
        assert_eq!(s.find_sessions_by_raw_id("same").unwrap().len(), 2);
    }

    #[test]
    fn upsert_updates_not_duplicates() {
        let mut s = Store::open_in_memory().unwrap();
        s.upsert_sessions(&[sess("a", AgentId::Claude, 1, 1)])
            .unwrap();

        for ts in 2..=6 {
            let mut updated = sess("a", AgentId::Claude, ts, ts * 10);
            updated.title = format!("v{ts}");
            s.upsert_sessions(&[updated]).unwrap();
        }
        assert_eq!(s.session_count().unwrap(), 1);

        let fts_n: i64 = s
            .conn
            .query_row("SELECT COUNT(*) FROM sessions_fts WHERE id='a'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(fts_n, 1);
        assert_eq!(
            s.get_session(&key(AgentId::Claude, "a"))
                .unwrap()
                .unwrap()
                .title,
            "v6"
        );
    }

    #[test]
    fn delete_removes_row_and_fts() {
        let mut s = Store::open_in_memory().unwrap();
        s.upsert_sessions(&[sess("a", AgentId::Claude, 1, 1)])
            .unwrap();
        s.delete_sessions(AgentId::Claude, &["a".into(), "nonexistent".into()])
            .unwrap();
        assert_eq!(s.session_count().unwrap(), 0);
        let fts_n: i64 = s
            .conn
            .query_row("SELECT COUNT(*) FROM sessions_fts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fts_n, 0);
    }

    #[test]
    fn hidden_sessions_are_excluded_from_normal_reads_but_stay_known() {
        let mut s = Store::open_in_memory().unwrap();
        s.upsert_sessions(&[
            sess("a", AgentId::Claude, 1, 10),
            sess("b", AgentId::Claude, 2, 20),
        ])
        .unwrap();
        let hidden_key = key(AgentId::Claude, "a");
        s.set_hidden(&hidden_key, true).unwrap();

        let all = s.get_all_sessions().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, "b");
        assert!(s.get_session(&hidden_key).unwrap().is_none());
        assert!(s.find_sessions_by_raw_id("a").unwrap().is_empty());
        assert_eq!(s.session_count().unwrap(), 1);
        assert!(s.get_known_sessions().unwrap().contains_key(&hidden_key));
        assert_eq!(s.get_hidden_sessions().unwrap()[0].id, "a");
    }

    #[test]
    fn apply_incremental_deletes_then_upserts_atomically() {
        let mut s = Store::open_in_memory().unwrap();
        s.upsert_sessions(&[
            sess("old", AgentId::Claude, 1, 1),
            sess("keep", AgentId::Codex, 2, 2),
        ])
        .unwrap();
        let new = sess("new", AgentId::Claude, 5, 5);
        s.apply_incremental(AgentId::Claude, std::slice::from_ref(&new), &["old".into()])
            .unwrap();
        let ids: Vec<_> = s
            .get_all_sessions()
            .unwrap()
            .into_iter()
            .map(|x| x.id)
            .collect();

        assert_eq!(ids, vec!["new".to_string(), "keep".to_string()]);
    }
}

use std::collections::HashSet;

use anyhow::Result;
use rusqlite::params;

use super::Store;
use crate::model::SessionKey;

impl Store {
    pub fn set_pinned(&self, key: &SessionKey, pinned: bool) -> Result<()> {
        self.conn.execute(
            "INSERT INTO session_overrides (agent, session_id, pinned, title, updated)
             VALUES (?1, ?2, ?3, NULL, ?4)
             ON CONFLICT(agent, session_id) DO UPDATE SET pinned = excluded.pinned, updated = excluded.updated",
            params![key.agent.as_str(), key.id, pinned as i64, now_secs_arg()],
        )?;
        Ok(())
    }

    pub fn set_title_override(&self, key: &SessionKey, title: Option<&str>) -> Result<()> {
        self.conn.execute(
            "INSERT INTO session_overrides (agent, session_id, pinned, title, updated)
             VALUES (?1, ?2, 0, ?3, ?4)
             ON CONFLICT(agent, session_id) DO UPDATE SET title = excluded.title, updated = excluded.updated",
            params![key.agent.as_str(), key.id, title, now_secs_arg()],
        )?;
        Ok(())
    }

    pub fn set_hidden(&self, key: &SessionKey, hidden: bool) -> Result<()> {
        self.conn.execute(
            "INSERT INTO session_overrides (agent, session_id, pinned, title, hidden, updated)
             VALUES (?1, ?2, 0, NULL, ?3, ?4)
             ON CONFLICT(agent, session_id) DO UPDATE SET hidden = excluded.hidden, updated = excluded.updated",
            params![key.agent.as_str(), key.id, hidden as i64, now_secs_arg()],
        )?;
        Ok(())
    }

    pub fn get_pinned_keys(&self) -> Result<HashSet<SessionKey>> {
        let mut stmt = self
            .conn
            .prepare("SELECT agent, session_id FROM session_overrides WHERE pinned = 1")?;
        let rows = stmt.query_map([], |r| {
            let agent_str: String = r.get(0)?;
            let agent = crate::config::AgentId::from_id(&agent_str)
                .unwrap_or(crate::config::AgentId::Claude);
            Ok(SessionKey::new(agent, r.get::<_, String>(1)?))
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn is_pinned(&self, key: &SessionKey) -> Result<bool> {
        let pinned: Option<i64> = self
            .conn
            .query_row(
                "SELECT pinned FROM session_overrides WHERE agent = ?1 AND session_id = ?2",
                params![key.agent.as_str(), key.id],
                |r| r.get(0),
            )
            .ok();
        Ok(pinned == Some(1))
    }

    pub fn get_hidden_keys(&self) -> Result<HashSet<SessionKey>> {
        let mut stmt = self
            .conn
            .prepare("SELECT agent, session_id FROM session_overrides WHERE hidden = 1")?;
        let rows = stmt.query_map([], |r| {
            let agent_str: String = r.get(0)?;
            let agent = crate::config::AgentId::from_id(&agent_str)
                .unwrap_or(crate::config::AgentId::Claude);
            Ok(SessionKey::new(agent, r.get::<_, String>(1)?))
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn is_hidden(&self, key: &SessionKey) -> Result<bool> {
        let hidden: Option<i64> = self
            .conn
            .query_row(
                "SELECT hidden FROM session_overrides WHERE agent = ?1 AND session_id = ?2",
                params![key.agent.as_str(), key.id],
                |r| r.get(0),
            )
            .ok();
        Ok(hidden == Some(1))
    }

    pub fn clear_hidden(&self) -> Result<()> {
        self.conn.execute(
            "UPDATE session_overrides SET hidden = 0 WHERE hidden = 1",
            [],
        )?;
        Ok(())
    }
}

fn now_secs_arg() -> i64 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AgentId;
    use crate::model::{Session, SessionKey};

    fn key(id: &str) -> SessionKey {
        SessionKey::new(AgentId::Claude, id)
    }

    fn sess(id: &str) -> Session {
        Session {
            id: id.into(),
            agent: AgentId::Claude,
            title: format!("parsed {id}"),
            directory: "/w".into(),
            timestamp: 1,
            content: String::new(),
            message_count: 1,
            mtime: 1,
            yolo: false,
            source_path: None,
        }
    }

    #[test]
    fn pin_toggles_and_lists() {
        let mut s = Store::open_in_memory().unwrap();
        s.upsert_sessions(&[sess("a"), sess("b")]).unwrap();
        assert!(s.get_pinned_keys().unwrap().is_empty());

        s.set_pinned(&key("a"), true).unwrap();
        assert!(s.is_pinned(&key("a")).unwrap());
        assert!(!s.is_pinned(&key("b")).unwrap());
        assert_eq!(
            s.get_pinned_keys().unwrap(),
            [key("a")].into_iter().collect()
        );

        s.set_pinned(&key("a"), false).unwrap();
        assert!(!s.is_pinned(&key("a")).unwrap());
        assert!(s.get_pinned_keys().unwrap().is_empty());
    }

    #[test]
    fn title_override_layers_over_parsed_title() {
        let mut s = Store::open_in_memory().unwrap();
        s.upsert_sessions(&[sess("a")]).unwrap();

        assert_eq!(s.get_session(&key("a")).unwrap().unwrap().title, "parsed a");

        s.set_title_override(&key("a"), Some("My Renamed Session"))
            .unwrap();
        assert_eq!(
            s.get_session(&key("a")).unwrap().unwrap().title,
            "My Renamed Session"
        );

        s.set_title_override(&key("a"), None).unwrap();
        assert_eq!(s.get_session(&key("a")).unwrap().unwrap().title, "parsed a");
    }

    #[test]
    fn pin_and_title_coexist_on_one_row() {
        let mut s = Store::open_in_memory().unwrap();
        s.upsert_sessions(&[sess("a")]).unwrap();
        s.set_pinned(&key("a"), true).unwrap();
        s.set_title_override(&key("a"), Some("Renamed")).unwrap();
        s.set_hidden(&key("a"), true).unwrap();

        assert!(s.is_pinned(&key("a")).unwrap());
        assert!(s.is_hidden(&key("a")).unwrap());
        assert_eq!(s.get_hidden_sessions().unwrap()[0].title, "Renamed");
    }

    #[test]
    fn overrides_survive_rebuild() {
        let mut s = Store::open_in_memory().unwrap();
        s.upsert_sessions(&[sess("a")]).unwrap();
        s.set_pinned(&key("a"), true).unwrap();
        s.set_title_override(&key("a"), Some("Kept")).unwrap();
        s.set_hidden(&key("a"), true).unwrap();

        s.clear_sessions().unwrap();
        assert!(s.is_pinned(&key("a")).unwrap());
        assert!(s.is_hidden(&key("a")).unwrap());

        s.upsert_sessions(&[sess("a")]).unwrap();
        assert!(s.get_session(&key("a")).unwrap().is_none());
        assert_eq!(s.get_hidden_sessions().unwrap()[0].title, "Kept");
    }

    #[test]
    fn overrides_are_scoped_by_agent() {
        let mut s = Store::open_in_memory().unwrap();
        let mut codex = sess("same");
        codex.agent = AgentId::Codex;
        s.upsert_sessions(&[sess("same"), codex]).unwrap();

        s.set_title_override(&key("same"), Some("Claude Only"))
            .unwrap();

        assert_eq!(
            s.get_session(&key("same")).unwrap().unwrap().title,
            "Claude Only"
        );
        assert_eq!(
            s.get_session(&SessionKey::new(AgentId::Codex, "same"))
                .unwrap()
                .unwrap()
                .title,
            "parsed same"
        );
    }

    #[test]
    fn hidden_is_scoped_by_agent_and_clearable() {
        let mut s = Store::open_in_memory().unwrap();
        let mut codex = sess("same");
        codex.agent = AgentId::Codex;
        s.upsert_sessions(&[sess("same"), codex]).unwrap();

        s.set_hidden(&key("same"), true).unwrap();
        assert!(s.is_hidden(&key("same")).unwrap());
        assert!(!s
            .is_hidden(&SessionKey::new(AgentId::Codex, "same"))
            .unwrap());
        assert_eq!(
            s.get_hidden_keys().unwrap(),
            [key("same")].into_iter().collect()
        );
        assert_eq!(s.get_all_sessions().unwrap().len(), 1);

        s.set_hidden(&key("same"), false).unwrap();
        assert!(!s.is_hidden(&key("same")).unwrap());
        assert_eq!(s.get_all_sessions().unwrap().len(), 2);

        s.set_hidden(&key("same"), true).unwrap();
        s.clear_hidden().unwrap();
        assert!(s.get_hidden_keys().unwrap().is_empty());
    }
}

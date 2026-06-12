use anyhow::Result;
use rusqlite::{params, OptionalExtension};

use super::Store;
use crate::config::AgentId;
use crate::model::{ForkLink, ForkMode};

fn row_to_fork_link(row: &rusqlite::Row) -> rusqlite::Result<ForkLink> {
    let parent_agent_str: String = row.get("parent_agent")?;
    let fork_agent_str: String = row.get("fork_agent")?;
    let mode_str: String = row.get("mode")?;
    Ok(ForkLink {
        fork_session_id: row.get("fork_session_id")?,
        parent_session_id: row.get("parent_session_id")?,
        parent_agent: AgentId::from_id(&parent_agent_str).unwrap_or(AgentId::Claude),
        fork_agent: AgentId::from_id(&fork_agent_str).unwrap_or(AgentId::Claude),
        mode: mode_str.parse().unwrap_or(ForkMode::Native),
        directory: row.get("directory")?,
        created: row.get("created")?,
    })
}

impl Store {
    pub fn insert_fork_link(&self, link: &ForkLink) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO fork_links
                (fork_session_id, parent_session_id, parent_agent, fork_agent, mode, directory, created)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                link.fork_session_id,
                link.parent_session_id,
                link.parent_agent.as_str(),
                link.fork_agent.as_str(),
                link.mode.as_str(),
                link.directory,
                link.created,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn set_fork_session_id(&self, rowid: i64, fork_session_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE fork_links SET fork_session_id = ?1 WHERE id = ?2",
            params![fork_session_id, rowid],
        )?;
        Ok(())
    }

    pub fn list_fork_links(&self) -> Result<Vec<ForkLink>> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM fork_links ORDER BY created DESC, id DESC")?;
        let rows = stmt.query_map([], row_to_fork_link)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn reconcile_fork_links(&self) -> Result<usize> {
        let pending: Vec<(i64, String, String, String, i64)> = {
            let mut stmt = self.conn.prepare(
                "SELECT id, parent_session_id, fork_agent, directory, created
                 FROM fork_links
                 WHERE fork_session_id IS NULL
                   AND mode = 'native'
                   AND parent_agent = fork_agent",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, i64>(4)?,
                ))
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };

        let mut reconciled = 0;
        for (rowid, parent, agent, dir, created) in pending {
            let candidate: Option<String> = self
                .conn
                .query_row(
                    "SELECT s.id FROM sessions s
                     WHERE s.agent = ?1 AND s.directory = ?2
                       AND s.timestamp >= ?3 AND s.id != ?4
                       AND NOT EXISTS (
                           SELECT 1 FROM fork_links f
                           WHERE f.fork_agent = s.agent
                             AND f.fork_session_id IS NOT NULL
                             AND f.fork_session_id = s.id
                       )
                     ORDER BY s.timestamp DESC, s.id DESC
                     LIMIT 1",
                    params![agent, dir, created, parent],
                    |r| r.get::<_, String>(0),
                )
                .optional()?;

            if let Some(new_id) = candidate {
                self.set_fork_session_id(rowid, &new_id)?;
                reconciled += 1;
            }
        }
        Ok(reconciled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn native_link(
        agent: AgentId,
        parent: &str,
        fork: Option<&str>,
        dir: &str,
        created: i64,
    ) -> ForkLink {
        ForkLink {
            fork_session_id: fork.map(str::to_string),
            parent_session_id: parent.into(),
            parent_agent: agent,
            fork_agent: agent,
            mode: ForkMode::Native,
            directory: dir.into(),
            created,
        }
    }

    #[test]
    fn fork_link_insert_and_lazy_reconcile() {
        let s = Store::open_in_memory().unwrap();
        let link = native_link(AgentId::Codex, "parent-1", None, "/work", 7);
        let rowid = s.insert_fork_link(&link).unwrap();

        assert_eq!(s.list_fork_links().unwrap()[0].fork_session_id, None);

        s.set_fork_session_id(rowid, "child-9").unwrap();
        assert_eq!(
            s.list_fork_links().unwrap()[0].fork_session_id,
            Some("child-9".to_string())
        );
    }

    fn sess(id: &str, agent: AgentId, dir: &str, ts: i64) -> crate::model::Session {
        crate::model::Session {
            id: id.into(),
            agent,
            title: id.into(),
            directory: dir.into(),
            timestamp: ts,
            content: String::new(),
            message_count: 1,
            mtime: ts,
            yolo: false,
            source_path: None,
        }
    }

    #[test]
    fn reconcile_matches_newest_same_agent_dir_session() {
        let mut s = Store::open_in_memory().unwrap();

        s.insert_fork_link(&native_link(AgentId::Codex, "parent-1", None, "/work", 100))
            .unwrap();

        s.upsert_sessions(&[
            sess("parent-1", AgentId::Codex, "/work", 50),
            sess("child-new", AgentId::Codex, "/work", 150),
            sess("other", AgentId::Claude, "/work", 160),
        ])
        .unwrap();

        let n = s.reconcile_fork_links().unwrap();
        assert_eq!(n, 1);
        assert_eq!(
            s.list_fork_links().unwrap()[0].fork_session_id,
            Some("child-new".to_string()),
            "reconcile picks the newest same-agent+dir session after the link, not the parent"
        );

        assert_eq!(s.reconcile_fork_links().unwrap(), 0);
    }

    #[test]
    fn reconcile_leaves_link_pending_when_no_candidate() {
        let mut s = Store::open_in_memory().unwrap();
        s.insert_fork_link(&native_link(AgentId::Codex, "parent-1", None, "/work", 100))
            .unwrap();

        s.upsert_sessions(&[sess("parent-1", AgentId::Codex, "/work", 50)])
            .unwrap();
        assert_eq!(s.reconcile_fork_links().unwrap(), 0);
        assert_eq!(s.list_fork_links().unwrap()[0].fork_session_id, None);
    }

    #[test]
    fn reconcile_claimed_ids_are_scoped_by_agent() {
        let mut s = Store::open_in_memory().unwrap();

        s.insert_fork_link(&native_link(
            AgentId::Claude,
            "claude-parent",
            Some("child"),
            "/work",
            90,
        ))
        .unwrap();
        s.insert_fork_link(&native_link(
            AgentId::Codex,
            "codex-parent",
            None,
            "/work",
            100,
        ))
        .unwrap();
        s.upsert_sessions(&[
            sess("codex-parent", AgentId::Codex, "/work", 50),
            sess("child", AgentId::Codex, "/work", 150),
        ])
        .unwrap();

        assert_eq!(s.reconcile_fork_links().unwrap(), 1);
        assert!(s
            .list_fork_links()
            .unwrap()
            .iter()
            .any(|link| link.fork_agent == AgentId::Codex
                && link.fork_session_id == Some("child".into())));
    }

    #[test]
    fn reconcile_ignores_translated_links() {
        let mut s = Store::open_in_memory().unwrap();
        s.insert_fork_link(&ForkLink {
            fork_session_id: None,
            parent_session_id: "parent-1".into(),
            parent_agent: AgentId::Claude,
            fork_agent: AgentId::Codex,
            mode: ForkMode::Translated,
            directory: "/work".into(),
            created: 100,
        })
        .unwrap();
        s.upsert_sessions(&[sess("child", AgentId::Codex, "/work", 150)])
            .unwrap();

        assert_eq!(s.reconcile_fork_links().unwrap(), 0);
        assert_eq!(s.list_fork_links().unwrap()[0].fork_session_id, None);
    }
}

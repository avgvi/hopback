use std::collections::HashMap;

use anyhow::Result;
use rusqlite::params;

use super::Store;
use crate::config::AgentId;
use crate::model::Session;

impl Store {
    pub fn get_dir_mtimes(&self, agent: AgentId) -> Result<HashMap<String, i64>> {
        let mut stmt = self
            .conn
            .prepare("SELECT directory, mtime FROM dir_mtimes WHERE agent = ?1")?;
        let rows = stmt.query_map(params![agent.as_str()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        let mut out = HashMap::new();
        for r in rows {
            let (dir, mtime) = r?;
            out.insert(dir, mtime);
        }
        Ok(out)
    }

    pub fn apply_pass(
        &mut self,
        agent: AgentId,
        upserts: &[Session],
        deleted: &[String],
        dir_agent: Option<AgentId>,
        dir_mtimes: &[(String, i64)],
        removed_dirs: &[String],
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        for id in deleted {
            super::sessions::delete_one(&tx, agent, id)?;
        }
        for s in upserts {
            super::sessions::upsert_one(&tx, s)?;
        }
        if let Some(agent) = dir_agent {
            for (dir, mtime) in dir_mtimes {
                tx.execute(
                    "INSERT INTO dir_mtimes (agent, directory, mtime) VALUES (?1, ?2, ?3)
                     ON CONFLICT(agent, directory) DO UPDATE SET mtime = excluded.mtime",
                    params![agent.as_str(), dir, mtime],
                )?;
            }
            for dir in removed_dirs {
                tx.execute(
                    "DELETE FROM dir_mtimes WHERE agent = ?1 AND directory = ?2",
                    params![agent.as_str(), dir],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_mtimes_record_update_and_remove() {
        let mut s = Store::open_in_memory().unwrap();
        s.apply_pass(
            AgentId::OpenCode,
            &[],
            &[],
            Some(AgentId::OpenCode),
            &[("/a".into(), 100), ("/b".into(), 200)],
            &[],
        )
        .unwrap();
        let m = s.get_dir_mtimes(AgentId::OpenCode).unwrap();
        assert_eq!(m.get("/a"), Some(&100));
        assert_eq!(m.get("/b"), Some(&200));

        assert!(s.get_dir_mtimes(AgentId::Claude).unwrap().is_empty());

        s.apply_pass(
            AgentId::OpenCode,
            &[],
            &[],
            Some(AgentId::OpenCode),
            &[("/a".into(), 150)],
            &["/b".into()],
        )
        .unwrap();
        let m = s.get_dir_mtimes(AgentId::OpenCode).unwrap();
        assert_eq!(m.get("/a"), Some(&150));
        assert_eq!(m.get("/b"), None);
    }
}

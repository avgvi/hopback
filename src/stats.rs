use anyhow::Result;

use crate::config::AgentId;
use crate::store::Store;
use crate::tui::format::time_ago;

#[derive(Debug, Clone, PartialEq)]
pub struct AgentStat {
    pub agent: AgentId,
    pub sessions: i64,
    pub messages: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DirStat {
    pub directory: String,
    pub sessions: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Stats {
    pub total_sessions: i64,
    pub total_messages: i64,
    pub per_agent: Vec<AgentStat>,
    pub top_dirs: Vec<DirStat>,

    pub oldest: Option<i64>,
    pub newest: Option<i64>,
}

impl Store {
    pub fn compute_stats(&self, top_dirs_limit: usize) -> Result<Stats> {
        let visible_from = "FROM sessions s LEFT JOIN session_overrides o ON o.agent = s.agent AND o.session_id = s.id WHERE COALESCE(o.hidden, 0) = 0";
        let total_sessions: i64 =
            self.conn
                .query_row(&format!("SELECT COUNT(*) {visible_from}"), [], |r| r.get(0))?;
        let total_messages: i64 = self.conn.query_row(
            &format!("SELECT COALESCE(SUM(message_count), 0) {visible_from}"),
            [],
            |r| r.get(0),
        )?;

        let (oldest, newest): (Option<i64>, Option<i64>) = self.conn.query_row(
            &format!("SELECT MIN(timestamp), MAX(timestamp) {visible_from}"),
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;

        let mut per_agent = Vec::new();
        {
            let mut stmt = self.conn.prepare(
                "SELECT s.agent, COUNT(*), COALESCE(SUM(s.message_count), 0)
                 FROM sessions s
                 LEFT JOIN session_overrides o ON o.agent = s.agent AND o.session_id = s.id
                 WHERE COALESCE(o.hidden, 0) = 0
                 GROUP BY s.agent ORDER BY COUNT(*) DESC",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })?;
            for row in rows.flatten() {
                let (agent_str, sessions, messages) = row;

                if let Some(agent) = AgentId::from_id(&agent_str) {
                    per_agent.push(AgentStat {
                        agent,
                        sessions,
                        messages,
                    });
                }
            }
        }

        let mut top_dirs = Vec::new();
        {
            let mut stmt = self.conn.prepare(
                "SELECT s.directory, COUNT(*) FROM sessions s
                 LEFT JOIN session_overrides o ON o.agent = s.agent AND o.session_id = s.id
                 WHERE s.directory != '' AND COALESCE(o.hidden, 0) = 0
                 GROUP BY s.directory
                 ORDER BY COUNT(*) DESC, directory LIMIT ?1",
            )?;
            let rows = stmt.query_map([top_dirs_limit as i64], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })?;
            for row in rows.flatten() {
                top_dirs.push(DirStat {
                    directory: row.0,
                    sessions: row.1,
                });
            }
        }

        Ok(Stats {
            total_sessions,
            total_messages,
            per_agent,
            top_dirs,
            oldest,
            newest,
        })
    }
}

pub fn render(stats: &Stats, now_secs: i64, home: &str) -> String {
    use std::fmt::Write;
    let mut out = String::new();

    let _ = writeln!(out, "hopback — statistics\n");
    let avg = if stats.total_sessions > 0 {
        stats.total_messages as f64 / stats.total_sessions as f64
    } else {
        0.0
    };
    let _ = writeln!(
        out,
        "{} sessions · {} messages · {:.1} msg/session",
        stats.total_sessions, stats.total_messages, avg
    );

    if let (Some(oldest), Some(newest)) = (stats.oldest, stats.newest) {
        let _ = writeln!(
            out,
            "span: {} → {}",
            time_ago(now_secs, oldest),
            time_ago(now_secs, newest)
        );
    }

    if !stats.per_agent.is_empty() {
        let _ = writeln!(out, "\nby agent:");
        let w = stats
            .per_agent
            .iter()
            .map(|a| a.agent.badge().len())
            .max()
            .unwrap_or(0);
        for a in &stats.per_agent {
            let _ = writeln!(
                out,
                "  {:<width$}  {:>5} sessions  {:>7} messages",
                a.agent.badge(),
                a.sessions,
                a.messages,
                width = w
            );
        }
    }

    if !stats.top_dirs.is_empty() {
        let _ = writeln!(out, "\ntop directories:");
        for d in &stats.top_dirs {
            let shown = collapse_home(&d.directory, home);
            let _ = writeln!(out, "  {:>4}  {}", d.sessions, shown);
        }
    }

    out
}

fn collapse_home(path: &str, home: &str) -> String {
    let home = home.strip_suffix('/').unwrap_or(home);
    if home.is_empty() {
        return path.to_string();
    }
    if path == home {
        return "~".to_string();
    }
    if let Some(rest) = path.strip_prefix(home) {
        if rest.starts_with('/') {
            return format!("~{rest}");
        }
    }
    path.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Session;

    fn sess(id: &str, agent: AgentId, dir: &str, ts: i64, msgs: u32) -> Session {
        Session {
            id: id.into(),
            agent,
            title: id.into(),
            directory: dir.into(),
            timestamp: ts,
            content: String::new(),
            message_count: msgs,
            mtime: ts,
            yolo: false,
            source_path: None,
        }
    }

    fn store_with(sessions: &[Session]) -> Store {
        let mut s = Store::open_in_memory().unwrap();
        s.upsert_sessions(sessions).unwrap();
        s
    }

    #[test]
    fn aggregates_totals_and_per_agent() {
        let store = store_with(&[
            sess("a", AgentId::Claude, "/work/api", 100, 4),
            sess("b", AgentId::Claude, "/work/api", 200, 6),
            sess("c", AgentId::Codex, "/work/web", 300, 2),
        ]);
        let stats = store.compute_stats(10).unwrap();
        assert_eq!(stats.total_sessions, 3);
        assert_eq!(stats.total_messages, 12);

        assert_eq!(stats.per_agent[0].agent, AgentId::Claude);
        assert_eq!(stats.per_agent[0].sessions, 2);
        assert_eq!(stats.per_agent[0].messages, 10);
        assert_eq!(stats.per_agent[1].agent, AgentId::Codex);
        assert_eq!(stats.oldest, Some(100));
        assert_eq!(stats.newest, Some(300));
    }

    #[test]
    fn top_dirs_ranked_and_limited() {
        let store = store_with(&[
            sess("a", AgentId::Claude, "/work/api", 1, 1),
            sess("b", AgentId::Claude, "/work/api", 2, 1),
            sess("c", AgentId::Codex, "/work/web", 3, 1),
            sess("d", AgentId::Codex, "", 4, 1),
        ]);
        let stats = store.compute_stats(1).unwrap();
        assert_eq!(stats.top_dirs.len(), 1);
        assert_eq!(stats.top_dirs[0].directory, "/work/api");
        assert_eq!(stats.top_dirs[0].sessions, 2);
    }

    #[test]
    fn empty_store_is_safe() {
        let store = store_with(&[]);
        let stats = store.compute_stats(10).unwrap();
        assert_eq!(stats.total_sessions, 0);
        assert_eq!(stats.total_messages, 0);
        assert!(stats.per_agent.is_empty());
        assert_eq!(stats.oldest, None);

        let text = render(&stats, 1000, "/home/u");
        assert!(text.contains("0 sessions"));
    }

    #[test]
    fn render_collapses_home_and_lists_agents() {
        let store = store_with(&[sess("a", AgentId::Claude, "/home/u/proj", 100, 3)]);
        let stats = store.compute_stats(10).unwrap();
        let text = render(&stats, 1000, "/home/u");
        assert!(text.contains("1 sessions"));
        assert!(text.contains("claude"));
        assert!(text.contains("~/proj"));
    }

    #[test]
    fn stats_exclude_hidden_sessions() {
        let store = store_with(&[
            sess("a", AgentId::Claude, "/work/api", 100, 4),
            sess("b", AgentId::Codex, "/work/web", 200, 6),
        ]);
        store
            .set_hidden(&crate::model::SessionKey::new(AgentId::Claude, "a"), true)
            .unwrap();

        let stats = store.compute_stats(10).unwrap();
        assert_eq!(stats.total_sessions, 1);
        assert_eq!(stats.total_messages, 6);
        assert_eq!(stats.per_agent.len(), 1);
        assert_eq!(stats.per_agent[0].agent, AgentId::Codex);
        assert_eq!(stats.top_dirs[0].directory, "/work/web");
    }

    #[test]
    fn collapse_home_only_on_boundary() {
        assert_eq!(collapse_home("/home/u/proj", "/home/u"), "~/proj");
        assert_eq!(collapse_home("/home/u", "/home/u"), "~");

        assert_eq!(collapse_home("/home/username", "/home/u"), "/home/username");
    }
}

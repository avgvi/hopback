use anyhow::Result;
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use rusqlite::params;

use crate::config::AgentId;
use crate::model::{Session, SessionKey};
use crate::store::Store;

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub session: Session,
    pub score: f64,
}

#[derive(Debug, Clone, Default)]
pub struct Filters {
    pub agents_include: Vec<AgentId>,
    pub agents_exclude: Vec<AgentId>,
    pub dirs_include: Vec<String>,
    pub dirs_exclude: Vec<String>,

    pub agents_include_unmatched: bool,

    pub date_after: Option<i64>,

    pub date_before: Option<i64>,

    pub date_negated: bool,
}

impl Filters {
    pub fn matches_pub(&self, s: &Session) -> bool {
        self.matches(s)
    }

    pub fn merge_agent_include(&mut self, agent: AgentId) {
        self.agents_include_unmatched = false;
        if !self.agents_include.contains(&agent) {
            self.agents_include.push(agent);
        }
    }

    pub fn merge_dir_include(&mut self, dir: String) {
        if !self.dirs_include.contains(&dir) {
            self.dirs_include.push(dir);
        }
    }

    fn matches(&self, s: &Session) -> bool {
        if self.agents_exclude.contains(&s.agent) {
            return false;
        }
        if self.agents_include_unmatched {
            return false;
        }
        if !self.agents_include.is_empty() && !self.agents_include.contains(&s.agent) {
            return false;
        }

        let dir_lower = s.directory.to_lowercase();
        if self
            .dirs_exclude
            .iter()
            .any(|d| dir_lower.contains(&d.to_lowercase()))
        {
            return false;
        }
        if !self.dirs_include.is_empty()
            && !self
                .dirs_include
                .iter()
                .any(|d| dir_lower.contains(&d.to_lowercase()))
        {
            return false;
        }

        if self.date_after.is_some() || self.date_before.is_some() {
            let in_range = self.date_after.is_none_or(|a| s.timestamp >= a)
                && self.date_before.is_none_or(|b| s.timestamp < b);
            if in_range == self.date_negated {
                return false;
            }
        }

        true
    }
}

const EXACT_TITLE_BONUS: f64 = 1_000_000.0;
const EXACT_CONTENT_BONUS: f64 = 10_000.0;
const FTS_BASE: f64 = 1_000.0;
const FUZZY_MAX: f64 = 500.0;

pub const DEFAULT_LIMIT: usize = 200;

pub fn search(store: &Store, query: &str, limit: usize) -> Result<Vec<SearchResult>> {
    search_filtered(store, query, &Filters::default(), limit)
}

pub fn search_filtered(
    store: &Store,
    query: &str,
    filters: &Filters,
    limit: usize,
) -> Result<Vec<SearchResult>> {
    let q = query.trim();
    let fts = if q.is_empty() {
        std::collections::HashMap::new()
    } else {
        fts_scores(store, q)?
    };
    let sessions = store.get_all_sessions()?;
    Ok(rank(&sessions, q, filters, &fts, limit))
}

/// Every match for `query`, in display order and without a limit: ranked like
/// [`search_filtered`], then with pinned sessions moved to the top as the TUI
/// shows them. Used by the paged, non-interactive listings.
pub fn search_records(
    store: &Store,
    query: &str,
    filters: &Filters,
    include_hidden: bool,
) -> Result<Vec<crate::store::sessions::SessionRecord>> {
    let q = query.trim();
    let fts = if q.is_empty() {
        std::collections::HashMap::new()
    } else {
        fts_scores(store, q)?
    };
    let mut by_key = std::collections::HashMap::new();
    let mut sessions = Vec::new();
    for record in store.list_records(include_hidden)? {
        sessions.push(record.session.clone());
        by_key.insert(record.session.key(), record);
    }
    let mut out: Vec<_> = rank(&sessions, q, filters, &fts, usize::MAX)
        .into_iter()
        .filter_map(|r| by_key.remove(&r.session.key()))
        .collect();
    out.sort_by_key(|r| !r.pinned);
    Ok(out)
}

pub fn rank(
    sessions: &[Session],
    query: &str,
    filters: &Filters,
    fts: &std::collections::HashMap<SessionKey, f64>,
    limit: usize,
) -> Vec<SearchResult> {
    if query.is_empty() {
        let mut listed: Vec<SearchResult> = sessions
            .iter()
            .filter(|s| filters.matches(s))
            .map(|s| SearchResult {
                session: s.clone(),
                score: 0.0,
            })
            .collect();
        listed.sort_by_key(|r| std::cmp::Reverse(r.session.timestamp));
        listed.truncate(limit);
        return listed;
    }

    let mut matcher = Matcher::new(Config::DEFAULT);
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
    let q_lower = query.to_lowercase();

    let mut fuzzy_raw: Vec<(usize, f64)> = Vec::new();
    let mut max_fuzzy = 0.0_f64;
    let mut buf: Vec<char> = Vec::new();
    for (i, s) in sessions.iter().enumerate() {
        if !filters.matches(s) {
            continue;
        }
        let haystack_str = format!("{}\n{}", s.title, s.directory);
        let haystack = Utf32Str::new(&haystack_str, &mut buf);
        if let Some(score) = pattern.score(haystack, &mut matcher) {
            let score = score as f64;
            if score > max_fuzzy {
                max_fuzzy = score;
            }
            fuzzy_raw.push((i, score));
        }
    }

    let mut results: Vec<SearchResult> = Vec::new();
    for (i, s) in sessions.iter().enumerate() {
        if !filters.matches(s) {
            continue;
        }
        let mut score = 0.0;
        let mut is_candidate = false;

        if let Some(&bm25) = fts.get(&s.key()) {
            score += FTS_BASE + bm25;
            is_candidate = true;
        }

        if let Some(&(_, raw)) = fuzzy_raw.iter().find(|(idx, _)| *idx == i) {
            let norm = if max_fuzzy > 0.0 {
                raw / max_fuzzy
            } else {
                0.0
            };
            score += norm * FUZZY_MAX;
            is_candidate = true;
        }

        if contains_word_bounded(&s.title.to_lowercase(), &q_lower) {
            score += EXACT_TITLE_BONUS;
            is_candidate = true;
        } else if contains_word_bounded(&s.content.to_lowercase(), &q_lower) {
            score += EXACT_CONTENT_BONUS;
            is_candidate = true;
        }

        if is_candidate {
            results.push(SearchResult {
                session: s.clone(),
                score,
            });
        }
    }

    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.session.timestamp.cmp(&a.session.timestamp))
    });
    results.truncate(limit);
    results
}

pub fn first_match_offset(content: &str, query: &str) -> Option<usize> {
    let terms: Vec<String> = query
        .split_whitespace()
        .map(|t| t.to_lowercase())
        .filter(|t| !t.is_empty())
        .collect();
    if terms.is_empty() {
        return None;
    }

    let max_term = terms.iter().map(|t| t.len()).max().unwrap_or(0);
    let window = max_term.saturating_mul(3) + 4;
    for (off, _) in content.char_indices() {
        let end = floor_boundary(content, (off + window).min(content.len()));
        let chunk_lower = content[off..end].to_lowercase();
        if terms.iter().any(|t| chunk_lower.starts_with(t.as_str())) {
            return Some(off);
        }
    }
    None
}

fn floor_boundary(s: &str, idx: usize) -> usize {
    if idx >= s.len() {
        return s.len();
    }
    let mut i = idx;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn fts_scores(store: &Store, query: &str) -> Result<std::collections::HashMap<SessionKey, f64>> {
    let mut out = std::collections::HashMap::new();
    let match_query = build_fts_query(query);
    if match_query.is_empty() {
        return Ok(out);
    }

    let mut stmt = store.conn.prepare(
        "SELECT agent, id, bm25(sessions_fts) AS rank
         FROM sessions_fts
         WHERE sessions_fts MATCH ?1
         ORDER BY rank
         LIMIT 1000",
    )?;
    let rows = stmt.query_map(params![match_query], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, f64>(2)?,
        ))
    });
    let rows = match rows {
        Ok(r) => r,
        Err(_) => return Ok(out),
    };
    for r in rows.flatten() {
        let (agent_str, id, bm25) = r;
        let Some(agent) = AgentId::from_id(&agent_str) else {
            continue;
        };

        let positive = (-bm25).max(0.0);
        let bounded = positive / (1.0 + positive) * (FTS_BASE - 1.0);
        out.insert(SessionKey::new(agent, id), bounded);
    }
    Ok(out)
}

fn contains_word_bounded(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    if !needle.chars().any(|c| c.is_alphanumeric()) {
        return haystack.contains(needle);
    }
    let hb = haystack.as_bytes();
    let mut from = 0;
    while let Some(rel) = haystack[from..].find(needle) {
        let start = from + rel;
        let end = start + needle.len();
        let before_ok = start == 0 || !(hb[start - 1] as char).is_alphanumeric();
        let after_ok = end >= hb.len() || !(hb[end] as char).is_alphanumeric();
        if before_ok && after_ok {
            return true;
        }
        from = start + 1;
    }
    false
}

fn build_fts_query(query: &str) -> String {
    let terms: Vec<String> = query
        .split_whitespace()
        .map(|t| {
            t.chars()
                .filter(|c| c.is_alphanumeric())
                .collect::<String>()
                .to_lowercase()
        })
        .filter(|t| !t.is_empty())
        .map(|t| format!("{t}*"))
        .collect();
    terms.join(" OR ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AgentId;

    fn sess(id: &str, title: &str, content: &str, ts: i64) -> Session {
        Session {
            id: id.into(),
            agent: AgentId::Claude,
            title: title.into(),
            directory: "/work/proj".into(),
            timestamp: ts,
            content: content.into(),
            message_count: 1,
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
    fn empty_query_lists_newest_first() {
        let store = store_with(&[
            sess("a", "older", "x", 100),
            sess("b", "newer", "y", 300),
            sess("c", "middle", "z", 200),
        ]);
        let r = search(&store, "", 10).unwrap();
        let ids: Vec<_> = r.iter().map(|x| x.session.id.clone()).collect();
        assert_eq!(ids, vec!["b", "c", "a"]);
    }

    #[test]
    fn empty_query_respects_limit() {
        let store = store_with(&[
            sess("a", "t", "x", 1),
            sess("b", "t", "y", 2),
            sess("c", "t", "z", 3),
        ]);
        assert_eq!(search(&store, "   ", 2).unwrap().len(), 2);
    }

    #[test]
    fn fuzzy_finds_typo() {
        let store = store_with(&[
            sess(
                "hit",
                "authentication middleware",
                "we built the auth layer",
                10,
            ),
            sess("miss", "database schema", "tables and columns", 20),
        ]);
        let r = search(&store, "auth midleware", 10).unwrap();
        assert!(!r.is_empty());
        assert_eq!(
            r[0].session.id, "hit",
            "typo'd query should surface the auth session first"
        );
    }

    #[test]
    fn exact_phrase_outranks_pure_fuzzy() {
        let store = store_with(&[
            sess("fuzzy", "authentcation servce", "stuff", 100),
            sess("exact", "authentication service", "stuff", 10),
        ]);
        let r = search(&store, "authentication service", 10).unwrap();
        assert_eq!(r[0].session.id, "exact");
    }

    #[test]
    fn exact_token_found_among_many_fuzzy() {
        let mut sessions = vec![sess("exact", "deploy 110 release", "notes", 50)];
        for (i, n) in ["10", "100", "1100", "11", "1010", "0110"]
            .iter()
            .enumerate()
        {
            sessions.push(sess(
                &format!("f{i}"),
                &format!("build {n} pipeline"),
                "notes",
                100 + i as i64,
            ));
        }
        let store = store_with(&sessions);
        let r = search(&store, "110", 10).unwrap();
        assert_eq!(r[0].session.id, "exact");
    }

    #[test]
    fn content_match_surfaces_session() {
        let store = store_with(&[
            sess(
                "a",
                "random title",
                "we discussed kubernetes autoscaling at length",
                10,
            ),
            sess("b", "another title", "totally unrelated text", 20),
        ]);
        let r = search(&store, "kubernetes", 10).unwrap();
        assert_eq!(r[0].session.id, "a");
    }

    #[test]
    fn fts_distinguishes_same_raw_id_across_agents() {
        let mut claude = sess("same", "claude title", "sharedneedle from claude", 10);
        claude.agent = AgentId::Claude;
        let mut codex = sess("same", "codex title", "sharedneedle from codex", 20);
        codex.agent = AgentId::Codex;
        let store = store_with(&[claude, codex]);

        let r = search(&store, "sharedneedle", 10).unwrap();
        let keys: std::collections::HashSet<_> = r.into_iter().map(|x| x.session.key()).collect();
        assert!(keys.contains(&SessionKey::new(AgentId::Claude, "same")));
        assert!(keys.contains(&SessionKey::new(AgentId::Codex, "same")));
    }

    #[test]
    fn malformed_query_does_not_crash() {
        let store = store_with(&[sess("a", "hello world", "x", 10)]);

        for q in ["\"", "*", "(", "AND OR NOT", "a:b", "()\""] {
            let _ = search(&store, q, 10).unwrap();
        }
    }

    #[test]
    fn no_match_returns_empty() {
        let store = store_with(&[sess("a", "hello world", "foo bar", 10)]);
        let r = search(&store, "zzzzqqqxxx", 10).unwrap();
        assert!(r.is_empty());
    }

    fn sess_at(id: &str, agent: AgentId, dir: &str, ts: i64) -> Session {
        Session {
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
    fn filter_default_matches_everything() {
        let f = Filters::default();
        assert!(f.matches(&sess_at("a", AgentId::Claude, "/w", 10)));
    }

    #[test]
    fn filter_agent_include_is_or() {
        let f = Filters {
            agents_include: vec![AgentId::Claude, AgentId::Codex],
            ..Default::default()
        };
        assert!(f.matches(&sess_at("a", AgentId::Claude, "/w", 1)));
        assert!(f.matches(&sess_at("b", AgentId::Codex, "/w", 1)));
        assert!(!f.matches(&sess_at("c", AgentId::Vibe, "/w", 1)));
    }

    #[test]
    fn filter_unmatched_agent_include_matches_nothing() {
        let f = Filters {
            agents_include_unmatched: true,
            ..Default::default()
        };
        assert!(!f.matches(&sess_at("a", AgentId::Claude, "/w", 1)));
    }

    #[test]
    fn filter_exclude_wins_over_include() {
        let f = Filters {
            agents_include: vec![AgentId::Claude],
            agents_exclude: vec![AgentId::Claude],
            ..Default::default()
        };
        assert!(!f.matches(&sess_at("a", AgentId::Claude, "/w", 1)));
    }

    #[test]
    fn filter_dir_substring_case_insensitive() {
        let f = Filters {
            dirs_include: vec!["Project".into()],
            ..Default::default()
        };
        assert!(f.matches(&sess_at("a", AgentId::Claude, "/home/u/myproject/src", 1)));
        assert!(!f.matches(&sess_at("b", AgentId::Claude, "/home/u/other", 1)));

        let g = Filters {
            dirs_exclude: vec!["test".into()],
            ..Default::default()
        };
        assert!(!g.matches(&sess_at("c", AgentId::Claude, "/repo/tests/x", 1)));
        assert!(g.matches(&sess_at("d", AgentId::Claude, "/repo/src/x", 1)));
    }

    #[test]
    fn filter_date_window_inclusive_lower_exclusive_upper() {
        let f = Filters {
            date_after: Some(100),
            date_before: Some(200),
            ..Default::default()
        };
        assert!(f.matches(&sess_at("a", AgentId::Claude, "/w", 100)));
        assert!(f.matches(&sess_at("b", AgentId::Claude, "/w", 199)));
        assert!(!f.matches(&sess_at("c", AgentId::Claude, "/w", 200)));
        assert!(!f.matches(&sess_at("d", AgentId::Claude, "/w", 99)));
    }

    #[test]
    fn filter_date_negated_inverts_window() {
        let f = Filters {
            date_after: Some(100),
            date_before: Some(200),
            date_negated: true,
            ..Default::default()
        };
        assert!(!f.matches(&sess_at("a", AgentId::Claude, "/w", 150)));
        assert!(f.matches(&sess_at("b", AgentId::Claude, "/w", 50)));
        assert!(f.matches(&sess_at("c", AgentId::Claude, "/w", 300)));
    }

    #[test]
    fn filter_date_after_only_is_open_ended() {
        let f = Filters {
            date_after: Some(100),
            ..Default::default()
        };
        assert!(f.matches(&sess_at("a", AgentId::Claude, "/w", 100)));
        assert!(f.matches(&sess_at("b", AgentId::Claude, "/w", 9999)));
        assert!(!f.matches(&sess_at("c", AgentId::Claude, "/w", 99)));
    }

    #[test]
    fn first_match_offset_is_case_insensitive_and_in_original_bytes() {
        assert_eq!(first_match_offset("Hello World", "world"), Some(6));
        assert_eq!(first_match_offset("Hello World", "WORLD"), Some(6));
        assert_eq!(first_match_offset("nothing here", "absent"), None);
        assert_eq!(first_match_offset("anything", ""), None);
    }

    #[test]
    fn first_match_offset_multibyte_offset_indexes_original() {
        let content = "İİİ match here";
        let off = first_match_offset(content, "match").unwrap();
        assert!(content.is_char_boundary(off));
        assert!(content[off..].starts_with("match"));
    }
}

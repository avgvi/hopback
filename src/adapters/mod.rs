use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::config::AgentId;
use crate::model::{Session, SessionKey};

pub type KnownSessions = HashMap<SessionKey, i64>;

pub struct IncrementalResult {
    pub upserts: Vec<Session>,
    pub deleted: Vec<String>,
}

pub trait Adapter: Send + Sync {
    fn id(&self) -> AgentId;

    fn badge(&self) -> &str {
        self.id().badge()
    }

    fn supports_yolo(&self) -> bool {
        false
    }

    fn detects_yolo(&self) -> bool {
        false
    }

    fn supports_fork(&self) -> bool {
        false
    }

    /// True when each session has its own file, so `Session::mtime` says when
    /// that session was last written. Adapters that read every session from
    /// one shared database return false.
    fn mtime_is_per_session(&self) -> bool {
        true
    }

    fn is_available(&self) -> bool;

    fn scan(&self) -> Result<HashMap<String, (PathBuf, i64)>>;

    fn uses_dir_mtime_fastpath(&self) -> bool {
        false
    }

    fn scan_dirs(&self) -> Result<HashMap<String, i64>> {
        Ok(HashMap::new())
    }

    fn parse(&self, path: &Path) -> Result<Option<Session>>;

    fn resume_command(&self, session: &Session, yolo: bool) -> Vec<String>;

    fn fork_command(
        &self,
        _seed: &Session,
        _new_id: Option<&str>,
        _name: Option<&str>,
        _yolo: bool,
    ) -> Option<Vec<String>> {
        None
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

        let current = self.scan()?;
        let mut upserts = Vec::new();

        for (id, (path, mtime)) in &current {
            let key = SessionKey::new(my_agent, id.clone());
            let changed = match known.get(&key) {
                None => true,
                Some(known_mtime) => *mtime > *known_mtime,
            };
            if !changed {
                continue;
            }

            match self.parse(path) {
                Ok(Some(mut session)) => {
                    session.mtime = *mtime;
                    on_session(&session);
                    upserts.push(session);
                }
                Ok(None) => {}
                Err(_) => {}
            }
        }

        let deleted = known
            .iter()
            .filter(|(key, _)| key.agent == my_agent && !current.contains_key(&key.id))
            .map(|(key, _)| key.id.clone())
            .collect();

        Ok(IncrementalResult { upserts, deleted })
    }
}

pub mod antigravity;
pub mod claude;
pub mod codex;
pub mod copilot;
pub mod copilot_vscode;
pub mod crush;
pub mod cursor;
pub mod gemini;
pub mod goose;
pub mod grok;
pub mod kimi;
pub mod kiro;
pub mod opencode;
pub mod pi;
pub mod registry;
pub mod vibe;

pub fn mtime_nanos(path: &Path) -> Result<i64> {
    let meta = std::fs::metadata(path)?;
    let modified = meta.modified()?;
    let dur = modified
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| anyhow::anyhow!("mtime before epoch: {e}"))?;
    Ok(dur.as_nanos() as i64)
}

pub fn nanos_to_secs(nanos: i64) -> i64 {
    nanos / 1_000_000_000
}

pub fn parse_iso8601_secs(s: &str) -> Option<i64> {
    let b = s.as_bytes();

    if b.len() < 19 {
        return None;
    }

    if b[4] != b'-' || b[7] != b'-' || b[13] != b':' || b[16] != b':' {
        return None;
    }
    if b[10] != b'T' && b[10] != b' ' {
        return None;
    }
    let year = digits(&b[0..4])?;
    let month = digits(&b[5..7])?;
    let day = digits(&b[8..10])?;
    let hour = digits(&b[11..13])?;
    let min = digits(&b[14..16])?;
    let sec = digits(&b[17..19])?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || min > 59 || sec > 60 {
        return None;
    }

    let mut i = 19usize;
    if i < b.len() && b[i] == b'.' {
        i += 1;
        let start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == start {
            return None;
        }
    }

    let offset_secs: i64 = if i >= b.len() {
        0
    } else {
        match b[i] {
            b'Z' | b'z' => {
                i += 1;
                0
            }
            b'+' | b'-' => {
                let sign = if b[i] == b'-' { -1 } else { 1 };
                i += 1;

                if i + 2 > b.len() {
                    return None;
                }
                let oh = digits(&b[i..i + 2])?;
                i += 2;
                if i < b.len() && b[i] == b':' {
                    i += 1;
                }
                let om = if i + 2 <= b.len() && b[i].is_ascii_digit() {
                    let v = digits(&b[i..i + 2])?;
                    i += 2;
                    v
                } else {
                    0
                };
                if oh > 23 || om > 59 {
                    return None;
                }
                sign * (oh * 3600 + om * 60)
            }
            _ => return None,
        }
    };

    if i != b.len() {
        return None;
    }

    let days = days_from_civil(year, month, day);
    let sec = sec.min(59);
    let local = days * 86_400 + hour * 3600 + min * 60 + sec;
    Some(local - offset_secs)
}

fn digits(b: &[u8]) -> Option<i64> {
    if b.is_empty() {
        return None;
    }
    let mut n: i64 = 0;
    for &c in b {
        if !c.is_ascii_digit() {
            return None;
        }
        n = n * 10 + (c - b'0') as i64;
    }
    Some(n)
}

fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Session;

    struct StubAdapter {
        available: bool,
        sessions: HashMap<String, (PathBuf, i64, Session)>,
    }

    fn sess(id: &str, mtime: i64) -> Session {
        Session {
            id: id.to_string(),
            agent: AgentId::Claude,
            title: format!("title {id}"),
            directory: "/tmp".into(),
            timestamp: 0,
            content: String::new(),
            message_count: 1,
            mtime,
            yolo: false,
            source_path: None,
        }
    }

    impl Adapter for StubAdapter {
        fn id(&self) -> AgentId {
            AgentId::Claude
        }
        fn is_available(&self) -> bool {
            self.available
        }
        fn scan(&self) -> Result<HashMap<String, (PathBuf, i64)>> {
            Ok(self
                .sessions
                .iter()
                .map(|(id, (p, m, _))| (id.clone(), (p.clone(), *m)))
                .collect())
        }
        fn parse(&self, path: &Path) -> Result<Option<Session>> {
            Ok(self
                .sessions
                .values()
                .find(|(p, _, _)| p == path)
                .map(|(_, _, s)| s.clone()))
        }
        fn resume_command(&self, session: &Session, _yolo: bool) -> Vec<String> {
            vec!["stub".into(), session.id.clone()]
        }
    }

    fn stub_with(ids: &[(&str, i64)], available: bool) -> StubAdapter {
        let sessions = ids
            .iter()
            .map(|(id, m)| {
                (
                    id.to_string(),
                    (PathBuf::from(format!("/files/{id}")), *m, sess(id, *m)),
                )
            })
            .collect();
        StubAdapter {
            available,
            sessions,
        }
    }

    #[test]
    fn stub_adapter_registers_and_resumes() {
        let a = stub_with(&[("x", 1)], true);
        assert_eq!(a.id(), AgentId::Claude);
        assert!(!a.supports_fork());
        assert!(a.fork_command(&sess("x", 1), None, None, false).is_none());
        assert_eq!(a.resume_command(&sess("x", 1), false), vec!["stub", "x"]);
    }

    #[test]
    fn incremental_parses_only_new_and_changed() {
        let a = stub_with(&[("a", 100), ("b", 200), ("c", 50)], true);

        let known: KnownSessions = [
            (SessionKey::new(AgentId::Claude, "a"), 100),
            (SessionKey::new(AgentId::Claude, "b"), 150),
        ]
        .into_iter()
        .collect();

        let mut streamed = Vec::new();
        let res = a
            .find_incremental(&known, &mut |s| streamed.push(s.id.clone()))
            .unwrap();

        let mut upserted: Vec<_> = res.upserts.iter().map(|s| s.id.clone()).collect();
        upserted.sort();

        assert_eq!(upserted, vec!["b", "c"]);
        assert_eq!(streamed.len(), 2);
        assert!(res.deleted.is_empty());
    }

    #[test]
    fn incremental_detects_deletions() {
        let a = stub_with(&[("a", 100)], true);
        let known: KnownSessions = [
            (SessionKey::new(AgentId::Claude, "a"), 100),
            (SessionKey::new(AgentId::Claude, "gone"), 10),
            (SessionKey::new(AgentId::Codex, "other"), 10),
        ]
        .into_iter()
        .collect();

        let res = a.find_incremental(&known, &mut |_| {}).unwrap();
        assert_eq!(res.deleted, vec!["gone".to_string()]);
        assert!(res.upserts.is_empty());
    }

    #[test]
    fn unavailable_agent_marks_all_its_known_as_deleted() {
        let a = stub_with(&[], false);
        let known: KnownSessions = [
            (SessionKey::new(AgentId::Claude, "a"), 1),
            (SessionKey::new(AgentId::Codex, "b"), 1),
        ]
        .into_iter()
        .collect();
        let res = a.find_incremental(&known, &mut |_| {}).unwrap();
        assert_eq!(res.deleted, vec!["a".to_string()]);
    }

    #[test]
    fn iso8601_parses_the_unix_epoch() {
        assert_eq!(parse_iso8601_secs("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_iso8601_secs("1970-01-01T00:00:01Z"), Some(1));
    }

    #[test]
    fn iso8601_parses_claude_codex_shape_with_millis() {
        let days = days_from_civil(2026, 6, 12);
        let expected = days * 86_400 + 6 * 3600 + 40 * 60 + 46;
        assert_eq!(
            parse_iso8601_secs("2026-06-12T06:40:46.538Z"),
            Some(expected)
        );

        assert_eq!(
            parse_iso8601_secs("2026-06-12T06:40:46.999Z"),
            Some(expected)
        );

        assert_eq!(parse_iso8601_secs("2026-06-12T06:40:46Z"), Some(expected));
    }

    #[test]
    fn iso8601_honors_timezone_offsets() {
        let base = days_from_civil(2026, 1, 2) * 86_400;
        assert_eq!(
            parse_iso8601_secs("2026-01-02T12:00:00+02:00"),
            Some(base + 10 * 3600)
        );
        assert_eq!(
            parse_iso8601_secs("2026-01-02T12:00:00-05:00"),
            Some(base + 17 * 3600)
        );
        assert_eq!(
            parse_iso8601_secs("2026-01-02T12:00:00-0500"),
            Some(base + 17 * 3600)
        );
        assert_eq!(
            parse_iso8601_secs("2026-01-02T12:00:00"),
            Some(base + 12 * 3600)
        );
    }

    #[test]
    fn iso8601_rejects_malformed() {
        for bad in [
            "",
            "not a date",
            "2026-06-12",
            "2026/06/12T00:00:00Z",
            "2026-13-01T00:00:00Z",
            "2026-06-12T24:00:00Z",
            "2026-06-12T06:40:46.Z",
            "2026-06-12T06:40:46Q",
            "2026-06-12T06:40:46Zextra",
        ] {
            assert_eq!(parse_iso8601_secs(bad), None, "should reject {bad:?}");
        }
    }

    #[test]
    fn days_from_civil_matches_known_dates() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(1970, 1, 2), 1);
        assert_eq!(days_from_civil(1969, 12, 31), -1);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
    }
}

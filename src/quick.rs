//! Quick continue: `hb .` resumes the newest session of the current folder,
//! whatever agent it belongs to, without opening the TUI.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::adapters::registry;
use crate::config::AgentId;
use crate::model::Session;

/// The query that asks for a quick continue instead of a search.
pub const CONTINUE_QUERY: &str = ".";

#[derive(Debug, Clone, PartialEq)]
pub struct Pick {
    pub session: Session,
    /// The folder the session ran in, as the agent recorded it.
    pub directory: PathBuf,
    /// False when nothing ran in the requested folder itself and the session
    /// comes from the closest parent folder that has one.
    pub exact: bool,
}

/// When a session was last used, in Unix seconds.
///
/// `timestamp` is when the conversation started. For agents that keep one file
/// per session the file's modification time says when it was last written,
/// which is what "newest" should mean for continuing work. Agents that keep
/// every session in one database share that database's mtime, so it says
/// nothing about a single session and only the timestamp is used.
pub fn last_activity_secs(session: &Session, per_session_mtime: bool) -> i64 {
    if per_session_mtime {
        session.timestamp.max(session.mtime / 1_000_000_000)
    } else {
        session.timestamp
    }
}

fn per_session_mtime_by_agent() -> HashMap<AgentId, bool> {
    registry::all()
        .iter()
        .map(|a| (a.id(), a.mtime_is_per_session()))
        .collect()
}

fn trim_dir(dir: &str) -> &str {
    let trimmed = dir.trim_end_matches('/');
    if trimmed.is_empty() && dir.starts_with('/') {
        "/"
    } else {
        trimmed
    }
}

/// Finds the session to continue from `cwd`: the most recently used one whose
/// directory is `cwd`, else the most recently used one in the closest parent
/// folder that has any. `agent` narrows the search to one agent.
pub fn pick_for_directory(
    sessions: &[Session],
    cwd: &Path,
    agent: Option<AgentId>,
) -> Option<Pick> {
    pick_with(sessions, cwd, agent, &per_session_mtime_by_agent())
}

fn pick_with(
    sessions: &[Session],
    cwd: &Path,
    agent: Option<AgentId>,
    per_session_mtime: &HashMap<AgentId, bool>,
) -> Option<Pick> {
    // Sessions record the folder as the agent saw it, which can differ from
    // the shell's view by a symlink (/tmp vs /private/tmp on macOS). Compare
    // both the recorded and the resolved form.
    let mut resolved: HashMap<&str, Option<PathBuf>> = HashMap::new();
    let mut by_dir: HashMap<PathBuf, Vec<&Session>> = HashMap::new();
    for s in sessions {
        if agent.is_some_and(|a| a != s.agent) || s.directory.is_empty() {
            continue;
        }
        let recorded = trim_dir(&s.directory);
        by_dir.entry(PathBuf::from(recorded)).or_default().push(s);
        let canonical = resolved
            .entry(recorded)
            .or_insert_with(|| std::fs::canonicalize(recorded).ok());
        if let Some(c) = canonical {
            if c.as_path() != Path::new(recorded) {
                by_dir.entry(c.clone()).or_default().push(s);
            }
        }
    }

    let canonical_cwd = std::fs::canonicalize(cwd).ok();
    let mut chains: Vec<Vec<&Path>> = vec![cwd.ancestors().collect()];
    if let Some(c) = canonical_cwd.as_deref() {
        if c != cwd {
            chains.push(c.ancestors().collect());
        }
    }
    let depth = chains.iter().map(Vec::len).max().unwrap_or(0);

    for level in 0..depth {
        let mut candidates: Vec<&Session> = Vec::new();
        for chain in &chains {
            if let Some(found) = chain.get(level).and_then(|dir| by_dir.get(*dir)) {
                candidates.extend(found.iter().copied());
            }
        }
        let newest = candidates.into_iter().max_by_key(|s| {
            let per_file = per_session_mtime.get(&s.agent).copied().unwrap_or(true);
            (last_activity_secs(s, per_file), s.timestamp)
        });
        if let Some(s) = newest {
            return Some(Pick {
                session: s.clone(),
                directory: PathBuf::from(trim_dir(&s.directory)),
                exact: level == 0,
            });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sess(id: &str, agent: AgentId, dir: &str, started: i64, mtime_secs: i64) -> Session {
        Session {
            id: id.into(),
            agent,
            title: id.into(),
            directory: dir.into(),
            timestamp: started,
            content: String::new(),
            message_count: 1,
            mtime: mtime_secs * 1_000_000_000,
            yolo: false,
            source_path: None,
        }
    }

    fn per_file() -> HashMap<AgentId, bool> {
        [
            (AgentId::Claude, true),
            (AgentId::Codex, true),
            (AgentId::OpenCode, false),
        ]
        .into_iter()
        .collect()
    }

    fn pick(sessions: &[Session], cwd: &str, agent: Option<AgentId>) -> Option<Pick> {
        pick_with(sessions, Path::new(cwd), agent, &per_file())
    }

    #[test]
    fn newest_session_of_the_folder_wins_across_agents() {
        let sessions = vec![
            sess("old-claude", AgentId::Claude, "/nx/app", 100, 100),
            sess("new-codex", AgentId::Codex, "/nx/app", 300, 300),
            sess("elsewhere", AgentId::Claude, "/nx/other", 900, 900),
        ];
        let p = pick(&sessions, "/nx/app", None).unwrap();
        assert_eq!(p.session.id, "new-codex");
        assert!(p.exact);
        assert_eq!(p.directory, PathBuf::from("/nx/app"));
    }

    #[test]
    fn a_session_used_recently_beats_one_started_later() {
        let sessions = vec![
            sess("started-later", AgentId::Claude, "/nx/app", 500, 500),
            sess("long-running", AgentId::Codex, "/nx/app", 100, 900),
        ];
        assert_eq!(
            pick(&sessions, "/nx/app", None).unwrap().session.id,
            "long-running"
        );
    }

    #[test]
    fn a_shared_database_mtime_does_not_count_as_activity() {
        // Every OpenCode session carries the database's mtime.
        let sessions = vec![
            sess("stale-opencode", AgentId::OpenCode, "/nx/app", 100, 9_999),
            sess("claude", AgentId::Claude, "/nx/app", 400, 400),
        ];
        assert_eq!(
            pick(&sessions, "/nx/app", None).unwrap().session.id,
            "claude"
        );
    }

    #[test]
    fn agent_narrows_the_choice() {
        let sessions = vec![
            sess("claude", AgentId::Claude, "/nx/app", 100, 100),
            sess("codex", AgentId::Codex, "/nx/app", 300, 300),
        ];
        let p = pick(&sessions, "/nx/app", Some(AgentId::Claude)).unwrap();
        assert_eq!(p.session.id, "claude");
        assert!(pick(&sessions, "/nx/app", Some(AgentId::OpenCode)).is_none());
    }

    #[test]
    fn falls_back_to_the_closest_parent_folder() {
        let sessions = vec![
            sess("root", AgentId::Claude, "/nx", 900, 900),
            sess("repo", AgentId::Codex, "/nx/repo", 100, 100),
            sess("sibling", AgentId::Claude, "/nx/repo/other", 950, 950),
        ];
        let p = pick(&sessions, "/nx/repo/src/deep", None).unwrap();
        assert_eq!(
            p.session.id, "repo",
            "closest parent, not the newest parent"
        );
        assert!(!p.exact);
        assert_eq!(p.directory, PathBuf::from("/nx/repo"));
    }

    #[test]
    fn child_folders_are_not_parents() {
        let sessions = vec![sess("child", AgentId::Claude, "/nx/app/sub", 1, 1)];
        assert!(pick(&sessions, "/nx/app", None).is_none());
    }

    #[test]
    fn nothing_anywhere_is_none() {
        assert!(pick(&[], "/nx/app", None).is_none());
        let sessions = vec![sess("x", AgentId::Claude, "", 1, 1)];
        assert!(pick(&sessions, "/nx/app", None).is_none());
    }

    #[test]
    fn trailing_slashes_do_not_matter() {
        let sessions = vec![sess("a", AgentId::Claude, "/nx/app/", 1, 1)];
        assert_eq!(pick(&sessions, "/nx/app", None).unwrap().session.id, "a");
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_folders_match_their_target() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        std::fs::create_dir_all(&real).unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let real = std::fs::canonicalize(&real).unwrap();

        // Recorded through the symlink, asked for by the real path.
        let via_link = vec![sess("a", AgentId::Claude, link.to_str().unwrap(), 1, 1)];
        let p = pick_with(&via_link, &real, None, &per_file()).unwrap();
        assert_eq!(p.session.id, "a");
        assert!(p.exact);

        // Recorded by the real path, asked for through the symlink.
        let via_real = vec![sess("b", AgentId::Claude, real.to_str().unwrap(), 1, 1)];
        let p = pick_with(&via_real, &link, None, &per_file()).unwrap();
        assert_eq!(p.session.id, "b");
        assert!(p.exact);
    }
}

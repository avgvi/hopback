//! One-time import of user metadata from an old `aiz` database.
//!
//! The session index itself is rebuilt from the agents' files, so only the
//! rows a user created by hand are copied: pins, local renames, hidden flags,
//! settings (the permission policy) and fork links. The old database is opened
//! read-only and is never changed or removed.

use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OpenFlags};

use super::Store;

pub const LEGACY_IMPORT_KEY: &str = "legacy_import";

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct LegacyImport {
    pub overrides: usize,
    pub settings: usize,
    pub fork_links: usize,
}

impl LegacyImport {
    pub fn total(&self) -> usize {
        self.overrides + self.settings + self.fork_links
    }
}

fn open_read_only(path: &Path) -> Result<Connection> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;

    // A plain read-only connection to a WAL database creates `-shm` and `-wal`
    // files beside it. Immutable mode reads the main file only and leaves the
    // old directory exactly as it was, so use it unless a non-empty WAL holds
    // changes that were never checkpointed.
    let mut wal = path.as_os_str().to_owned();
    wal.push("-wal");
    let pending_wal = std::fs::metadata(&wal)
        .map(|m| m.len() > 0)
        .unwrap_or(false);

    if !pending_wal {
        let encoded = path
            .to_string_lossy()
            .replace('%', "%25")
            .replace('?', "%3f")
            .replace('#', "%23");
        let uri = format!("file:{encoded}?immutable=1");
        if let Ok(conn) = Connection::open_with_flags(uri, flags | OpenFlags::SQLITE_OPEN_URI) {
            if readable(&conn) {
                return Ok(conn);
            }
        }
    }

    let conn = Connection::open_with_flags(path, flags)
        .with_context(|| format!("opening legacy store at {}", path.display()))?;
    if !readable(&conn) {
        anyhow::bail!("legacy store at {} is not readable", path.display());
    }
    Ok(conn)
}

fn readable(conn: &Connection) -> bool {
    conn.query_row("SELECT COUNT(*) FROM sqlite_master", [], |r| {
        r.get::<_, i64>(0)
    })
    .is_ok()
}

fn has_table(conn: &Connection, table: &str) -> bool {
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        params![table],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n > 0)
    .unwrap_or(false)
}

fn has_column(conn: &Connection, table: &str, column: &str) -> bool {
    conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name = ?2",
        params![table, column],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n > 0)
    .unwrap_or(false)
}

impl Store {
    /// Copies metadata rows from `legacy_db` into this store. Rows that
    /// already exist here win, so running it twice changes nothing.
    pub fn import_legacy_metadata(&mut self, legacy_db: &Path) -> Result<LegacyImport> {
        let old = open_read_only(legacy_db)?;
        let mut report = LegacyImport::default();
        let tx = self.conn.transaction()?;

        if has_table(&old, "session_overrides") {
            let hidden_expr = if has_column(&old, "session_overrides", "hidden") {
                "hidden"
            } else {
                "0"
            };
            let mut stmt = old.prepare(&format!(
                "SELECT agent, session_id, pinned, title, {hidden_expr}, updated
                 FROM session_overrides"
            ))?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, i64>(5)?,
                ))
            })?;
            for row in rows {
                let (agent, session_id, pinned, title, hidden, updated) = row?;
                report.overrides += tx.execute(
                    "INSERT OR IGNORE INTO session_overrides
                        (agent, session_id, pinned, title, hidden, updated)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![agent, session_id, pinned, title, hidden, updated],
                )?;
            }
        }

        if has_table(&old, "settings") {
            let mut stmt = old.prepare("SELECT key, value, updated FROM settings")?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            })?;
            for row in rows {
                let (key, value, updated) = row?;
                report.settings += tx.execute(
                    "INSERT OR IGNORE INTO settings (key, value, updated) VALUES (?1, ?2, ?3)",
                    params![key, value, updated],
                )?;
            }
        }

        if has_table(&old, "fork_links") && has_column(&old, "fork_links", "parent_agent") {
            let already: i64 = tx.query_row("SELECT COUNT(*) FROM fork_links", [], |r| r.get(0))?;
            if already == 0 {
                let mut stmt = old.prepare(
                    "SELECT fork_session_id, parent_session_id, parent_agent, fork_agent,
                            mode, directory, created
                     FROM fork_links ORDER BY id",
                )?;
                let rows = stmt.query_map([], |r| {
                    Ok((
                        r.get::<_, Option<String>>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, String>(5)?,
                        r.get::<_, i64>(6)?,
                    ))
                })?;
                for row in rows {
                    let (fork_id, parent_id, parent_agent, fork_agent, mode, directory, created) =
                        row?;
                    report.fork_links += tx.execute(
                        "INSERT INTO fork_links
                            (fork_session_id, parent_session_id, parent_agent, fork_agent,
                             mode, directory, created)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                        params![
                            fork_id,
                            parent_id,
                            parent_agent,
                            fork_agent,
                            mode,
                            directory,
                            created
                        ],
                    )?;
                }
            }
        }

        tx.execute(
            "INSERT INTO settings (key, value, updated) VALUES (?1, ?2, 0)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![
                LEGACY_IMPORT_KEY,
                format!(
                    "aiz overrides={} settings={} fork_links={}",
                    report.overrides, report.settings, report.fork_links
                )
            ],
        )?;
        tx.commit()?;
        Ok(report)
    }
}

/// Opens the store at `path`. When the file is being created for the first
/// time and an old aiz database exists at `legacy_db`, its metadata is copied
/// over. Returns the store and the import report, if an import ran.
pub fn open_with_legacy_import(
    path: &Path,
    legacy_db: &Path,
) -> Result<(Store, Option<LegacyImport>)> {
    let first_run = !path.exists();
    let mut store = Store::open(path)?;
    if !first_run || !legacy_db.is_file() {
        return Ok((store, None));
    }
    let report = store.import_legacy_metadata(legacy_db)?;
    Ok((store, Some(report)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AgentId;
    use crate::model::{ForkLink, ForkMode, PermissionPolicy, SessionKey};
    use tempfile::tempdir;

    fn key(id: &str) -> SessionKey {
        SessionKey::new(AgentId::Claude, id)
    }

    fn seed_legacy(path: &Path) {
        let old = Store::open(path).unwrap();
        old.set_pinned(&key("pinned"), true).unwrap();
        old.set_title_override(&key("renamed"), Some("My title"))
            .unwrap();
        old.set_hidden(&key("hidden"), true).unwrap();
        old.set_permission_policy(PermissionPolicy::AlwaysYolo)
            .unwrap();
        old.insert_fork_link(&ForkLink {
            fork_session_id: Some("child".into()),
            parent_session_id: "pinned".into(),
            parent_agent: AgentId::Claude,
            fork_agent: AgentId::Codex,
            mode: ForkMode::Translated,
            directory: "/w".into(),
            created: 7,
        })
        .unwrap();
    }

    #[test]
    fn first_run_copies_pins_renames_hidden_settings_and_forks() {
        let dir = tempdir().unwrap();
        let legacy = dir.path().join("aiz").join("aiz.db");
        seed_legacy(&legacy);
        let legacy_bytes = std::fs::read(&legacy).unwrap();

        let new_path = dir.path().join("hopback").join("hopback.db");
        let (store, report) = open_with_legacy_import(&new_path, &legacy).unwrap();
        let report = report.expect("first run should import");
        assert_eq!(
            report,
            LegacyImport {
                overrides: 3,
                settings: 1,
                fork_links: 1
            }
        );

        assert!(store.is_pinned(&key("pinned")).unwrap());
        assert!(store.is_hidden(&key("hidden")).unwrap());
        let title: Option<String> = store
            .conn
            .query_row(
                "SELECT title FROM session_overrides WHERE session_id = 'renamed'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(title.as_deref(), Some("My title"));
        assert_eq!(
            store.get_permission_policy().unwrap(),
            Some(PermissionPolicy::AlwaysYolo)
        );
        assert_eq!(store.list_fork_links().unwrap().len(), 1);
        assert!(store.get_setting(LEGACY_IMPORT_KEY).unwrap().is_some());

        assert!(legacy.exists(), "the old database must be left in place");
        assert_eq!(std::fs::read(&legacy).unwrap(), legacy_bytes);
        let mut names: Vec<String> = std::fs::read_dir(legacy.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec!["aiz.db"],
            "no side files beside the old database"
        );
    }

    #[test]
    fn later_runs_do_not_import_again() {
        let dir = tempdir().unwrap();
        let legacy = dir.path().join("aiz.db");
        seed_legacy(&legacy);
        let new_path = dir.path().join("hopback.db");

        let (store, first) = open_with_legacy_import(&new_path, &legacy).unwrap();
        assert!(first.is_some());
        store.set_pinned(&key("pinned"), false).unwrap();
        drop(store);

        let (store, second) = open_with_legacy_import(&new_path, &legacy).unwrap();
        assert!(second.is_none());
        assert!(
            !store.is_pinned(&key("pinned")).unwrap(),
            "an unpin made in hopback must not be undone by the old data"
        );
    }

    #[test]
    fn missing_legacy_database_is_not_an_error() {
        let dir = tempdir().unwrap();
        let (store, report) =
            open_with_legacy_import(&dir.path().join("hopback.db"), &dir.path().join("nope.db"))
                .unwrap();
        assert!(report.is_none());
        assert!(store.get_pinned_keys().unwrap().is_empty());
    }

    #[test]
    fn imports_from_a_pre_hidden_schema() {
        let dir = tempdir().unwrap();
        let legacy = dir.path().join("old.db");
        {
            let conn = Connection::open(&legacy).unwrap();
            conn.execute_batch(
                "CREATE TABLE session_overrides (
                    agent TEXT NOT NULL,
                    session_id TEXT NOT NULL,
                    pinned INTEGER NOT NULL DEFAULT 0,
                    title TEXT,
                    updated INTEGER NOT NULL,
                    PRIMARY KEY (agent, session_id)
                 );
                 INSERT INTO session_overrides VALUES ('claude', 's', 1, NULL, 0);",
            )
            .unwrap();
        }
        let mut store = Store::open_in_memory().unwrap();
        let report = store.import_legacy_metadata(&legacy).unwrap();
        assert_eq!(report.overrides, 1);
        assert_eq!(report.total(), 1);
        assert!(store.is_pinned(&key("s")).unwrap());
        assert!(!store.is_hidden(&key("s")).unwrap());
    }
}

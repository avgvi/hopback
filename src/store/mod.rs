use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::Connection;

use crate::config::SCHEMA_VERSION;

pub mod dirs;
pub mod forks;
pub mod legacy;
pub mod overrides;
pub mod sessions;
pub mod settings;

struct Migration {
    version: i64,
    sql: &'static str,
}

const SCHEMA_LATEST: &str = r#"
CREATE TABLE sessions (
    agent         TEXT    NOT NULL,
    id            TEXT    NOT NULL,
    title         TEXT    NOT NULL,
    directory     TEXT    NOT NULL,
    timestamp     INTEGER NOT NULL,
    content       TEXT    NOT NULL,
    message_count INTEGER NOT NULL,
    mtime         INTEGER NOT NULL,
    yolo          INTEGER NOT NULL,
    source_path   TEXT,
    PRIMARY KEY (agent, id)
);
CREATE INDEX idx_sessions_agent     ON sessions(agent);
CREATE INDEX idx_sessions_timestamp ON sessions(timestamp);

CREATE VIRTUAL TABLE sessions_fts USING fts5(
    title,
    content,
    agent UNINDEXED,
    id UNINDEXED,
    tokenize = 'unicode61'
);

CREATE TABLE fork_links (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    fork_session_id   TEXT,
    parent_session_id TEXT    NOT NULL,
    parent_agent      TEXT    NOT NULL,
    fork_agent        TEXT    NOT NULL,
    mode              TEXT    NOT NULL,
    directory         TEXT    NOT NULL,
    created           INTEGER NOT NULL
);

CREATE TABLE dir_mtimes (
    agent     TEXT    NOT NULL,
    directory TEXT    NOT NULL,
    mtime     INTEGER NOT NULL,
    PRIMARY KEY (agent, directory)
);

CREATE TABLE session_overrides (
    agent      TEXT NOT NULL,
    session_id TEXT NOT NULL,
    pinned     INTEGER NOT NULL DEFAULT 0,
    title      TEXT,
    hidden     INTEGER NOT NULL DEFAULT 0,
    updated    INTEGER NOT NULL,
    PRIMARY KEY (agent, session_id)
);

CREATE TABLE settings (
    key     TEXT PRIMARY KEY,
    value   TEXT    NOT NULL,
    updated INTEGER NOT NULL
);
"#;

const SCHEMA_V1: &str = SCHEMA_LATEST;

const SCHEMA_V2: &str = "";
const SCHEMA_V3: &str = "";

const SCHEMA_V4_RESET: &str = r#"
DROP TABLE IF EXISTS sessions_fts;
DROP TABLE IF EXISTS sessions;
DROP TABLE IF EXISTS session_overrides;
DROP TABLE IF EXISTS seeds;
DROP TABLE IF EXISTS pins;
DROP TABLE IF EXISTS fork_links;
DROP TABLE IF EXISTS dir_mtimes;

CREATE TABLE sessions (
    agent         TEXT    NOT NULL,
    id            TEXT    NOT NULL,
    title         TEXT    NOT NULL,
    directory     TEXT    NOT NULL,
    timestamp     INTEGER NOT NULL,
    content       TEXT    NOT NULL,
    message_count INTEGER NOT NULL,
    mtime         INTEGER NOT NULL,
    yolo          INTEGER NOT NULL,
    PRIMARY KEY (agent, id)
);
CREATE INDEX idx_sessions_agent     ON sessions(agent);
CREATE INDEX idx_sessions_timestamp ON sessions(timestamp);

CREATE VIRTUAL TABLE sessions_fts USING fts5(
    title,
    content,
    agent UNINDEXED,
    id UNINDEXED,
    tokenize = 'unicode61'
);

CREATE TABLE fork_links (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    fork_session_id   TEXT,
    parent_session_id TEXT    NOT NULL,
    agent             TEXT    NOT NULL,
    directory         TEXT    NOT NULL,
    created           INTEGER NOT NULL
);

CREATE TABLE dir_mtimes (
    agent     TEXT    NOT NULL,
    directory TEXT    NOT NULL,
    mtime     INTEGER NOT NULL,
    PRIMARY KEY (agent, directory)
);

CREATE TABLE session_overrides (
    agent      TEXT NOT NULL,
    session_id TEXT NOT NULL,
    pinned     INTEGER NOT NULL DEFAULT 0,
    title      TEXT,
    updated    INTEGER NOT NULL,
    PRIMARY KEY (agent, session_id)
);
"#;

const SCHEMA_V5_RESET: &str = r#"
DROP TABLE IF EXISTS sessions_fts;
DROP TABLE IF EXISTS sessions;
DROP TABLE IF EXISTS session_overrides;
DROP TABLE IF EXISTS seeds;
DROP TABLE IF EXISTS pins;
DROP TABLE IF EXISTS fork_links;
DROP TABLE IF EXISTS dir_mtimes;

CREATE TABLE sessions (
    agent         TEXT    NOT NULL,
    id            TEXT    NOT NULL,
    title         TEXT    NOT NULL,
    directory     TEXT    NOT NULL,
    timestamp     INTEGER NOT NULL,
    content       TEXT    NOT NULL,
    message_count INTEGER NOT NULL,
    mtime         INTEGER NOT NULL,
    yolo          INTEGER NOT NULL,
    source_path   TEXT,
    PRIMARY KEY (agent, id)
);
CREATE INDEX idx_sessions_agent     ON sessions(agent);
CREATE INDEX idx_sessions_timestamp ON sessions(timestamp);

CREATE VIRTUAL TABLE sessions_fts USING fts5(
    title,
    content,
    agent UNINDEXED,
    id UNINDEXED,
    tokenize = 'unicode61'
);

CREATE TABLE fork_links (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    fork_session_id   TEXT,
    parent_session_id TEXT    NOT NULL,
    parent_agent      TEXT    NOT NULL,
    fork_agent        TEXT    NOT NULL,
    mode              TEXT    NOT NULL,
    directory         TEXT    NOT NULL,
    created           INTEGER NOT NULL
);

CREATE TABLE dir_mtimes (
    agent     TEXT    NOT NULL,
    directory TEXT    NOT NULL,
    mtime     INTEGER NOT NULL,
    PRIMARY KEY (agent, directory)
);

CREATE TABLE session_overrides (
    agent      TEXT NOT NULL,
    session_id TEXT NOT NULL,
    pinned     INTEGER NOT NULL DEFAULT 0,
    title      TEXT,
    updated    INTEGER NOT NULL,
    PRIMARY KEY (agent, session_id)
);
"#;

const SCHEMA_V6_SETTINGS: &str = r#"
CREATE TABLE IF NOT EXISTS settings (
    key     TEXT PRIMARY KEY,
    value   TEXT    NOT NULL,
    updated INTEGER NOT NULL
);
"#;

const SCHEMA_V7_REMOVE_UNUSED_LAUNCHER_TABLES: &str = r#"
DROP TABLE IF EXISTS seeds;
DROP TABLE IF EXISTS pins;
"#;

const SCHEMA_V8_HIDDEN_OVERRIDES: &str = r#"
ALTER TABLE session_overrides ADD COLUMN hidden INTEGER NOT NULL DEFAULT 0;
"#;

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        sql: SCHEMA_V1,
    },
    Migration {
        version: 2,
        sql: SCHEMA_V2,
    },
    Migration {
        version: 3,
        sql: SCHEMA_V3,
    },
    Migration {
        version: 4,
        sql: SCHEMA_V4_RESET,
    },
    Migration {
        version: 5,
        sql: SCHEMA_V5_RESET,
    },
    Migration {
        version: 6,
        sql: SCHEMA_V6_SETTINGS,
    },
    Migration {
        version: 7,
        sql: SCHEMA_V7_REMOVE_UNUSED_LAUNCHER_TABLES,
    },
    Migration {
        version: 8,
        sql: SCHEMA_V8_HIDDEN_OVERRIDES,
    },
];

pub struct Store {
    pub(crate) conn: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Store> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let conn = Connection::open(path)
            .with_context(|| format!("opening store at {}", path.display()))?;
        Self::configure_connection(&conn)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        let mut store = Store { conn };
        store.migrate(MIGRATIONS, SCHEMA_VERSION)?;
        Ok(store)
    }

    pub fn open_in_memory() -> Result<Store> {
        let conn = Connection::open_in_memory()?;
        Self::configure_connection(&conn)?;
        let mut store = Store { conn };
        store.migrate(MIGRATIONS, SCHEMA_VERSION)?;
        Ok(store)
    }

    fn configure_connection(conn: &Connection) -> Result<()> {
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        Ok(())
    }

    fn migrate(&mut self, migrations: &[Migration], target: i64) -> Result<()> {
        let current: i64 = self
            .conn
            .pragma_query_value(None, "user_version", |row| row.get(0))?;
        if current >= target {
            return Ok(());
        }
        let tx = self.conn.transaction()?;
        for m in migrations {
            if m.version > current && m.version <= target {
                tx.execute_batch(m.sql)
                    .with_context(|| format!("applying migration v{}", m.version))?;
            }
        }

        tx.pragma_update(None, "user_version", target)?;
        tx.commit()?;
        Ok(())
    }

    pub fn schema_version(&self) -> Result<i64> {
        Ok(self
            .conn
            .pragma_query_value(None, "user_version", |row| row.get(0))?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn opens_and_sets_schema_version() {
        let s = Store::open_in_memory().unwrap();
        assert_eq!(s.schema_version().unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn open_creates_file_and_enables_wal() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nested").join("hopback.db");
        let s = Store::open(&path).unwrap();
        assert!(path.exists());
        let mode: String = s
            .conn
            .pragma_query_value(None, "journal_mode", |r| r.get(0))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal");
    }

    #[test]
    fn bumping_user_version_migrates_not_drops() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("hopback.db");

        {
            let conn = Connection::open(&path).unwrap();
            Store::configure_connection(&conn).unwrap();
            let mut s = Store { conn };
            s.migrate(
                &[Migration {
                    version: 1,
                    sql: SCHEMA_V1,
                }],
                1,
            )
            .unwrap();
            assert_eq!(s.schema_version().unwrap(), 1);
            s.conn
                .execute(
                    "INSERT INTO session_overrides (agent, session_id, pinned, title, updated)
                     VALUES ('claude','s',1,'title',0)",
                    [],
                )
                .unwrap();
        }

        let migrations = &[
            Migration {
                version: 1,
                sql: SCHEMA_V1,
            },
            Migration {
                version: 2,
                sql: "ALTER TABLE session_overrides ADD COLUMN extra TEXT;",
            },
        ];
        let conn = Connection::open(&path).unwrap();
        Store::configure_connection(&conn).unwrap();
        let mut s = Store { conn };
        s.migrate(migrations, 2).unwrap();

        assert_eq!(s.schema_version().unwrap(), 2);
        let count: i64 = s
            .conn
            .query_row(
                "SELECT COUNT(*) FROM session_overrides WHERE agent='claude' AND session_id='s'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "row from v1 must survive the migration");

        s.conn
            .execute(
                "UPDATE session_overrides SET extra='x' WHERE agent='claude' AND session_id='s'",
                [],
            )
            .unwrap();
    }

    #[test]
    fn v8_migration_adds_hidden_and_keeps_prior_overrides() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("hopback.db");

        {
            let conn = Connection::open(&path).unwrap();
            Store::configure_connection(&conn).unwrap();
            conn.execute_batch(SCHEMA_V6_SETTINGS).unwrap();
            conn.execute_batch(
                "CREATE TABLE seeds (name TEXT PRIMARY KEY);
                 CREATE TABLE pins (id TEXT PRIMARY KEY);
                 CREATE TABLE session_overrides (
                    agent TEXT NOT NULL,
                    session_id TEXT NOT NULL,
                    pinned INTEGER NOT NULL DEFAULT 0,
                    title TEXT,
                    updated INTEGER NOT NULL,
                    PRIMARY KEY (agent, session_id)
                 );",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO session_overrides (agent, session_id, pinned, title, updated)
                 VALUES ('claude','s',1,'title',0)",
                [],
            )
            .unwrap();
            conn.pragma_update(None, "user_version", 6).unwrap();
        }

        let store = Store::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), SCHEMA_VERSION);
        store
            .set_permission_policy(crate::model::PermissionPolicy::AlwaysNormal)
            .unwrap();
        assert_eq!(
            store.get_permission_policy().unwrap(),
            Some(crate::model::PermissionPolicy::AlwaysNormal)
        );
        let count: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM session_overrides WHERE agent = 'claude' AND session_id = 's'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
        let removed: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name IN ('seeds', 'pins')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(removed, 0);
        let hidden_cols: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('session_overrides') WHERE name = 'hidden'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hidden_cols, 1);
        assert!(!store
            .is_hidden(&crate::model::SessionKey::new(
                crate::config::AgentId::Claude,
                "s"
            ))
            .unwrap());
    }

    #[test]
    fn concurrent_reader_and_writer_under_wal() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("hopback.db");
        let writer = Store::open(&path).unwrap();

        let reader = Store::open(&path).unwrap();

        writer
            .conn
            .execute(
                "INSERT INTO session_overrides (agent, session_id, pinned, title, updated)
                 VALUES ('claude','a',1,NULL,0)",
                [],
            )
            .unwrap();

        let n: i64 = reader
            .conn
            .query_row("SELECT COUNT(*) FROM session_overrides", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);

        writer
            .conn
            .execute(
                "INSERT INTO session_overrides (agent, session_id, pinned, title, updated)
                 VALUES ('claude','b',1,NULL,0)",
                [],
            )
            .unwrap();
        let n2: i64 = reader
            .conn
            .query_row("SELECT COUNT(*) FROM session_overrides", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n2, 2);
    }
}

//! Fixture tests for Goose's session store, one SQLite database at
//! `~/.local/share/goose/sessions/sessions.db`. The tables are the ones Goose
//! 1.50 creates (schema version 16), cut down to the columns that matter, and
//! the row shapes follow a real database. All text here is made up.

use std::fs;
use std::path::{Path, PathBuf};

use hopback::adapters::goose::GooseAdapter;
use hopback::adapters::{parse_iso8601_secs, Adapter, KnownSessions};
use hopback::config::AgentId;
use hopback::model::{Session, SessionKey};
use rusqlite::{params, Connection};

const SCHEMA: &str = "
    CREATE TABLE sessions (
        id TEXT PRIMARY KEY,
        name TEXT NOT NULL DEFAULT '',
        description TEXT NOT NULL DEFAULT '',
        user_set_name BOOLEAN DEFAULT FALSE,
        session_type TEXT NOT NULL DEFAULT 'user',
        working_dir TEXT NOT NULL,
        created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
        updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
        goose_mode TEXT NOT NULL DEFAULT 'auto',
        archived_at TIMESTAMP,
        parent_session_id TEXT
    );
    CREATE TABLE messages (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        message_id TEXT,
        session_id TEXT NOT NULL REFERENCES sessions(id),
        role TEXT NOT NULL,
        content_json TEXT NOT NULL,
        created_timestamp INTEGER NOT NULL,
        timestamp TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
        tokens INTEGER,
        metadata_json TEXT
    );";

const VISIBLE: &str = r#"{"userVisible":true,"agentVisible":true}"#;
const HIDDEN: &str = r#"{"userVisible":false,"agentVisible":true,"turnContext":true}"#;

fn text(body: &str) -> String {
    format!(r#"[{{"type":"text","text":"{body}"}}]"#)
}

struct Fixture {
    _tmp: tempfile::TempDir,
    db: PathBuf,
}

impl Fixture {
    fn new() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("sessions").join("sessions.db");
        fs::create_dir_all(db.parent().unwrap()).unwrap();
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        Fixture { _tmp: tmp, db }
    }

    fn conn(&self) -> Connection {
        Connection::open(&self.db).unwrap()
    }

    fn session(&self, id: &str, name: &str, dir: &str, updated_at: &str) {
        self.conn()
            .execute(
                "INSERT INTO sessions (id, name, working_dir, created_at, updated_at)
                 VALUES (?1, ?2, ?3, '2026-03-01 09:00:00', ?4)",
                params![id, name, dir, updated_at],
            )
            .unwrap();
    }

    fn message(&self, session: &str, role: &str, content_json: &str, metadata: &str, at: i64) {
        self.conn()
            .execute(
                "INSERT INTO messages (session_id, role, content_json, created_timestamp, metadata_json)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![session, role, content_json, at, metadata],
            )
            .unwrap();
    }

    /// A session with one visible prompt and one reply.
    fn simple(&self, id: &str, name: &str) {
        self.session(id, name, "/work/app", "2026-03-01 10:00:00");
        self.message(id, "user", &text("Prompt"), VISIBLE, 1);
        self.message(id, "assistant", &text("Reply"), VISIBLE, 2);
    }

    fn adapter(&self) -> GooseAdapter {
        GooseAdapter::with_db_path(self.db.clone())
    }

    fn load(&self) -> Vec<Session> {
        let mut upserts = self
            .adapter()
            .find_incremental(&KnownSessions::new(), &mut |_| {})
            .unwrap()
            .upserts;
        upserts.sort_by(|a, b| a.id.cmp(&b.id));
        upserts
    }
}

fn ids(sessions: &[Session]) -> Vec<&str> {
    sessions.iter().map(|s| s.id.as_str()).collect()
}

fn side_files(db: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(db.parent().unwrap())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    names
}

#[test]
fn reads_a_session_with_its_messages() {
    let fx = Fixture::new();
    fx.session(
        "20260301_1",
        "Uploader retries",
        "/work/app",
        "2026-03-01 10:00:05",
    );
    let s = "20260301_1";
    fx.message(
        s,
        "user",
        &text("Add retry logic to the uploader"),
        VISIBLE,
        10,
    );
    // Goose sends this with every prompt; the person never sees it.
    fx.message(
        s,
        "user",
        &text("<turn-context>hidden context</turn-context>"),
        HIDDEN,
        10,
    );
    fx.message(
        s,
        "assistant",
        r#"[{"type":"thinking","thinking":"private reasoning","signature":"sig"},
            {"type":"text","text":"Added a retry loop with backoff."},
            {"type":"toolRequest","id":"t1","toolCall":{"status":"success","value":{"name":"developer__shell","arguments":{"command":"hidden arguments"}}}}]"#,
        VISIBLE,
        11,
    );
    fx.message(
        s,
        "user",
        r#"[{"type":"toolResponse","id":"t1","toolResult":{"status":"success","value":[{"type":"text","text":"tool output stays out"}]}}]"#,
        VISIBLE,
        12,
    );
    fx.message(s, "user", &text("Now cap it at five tries"), VISIBLE, 13);

    let adapter = fx.adapter();
    assert!(adapter.is_available());
    assert!(!adapter.mtime_is_per_session());

    let scan = adapter.scan().unwrap();
    assert_eq!(scan.len(), 1);
    assert_eq!(scan["20260301_1"].0, fx.db);

    let sessions = fx.load();
    assert_eq!(sessions.len(), 1);
    let session = &sessions[0];
    assert_eq!(session.id, "20260301_1");
    assert_eq!(session.agent, AgentId::Goose);
    assert_eq!(session.title, "Uploader retries");
    assert_eq!(session.directory, "/work/app");
    assert_eq!(
        Some(session.timestamp),
        parse_iso8601_secs("2026-03-01T10:00:05Z")
    );
    assert_eq!(session.message_count, 3, "two prompts and one reply");
    assert_eq!(session.mtime, scan["20260301_1"].1);
    assert_eq!(
        session.source_path.as_deref(),
        Some(fx.db.to_string_lossy().as_ref())
    );
    assert_eq!(
        session.content,
        "» Add retry logic to the uploader\n\n  Added a retry loop with backoff. [calling developer__shell]\n\n» Now cap it at five tries"
    );
    for hidden in [
        "hidden context",
        "private reasoning",
        "hidden arguments",
        "tool output",
    ] {
        assert!(!session.content.contains(hidden), "{hidden} leaked");
    }
}

#[test]
fn drops_sessions_that_are_not_the_persons_own() {
    let fx = Fixture::new();
    fx.simple("kept", "Kept");

    fx.simple("subagent", "Subagent work");
    fx.conn()
        .execute(
            "UPDATE sessions SET session_type = 'sub_agent' WHERE id = 'subagent'",
            [],
        )
        .unwrap();

    fx.simple("hidden", "Hidden");
    fx.conn()
        .execute(
            "UPDATE sessions SET session_type = 'hidden' WHERE id = 'hidden'",
            [],
        )
        .unwrap();

    fx.simple("archived", "Archived");
    fx.conn()
        .execute(
            "UPDATE sessions SET archived_at = '2026-03-02 00:00:00' WHERE id = 'archived'",
            [],
        )
        .unwrap();

    // Opened but never used.
    fx.session("empty", "Empty", "/work/app", "2026-03-01 10:00:00");

    // Only context Goose added, and a reply to it.
    fx.session("no-prompt", "No prompt", "/work/app", "2026-03-01 10:00:00");
    fx.message("no-prompt", "user", &text("context"), HIDDEN, 1);
    fx.message("no-prompt", "assistant", &text("Reply"), VISIBLE, 2);

    // A scheduled run is still a session the person can open again.
    fx.simple("scheduled", "Nightly report");
    fx.conn()
        .execute(
            "UPDATE sessions SET session_type = 'scheduled' WHERE id = 'scheduled'",
            [],
        )
        .unwrap();

    assert_eq!(ids(&fx.load()), ["kept", "scheduled"]);
}

#[test]
fn title_falls_back_to_the_description_then_the_first_prompt() {
    let fx = Fixture::new();
    fx.simple("named", "  Named session  ");

    fx.simple("described", "");
    fx.conn()
        .execute(
            "UPDATE sessions SET description = 'From the description' WHERE id = 'described'",
            [],
        )
        .unwrap();

    fx.session("bare", "", "/work/app", "2026-03-01 10:00:00");
    fx.message("bare", "user", &text("the first thing I asked"), VISIBLE, 1);

    let sessions = fx.load();
    let titles: Vec<(&str, &str)> = sessions
        .iter()
        .map(|s| (s.id.as_str(), s.title.as_str()))
        .collect();
    assert_eq!(
        titles,
        [
            ("bare", "the first thing I asked"),
            ("described", "From the description"),
            ("named", "Named session"),
        ]
    );
}

#[test]
fn reads_an_older_database_without_the_newer_columns() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("sessions.db");
    let conn = Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE sessions (id TEXT PRIMARY KEY, description TEXT NOT NULL DEFAULT '',
                                working_dir TEXT NOT NULL, updated_at TIMESTAMP);
         CREATE TABLE messages (id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL,
                                role TEXT NOT NULL, content_json TEXT NOT NULL);
         INSERT INTO sessions VALUES ('old_1', 'Older layout', '/work/old', '2026-01-05 08:00:00');
         INSERT INTO messages (session_id, role, content_json)
             VALUES ('old_1', 'user', '[{\"type\":\"text\",\"text\":\"Prompt\"}]');",
    )
    .unwrap();
    drop(conn);

    let sessions = GooseAdapter::with_db_path(db)
        .find_incremental(&KnownSessions::new(), &mut |_| {})
        .unwrap()
        .upserts;
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].title, "Older layout");
    assert_eq!(sessions[0].directory, "/work/old");
    assert_eq!(
        Some(sessions[0].timestamp),
        parse_iso8601_secs("2026-01-05T08:00:00Z")
    );
}

#[test]
fn reading_leaves_no_files_beside_the_database() {
    let fx = Fixture::new();
    fx.simple("20260301_1", "Session");
    // Goose runs its database in WAL mode. Once the last writer closes, the
    // `-wal` and `-shm` files are gone, and a reader must not bring them back.
    let conn = fx.conn();
    let mode: String = conn
        .query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    drop(conn);
    assert_eq!(side_files(&fx.db), ["sessions.db"]);

    let adapter = fx.adapter();
    assert_eq!(adapter.scan().unwrap().len(), 1);
    assert_eq!(fx.load().len(), 1);
    assert_eq!(side_files(&fx.db), ["sessions.db"]);
}

#[test]
fn reads_messages_still_in_the_write_ahead_log() {
    let fx = Fixture::new();
    let writer = fx.conn();
    let mode: String = writer
        .query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    // Goose is still running: its connection is open and this session has not
    // been checkpointed into the main file.
    writer
        .execute_batch(
            "INSERT INTO sessions (id, name, working_dir) VALUES ('live', 'Live session', '/work/app');
             INSERT INTO messages (session_id, role, content_json, created_timestamp)
                 VALUES ('live', 'user', '[{\"type\":\"text\",\"text\":\"Prompt\"}]', 1);",
        )
        .unwrap();

    assert_eq!(ids(&fx.load()), ["live"]);
    drop(writer);
}

#[test]
fn incremental_reports_new_changed_and_deleted_sessions() {
    let fx = Fixture::new();
    fx.simple("a", "A");
    fx.simple("b", "B");
    let adapter = fx.adapter();
    let mtime = adapter.scan().unwrap()["a"].1;

    let known: KnownSessions = [
        (SessionKey::new(AgentId::Goose, "a"), mtime),
        (SessionKey::new(AgentId::Goose, "b"), mtime - 1),
        (SessionKey::new(AgentId::Goose, "gone"), mtime),
        (SessionKey::new(AgentId::Claude, "other-agent"), mtime),
    ]
    .into_iter()
    .collect();

    let mut streamed = Vec::new();
    let result = adapter
        .find_incremental(&known, &mut |s| streamed.push(s.id.clone()))
        .unwrap();
    assert_eq!(ids(&result.upserts), ["b"], "only the stale one is re-read");
    assert_eq!(streamed, ["b"]);
    assert_eq!(result.deleted, ["gone"]);
}

#[test]
fn a_missing_database_removes_its_sessions_and_a_broken_one_keeps_them() {
    let tmp = tempfile::tempdir().unwrap();
    let known: KnownSessions = [(SessionKey::new(AgentId::Goose, "a"), 1)]
        .into_iter()
        .collect();

    let missing = GooseAdapter::with_db_path(tmp.path().join("sessions.db"));
    assert!(!missing.is_available());
    let result = missing.find_incremental(&known, &mut |_| {}).unwrap();
    assert_eq!(result.deleted, ["a"]);

    let broken_path = tmp.path().join("broken.db");
    fs::write(&broken_path, "this is not a database").unwrap();
    let broken = GooseAdapter::with_db_path(broken_path);
    assert!(broken.is_available());
    let result = broken.find_incremental(&known, &mut |_| {}).unwrap();
    assert!(result.upserts.is_empty());
    assert!(
        result.deleted.is_empty(),
        "an unreadable store drops nothing"
    );
    assert!(broken.scan().unwrap().is_empty());
}

#[test]
fn resume_and_fork_commands() {
    let fx = Fixture::new();
    fx.simple("20260301_1", "Session");
    let adapter = fx.adapter();
    let session = &fx.load()[0];

    // The program is `goose`, or the full path of the copy inside the desktop
    // app when `goose` is not on PATH.
    let resume = adapter.resume_command(session, false);
    assert!(resume[0].ends_with("goose"), "{resume:?}");
    assert_eq!(
        resume[1..],
        ["session", "--resume", "--session-id", "20260301_1"]
    );
    assert_eq!(adapter.resume_command(session, true), resume);
    assert!(!adapter.supports_yolo());

    assert!(adapter.supports_fork());
    let fork = adapter
        .fork_command(session, None, Some("ignored name"), false)
        .unwrap();
    assert_eq!(fork[0], resume[0]);
    assert_eq!(
        fork[1..],
        [
            "session",
            "--resume",
            "--session-id",
            "20260301_1",
            "--fork"
        ]
    );
}

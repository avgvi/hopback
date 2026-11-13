//! Fixture tests for the two SQLite layouts OpenCode has used:
//! 1.x (`session`, `message`, `part`) and 2.x (`session_v2`,
//! `session_message`, with the text of a turn inline in the message row).

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use hopback::adapters::opencode::{default_db_path, load_db_sessions, OpenCodeAdapter};
use hopback::adapters::{Adapter, KnownSessions};
use hopback::config::AgentId;
use hopback::model::{Session, SessionKey};
use rusqlite::{params, Connection};
use serde_json::{json, Value};

const V2_SCHEMA: &str = "
    CREATE TABLE session_v2 (
        id TEXT PRIMARY KEY,
        project_id TEXT NOT NULL,
        workspace_id TEXT,
        parent_id TEXT,
        fork_session_id TEXT,
        slug TEXT NOT NULL,
        directory TEXT NOT NULL,
        path TEXT,
        title TEXT,
        version TEXT NOT NULL,
        time_created INTEGER NOT NULL,
        time_updated INTEGER NOT NULL,
        time_archived INTEGER
    );
    CREATE TABLE session_message (
        id TEXT PRIMARY KEY,
        session_id TEXT NOT NULL,
        type TEXT NOT NULL,
        seq INTEGER NOT NULL,
        time_created INTEGER NOT NULL,
        time_updated INTEGER NOT NULL,
        data TEXT NOT NULL
    );
    CREATE UNIQUE INDEX session_message_session_seq_idx ON session_message (session_id, seq);
    CREATE TABLE session_part (
        message_id TEXT NOT NULL,
        session_id TEXT NOT NULL,
        ordinal INTEGER NOT NULL,
        type TEXT NOT NULL,
        call_id TEXT,
        time_created INTEGER NOT NULL,
        time_updated INTEGER NOT NULL,
        data TEXT NOT NULL,
        PRIMARY KEY (message_id, ordinal)
    );";

const V1_SCHEMA: &str = "
    CREATE TABLE session (
        id TEXT PRIMARY KEY,
        project_id TEXT NOT NULL,
        parent_id TEXT,
        slug TEXT NOT NULL,
        directory TEXT NOT NULL,
        title TEXT NOT NULL,
        version TEXT NOT NULL,
        time_created INTEGER NOT NULL,
        time_updated INTEGER NOT NULL,
        time_archived INTEGER
    );
    CREATE TABLE message (
        id TEXT PRIMARY KEY,
        session_id TEXT NOT NULL,
        time_created INTEGER NOT NULL,
        time_updated INTEGER NOT NULL,
        data TEXT NOT NULL
    );
    CREATE TABLE part (
        id TEXT PRIMARY KEY,
        message_id TEXT NOT NULL,
        session_id TEXT NOT NULL,
        time_created INTEGER NOT NULL,
        time_updated INTEGER NOT NULL,
        data TEXT NOT NULL
    );";

const CREATED_MS: i64 = 1_790_000_000_000;
const UPDATED_MS: i64 = 1_790_000_600_500;

fn new_db(schema: &str) -> (tempfile::TempDir, PathBuf, Connection) {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("opencode.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(schema).unwrap();
    (tmp, path, conn)
}

fn v2_session(conn: &Connection, id: &str, title: Option<&str>, directory: &str) {
    conn.execute(
        "INSERT INTO session_v2
            (id, project_id, slug, directory, title, version, time_created, time_updated)
         VALUES (?1, 'prj_1', 'calm-otter', ?2, ?3, '2.0.0', ?4, ?5)",
        params![id, directory, title, CREATED_MS, UPDATED_MS],
    )
    .unwrap();
}

/// Appends one `session_message` row, numbering `seq` per session.
fn v2_message(conn: &Connection, session_id: &str, kind: &str, data: Value) {
    let seq: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM session_message WHERE session_id = ?1",
            [session_id],
            |row| row.get(0),
        )
        .unwrap();
    conn.execute(
        "INSERT INTO session_message
            (id, session_id, type, seq, time_created, time_updated, data)
         VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6)",
        params![
            format!("msg_{session_id}_{seq}"),
            session_id,
            kind,
            seq,
            CREATED_MS + seq,
            data.to_string()
        ],
    )
    .unwrap();
}

fn v2_user(conn: &Connection, session_id: &str, text: &str) {
    v2_message(
        conn,
        session_id,
        "user",
        json!({"time": {"created": CREATED_MS}, "text": text, "files": [], "agents": []}),
    );
}

fn v2_assistant(conn: &Connection, session_id: &str, content: Value) {
    v2_message(
        conn,
        session_id,
        "assistant",
        json!({
            "time": {"created": CREATED_MS},
            "agent": "build",
            "model": {"id": "model-x"},
            "content": content,
            "finish": "stop"
        }),
    );
}

fn v1_session(conn: &Connection, id: &str, parent: Option<&str>, title: &str, directory: &str) {
    conn.execute(
        "INSERT INTO session
            (id, project_id, parent_id, slug, directory, title, version, time_created, time_updated)
         VALUES (?1, 'prj_1', ?2, 'calm-otter', ?3, ?4, '1.0.0', ?5, ?6)",
        params![id, parent, directory, title, CREATED_MS, UPDATED_MS],
    )
    .unwrap();
}

/// Inserts one 1.x message and its parts. `at` orders messages in a session.
fn v1_message(conn: &Connection, session_id: &str, at: i64, role: &str, parts: &[Value]) {
    let message_id = format!("msg_{session_id}_{at}");
    conn.execute(
        "INSERT INTO message (id, session_id, time_created, time_updated, data)
         VALUES (?1, ?2, ?3, ?3, ?4)",
        params![
            message_id,
            session_id,
            at,
            json!({"role": role}).to_string()
        ],
    )
    .unwrap();
    for (n, part) in parts.iter().enumerate() {
        conn.execute(
            "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data)
             VALUES (?1, ?2, ?3, ?4, ?4, ?5)",
            params![
                format!("prt_{session_id}_{at}_{n}"),
                message_id,
                session_id,
                at * 10 + n as i64,
                part.to_string()
            ],
        )
        .unwrap();
    }
}

fn text_part(text: &str) -> Value {
    json!({"type": "text", "text": text})
}

/// A 2.x database with one session of every kind the adapter must tell apart.
fn v2_fixture() -> (tempfile::TempDir, PathBuf, OpenCodeAdapter) {
    let (tmp, path, conn) = new_db(V2_SCHEMA);

    v2_session(
        &conn,
        "ses_main",
        Some("Add retry to uploader"),
        "/work/app",
    );
    v2_user(&conn, "ses_main", "add a retry to the uploader");
    v2_message(
        &conn,
        "ses_main",
        "system",
        json!({"text": "SYSTEM-CONTEXT", "description": "instructions"}),
    );
    v2_message(
        &conn,
        "ses_main",
        "synthetic",
        json!({"text": "SYNTHETIC-NOTE", "description": "reminder"}),
    );
    v2_assistant(
        &conn,
        "ses_main",
        json!([
            {"type": "reasoning", "text": "HIDDEN-REASONING"},
            {"type": "text", "text": "I will add a retry loop."},
            {"type": "tool", "id": "call_1", "name": "edit", "state": {"output": "TOOL-OUTPUT"}},
            {"type": "text", "text": "Done, three attempts."}
        ]),
    );
    v2_message(
        &conn,
        "ses_main",
        "compaction",
        json!({"status": "done", "summary": "COMPACTION-SUMMARY"}),
    );
    v2_message(&conn, "ses_main", "idle", json!({"outcome": "ok"}));
    // An assistant turn that only ran a tool has no text and is not counted.
    v2_assistant(
        &conn,
        "ses_main",
        json!([{"type": "tool", "id": "call_2", "name": "bash", "state": {}}]),
    );
    v2_message(&conn, "ses_main", "user", json!("not an object"));
    conn.execute(
        "INSERT INTO session_message VALUES ('msg_bad', 'ses_main', 'user', 99, 1, 1, '{broken')",
        [],
    )
    .unwrap();

    // A subagent run: has a parent, hidden from OpenCode's own session list.
    v2_session(&conn, "ses_child", Some("Explore subagent"), "/work/app");
    conn.execute(
        "UPDATE session_v2 SET parent_id = 'ses_main' WHERE id = 'ses_child'",
        [],
    )
    .unwrap();
    v2_user(&conn, "ses_child", "look for the config loader");

    // A fork is a top-level session of its own.
    v2_session(&conn, "ses_fork", Some("Fork of retry work"), "/work/app");
    conn.execute(
        "UPDATE session_v2 SET fork_session_id = 'ses_main' WHERE id = 'ses_fork'",
        [],
    )
    .unwrap();
    v2_user(&conn, "ses_fork", "try exponential backoff instead");

    v2_session(&conn, "ses_untitled", None, "/work/lib");
    v2_user(&conn, "ses_untitled", "  rename   the\nparser module  ");
    v2_assistant(
        &conn,
        "ses_untitled",
        json!([{"type": "text", "text": "Renamed."}]),
    );

    v2_session(&conn, "ses_archived", Some("Old cleanup"), "/work/app");
    conn.execute(
        "UPDATE session_v2 SET time_archived = ?1 WHERE id = 'ses_archived'",
        [UPDATED_MS],
    )
    .unwrap();
    v2_user(&conn, "ses_archived", "remove the dead flags");

    // No title and no prompt: nothing to list.
    v2_session(&conn, "ses_blank", None, "/work/app");
    v2_session(&conn, "ses_blank_title", Some("   "), "/work/app");
    v2_assistant(
        &conn,
        "ses_blank_title",
        json!([{"type": "text", "text": "an answer with no question"}]),
    );

    // Titled but never used: still listed, with no messages.
    v2_session(&conn, "ses_titled_empty", Some("New session"), "/work/app");

    drop(conn);
    let adapter = OpenCodeAdapter::with_db_path(path.clone());
    (tmp, path, adapter)
}

fn all_sessions(adapter: &OpenCodeAdapter) -> Vec<Session> {
    let mut sessions = adapter
        .find_incremental(&KnownSessions::new(), &mut |_| {})
        .unwrap()
        .upserts;
    sessions.sort_by(|a, b| a.id.cmp(&b.id));
    sessions
}

fn ids(sessions: &[Session]) -> Vec<&str> {
    sessions.iter().map(|s| s.id.as_str()).collect()
}

fn by_id<'a>(sessions: &'a [Session], id: &str) -> &'a Session {
    sessions
        .iter()
        .find(|s| s.id == id)
        .unwrap_or_else(|| panic!("session {id} not found"))
}

fn known_from(sessions: &[Session]) -> KnownSessions {
    sessions.iter().map(|s| (s.key(), s.mtime)).collect()
}

fn mtime_nanos(path: &Path) -> i64 {
    let modified = fs::metadata(path).unwrap().modified().unwrap();
    modified.duration_since(UNIX_EPOCH).unwrap().as_nanos() as i64
}

fn set_mtime(path: &Path, secs: u64) -> i64 {
    let time = UNIX_EPOCH + Duration::from_secs(secs);
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(time)
        .unwrap();
    assert_eq!(fs::metadata(path).unwrap().modified().unwrap(), time);
    Duration::from_secs(secs).as_nanos() as i64
}

fn wal_path(db: &Path) -> PathBuf {
    let mut name = db.as_os_str().to_owned();
    name.push("-wal");
    PathBuf::from(name)
}

#[test]
fn v2_scan_lists_top_level_sessions_only() {
    let (_tmp, path, adapter) = v2_fixture();
    assert!(adapter.is_available());
    assert!(!adapter.mtime_is_per_session());

    let scanned = adapter.scan().unwrap();
    let mut found: Vec<&str> = scanned.keys().map(String::as_str).collect();
    found.sort();
    assert_eq!(
        found,
        vec![
            "ses_archived",
            "ses_fork",
            "ses_main",
            "ses_titled_empty",
            "ses_untitled"
        ]
    );
    // Every session shares the one database file and its change time.
    let db_mtime = mtime_nanos(&path);
    for (file, mtime) in scanned.values() {
        assert_eq!(file, &path);
        assert_eq!(*mtime, db_mtime);
    }
    // Sessions come from the database, not from per-session files.
    assert!(adapter.parse(&path).unwrap().is_none());
}

#[test]
fn v2_parses_roles_text_title_and_times() {
    let (_tmp, path, adapter) = v2_fixture();
    let sessions = all_sessions(&adapter);
    assert_eq!(
        ids(&sessions),
        vec![
            "ses_archived",
            "ses_fork",
            "ses_main",
            "ses_titled_empty",
            "ses_untitled"
        ]
    );

    let main = by_id(&sessions, "ses_main");
    assert_eq!(main.agent, AgentId::OpenCode);
    assert_eq!(main.title, "Add retry to uploader");
    assert_eq!(main.directory, "/work/app");
    assert_eq!(main.timestamp, UPDATED_MS / 1000);
    assert_eq!(main.mtime, mtime_nanos(&path));
    assert_eq!(main.source_path.as_deref(), path.to_str());
    assert!(!main.yolo);
    // One user prompt and one assistant reply have text. The tool-only turn,
    // the system, synthetic, compaction and idle rows and the two unreadable
    // rows do not count.
    assert_eq!(main.message_count, 2);
    assert_eq!(
        main.content,
        "» add a retry to the uploader\n\n  I will add a retry loop.\nDone, three attempts."
    );
    for hidden in [
        "HIDDEN-REASONING",
        "TOOL-OUTPUT",
        "SYSTEM-CONTEXT",
        "SYNTHETIC-NOTE",
        "COMPACTION-SUMMARY",
    ] {
        assert!(!main.content.contains(hidden), "{hidden} leaked");
    }

    // No stored title: the first prompt, with whitespace collapsed.
    let untitled = by_id(&sessions, "ses_untitled");
    assert_eq!(untitled.title, "rename the parser module");
    assert_eq!(untitled.directory, "/work/lib");
    assert_eq!(untitled.message_count, 2);

    let empty = by_id(&sessions, "ses_titled_empty");
    assert_eq!(empty.title, "New session");
    assert_eq!(empty.message_count, 0);
    assert_eq!(empty.content, "");

    assert_eq!(by_id(&sessions, "ses_fork").message_count, 1);
    assert_eq!(by_id(&sessions, "ses_archived").title, "Old cleanup");
}

#[test]
fn v2_long_first_prompt_is_truncated_for_the_title() {
    let (_tmp, path, conn) = new_db(V2_SCHEMA);
    v2_session(&conn, "ses_long", None, "/work/app");
    v2_user(&conn, "ses_long", &"word ".repeat(60));
    drop(conn);

    let sessions = all_sessions(&OpenCodeAdapter::with_db_path(path));
    let title = &sessions[0].title;
    assert_eq!(title.chars().count(), 100);
    assert!(title.ends_with("..."));
}

#[test]
fn v2_find_incremental_reports_changes_and_deletions() {
    let (_tmp, path, adapter) = v2_fixture();

    let mut streamed = Vec::new();
    let first = adapter
        .find_incremental(&KnownSessions::new(), &mut |s| streamed.push(s.id.clone()))
        .unwrap();
    assert_eq!(first.upserts.len(), 5);
    assert_eq!(streamed.len(), 5);
    assert!(first.deleted.is_empty());

    // Nothing changed: nothing to do.
    let mut known = known_from(&first.upserts);
    let second = adapter.find_incremental(&known, &mut |_| {}).unwrap();
    assert!(second.upserts.is_empty());
    assert!(second.deleted.is_empty());

    // Known ids that are gone, or that are now filtered out, are deleted.
    // Another agent's sessions are left alone.
    known.insert(SessionKey::new(AgentId::OpenCode, "ses_gone"), 1);
    known.insert(SessionKey::new(AgentId::OpenCode, "ses_child"), 1);
    known.insert(SessionKey::new(AgentId::Claude, "ses_other_agent"), 1);
    let third = adapter.find_incremental(&known, &mut |_| {}).unwrap();
    let deleted: HashSet<&str> = third.deleted.iter().map(String::as_str).collect();
    assert_eq!(deleted, HashSet::from(["ses_gone", "ses_child"]));
    assert!(third.upserts.is_empty());

    // The database is one file, so a newer file re-reads every session.
    let conn = Connection::open(&path).unwrap();
    v2_user(&conn, "ses_fork", "and cap it at thirty seconds");
    conn.execute("DELETE FROM session_v2 WHERE id = 'ses_archived'", [])
        .unwrap();
    drop(conn);
    let bumped = set_mtime(&path, 4_000_000_000);

    let known = known_from(&first.upserts);
    let fourth = adapter.find_incremental(&known, &mut |_| {}).unwrap();
    assert_eq!(fourth.upserts.len(), 4);
    assert!(fourth.upserts.iter().all(|s| s.mtime == bumped));
    let fork = by_id(&fourth.upserts, "ses_fork");
    assert_eq!(fork.message_count, 2);
    assert!(fork.content.ends_with("» and cap it at thirty seconds"));
    assert_eq!(fourth.deleted, vec!["ses_archived".to_string()]);
}

#[test]
fn wal_only_writes_change_the_mtime_and_are_read() {
    let (_tmp, path, conn) = new_db(V2_SCHEMA);
    let mode: String = conn
        .query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    // Keep everything in the WAL, as while OpenCode is running.
    conn.execute_batch("PRAGMA wal_autocheckpoint = 0;")
        .unwrap();
    v2_session(&conn, "ses_a", Some("First"), "/work/app");
    v2_user(&conn, "ses_a", "first prompt");

    let wal = wal_path(&path);
    assert!(wal.is_file());
    let db_time = set_mtime(&path, 1_800_000_000);
    let wal_time = set_mtime(&wal, 1_800_000_060);

    let adapter = OpenCodeAdapter::with_db_path(path.clone());
    let scanned = adapter.scan().unwrap();
    assert_eq!(scanned["ses_a"].1, wal_time);
    let known: KnownSessions = [(SessionKey::new(AgentId::OpenCode, "ses_a"), wal_time)]
        .into_iter()
        .collect();
    let unchanged = adapter.find_incremental(&known, &mut |_| {}).unwrap();
    assert!(unchanged.upserts.is_empty());

    // A new session that exists only in the WAL: the main file is untouched.
    v2_session(&conn, "ses_b", Some("Second"), "/work/app");
    v2_user(&conn, "ses_b", "second prompt");
    assert_eq!(mtime_nanos(&path), db_time);
    let newer_wal_time = set_mtime(&wal, 1_800_000_120);

    let changed = adapter.find_incremental(&known, &mut |_| {}).unwrap();
    let mut upserts = changed.upserts;
    upserts.sort_by(|a, b| a.id.cmp(&b.id));
    assert_eq!(ids(&upserts), vec!["ses_a", "ses_b"]);
    assert!(upserts.iter().all(|s| s.mtime == newer_wal_time));
    assert_eq!(by_id(&upserts, "ses_b").content, "» second prompt");
    assert!(changed.deleted.is_empty());

    // Reading left the database alone.
    assert_eq!(mtime_nanos(&path), db_time);
    drop(conn);
}

#[test]
fn v1_schema_is_still_read() {
    let (_tmp, path, conn) = new_db(V1_SCHEMA);
    v1_session(&conn, "ses_v1_main", None, "Fix flaky test", "/work/old");
    v1_message(
        &conn,
        "ses_v1_main",
        1,
        "user",
        &[text_part("why does the date test fail")],
    );
    v1_message(
        &conn,
        "ses_v1_main",
        2,
        "assistant",
        &[
            json!({"type": "step-start"}),
            text_part("It depends on the local timezone."),
            json!({"type": "tool", "tool": "bash", "text": "TOOL-OUTPUT"}),
            text_part("Pin it to UTC."),
        ],
    );
    // Tool-only turns and unknown roles are not messages.
    v1_message(
        &conn,
        "ses_v1_main",
        3,
        "assistant",
        &[json!({"type": "tool", "tool": "edit"})],
    );
    v1_message(&conn, "ses_v1_main", 4, "system", &[text_part("SYSTEM")]);

    v1_session(
        &conn,
        "ses_v1_child",
        Some("ses_v1_main"),
        "Subagent",
        "/work/old",
    );
    v1_message(&conn, "ses_v1_child", 1, "user", &[text_part("search")]);

    // 1.x titles are NOT NULL; an empty one falls back to the first prompt.
    v1_session(&conn, "ses_v1_untitled", None, "", "/work/old");
    v1_message(
        &conn,
        "ses_v1_untitled",
        1,
        "user",
        &[text_part("bump the version")],
    );
    v1_session(&conn, "ses_v1_blank", None, "", "/work/old");
    drop(conn);

    let adapter = OpenCodeAdapter::with_db_path(path.clone());
    let mut scanned: Vec<String> = adapter.scan().unwrap().into_keys().collect();
    scanned.sort();
    assert_eq!(scanned, vec!["ses_v1_main", "ses_v1_untitled"]);

    let sessions = all_sessions(&adapter);
    assert_eq!(ids(&sessions), vec!["ses_v1_main", "ses_v1_untitled"]);
    let main = by_id(&sessions, "ses_v1_main");
    assert_eq!(main.title, "Fix flaky test");
    assert_eq!(main.directory, "/work/old");
    assert_eq!(main.timestamp, UPDATED_MS / 1000);
    assert_eq!(main.message_count, 2);
    assert_eq!(
        main.content,
        "» why does the date test fail\n\n  It depends on the local timezone.\nPin it to UTC."
    );
    assert_eq!(
        by_id(&sessions, "ses_v1_untitled").title,
        "bump the version"
    );
}

#[test]
fn half_migrated_database_reads_each_session_once() {
    let (_tmp, path, conn) = new_db(V1_SCHEMA);
    conn.execute_batch(V2_SCHEMA).unwrap();
    conn.execute_batch("CREATE TABLE kv (key TEXT PRIMARY KEY, value TEXT NOT NULL);")
        .unwrap();

    // Three 1.x sessions. OpenCode 2 copies newest id first and has reached
    // `ses_b`, so `ses_a` is still only in the 1.x tables.
    for id in ["ses_a", "ses_b", "ses_c"] {
        v1_session(&conn, id, None, &format!("{id} title"), "/work/old");
        v1_message(&conn, id, 1, "user", &[text_part("old prompt")]);
    }
    for id in ["ses_b", "ses_c"] {
        v2_session(&conn, id, Some(&format!("{id} title")), "/work/old");
        v2_user(&conn, id, "old prompt");
    }
    v2_user(&conn, "ses_c", "prompt written by OpenCode 2");
    let set_state = |state: Value| {
        conn.execute(
            "INSERT OR REPLACE INTO kv (key, value) VALUES ('migration.v1-v2', ?1)",
            [state.to_string()],
        )
        .unwrap();
    };
    set_state(json!({"phase": "sessions", "cursor": "ses_b"}));

    let adapter = OpenCodeAdapter::with_db_path(path.clone());
    let sessions = all_sessions(&adapter);
    assert_eq!(ids(&sessions), vec!["ses_a", "ses_b", "ses_c"]);
    assert_eq!(by_id(&sessions, "ses_a").message_count, 1);
    // The copied session is read from the 2.x tables, not doubled.
    assert_eq!(by_id(&sessions, "ses_c").message_count, 2);

    // After the copy, a session deleted in OpenCode 2 stays deleted even
    // though its 1.x rows are still there.
    v2_session(&conn, "ses_a", Some("ses_a title"), "/work/old");
    conn.execute("DELETE FROM session_v2 WHERE id = 'ses_b'", [])
        .unwrap();
    set_state(json!({"phase": "completed"}));
    let sessions = all_sessions(&adapter);
    assert_eq!(ids(&sessions), vec!["ses_a", "ses_c"]);
}

#[test]
fn resume_and_fork_commands_use_the_scanned_session() {
    let (_tmp, _path, adapter) = v2_fixture();
    let sessions = all_sessions(&adapter);
    let untitled = by_id(&sessions, "ses_untitled");

    assert_eq!(
        adapter.resume_command(untitled, false),
        vec!["opencode", "/work/lib", "--session", "ses_untitled"]
    );
    // OpenCode has no flag hopback maps yolo to.
    assert!(!adapter.supports_yolo());
    assert_eq!(
        adapter.resume_command(untitled, true),
        adapter.resume_command(untitled, false)
    );

    let mut no_directory = untitled.clone();
    no_directory.directory.clear();
    assert_eq!(
        adapter.resume_command(&no_directory, false),
        vec!["opencode", "--session", "ses_untitled"]
    );

    assert!(adapter.supports_fork());
    assert_eq!(
        adapter
            .fork_command(untitled, None, Some("try again"), false)
            .unwrap(),
        vec![
            "opencode",
            "run",
            "--session",
            "ses_untitled",
            "--fork",
            "--title",
            "try again"
        ]
    );
}

#[test]
fn missing_database_drops_only_this_agents_sessions() {
    let tmp = tempfile::tempdir().unwrap();
    let adapter = OpenCodeAdapter::with_db_path(tmp.path().join("opencode.db"));
    assert!(!adapter.is_available());
    assert!(adapter.scan().unwrap().is_empty());

    let known: KnownSessions = [
        (SessionKey::new(AgentId::OpenCode, "ses_old"), 1),
        (SessionKey::new(AgentId::Claude, "other"), 1),
    ]
    .into_iter()
    .collect();
    let res = adapter.find_incremental(&known, &mut |_| {}).unwrap();
    assert!(res.upserts.is_empty());
    assert_eq!(res.deleted, vec!["ses_old".to_string()]);
}

#[test]
fn unreadable_database_keeps_known_sessions() {
    let tmp = tempfile::tempdir().unwrap();
    let known: KnownSessions = [(SessionKey::new(AgentId::OpenCode, "ses_old"), 1)]
        .into_iter()
        .collect();

    // Not a SQLite file at all.
    let garbage = tmp.path().join("garbage.db");
    fs::write(&garbage, "this is not a sqlite database, just some text\n").unwrap();
    // A SQLite file without any OpenCode session table.
    let foreign = tmp.path().join("foreign.db");
    Connection::open(&foreign)
        .unwrap()
        .execute_batch("CREATE TABLE notes (id TEXT);")
        .unwrap();

    for path in [garbage, foreign] {
        let adapter = OpenCodeAdapter::with_db_path(path.clone());
        assert!(adapter.is_available());
        assert!(adapter.scan().unwrap().is_empty());
        assert!(load_db_sessions(&path, 7).is_empty());
        let res = adapter.find_incremental(&known, &mut |_| {}).unwrap();
        assert!(res.upserts.is_empty());
        assert!(res.deleted.is_empty());
    }
}

#[test]
fn load_db_sessions_stamps_mtime_and_source_path() {
    let (_tmp, path, _adapter) = v2_fixture();
    let sessions = load_db_sessions(&path, 42);
    assert_eq!(sessions.len(), 5);
    for session in &sessions {
        assert_eq!(session.mtime, 42);
        assert_eq!(session.source_path.as_deref(), path.to_str());
    }
    assert!(default_db_path().ends_with(".local/share/opencode/opencode.db"));
}

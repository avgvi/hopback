//! Fixture tests for Cursor CLI chats: one SQLite file per chat at
//! `~/.cursor/chats/<workspace>/<chat id>/store.db`, with a `meta` and a
//! `blobs` table.
//!
//! No real Cursor store was available when these were written. The fixtures
//! cover a plain key-value layout plus the shapes reported from real stores:
//! `meta(key, value)` holding hex-encoded JSON, `blobs(id, data)` mixing JSON
//! messages with binary records, and workspace directories named by a hash.

use std::cell::Cell;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use hopback::adapters::cursor::CursorAdapter;
use hopback::adapters::{mtime_nanos, parse_iso8601_secs, Adapter, KnownSessions};
use hopback::config::AgentId;
use hopback::model::{Session, SessionKey};
use rusqlite::{Connection, ToSql};
use serde_json::{json, Value};

const CHAT_ID: &str = "027f8b2f-d09c-4a69-98b0-b53f0118605d";
const OTHER_ID: &str = "5d1c0a52-7e0b-4f0e-9a43-2f6f8f3a9c11";
const HASHED_WORKSPACE: &str = "9b74c9897bac770ffc029102a200c5de";
const ENCODED_WORKSPACE: &str = "%2Fwork%2Fapp";
const CREATED_MS: i64 = 1_784_282_400_000;
const CREATED_SECS: i64 = 1_784_282_400;

#[derive(Clone, Copy)]
enum Schema {
    /// Both tables with `key`/`value` columns.
    KeyValue,
    /// The layout reported from real stores.
    Reported,
    /// Both tables with `id`/`data` columns.
    IdData,
}

impl Schema {
    fn sql(self) -> &'static str {
        match self {
            Schema::KeyValue => {
                "CREATE TABLE meta (key TEXT, value BLOB);
                 CREATE TABLE blobs (key TEXT, value BLOB);"
            }
            Schema::Reported => {
                "CREATE TABLE blobs (id TEXT PRIMARY KEY, data BLOB);
                 CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT);"
            }
            Schema::IdData => {
                "CREATE TABLE meta (id TEXT PRIMARY KEY, data BLOB);
                 CREATE TABLE blobs (id TEXT PRIMARY KEY, data BLOB);"
            }
        }
    }

    fn meta_columns(self) -> &'static str {
        match self {
            Schema::KeyValue | Schema::Reported => "key, value",
            Schema::IdData => "id, data",
        }
    }

    fn blob_columns(self) -> &'static str {
        match self {
            Schema::KeyValue => "key, value",
            Schema::Reported | Schema::IdData => "id, data",
        }
    }
}

struct Store {
    conn: Connection,
    path: PathBuf,
    schema: Schema,
    rows: Cell<u32>,
}

impl Store {
    fn create(root: &Path, workspace: &str, id: &str, schema: Schema) -> Store {
        let dir = root.join(workspace).join(id);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("store.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(schema.sql()).unwrap();
        Store {
            conn,
            path,
            schema,
            rows: Cell::new(0),
        }
    }

    fn insert(&self, table: &str, columns: &str, value: &dyn ToSql) {
        let key = self.rows.get().to_string();
        self.rows.set(self.rows.get() + 1);
        self.conn
            .execute(
                &format!("INSERT INTO {table} ({columns}) VALUES (?1, ?2)"),
                rusqlite::params![key, value],
            )
            .unwrap();
    }

    fn meta(&self, value: impl ToSql) -> &Store {
        self.insert("meta", self.schema.meta_columns(), &value);
        self
    }

    /// A blob row holding anything: text, bytes, a number.
    fn blob(&self, value: impl ToSql) -> &Store {
        self.insert("blobs", self.schema.blob_columns(), &value);
        self
    }

    /// A blob row holding a JSON record as bytes, as Cursor stores them.
    fn record(&self, value: Value) -> &Store {
        self.blob(value.to_string().into_bytes())
    }
}

fn hex(text: &str) -> String {
    text.bytes().map(|b| format!("{b:02x}")).collect()
}

fn user(text: &str) -> Value {
    json!({"role": "user", "content": [{"type": "text", "text": text}]})
}

fn assistant(text: &str) -> Value {
    json!({"role": "assistant", "content": [{"type": "text", "text": text}]})
}

fn adapter(root: &Path) -> CursorAdapter {
    CursorAdapter::with_chats_dir(root.to_path_buf())
}

fn parse(path: &Path) -> Option<Session> {
    CursorAdapter::with_chats_dir(PathBuf::from("/nonexistent"))
        .parse(path)
        .unwrap()
}

fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    names
}

#[test]
fn key_value_store_with_plain_json_meta() {
    let tmp = tempfile::tempdir().unwrap();
    let id = "cursor-session-id";
    let store = Store::create(tmp.path(), ENCODED_WORKSPACE, id, Schema::KeyValue);
    store.meta(json!({"createdAt": CREATED_MS, "name": "Cursor adapter work"}).to_string());
    store
        .blob(json!({"id": "u1", "role": "user", "content": [{"type": "text", "text": "Add Cursor support"}]}).to_string())
        .blob(json!({"id": "a1", "role": "assistant", "content": [{"type": "text", "text": "Added the adapter"}]}).to_string())
        .blob(format!("legacy plain prompt${CHAT_ID}"));
    let path = store.path.clone();
    drop(store);

    let adapter = adapter(tmp.path());
    let scan = adapter.scan().unwrap();
    assert_eq!(scan.keys().collect::<Vec<_>>(), vec![id]);
    let (scanned_path, scanned_mtime) = &scan[id];
    assert_eq!(scanned_path, &path);

    let s = adapter.parse(scanned_path).unwrap().expect("a session");
    assert_eq!(s.id, id);
    assert_eq!(s.agent, AgentId::Cursor);
    assert_eq!(s.title, "Cursor adapter work");
    assert_eq!(s.directory, "/work/app", "decoded from the workspace name");
    assert_eq!(s.timestamp, CREATED_SECS);
    assert_eq!(s.message_count, 3, "two prompts and one reply");
    assert_eq!(
        s.content,
        "» Add Cursor support\n\n  Added the adapter\n\n» legacy plain prompt"
    );
    assert_eq!(s.mtime, *scanned_mtime);
    assert!(!s.yolo);
    assert_eq!(s.source_path.as_deref(), path.to_str());

    assert_eq!(
        adapter.resume_command(&s, false),
        vec!["agent", "--resume", id]
    );
    assert_eq!(
        adapter.resume_command(&s, true),
        vec!["agent", "--yolo", "--resume", id]
    );
}

#[test]
fn reported_schema_with_hex_encoded_meta_and_binary_blobs() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::create(tmp.path(), HASHED_WORKSPACE, CHAT_ID, Schema::Reported);
    let meta = json!({
        "agentId": CHAT_ID,
        "latestRootBlobId": "f3a1",
        "name": "Hex titled chat",
        "mode": "default",
        "createdAt": CREATED_MS,
        "lastUsedModel": "model-x"
    });
    store.meta(hex(&meta.to_string()));
    store
        .record(json!({"role": "system", "content": "You are a coding agent."}))
        .record(user("rename the config loader"))
        // A binary node that references other blobs.
        .blob(vec![0x0a, 0x20, 0xff, 0xfe, 0x00, 0x12, 0x9c, 0x01])
        .blob(vec![0x0a, 0x05, b'{', 0xff, b'}', 0x12])
        .record(json!({"role": "assistant", "content": [
            {"type": "tool-call", "toolCallId": "c1", "toolName": "edit", "args": {"path": "a.rs"}},
            {"type": "text", "text": "Renamed it."}
        ]}))
        .record(json!({"role": "tool", "name": "edit", "content": [
            {"type": "tool-result", "toolCallId": "c1", "result": "tool output"}
        ]}));
    let path = store.path.clone();
    drop(store);

    let s = parse(&path).expect("a session");
    assert_eq!(s.id, CHAT_ID);
    assert_eq!(s.title, "Hex titled chat");
    assert_eq!(s.timestamp, CREATED_SECS);
    assert_eq!(s.directory, "", "a hashed workspace name is not a path");
    assert_eq!(s.message_count, 2);
    assert_eq!(s.content, "» rename the config loader\n\n  Renamed it.");
}

#[test]
fn hex_encoded_meta_stored_as_a_blob_in_id_data_columns() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::create(tmp.path(), HASHED_WORKSPACE, CHAT_ID, Schema::IdData);
    let meta = json!({"title": "Blob hex title", "createdAt": CREATED_SECS, "cwd": "/work/hex"});
    store.meta(hex(&meta.to_string()).to_uppercase().into_bytes());
    store.record(user("hello")).record(assistant("hi"));
    let path = store.path.clone();
    drop(store);

    let s = parse(&path).expect("a session");
    assert_eq!(s.title, "Blob hex title");
    assert_eq!(s.directory, "/work/hex");
    assert_eq!(s.timestamp, CREATED_SECS, "small numbers are seconds");
    assert_eq!(s.message_count, 2);
}

#[test]
fn meta_json_wrapped_in_a_json_string_is_unwrapped() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::create(tmp.path(), HASHED_WORKSPACE, CHAT_ID, Schema::Reported);
    let meta = json!({"name": "Quoted hex", "createdAt": CREATED_MS});
    store.meta(Value::String(hex(&meta.to_string())).to_string());
    store.record(user("hello"));
    let path = store.path.clone();
    drop(store);

    let s = parse(&path).expect("a session");
    assert_eq!(s.title, "Quoted hex");
    assert_eq!(s.timestamp, CREATED_SECS);
}

#[test]
fn directory_comes_from_meta_before_the_workspace_name() {
    let tmp = tempfile::tempdir().unwrap();

    let store = Store::create(tmp.path(), ENCODED_WORKSPACE, CHAT_ID, Schema::Reported);
    store
        .meta(json!({"name": ""}).to_string())
        .meta(
            json!({"cwd": "/from/meta", "workspace": "/ignored", "title": "Second row"})
                .to_string(),
        )
        .meta(json!({"cwd": "/later/row", "name": "Later row"}).to_string());
    store.record(user("hello"));
    let first = store.path.clone();
    drop(store);

    let store = Store::create(tmp.path(), ENCODED_WORKSPACE, OTHER_ID, Schema::Reported);
    store.meta(json!({"workspace": "/from/workspace"}).to_string());
    store.record(user("hello"));
    let second = store.path.clone();
    drop(store);

    let s = parse(&first).unwrap();
    assert_eq!(s.directory, "/from/meta");
    assert_eq!(s.title, "Second row", "first non-empty value wins");
    assert_eq!(parse(&second).unwrap().directory, "/from/workspace");
}

#[test]
fn user_query_tags_are_unwrapped_and_user_info_gives_the_workspace() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::create(tmp.path(), HASHED_WORKSPACE, CHAT_ID, Schema::Reported);
    store
        .record(json!({"role": "user", "content": [
            {"type": "text", "text": "<user_info>\nOS Version: darwin\nWorkspace Path: /work/tagged\nShell: zsh\n</user_info>"}
        ]}))
        .record(json!({"role": "user", "content": [
            {"type": "text", "text": "<user_info>\nOS Version: darwin\n</user_info>"},
            {"type": "text", "text": "<user_query>\nfix the flaky test\n</user_query>"}
        ]}))
        .record(assistant("Workspace Path: /not/a/prompt"))
        .record(user("mentioning Workspace Path: /typed/by/user changes nothing"));
    let path = store.path.clone();
    drop(store);

    let s = parse(&path).expect("a session");
    assert_eq!(s.title, "fix the flaky test");
    assert_eq!(s.directory, "/work/tagged");
    assert_eq!(s.message_count, 3, "the context-only message is dropped");
    assert!(s.content.starts_with("» fix the flaky test\n\n"));
    assert!(!s.content.contains("user_info"));
    assert!(!s.content.contains("user_query"));
    assert!(!s.content.contains("OS Version"));
}

#[test]
fn bubble_types_nested_lists_and_message_timestamps() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::create(tmp.path(), HASHED_WORKSPACE, CHAT_ID, Schema::KeyValue);
    let long_prompt = format!("first {}", "x".repeat(120));
    store
        .record(json!({"createdAt": CREATED_MS, "messages": [
            {"type": 1, "text": long_prompt, "timestamp": CREATED_SECS + 10},
            {"type": 2, "text": "numeric bubble reply", "timestamp": (CREATED_SECS + 20) * 1000},
            {"type": 3, "text": "some other bubble"}
        ]}))
        .record(json!({"conversation": [
            {"type": "user", "content": "string content prompt", "timestamp": "2026-07-17T10:00:50Z"},
            {"type": "ai", "content": ["part one", {"data": "part two"}]},
            {"type": "assistant", "text": "   "}
        ]}))
        .record(json!([{"role": "assistant", "content": "reply in a bare list"}]));
    let path = store.path.clone();
    drop(store);

    let s = parse(&path).expect("a session");
    assert_eq!(
        s.message_count, 5,
        "unknown types and blank text are skipped"
    );
    assert_eq!(
        s.title,
        format!("first {}...", "x".repeat(71)),
        "first prompt, cut to 80 characters"
    );
    assert_eq!(
        s.timestamp,
        parse_iso8601_secs("2026-07-17T10:00:50Z").unwrap(),
        "the latest message timestamp beats createdAt"
    );
    assert!(s.timestamp > CREATED_SECS + 20);
    assert!(s.content.contains("  numeric bubble reply"));
    assert!(s.content.contains("» string content prompt"));
    assert!(s.content.contains("  part one\npart two"));
    assert!(s.content.ends_with("  reply in a bare list"));
    assert!(!s.content.contains("some other bubble"));
}

#[test]
fn a_message_name_is_not_the_chat_title_but_a_record_title_is() {
    let tmp = tempfile::tempdir().unwrap();

    let store = Store::create(tmp.path(), HASHED_WORKSPACE, CHAT_ID, Schema::Reported);
    store
        .record(json!({"role": "tool", "name": "read_file", "content": "tool output"}))
        .record(user("the real prompt"));
    let unnamed = store.path.clone();
    drop(store);

    let store = Store::create(tmp.path(), HASHED_WORKSPACE, OTHER_ID, Schema::Reported);
    store
        .record(user("the real prompt"))
        .record(json!({"title": "Title kept in a blob", "createdAt": CREATED_MS}));
    let named = store.path.clone();
    drop(store);

    assert_eq!(parse(&unnamed).unwrap().title, "the real prompt");
    let s = parse(&named).unwrap();
    assert_eq!(s.title, "Title kept in a blob");
    assert_eq!(s.timestamp, CREATED_SECS);
}

#[test]
fn plain_user_blobs_and_json_inside_binary_framing() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::create(tmp.path(), HASHED_WORKSPACE, CHAT_ID, Schema::Reported);
    let mut framed = vec![0x0a, 0x30];
    framed.extend_from_slice(br#"{"role":"assistant","content":"embedded reply"}"#);
    framed.extend_from_slice(&[0x12, 0x00]);
    store
        .blob(format!("price is $5${CHAT_ID}"))
        .blob(format!("\0\0 padded prompt \n${CHAT_ID}\n\0").into_bytes())
        .blob("costs $5 and no id follows")
        .blob(format!("${CHAT_ID}"))
        .blob("just some text")
        .blob(42_i64)
        .blob(framed);
    let path = store.path.clone();
    drop(store);

    let s = parse(&path).expect("a session");
    assert_eq!(
        s.content,
        "» price is $5\n\n» padded prompt\n\n  embedded reply"
    );
    assert_eq!(s.message_count, 3);
    assert_eq!(s.title, "price is $5");
}

#[test]
fn timestamp_falls_back_to_the_store_mtime() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::create(tmp.path(), HASHED_WORKSPACE, CHAT_ID, Schema::Reported);
    store.meta(hex(&json!({"name": "No dates", "createdAt": 0}).to_string()));
    store.record(user("hello"));
    let path = store.path.clone();
    drop(store);

    let s = parse(&path).expect("a session");
    assert_eq!(s.mtime, mtime_nanos(&path).unwrap());
    assert_eq!(s.timestamp, s.mtime / 1_000_000_000);
}

#[test]
fn stores_without_a_prompt_are_not_sessions() {
    let tmp = tempfile::tempdir().unwrap();

    let store = Store::create(tmp.path(), "w", "only-replies", Schema::Reported);
    store.meta(json!({"name": "Named but empty"}).to_string());
    store
        .record(assistant("nobody asked"))
        .record(user("   "))
        .blob(vec![0x00, 0xff, 0x10]);
    drop(store);

    drop(Store::create(
        tmp.path(),
        "w",
        "empty-tables",
        Schema::KeyValue,
    ));

    let other = tmp.path().join("w").join("other-schema");
    fs::create_dir_all(&other).unwrap();
    // Tables with column names this adapter does not know.
    Connection::open(other.join("store.db"))
        .unwrap()
        .execute_batch(
            "CREATE TABLE meta (k TEXT, v TEXT);
             INSERT INTO meta VALUES ('0', '{\"name\":\"Unknown columns\"}');
             CREATE TABLE blobs (hash TEXT, body BLOB);
             INSERT INTO blobs VALUES ('h', '{\"role\":\"user\",\"content\":\"unreachable\"}');",
        )
        .unwrap();

    let zero = tmp.path().join("w").join("zero-bytes");
    fs::create_dir_all(&zero).unwrap();
    fs::write(zero.join("store.db"), b"").unwrap();

    let adapter = adapter(tmp.path());
    let scan = adapter.scan().unwrap();
    assert_eq!(scan.len(), 4);
    for (id, (path, _)) in &scan {
        assert!(adapter.parse(path).unwrap().is_none(), "{id} has no prompt");
    }
}

#[test]
fn a_rowid_less_blobs_table_is_still_read() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join(HASHED_WORKSPACE).join(CHAT_ID);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("store.db");
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(
        "CREATE TABLE blobs (id TEXT PRIMARY KEY, data BLOB) WITHOUT ROWID;
         CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT) WITHOUT ROWID;",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO meta VALUES ('0', ?1)",
        [hex(&json!({"name": "No rowid"}).to_string())],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO blobs VALUES ('a', ?1)",
        [user("hello").to_string().into_bytes()],
    )
    .unwrap();
    drop(conn);

    let s = parse(&path).expect("a session");
    assert_eq!(s.title, "No rowid");
    assert_eq!(s.content, "» hello");
}

#[test]
fn scan_finds_every_store_and_its_ids_match_parse() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    for (workspace, id, schema) in [
        (ENCODED_WORKSPACE, CHAT_ID, Schema::KeyValue),
        (HASHED_WORKSPACE, OTHER_ID, Schema::Reported),
        ("nested/deeper", "deep-chat", Schema::IdData),
    ] {
        let store = Store::create(root, workspace, id, schema);
        store.record(user("hello")).record(assistant("hi"));
    }

    // Things that are not chats.
    fs::write(root.join(HASHED_WORKSPACE).join("notes.db"), b"x").unwrap();
    fs::write(
        root.join(HASHED_WORKSPACE)
            .join(OTHER_ID)
            .join("store.db.bak"),
        b"x",
    )
    .unwrap();
    fs::create_dir_all(root.join("w2").join("dir-chat").join("store.db")).unwrap();

    let adapter = adapter(root);
    assert!(adapter.is_available());
    let scan = adapter.scan().unwrap();
    let mut ids: Vec<&str> = scan.keys().map(String::as_str).collect();
    ids.sort();
    assert_eq!(ids, vec![CHAT_ID, OTHER_ID, "deep-chat"]);

    for (id, (path, mtime)) in &scan {
        assert_eq!(path.file_name().unwrap(), "store.db");
        let s = adapter.parse(path).unwrap().expect("a session");
        assert_eq!(&s.id, id, "scan and parse must agree on the id");
        assert_eq!(s.mtime, *mtime);
        assert_eq!(s.source_path.as_deref(), path.to_str());
    }
}

#[test]
fn scan_mtime_is_the_newer_of_the_store_and_its_wal() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::create(tmp.path(), HASHED_WORKSPACE, CHAT_ID, Schema::Reported);
    store.record(user("hello"));
    let path = store.path.clone();
    drop(store);

    let adapter = adapter(tmp.path());
    let db_mtime = mtime_nanos(&path).unwrap();
    assert_eq!(adapter.scan().unwrap()[CHAT_ID].1, db_mtime);

    // An empty WAL: nothing to replay, but its mtime still counts.
    let wal = path.with_file_name("store.db-wal");
    let later = SystemTime::now() + Duration::from_secs(120);
    fs::File::create(&wal).unwrap().set_modified(later).unwrap();
    let wal_mtime = mtime_nanos(&wal).unwrap();
    assert!(wal_mtime > db_mtime);

    assert_eq!(adapter.scan().unwrap()[CHAT_ID].1, wal_mtime);
    assert_eq!(adapter.parse(&path).unwrap().unwrap().mtime, wal_mtime);

    // An older WAL does not pull the mtime back.
    let earlier = SystemTime::now() - Duration::from_secs(3600);
    fs::File::options()
        .write(true)
        .open(&wal)
        .unwrap()
        .set_modified(earlier)
        .unwrap();
    assert_eq!(adapter.scan().unwrap()[CHAT_ID].1, db_mtime);
}

#[test]
fn the_newer_store_wins_when_two_workspaces_hold_the_same_chat_id() {
    let tmp = tempfile::tempdir().unwrap();
    for (workspace, prompt) in [("workspace-a", "older copy"), ("workspace-b", "newer copy")] {
        let store = Store::create(tmp.path(), workspace, CHAT_ID, Schema::Reported);
        store.record(user(prompt));
    }
    let old = tmp
        .path()
        .join("workspace-a")
        .join(CHAT_ID)
        .join("store.db");
    fs::File::options()
        .write(true)
        .open(&old)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(3600))
        .unwrap();

    let adapter = adapter(tmp.path());
    let scan = adapter.scan().unwrap();
    assert_eq!(scan.len(), 1);
    let s = adapter.parse(&scan[CHAT_ID].0).unwrap().unwrap();
    assert_eq!(s.title, "newer copy");
}

#[test]
fn incremental_picks_up_writes_that_are_still_in_the_wal() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::create(tmp.path(), ENCODED_WORKSPACE, CHAT_ID, Schema::Reported);
    // Cursor keeps the database open while a chat runs.
    store
        .conn
        .execute_batch("PRAGMA journal_mode = WAL; PRAGMA wal_autocheckpoint = 0;")
        .unwrap();
    store.record(json!({"role": "user", "content": "Initial prompt"}));

    let adapter = adapter(tmp.path());
    let first = adapter
        .find_incremental(&KnownSessions::new(), &mut |_| {})
        .unwrap();
    assert_eq!(first.upserts.len(), 1);
    let initial = &first.upserts[0];
    assert_eq!(initial.content, "» Initial prompt");

    let known: KnownSessions = [(SessionKey::new(AgentId::Cursor, CHAT_ID), initial.mtime)]
        .into_iter()
        .collect();
    let unchanged = adapter.find_incremental(&known, &mut |_| {}).unwrap();
    assert!(unchanged.upserts.is_empty());
    assert!(unchanged.deleted.is_empty());

    std::thread::sleep(Duration::from_millis(20));
    store.record(json!({"role": "assistant", "content": "Written to the WAL"}));

    let mut streamed = Vec::new();
    let res = adapter
        .find_incremental(&known, &mut |s| streamed.push(s.id.clone()))
        .unwrap();
    assert_eq!(streamed, vec![CHAT_ID]);
    assert_eq!(res.upserts.len(), 1);
    assert!(res.upserts[0].content.contains("  Written to the WAL"));
    assert_eq!(res.upserts[0].message_count, 2);
    assert!(res.upserts[0].mtime > initial.mtime);
    assert!(res.deleted.is_empty());
}

#[test]
fn incremental_reports_new_chats_and_removed_ones() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    for id in [CHAT_ID, OTHER_ID] {
        let store = Store::create(root, HASHED_WORKSPACE, id, Schema::Reported);
        store.record(user("hello")).record(assistant("hi"));
    }
    // Not a database: must not abort the pass, and must not be dropped from
    // the index while its file is still there.
    let broken = root.join(HASHED_WORKSPACE).join("broken-chat");
    fs::create_dir_all(&broken).unwrap();
    fs::write(broken.join("store.db"), b"this is not a sqlite file").unwrap();

    let adapter = adapter(root);
    assert!(adapter.parse(&broken.join("store.db")).is_err());

    let scan = adapter.scan().unwrap();
    let known: KnownSessions = [
        (SessionKey::new(AgentId::Cursor, CHAT_ID), scan[CHAT_ID].1),
        (SessionKey::new(AgentId::Cursor, "broken-chat"), 1),
        (SessionKey::new(AgentId::Cursor, "removed-chat"), 1),
        (SessionKey::new(AgentId::Codex, "someone-elses"), 1),
    ]
    .into_iter()
    .collect();

    let res = adapter.find_incremental(&known, &mut |_| {}).unwrap();
    let ids: Vec<&str> = res.upserts.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, vec![OTHER_ID]);
    assert_eq!(res.upserts[0].mtime, scan[OTHER_ID].1);
    assert_eq!(res.deleted, vec!["removed-chat".to_string()]);
}

#[test]
fn a_missing_chats_directory_means_unavailable_and_drops_known_chats() {
    let tmp = tempfile::tempdir().unwrap();
    let adapter = adapter(&tmp.path().join("no-such-dir"));
    assert!(!adapter.is_available());
    assert!(adapter.scan().unwrap().is_empty());

    let known: KnownSessions = [
        (SessionKey::new(AgentId::Cursor, CHAT_ID), 1),
        (SessionKey::new(AgentId::Codex, "someone-elses"), 1),
    ]
    .into_iter()
    .collect();
    let res = adapter.find_incremental(&known, &mut |_| {}).unwrap();
    assert!(res.upserts.is_empty());
    assert_eq!(res.deleted, vec![CHAT_ID.to_string()]);
}

#[test]
fn reading_a_closed_wal_store_leaves_its_directory_untouched() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::create(tmp.path(), ENCODED_WORKSPACE, CHAT_ID, Schema::Reported);
    store
        .conn
        .execute_batch("PRAGMA journal_mode = WAL;")
        .unwrap();
    store.meta(hex(&json!({"name": "Closed chat"}).to_string()));
    store.record(user("hello"));
    let path = store.path.clone();
    drop(store);

    let dir = path.parent().unwrap();
    assert_eq!(
        entries(dir),
        vec!["store.db"],
        "closing checkpoints the WAL"
    );
    let before = mtime_nanos(&path).unwrap();

    let adapter = adapter(tmp.path());
    let res = adapter
        .find_incremental(&KnownSessions::new(), &mut |_| {})
        .unwrap();
    assert_eq!(res.upserts.len(), 1);
    assert_eq!(res.upserts[0].title, "Closed chat");

    assert_eq!(entries(dir), vec!["store.db"], "no -shm or -wal created");
    assert_eq!(mtime_nanos(&path).unwrap(), before);
}

#[test]
fn capabilities() {
    let adapter = CursorAdapter::with_chats_dir(PathBuf::from("/nonexistent"));
    assert_eq!(adapter.id(), AgentId::Cursor);
    assert_eq!(adapter.badge(), "cursor");
    assert!(adapter.supports_yolo());
    assert!(!adapter.detects_yolo());
    assert!(!adapter.supports_fork());
    assert!(adapter.mtime_is_per_session());
    assert!(!adapter.uses_dir_mtime_fastpath());

    let session = Session {
        id: CHAT_ID.to_string(),
        agent: AgentId::Cursor,
        title: String::new(),
        directory: String::new(),
        timestamp: 0,
        content: String::new(),
        message_count: 0,
        mtime: 0,
        yolo: false,
        source_path: None,
    };
    assert!(adapter.fork_command(&session, None, None, false).is_none());
    assert_eq!(CursorAdapter::default().id(), AgentId::Cursor);
}

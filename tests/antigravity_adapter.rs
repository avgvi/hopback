//! Fixture tests for the Antigravity CLI store under `~/.gemini/antigravity-cli`:
//! one SQLite database per conversation in `conversations/<id>.db`, whose
//! steps are protobuf blobs, and a generated JSONL transcript of the same
//! conversation in `brain/<id>/.system_generated/logs/`.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use hopback::adapters::antigravity::AntigravityAdapter;
use hopback::adapters::{mtime_nanos, parse_iso8601_secs, Adapter, KnownSessions};
use hopback::config::AgentId;
use hopback::model::SessionKey;
use rusqlite::Connection;
use serde_json::{json, Value};

const DB_ID: &str = "11111111-2222-4333-8444-555555555555";
const TRAJECTORY_ID: &str = "99999999-8888-4777-8666-555555555555";
const CLEARED_ID: &str = "22222222-2222-4333-8444-555555555555";
const TRANSCRIPT_ID: &str = "33333333-2222-4333-8444-555555555555";
const EMPTY_ID: &str = "44444444-2222-4333-8444-555555555555";
const BROKEN_ID: &str = "55555555-2222-4333-8444-555555555555";

const USER_INPUT: u64 = 14;
const PLANNER_RESPONSE: u64 = 15;
const TOOL_STEP: u64 = 132;
const DONE: u64 = 3;
const CLEARED: u64 = 5;

/// The schema every real conversation database has.
const SCHEMA: &str = r#"
CREATE TABLE trajectory_meta (trajectory_id text, cascade_id text, trajectory_type integer, source integer, PRIMARY KEY (trajectory_id));
CREATE TABLE steps (idx integer, step_type integer NOT NULL DEFAULT 0, status integer NOT NULL DEFAULT 0,
                    has_subtrajectory numeric NOT NULL DEFAULT false, metadata blob, error_details blob, permissions blob,
                    task_details blob, render_info blob, step_payload blob, step_format integer NOT NULL DEFAULT 0, PRIMARY KEY (idx));
CREATE TABLE trajectory_metadata_blob (id text DEFAULT "main", data blob, PRIMARY KEY (id));
"#;

fn varint(mut value: u64) -> Vec<u8> {
    let mut bytes = Vec::new();
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            bytes.push(byte);
            return bytes;
        }
        bytes.push(byte | 0x80);
    }
}

fn proto_int(field: u64, value: u64) -> Vec<u8> {
    [varint(field << 3), varint(value)].concat()
}

fn proto_bytes(field: u64, value: &[u8]) -> Vec<u8> {
    [
        varint(field << 3 | 2),
        varint(value.len() as u64),
        value.to_vec(),
    ]
    .concat()
}

fn proto_string(field: u64, value: &str) -> Vec<u8> {
    proto_bytes(field, value.as_bytes())
}

/// `#5 { #1 { #1 seconds, #2 nanos } }`, the step metadata.
fn step_metadata(seconds: u64) -> Vec<u8> {
    let timestamp = [proto_int(1, seconds), proto_int(2, 0)].concat();
    proto_bytes(5, &proto_bytes(1, &timestamp))
}

fn user_step(status: u64, text: Option<&str>, seconds: u64) -> Vec<u8> {
    let input = match text {
        Some(text) => proto_string(2, text),
        // A cleared step keeps only its attachment items.
        None => proto_bytes(3, b"opaque"),
    };
    [
        proto_int(1, USER_INPUT),
        proto_int(4, status),
        step_metadata(seconds),
        proto_bytes(19, &input),
    ]
    .concat()
}

fn planner_step(text: Option<&str>, thinking: Option<&str>, seconds: u64) -> Vec<u8> {
    let mut response = Vec::new();
    if let Some(text) = text {
        response.extend(proto_string(1, text));
    }
    if let Some(thinking) = thinking {
        response.extend(proto_string(3, thinking));
    }
    if let Some(text) = text {
        response.extend(proto_string(8, text));
    }
    [
        proto_int(1, PLANNER_RESPONSE),
        proto_int(4, DONE),
        step_metadata(seconds),
        proto_bytes(20, &response),
    ]
    .concat()
}

fn tool_step(seconds: u64) -> Vec<u8> {
    [
        proto_int(1, TOOL_STEP),
        proto_int(4, DONE),
        step_metadata(seconds),
        proto_string(21, "tool output that must not be indexed"),
    ]
    .concat()
}

/// `#1 { #1 uri }  #6 conversation id  #7 uri`
fn workspace_blob(id: &str, uri: &str) -> Vec<u8> {
    [
        proto_bytes(1, &proto_string(1, uri)),
        proto_string(6, id),
        proto_string(7, uri),
    ]
    .concat()
}

fn db_path(root: &Path, id: &str) -> PathBuf {
    root.join("conversations").join(format!("{id}.db"))
}

fn insert_step(conn: &Connection, idx: i64, payload: &[u8]) {
    conn.execute(
        "INSERT INTO steps (idx, step_payload) VALUES (?1, ?2)",
        rusqlite::params![idx, payload],
    )
    .unwrap();
}

/// Writes a conversation database in the rollback-journal mode, so closing it
/// leaves only the `.db` file behind.
fn write_db(root: &Path, id: &str, workspace_uri: Option<&str>, steps: &[Vec<u8>]) -> PathBuf {
    fs::create_dir_all(root.join("conversations")).unwrap();
    let path = db_path(root, id);
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(SCHEMA).unwrap();
    conn.execute(
        "INSERT INTO trajectory_meta VALUES (?1, ?2, 4, 17)",
        [TRAJECTORY_ID, id],
    )
    .unwrap();
    if let Some(uri) = workspace_uri {
        conn.execute(
            "INSERT INTO trajectory_metadata_blob VALUES ('main', ?1)",
            [workspace_blob(id, uri)],
        )
        .unwrap();
    }
    for (idx, payload) in steps.iter().enumerate() {
        insert_step(&conn, idx as i64, payload);
    }
    path
}

fn write_transcript(root: &Path, id: &str, name: &str, rows: &[Value]) -> PathBuf {
    let logs = root.join("brain").join(id).join(".system_generated/logs");
    fs::create_dir_all(&logs).unwrap();
    let path = logs.join(name);
    let body: String = rows.iter().map(|row| format!("{row}\n")).collect();
    fs::write(&path, body).unwrap();
    path
}

fn wrapped(prompt: &str) -> String {
    format!(
        "<USER_REQUEST>\n{prompt}\n</USER_REQUEST>\n<ADDITIONAL_METADATA>\nlocal time: placeholder\n</ADDITIONAL_METADATA>\n<USER_SETTINGS_CHANGE>\nplaceholder\n</USER_SETTINGS_CHANGE>"
    )
}

fn transcript_user(index: u64, status: &str, created_at: &str, prompt: &str) -> Value {
    json!({"step_index": index, "source": "USER_EXPLICIT", "type": "USER_INPUT", "status": status,
        "created_at": created_at, "content": wrapped(prompt)})
}

fn transcript_planner(
    index: u64,
    created_at: &str,
    content: Option<&str>,
    thinking: &str,
) -> Value {
    let mut row = json!({"step_index": index, "source": "MODEL", "type": "PLANNER_RESPONSE", "status": "DONE",
        "created_at": created_at, "thinking": thinking,
        "tool_calls": [{"name": "view_file", "args": {"path": "/work/app/login.rs"}}],
        "input_tokens": 10, "output_tokens": 5, "cache_read_tokens": 0});
    if let Some(content) = content {
        row["content"] = json!(content);
    }
    row
}

/// Rows a real transcript interleaves with the conversation and that carry no
/// text hopback should index.
fn transcript_noise(index: u64) -> Vec<Value> {
    vec![
        json!({"step_index": index, "source": "MODEL", "type": "GENERIC", "status": "DONE",
            "created_at": "2026-07-17T10:00:04Z", "content": "tool output that must not be indexed"}),
        json!({"step_index": index + 1, "source": "SYSTEM", "type": "CHECKPOINT", "status": "DONE",
            "created_at": "2026-07-17T10:00:04Z", "content": "checkpoint text"}),
        json!({"step_index": index + 2, "source": "SYSTEM", "type": "ERROR_MESSAGE", "status": "ERROR",
            "created_at": "2026-07-17T10:00:04Z", "content": "error text", "error": "boom"}),
        json!({"source": "SYSTEM", "type": "CONVERSATION_HISTORY", "content": "history text"}),
        json!({"source": "USER", "type": "EPHEMERAL_MESSAGE", "content": "ephemeral text"}),
    ]
}

/// A store with one session of each kind:
/// - `DB_ID`: database with a prompt, plus a transcript that must not be used
/// - `CLEARED_ID`: database whose only prompt was cleared, found through the transcript
/// - `TRANSCRIPT_ID`: transcript only
/// - `EMPTY_ID`: database without steps and a brain directory without logs
fn fixture() -> (tempfile::TempDir, AntigravityAdapter) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    write_db(
        root,
        DB_ID,
        Some("file:///work/app"),
        &[
            user_step(DONE, Some("Fix the login bug"), 1_784_282_400),
            planner_step(None, Some("thinking only"), 1_784_282_402),
            tool_step(1_784_282_403),
            planner_step(Some("Fixed it."), Some("thinking text"), 1_784_282_405),
            // Steps after the last indexed one must not move the timestamp.
            tool_step(1_784_282_999),
        ],
    );
    write_transcript(
        root,
        DB_ID,
        "transcript.jsonl",
        &[
            transcript_user(
                0,
                "DONE",
                "2026-07-17T10:00:00Z",
                "transcript copy of the prompt",
            ),
            transcript_planner(
                1,
                "2026-07-17T10:00:05Z",
                Some("transcript copy of the reply"),
                "t",
            ),
        ],
    );

    write_db(
        root,
        CLEARED_ID,
        Some("file:///work/my%20app"),
        &[
            user_step(CLEARED, None, 1_784_282_400),
            planner_step(Some("reply to a cleared prompt"), None, 1_784_282_401),
        ],
    );
    let mut cleared = vec![
        transcript_user(
            0,
            "CLEARED",
            "2026-07-17T11:00:00Z",
            "Rename the config module",
        ),
        transcript_planner(
            1,
            "2026-07-17T11:00:03Z",
            None,
            "Looking at the module layout.",
        ),
    ];
    cleared.extend(transcript_noise(2));
    cleared.push(transcript_planner(
        7,
        "2026-07-17T11:00:09Z",
        Some("Renamed it."),
        "t",
    ));
    write_transcript(root, CLEARED_ID, "transcript.jsonl", &cleared[..1]);
    write_transcript(root, CLEARED_ID, "transcript_full.jsonl", &cleared);

    write_transcript(
        root,
        TRANSCRIPT_ID,
        "transcript.jsonl",
        &[
            transcript_user(0, "DONE", "2026-07-17T12:00:00Z", "Add a changelog entry"),
            transcript_planner(1, "2026-07-17T12:00:02Z", Some("Added."), "t"),
        ],
    );

    write_db(root, EMPTY_ID, Some("file:///work/empty"), &[]);
    fs::create_dir_all(root.join("brain").join(EMPTY_ID).join("scratch")).unwrap();

    fs::create_dir_all(root.join("cache")).unwrap();
    fs::write(
        root.join("cache/last_conversations.json"),
        json!({"/work/stale": TRANSCRIPT_ID, "/work/other": {"conversationId": "someone-else"}})
            .to_string(),
    )
    .unwrap();
    fs::write(
        root.join("history.jsonl"),
        format!(
            "{}\n{}\n",
            json!({"display": "/goal placeholder", "timestamp": 1784282400000u64, "workspace": "/work/ignored", "type": "slash_command"}),
            json!({"display": "placeholder", "timestamp": 1784282400000u64, "workspace": "/work/docs", "conversationId": TRANSCRIPT_ID}),
        ),
    )
    .unwrap();

    let adapter = AntigravityAdapter::with_root(root.to_path_buf());
    (tmp, adapter)
}

fn dir_listing(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    names
}

#[test]
fn scan_lists_databases_and_transcript_only_sessions() {
    let (tmp, adapter) = fixture();
    let root = tmp.path();
    // Sidecar files next to a database are not sessions of their own.
    fs::write(
        root.join("conversations").join(format!("{DB_ID}.db-shm")),
        b"",
    )
    .unwrap();
    fs::write(root.join("conversations/notes.txt"), b"").unwrap();

    assert!(adapter.is_available());
    let scan = adapter.scan().unwrap();
    let mut ids: Vec<&str> = scan.keys().map(String::as_str).collect();
    ids.sort();
    assert_eq!(ids, vec![DB_ID, CLEARED_ID, TRANSCRIPT_ID, EMPTY_ID]);

    assert_eq!(scan[DB_ID].0, db_path(root, DB_ID));
    assert_eq!(scan[CLEARED_ID].0, db_path(root, CLEARED_ID));
    assert_eq!(scan[EMPTY_ID].0, db_path(root, EMPTY_ID));
    assert_eq!(
        scan[TRANSCRIPT_ID].0,
        root.join("brain")
            .join(TRANSCRIPT_ID)
            .join(".system_generated/logs/transcript.jsonl")
    );
}

#[test]
fn scan_ids_match_parsed_session_ids() {
    let (_tmp, adapter) = fixture();
    let mut parsed = 0;
    for (id, (path, mtime)) in adapter.scan().unwrap() {
        if let Some(session) = adapter.parse(&path).unwrap() {
            assert_eq!(session.id, id);
            assert_eq!(session.mtime, mtime);
            assert_eq!(session.source_path.as_deref(), path.to_str());
            parsed += 1;
        }
    }
    assert_eq!(parsed, 3);
}

#[test]
fn parses_a_conversation_database() {
    let (_tmp, adapter) = fixture();
    let scan = adapter.scan().unwrap();
    let (path, mtime) = &scan[DB_ID];
    let s = adapter.parse(path).unwrap().expect("session with a prompt");

    assert_eq!(s.id, DB_ID);
    assert_eq!(s.agent, AgentId::Antigravity);
    assert_eq!(s.directory, "/work/app");
    assert_eq!(s.title, "Fix the login bug");
    assert_eq!(s.message_count, 2, "one prompt and one visible reply");
    assert_eq!(s.content, "» Fix the login bug\n\n  Fixed it.");
    assert_eq!(s.timestamp, 1_784_282_405, "last indexed step");
    assert_eq!(s.mtime, *mtime);
    assert!(!s.yolo);
    assert_eq!(s.source_path.as_deref(), path.to_str());
}

#[test]
fn parses_the_documented_byte_layout() {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir_all(tmp.path().join("conversations")).unwrap();
    let path = db_path(tmp.path(), DB_ID);
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch(SCHEMA).unwrap();
    conn.execute_batch(
        "INSERT INTO trajectory_meta VALUES ('99999999-8888-4777-8666-555555555555', '11111111-2222-4333-8444-555555555555', 4, 17);
         INSERT INTO trajectory_metadata_blob VALUES ('main', X'0a120a1066696c653a2f2f2f776f726b2f617070322431313131313131312d323232322d343333332d383434342d3535353535353535353535353a1066696c653a2f2f2f776f726b2f617070');
         INSERT INTO steps (idx, step_type, status, step_payload) VALUES (0, 14, 3, X'080e20032a0a0a0808a0fae7d20610009a0113121146697820746865206c6f67696e20627567');
         INSERT INTO steps (idx, step_type, status, step_payload) VALUES (1, 15, 3, X'080f20032a0a0a0808a5fae7d2061000a2011a0a0946697865642069742e1a0d7468696e6b696e672074657874');",
    )
    .unwrap();
    drop(conn);

    let adapter = AntigravityAdapter::with_root(tmp.path().to_path_buf());
    let s = adapter.parse(&path).unwrap().unwrap();
    assert_eq!(s.id, DB_ID);
    assert_eq!(s.directory, "/work/app");
    assert_eq!(s.title, "Fix the login bug");
    assert_eq!(s.content, "» Fix the login bug\n\n  Fixed it.");
    assert_eq!(s.timestamp, 1_784_282_405);
}

#[test]
fn step_columns_are_used_when_the_payload_omits_type_and_status() {
    let tmp = tempfile::tempdir().unwrap();
    let path = write_db(tmp.path(), DB_ID, None, &[]);
    let conn = Connection::open(&path).unwrap();
    let prompt = proto_bytes(19, &proto_string(2, "prompt from columns"));
    let cleared = proto_bytes(19, &proto_string(2, "cleared by its column"));
    // { #1 { #1 seconds } }, as the `metadata` column stores it.
    let metadata = proto_bytes(1, &proto_int(1, 1_784_282_410));
    conn.execute(
        "INSERT INTO steps (idx, step_type, status, metadata, step_payload) VALUES (0, 14, 3, ?1, ?2)",
        rusqlite::params![metadata, prompt],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO steps (idx, step_type, status, step_payload) VALUES (1, 14, 5, ?1)",
        [cleared],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO steps (idx, step_type, status) VALUES (2, 15, 3)",
        [],
    )
    .unwrap();
    drop(conn);

    let adapter = AntigravityAdapter::with_root(tmp.path().to_path_buf());
    let s = adapter.parse(&path).unwrap().unwrap();
    assert_eq!(s.content, "» prompt from columns");
    assert_eq!(s.message_count, 1);
    assert_eq!(s.timestamp, 1_784_282_410);
    assert_eq!(s.directory, "", "no metadata blob and no workspace map");
}

#[test]
fn a_cleared_prompt_is_found_through_the_transcript() {
    let (_tmp, adapter) = fixture();
    let scan = adapter.scan().unwrap();
    let (path, _) = &scan[CLEARED_ID];
    let s = adapter.parse(path).unwrap().expect("transcript fallback");

    assert_eq!(s.id, CLEARED_ID);
    assert_eq!(s.title, "Rename the config module");
    assert_eq!(
        s.directory, "/work/my app",
        "the database still supplies the percent-decoded workspace"
    );
    assert_eq!(
        s.content, "» Rename the config module\n\n  Looking at the module layout.\n\n  Renamed it.",
        "the larger transcript_full.jsonl is read, thinking stands in for an empty reply"
    );
    assert_eq!(s.message_count, 3);
    assert_eq!(
        s.timestamp,
        parse_iso8601_secs("2026-07-17T11:00:09Z").unwrap()
    );
    assert_eq!(s.source_path.as_deref(), path.to_str());
    for hidden in [
        "tool output",
        "checkpoint",
        "error text",
        "history",
        "ephemeral",
        "METADATA",
    ] {
        assert!(!s.content.contains(hidden), "{hidden} must not be indexed");
    }
}

#[test]
fn a_transcript_only_session_takes_its_directory_from_the_workspace_map() {
    let (tmp, adapter) = fixture();
    let scan = adapter.scan().unwrap();
    let (path, mtime) = &scan[TRANSCRIPT_ID];
    let s = adapter.parse(path).unwrap().unwrap();

    assert_eq!(s.id, TRANSCRIPT_ID);
    assert_eq!(
        s.directory, "/work/docs",
        "history.jsonl overrides the cache"
    );
    assert_eq!(s.title, "Add a changelog entry");
    assert_eq!(s.content, "» Add a changelog entry\n\n  Added.");
    assert_eq!(s.message_count, 2);
    assert_eq!(
        s.timestamp,
        parse_iso8601_secs("2026-07-17T12:00:02Z").unwrap()
    );
    assert_eq!(s.mtime, *mtime);

    // Without history.jsonl the cache alone supplies the workspace.
    fs::remove_file(tmp.path().join("history.jsonl")).unwrap();
    let s = adapter.parse(path).unwrap().unwrap();
    assert_eq!(s.directory, "/work/stale");
}

#[test]
fn a_workspace_map_change_counts_as_a_change_to_transcript_only_sessions() {
    let (tmp, adapter) = fixture();
    let before = adapter.scan().unwrap();
    std::thread::sleep(Duration::from_millis(20));
    fs::write(
        tmp.path().join("history.jsonl"),
        json!({"workspace": "/work/moved", "conversationId": TRANSCRIPT_ID}).to_string(),
    )
    .unwrap();
    let after = adapter.scan().unwrap();

    assert!(after[TRANSCRIPT_ID].1 > before[TRANSCRIPT_ID].1);
    assert_eq!(
        after[TRANSCRIPT_ID].1,
        mtime_nanos(&tmp.path().join("history.jsonl")).unwrap()
    );
    assert_eq!(after[DB_ID].1, before[DB_ID].1);
}

#[test]
fn conversations_without_a_prompt_are_not_sessions() {
    let (tmp, adapter) = fixture();
    let scan = adapter.scan().unwrap();
    assert!(adapter.parse(&scan[EMPTY_ID].0).unwrap().is_none());

    // Only tool steps and a reply, in both sources.
    let path = write_db(
        tmp.path(),
        BROKEN_ID,
        None,
        &[tool_step(1), planner_step(Some("unprompted"), None, 2)],
    );
    write_transcript(
        tmp.path(),
        BROKEN_ID,
        "transcript.jsonl",
        &[transcript_planner(
            0,
            "2026-07-17T10:00:00Z",
            Some("unprompted"),
            "t",
        )],
    );
    assert!(adapter.parse(&path).unwrap().is_none());
}

#[test]
fn an_unreadable_database_falls_back_to_the_transcript_or_fails() {
    let (tmp, adapter) = fixture();
    let root = tmp.path();
    let path = db_path(root, BROKEN_ID);
    fs::write(&path, b"unsupported database").unwrap();

    assert!(
        adapter.parse(&path).is_err(),
        "an error keeps whatever is indexed, unlike Ok(None)"
    );

    write_transcript(
        root,
        BROKEN_ID,
        "transcript_full.jsonl",
        &[
            json!("not an object"),
            transcript_user(
                0,
                "DONE",
                "2026-07-17T10:00:00Z",
                "Recover from the transcript",
            ),
        ],
    );
    fs::write(
        root.join("cache/last_conversations.json"),
        json!({"/work/recovered": BROKEN_ID}).to_string(),
    )
    .unwrap();
    let s = adapter.parse(&path).unwrap().unwrap();
    assert_eq!(s.id, BROKEN_ID);
    assert_eq!(s.title, "Recover from the transcript");
    assert_eq!(s.directory, "/work/recovered");
    assert_eq!(
        dir_listing(&root.join("conversations"))
            .iter()
            .filter(|name| name.starts_with(BROKEN_ID))
            .count(),
        1
    );
}

#[test]
fn long_prompts_become_single_line_titles() {
    let tmp = tempfile::tempdir().unwrap();
    let prompt = format!("first line\nsecond line {}", "x".repeat(200));
    let path = write_db(
        tmp.path(),
        DB_ID,
        Some("/plain/path"),
        &[user_step(DONE, Some(&prompt), 1_784_282_400)],
    );
    let adapter = AntigravityAdapter::with_root(tmp.path().to_path_buf());
    let s = adapter.parse(&path).unwrap().unwrap();

    assert!(s.title.starts_with("first line second line xxx"));
    assert!(s.title.ends_with("..."));
    assert_eq!(s.title.chars().count(), 80);
    assert!(s.content.contains("first line\nsecond line"));
    assert_eq!(
        s.directory, "/plain/path",
        "a non-URI workspace is kept as is"
    );
}

#[test]
fn reading_a_checkpointed_wal_database_leaves_no_files_behind() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let path = write_db(root, DB_ID, Some("file:///work/app"), &[]);
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("PRAGMA journal_mode = WAL;").unwrap();
    insert_step(
        &conn,
        0,
        &user_step(DONE, Some("Prompt in a WAL database"), 1_784_282_400),
    );
    // Closing the last connection checkpoints and removes the sidecar files.
    drop(conn);
    let before = dir_listing(&root.join("conversations"));
    assert_eq!(before, vec![format!("{DB_ID}.db")]);

    let adapter = AntigravityAdapter::with_root(root.to_path_buf());
    let scan = adapter.scan().unwrap();
    let s = adapter.parse(&scan[DB_ID].0).unwrap().unwrap();
    assert_eq!(s.title, "Prompt in a WAL database");
    assert_eq!(dir_listing(&root.join("conversations")), before);
}

#[test]
fn steps_still_in_the_wal_are_read_and_move_the_mtime() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let path = write_db(root, DB_ID, Some("file:///work/app"), &[]);
    // An open writer with checkpoints off keeps new steps in the WAL only,
    // like a conversation that is still running.
    let conn = Connection::open(&path).unwrap();
    conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA wal_autocheckpoint = 0;")
        .unwrap();
    insert_step(&conn, 0, &user_step(DONE, Some("Prompt"), 1_784_282_400));
    insert_step(
        &conn,
        1,
        &planner_step(Some("First reply"), None, 1_784_282_401),
    );

    let adapter = AntigravityAdapter::with_root(root.to_path_buf());
    let mut wal = path.clone().into_os_string();
    wal.push("-wal");
    let wal = PathBuf::from(wal);

    let first = adapter
        .find_incremental(&KnownSessions::new(), &mut |_| {})
        .unwrap();
    assert_eq!(first.upserts.len(), 1);
    let session = &first.upserts[0];
    assert!(session.content.contains("First reply"));
    assert_eq!(
        session.mtime,
        mtime_nanos(&path).unwrap().max(mtime_nanos(&wal).unwrap())
    );

    let known: KnownSessions = [(session.key(), session.mtime)].into_iter().collect();
    let unchanged = adapter.find_incremental(&known, &mut |_| {}).unwrap();
    assert!(unchanged.upserts.is_empty());
    assert!(unchanged.deleted.is_empty());

    std::thread::sleep(Duration::from_millis(20));
    insert_step(
        &conn,
        2,
        &planner_step(Some("WAL reply"), None, 1_784_282_402),
    );
    assert!(mtime_nanos(&wal).unwrap() > mtime_nanos(&path).unwrap());

    let changed = adapter.find_incremental(&known, &mut |_| {}).unwrap();
    assert_eq!(changed.upserts.len(), 1);
    assert!(changed.upserts[0].content.contains("WAL reply"));
    assert!(changed.upserts[0].mtime > session.mtime);
    assert_eq!(changed.upserts[0].timestamp, 1_784_282_402);
    assert_eq!(changed.upserts[0].message_count, 3);
}

#[test]
fn incremental_index_upserts_sessions_and_drops_removed_ones() {
    let (tmp, adapter) = fixture();
    let known: KnownSessions = [
        (SessionKey::new(AgentId::Antigravity, "gone"), 1),
        (SessionKey::new(AgentId::Codex, "other-agent"), 1),
    ]
    .into_iter()
    .collect();

    let mut streamed = Vec::new();
    let res = adapter
        .find_incremental(&known, &mut |s| streamed.push(s.id.clone()))
        .unwrap();
    let mut ids: Vec<&str> = res.upserts.iter().map(|s| s.id.as_str()).collect();
    ids.sort();
    assert_eq!(ids, vec![DB_ID, CLEARED_ID, TRANSCRIPT_ID]);
    assert_eq!(streamed.len(), 3);
    assert_eq!(res.deleted, vec!["gone".to_string()]);

    // Nothing changed: nothing to do. Then a removed conversation disappears.
    let known: KnownSessions = res.upserts.iter().map(|s| (s.key(), s.mtime)).collect();
    let res = adapter.find_incremental(&known, &mut |_| {}).unwrap();
    assert!(res.upserts.is_empty());
    assert!(res.deleted.is_empty());

    fs::remove_file(db_path(tmp.path(), DB_ID)).unwrap();
    fs::remove_dir_all(tmp.path().join("brain").join(DB_ID)).unwrap();
    let res = adapter.find_incremental(&known, &mut |_| {}).unwrap();
    assert!(res.upserts.is_empty());
    assert_eq!(res.deleted, vec![DB_ID.to_string()]);
}

#[test]
fn a_missing_store_is_unavailable_and_drops_known_sessions() {
    let tmp = tempfile::tempdir().unwrap();
    let adapter = AntigravityAdapter::with_root(tmp.path().join("antigravity-cli"));
    assert!(!adapter.is_available());
    assert!(adapter.scan().unwrap().is_empty());

    let known: KnownSessions = [(SessionKey::new(AgentId::Antigravity, DB_ID), 1)]
        .into_iter()
        .collect();
    let res = adapter.find_incremental(&known, &mut |_| {}).unwrap();
    assert_eq!(res.deleted, vec![DB_ID.to_string()]);
}

#[test]
fn resume_command_takes_the_conversation_id() {
    let (_tmp, adapter) = fixture();
    let scan = adapter.scan().unwrap();
    let s = adapter.parse(&scan[DB_ID].0).unwrap().unwrap();

    assert_eq!(adapter.id(), AgentId::Antigravity);
    assert!(adapter.supports_yolo());
    assert!(!adapter.detects_yolo());
    assert!(!adapter.supports_fork());
    assert!(adapter.mtime_is_per_session());
    assert_eq!(
        adapter.resume_command(&s, false),
        vec!["agy", "--conversation", DB_ID]
    );
    assert_eq!(
        adapter.resume_command(&s, true),
        vec![
            "agy",
            "--dangerously-skip-permissions",
            "--conversation",
            DB_ID
        ]
    );
    assert!(adapter.fork_command(&s, None, None, false).is_none());
}

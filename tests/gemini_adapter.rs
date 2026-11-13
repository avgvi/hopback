//! Fixture tests for the Gemini CLI session store:
//! `~/.gemini/tmp/<project>/chats/session-<time>-<id8>.jsonl` append-only logs
//! that have to be replayed, the single-document `session-*.json` files older
//! builds wrote, and the project root kept in `.project_root` or
//! `projects.json`.
//!
//! The fixtures follow the CLI's source (gemini 0.67.0). No session file from
//! a real run was available when they were written.

use std::fs;
use std::path::{Path, PathBuf};

use hopback::adapters::gemini::GeminiAdapter;
use hopback::adapters::{mtime_nanos, parse_iso8601_secs, Adapter, KnownSessions};
use hopback::config::AgentId;
use hopback::model::SessionKey;
use serde_json::{json, Value};

const MAIN_ID: &str = "11111111-2222-4333-8444-555555555555";
const PLAIN_ID: &str = "66666666-2222-4333-8444-555555555555";
const LEGACY_ID: &str = "22222222-2222-4333-8444-555555555555";
const HASHED_ID: &str = "33333333-2222-4333-8444-555555555555";
const EMPTY_ID: &str = "dddddddd-2222-4333-8444-555555555555";
const SUB_NESTED_ID: &str = "aaaaaaaa-2222-4333-8444-555555555555";
const SUB_FLAT_ID: &str = "cccccccc-2222-4333-8444-555555555555";

const PROJECT_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";
const HASH_DIR: &str = "9f86d081884c7d659a2feae0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08";

fn chats_dir(root: &Path, project: &str) -> PathBuf {
    let dir = root.join("tmp").join(project).join("chats");
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_log(dir: &Path, name: &str, rows: &[Value]) -> PathBuf {
    fs::create_dir_all(dir).unwrap();
    let path = dir.join(name);
    let body: String = rows.iter().map(|r| format!("{r}\n")).collect();
    fs::write(&path, body).unwrap();
    path
}

fn meta(id: &str, start: &str) -> Value {
    json!({
        "sessionId": id,
        "projectHash": PROJECT_HASH,
        "startTime": start,
        "lastUpdated": start
    })
}

fn user(id: &str, text: &str) -> Value {
    json!({
        "id": id,
        "timestamp": "2026-07-17T10:00:01.000Z",
        "type": "user",
        "content": [{"text": text}]
    })
}

fn gemini(id: &str, text: &str) -> Value {
    json!({
        "id": id,
        "timestamp": "2026-07-17T10:00:05.000Z",
        "type": "gemini",
        "content": text,
        "thoughts": [],
        "tokens": {"input": 10, "output": 4, "cached": 0, "thoughts": 0, "tool": 0, "total": 14},
        "model": "gemini-x"
    })
}

fn touched(at: &str) -> Value {
    json!({"$set": {"lastUpdated": at}})
}

/// The log of one session as the CLI appends it: context the CLI injects, a
/// slash command, a tool-call turn that is rewritten and patched, an info
/// record, a prompt that is rewound, and a summary set at the end.
fn main_log() -> Vec<Value> {
    vec![
        meta(MAIN_ID, "2026-07-17T10:00:00.000Z"),
        user(
            "m0",
            "\n<session_context>\nThis is the Gemini CLI.\n</session_context>",
        ),
        user("c1", "/stats"),
        json!({"id": "i1", "timestamp": "2026-07-17T10:00:00.500Z", "type": "info",
            "content": "Update available"}),
        user("m1", "Add a retry to the fetcher"),
        touched("2026-07-17T10:00:01.000Z"),
        json!({"id": "m2", "timestamp": "2026-07-17T10:00:02.000Z", "type": "gemini", "content": "",
            "thoughts": [{"subject": "Plan", "description": "private reasoning", "timestamp": "2026-07-17T10:00:02.000Z"}],
            "tokens": null, "model": "gemini-x"}),
        // The same message again once its tool call is known.
        json!({"id": "m2", "timestamp": "2026-07-17T10:00:02.000Z", "type": "gemini", "content": "",
            "thoughts": [{"subject": "Plan", "description": "private reasoning", "timestamp": "2026-07-17T10:00:02.000Z"}],
            "tokens": null, "model": "gemini-x",
            "toolCalls": [{"id": "t1", "name": "read_file", "args": {"path": "fetch.ts"}, "status": "success"}]}),
        json!({"$patch": {"id": "m2", "toolCalls": [{"id": "t1",
            "result": [{"functionResponse": {"id": "t1", "name": "read_file", "response": {"output": "file body"}}}]}]}}),
        json!({"id": "r1", "timestamp": "2026-07-17T10:00:03.000Z", "type": "user",
            "content": [{"functionResponse": {"id": "t1", "name": "read_file", "response": {"output": "file body"}}}]}),
        gemini("m3", "Added the retry."),
        gemini("m3", "Added the retry."),
        touched("2026-07-17T10:00:05.000Z"),
        json!({"id": "e1", "timestamp": "2026-07-17T10:00:05.500Z", "type": "error",
            "content": "Quota warning"}),
        user("m4", "Discard this"),
        gemini("m5", "Discarded answer"),
        json!({"$rewindTo": "m4"}),
        json!({"$set": {"summary": "Retry for the fetcher"}}),
    ]
}

fn fixture() -> (tempfile::TempDir, GeminiAdapter) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    fs::write(
        root.join("projects.json"),
        json!({"projects": {"/work/app": "app", "/work/site": "site"}}).to_string(),
    )
    .unwrap();

    // Current layout: a slug directory with its ownership marker.
    let app = chats_dir(root, "app");
    fs::write(root.join("tmp/app/.project_root"), "/work/app").unwrap();
    write_log(&app, "session-2026-07-17T10-00-11111111.jsonl", &main_log());

    // A subagent log lives under its parent's id and is never listed.
    write_log(
        &app.join(MAIN_ID),
        &format!("{SUB_NESTED_ID}.jsonl"),
        &[
            json!({"sessionId": SUB_NESTED_ID, "projectHash": PROJECT_HASH, "kind": "subagent",
                "startTime": "2026-07-17T10:00:02.000Z", "lastUpdated": "2026-07-17T10:00:02.000Z",
                "directories": ["/work/app"]}),
            user("s1", "internal task"),
            gemini("s2", "done"),
        ],
    );
    // The kind alone must be enough, wherever the file sits.
    write_log(
        &app,
        "session-2026-07-17T10-30-cccccccc.jsonl",
        &[
            json!({"sessionId": SUB_FLAT_ID, "projectHash": PROJECT_HASH, "kind": "subagent",
                "startTime": "2026-07-17T10:30:00.000Z", "lastUpdated": "2026-07-17T10:30:00.000Z"}),
            user("s1", "internal task"),
            gemini("s2", "done"),
        ],
    );

    // Opened and closed without a prompt.
    write_log(
        &app,
        "session-2026-07-16T08-00-dddddddd.jsonl",
        &[
            meta(EMPTY_ID, "2026-07-16T08:00:00.000Z"),
            user("m0", "<session_context>\nsetup\n</session_context>"),
            user("c1", "/help"),
            json!({"id": "i1", "type": "info", "content": "Tips"}),
        ],
    );

    // Leftovers the CLI writes next to session files.
    fs::write(
        app.join("session-2026-07-15T08-00-eeeeeeee.jsonl.unreadable-1789000000000"),
        "garbage",
    )
    .unwrap();
    fs::write(
        app.join("session-2026-07-15T08-00-eeeeeeee.jsonl.tmp-4242"),
        "",
    )
    .unwrap();
    fs::write(root.join("tmp/app/logs.json"), "[]").unwrap();

    // A legacy single-document session in a slug directory that has no
    // marker, so its root comes from projects.json.
    let site = chats_dir(root, "site");
    let legacy = json!({
        "sessionId": LEGACY_ID,
        "projectHash": PROJECT_HASH,
        "startTime": "2026-05-01T09:00:00.000Z",
        "lastUpdated": "2026-05-01T09:05:00.000Z",
        "messages": [
            user("l1", "Rename the config module"),
            {"id": "l2", "timestamp": "2026-05-01T09:00:04.000Z", "type": "gemini", "content": "",
                "toolCalls": [{"id": "t9", "name": "run_shell_command", "args": {"command": "git mv a b"},
                    "result": "renamed", "status": "success"}]},
            gemini("l3", "Renamed it."),
            {"id": "l4", "type": "warning", "content": "Slow response"}
        ]
    });
    fs::write(
        site.join("session-2026-05-01T09-00-22222222.json"),
        serde_json::to_string_pretty(&legacy).unwrap(),
    )
    .unwrap();

    // A hash-named directory from before slugs, never migrated.
    let hashed = chats_dir(root, HASH_DIR);
    fs::write(
        hashed.join("session-2026-02-01T09-00-33333333.json"),
        json!({
            "sessionId": HASHED_ID,
            "projectHash": HASH_DIR,
            "startTime": "2026-02-01T09:00:00.000Z",
            "lastUpdated": "2026-02-01T09:01:00.000Z",
            "messages": [user("h1", "Old question"), gemini("h2", "Old answer")]
        })
        .to_string(),
    )
    .unwrap();

    let adapter = GeminiAdapter::with_root(root.to_path_buf());
    (tmp, adapter)
}

fn parse_id(adapter: &GeminiAdapter, id: &str) -> hopback::model::Session {
    let scan = adapter.scan().unwrap();
    adapter
        .parse(&scan[id].0)
        .unwrap()
        .unwrap_or_else(|| panic!("{id} should parse"))
}

#[test]
fn scan_lists_main_sessions_and_skips_subagents() {
    let (_tmp, adapter) = fixture();
    assert!(adapter.is_available());
    let scan = adapter.scan().unwrap();
    let mut ids: Vec<&str> = scan.keys().map(String::as_str).collect();
    ids.sort();
    assert_eq!(ids, vec![MAIN_ID, LEGACY_ID, HASHED_ID, EMPTY_ID]);

    let (path, mtime) = &scan[MAIN_ID];
    assert!(path.ends_with("tmp/app/chats/session-2026-07-17T10-00-11111111.jsonl"));
    assert_eq!(*mtime, mtime_nanos(path).unwrap());
}

#[test]
fn a_store_without_a_tmp_directory_is_unavailable() {
    let tmp = tempfile::tempdir().unwrap();
    let adapter = GeminiAdapter::with_root(tmp.path().join(".gemini"));
    assert!(!adapter.is_available());
    assert!(adapter.scan().unwrap().is_empty());
}

#[test]
fn replays_the_log_into_one_session() {
    let (_tmp, adapter) = fixture();
    let scan = adapter.scan().unwrap();
    let (path, mtime) = &scan[MAIN_ID];
    let s = adapter.parse(path).unwrap().expect("session with a prompt");

    assert_eq!(s.id, MAIN_ID, "the full uuid, not the 8 chars in the name");
    assert_eq!(s.agent, AgentId::Gemini);
    assert_eq!(s.directory, "/work/app", "from .project_root");
    assert_eq!(s.title, "Retry for the fetcher", "the stored summary");
    assert_eq!(
        s.content,
        "» Add a retry to the fetcher\n\n  Added the retry."
    );
    assert_eq!(s.message_count, 2, "one prompt and one reply with text");
    assert_eq!(
        s.timestamp,
        parse_iso8601_secs("2026-07-17T10:00:00.000Z").unwrap()
    );
    assert_eq!(s.mtime, *mtime);
    assert!(!s.yolo);
    assert_eq!(s.source_path.as_deref(), path.to_str());
}

#[test]
fn rewound_messages_and_noise_stay_out_of_the_content() {
    let (_tmp, adapter) = fixture();
    let s = parse_id(&adapter, MAIN_ID);
    for absent in [
        "Discard this",
        "Discarded answer",
        "session_context",
        "/stats",
        "Update available",
        "Quota warning",
        "private reasoning",
        "read_file",
        "file body",
    ] {
        assert!(
            !s.content.contains(absent),
            "{absent:?} leaked into content"
        );
    }
}

#[test]
fn a_patch_rewrites_removes_and_reorders_messages() {
    let tmp = tempfile::tempdir().unwrap();
    let chats = chats_dir(tmp.path(), "app");
    let path = write_log(
        &chats,
        "session-2026-07-18T10-00-66666666.jsonl",
        &[
            meta(PLAIN_ID, "2026-07-18T10:00:00.000Z"),
            user("a", "first prompt"),
            gemini("b", "draft answer"),
            user("c", "dropped prompt"),
            gemini("d", "late answer"),
            json!({"$patch": {
                "updates": [
                    {"id": "b", "content": [{"text": "final "}, {"text": "answer"}]},
                    {"id": "unknown", "content": "ignored"}
                ],
                "removeIds": ["c"],
                "orderIds": ["d", "b"]
            }}),
            json!({"$patch": {"id": "a", "content": "first prompt, compacted"}}),
        ],
    );
    let adapter = GeminiAdapter::with_root(tmp.path().to_path_buf());
    let s = adapter.parse(&path).unwrap().unwrap();
    assert_eq!(
        s.content,
        "» first prompt, compacted\n\n  late answer\n\n  final answer"
    );
    assert_eq!(s.message_count, 3);
    assert_eq!(s.title, "first prompt, compacted");
}

#[test]
fn a_rewind_to_an_unknown_id_clears_the_conversation() {
    let tmp = tempfile::tempdir().unwrap();
    let chats = chats_dir(tmp.path(), "app");
    let path = write_log(
        &chats,
        "session-2026-07-18T10-00-66666666.jsonl",
        &[
            meta(PLAIN_ID, "2026-07-18T10:00:00.000Z"),
            user("a", "first prompt"),
            gemini("b", "an answer"),
            json!({"$rewindTo": "not-in-this-log"}),
        ],
    );
    let adapter = GeminiAdapter::with_root(tmp.path().to_path_buf());
    assert!(adapter.parse(&path).unwrap().is_none());

    fs::write(
        &path,
        format!(
            "{}{}\n{}\n",
            fs::read_to_string(&path).unwrap(),
            user("c", "second try"),
            gemini("d", "second answer")
        ),
    )
    .unwrap();
    let s = adapter.parse(&path).unwrap().unwrap();
    assert_eq!(s.content, "» second try\n\n  second answer");
}

#[test]
fn title_falls_back_to_the_first_typed_prompt() {
    let tmp = tempfile::tempdir().unwrap();
    let chats = chats_dir(tmp.path(), "app");
    let typed = format!("explain @src/main.rs {}", "and more ".repeat(12));
    let path = write_log(
        &chats,
        "session-2026-07-18T10-00-66666666.jsonl",
        &[
            meta(PLAIN_ID, "2026-07-18T10:00:00.000Z"),
            user("c0", "?"),
            json!({"id": "a", "type": "user",
                "content": [{"text": typed}, {"text": "\n--- Content from referenced files ---\nfn main() {}"}],
                "displayContent": [{"text": typed}]}),
            gemini("b", "It prints nothing."),
            json!("not a record"),
            json!(["not", "a", "record"]),
        ],
    );
    // A line cut off by a crash must not hide the rest of the log.
    fs::write(
        &path,
        format!(
            "{}{{\"id\":\"z\",\"type\":\"us",
            fs::read_to_string(&path).unwrap()
        ),
    )
    .unwrap();

    let adapter = GeminiAdapter::with_root(tmp.path().to_path_buf());
    let s = adapter.parse(&path).unwrap().unwrap();
    assert_eq!(s.title.chars().count(), 80);
    assert!(s.title.starts_with("explain @src/main.rs and more"));
    assert!(s.title.ends_with("..."));
    assert!(s.content.starts_with("» explain @src/main.rs"));
    assert!(
        !s.content.contains("referenced files"),
        "the prompt is indexed as typed, without the expansion"
    );
    assert_eq!(s.message_count, 2);
    assert_eq!(s.directory, "", "no marker and no registry entry");
}

#[test]
fn timestamp_falls_back_to_last_update_and_then_to_mtime() {
    let tmp = tempfile::tempdir().unwrap();
    let chats = chats_dir(tmp.path(), "app");
    let adapter = GeminiAdapter::with_root(tmp.path().to_path_buf());

    let path = write_log(
        &chats,
        "session-2026-07-18T10-00-66666666.jsonl",
        &[
            json!({"sessionId": PLAIN_ID, "projectHash": PROJECT_HASH}),
            user("a", "hello there"),
            touched("2026-07-18T10:00:07.000Z"),
        ],
    );
    let s = adapter.parse(&path).unwrap().unwrap();
    assert_eq!(
        s.timestamp,
        parse_iso8601_secs("2026-07-18T10:00:07Z").unwrap()
    );

    let path = write_log(
        &chats,
        "session-2026-07-18T10-00-66666666.jsonl",
        &[
            json!({"sessionId": PLAIN_ID, "projectHash": PROJECT_HASH}),
            user("a", "hello there"),
        ],
    );
    let s = adapter.parse(&path).unwrap().unwrap();
    assert_eq!(s.timestamp, mtime_nanos(&path).unwrap() / 1_000_000_000);
}

#[test]
fn legacy_json_document_parses_with_its_root_from_the_registry() {
    let (_tmp, adapter) = fixture();
    let scan = adapter.scan().unwrap();
    let (path, _) = &scan[LEGACY_ID];
    assert!(path.ends_with("tmp/site/chats/session-2026-05-01T09-00-22222222.json"));

    let s = adapter.parse(path).unwrap().unwrap();
    assert_eq!(s.id, LEGACY_ID);
    assert_eq!(s.directory, "/work/site", "reverse lookup in projects.json");
    assert_eq!(s.title, "Rename the config module");
    assert_eq!(s.content, "» Rename the config module\n\n  Renamed it.");
    assert_eq!(s.message_count, 2);
    assert_eq!(
        s.timestamp,
        parse_iso8601_secs("2026-05-01T09:00:00.000Z").unwrap()
    );
    assert_eq!(s.source_path.as_deref(), path.to_str());
}

#[test]
fn unmigrated_hash_directory_has_no_known_root() {
    let (_tmp, adapter) = fixture();
    let s = parse_id(&adapter, HASHED_ID);
    assert_eq!(s.directory, "");
    assert_eq!(s.content, "» Old question\n\n  Old answer");
}

#[test]
fn a_resumed_legacy_session_is_listed_once_from_its_newer_log() {
    let (tmp, adapter) = fixture();
    // Resuming a .json session continues it in a .jsonl beside the old file.
    let site = tmp.path().join("tmp/site/chats");
    let converted = write_log(
        &site,
        "session-2026-05-01T09-00-22222222.jsonl",
        &[
            meta(LEGACY_ID, "2026-05-01T09:00:00.000Z"),
            user("l1", "Rename the config module"),
            gemini("l3", "Renamed it."),
            json!({"$set": {"sessionId": LEGACY_ID}}),
            user("l5", "Now update the imports"),
            touched("2026-05-02T12:00:00.000Z"),
        ],
    );

    let scan = adapter.scan().unwrap();
    assert_eq!(scan.len(), 4, "still one entry per session id");
    assert_eq!(scan[LEGACY_ID].0, converted);
    let s = adapter.parse(&scan[LEGACY_ID].0).unwrap().unwrap();
    assert_eq!(s.id, LEGACY_ID);
    assert!(s.content.ends_with("» Now update the imports"));
}

#[test]
fn subagent_and_promptless_files_are_not_sessions() {
    let (tmp, adapter) = fixture();
    let app = tmp.path().join("tmp/app/chats");
    for path in [
        app.join(MAIN_ID).join(format!("{SUB_NESTED_ID}.jsonl")),
        app.join("session-2026-07-17T10-30-cccccccc.jsonl"),
    ] {
        assert!(path.is_file());
        assert!(
            adapter.parse(&path).unwrap().is_none(),
            "{} is a subagent",
            path.display()
        );
    }

    let scan = adapter.scan().unwrap();
    assert!(adapter.parse(&scan[EMPTY_ID].0).unwrap().is_none());
    assert!(adapter
        .parse(&app.join("session-missing.jsonl"))
        .unwrap()
        .is_none());
}

#[test]
fn scan_ids_match_the_ids_parse_returns() {
    let (_tmp, adapter) = fixture();
    for (id, (path, _)) in adapter.scan().unwrap() {
        if let Some(session) = adapter.parse(&path).unwrap() {
            assert_eq!(session.id, id);
        }
    }
}

#[test]
fn incremental_index_follows_new_changed_and_deleted_sessions() {
    let (tmp, adapter) = fixture();

    // A subagent indexed by mistake must disappear on the next pass.
    let known: KnownSessions = [
        (SessionKey::new(AgentId::Gemini, SUB_FLAT_ID), 1),
        (SessionKey::new(AgentId::Codex, "someone-elses"), 1),
    ]
    .into_iter()
    .collect();
    let mut streamed = Vec::new();
    let first = adapter
        .find_incremental(&known, &mut |s| streamed.push(s.id.clone()))
        .unwrap();
    let mut ids: Vec<&str> = first.upserts.iter().map(|s| s.id.as_str()).collect();
    ids.sort();
    assert_eq!(ids, vec![MAIN_ID, LEGACY_ID, HASHED_ID]);
    assert_eq!(streamed.len(), 3);
    assert_eq!(first.deleted, vec![SUB_FLAT_ID.to_string()]);

    // Nothing changed: nothing to parse.
    let mut known: KnownSessions = first.upserts.iter().map(|s| (s.key(), s.mtime)).collect();
    let second = adapter.find_incremental(&known, &mut |_| {}).unwrap();
    assert!(second.upserts.is_empty());
    assert!(second.deleted.is_empty());

    // An older known mtime means the file was appended to since.
    known.insert(SessionKey::new(AgentId::Gemini, MAIN_ID), 1);
    fs::remove_file(
        tmp.path()
            .join("tmp/site/chats/session-2026-05-01T09-00-22222222.json"),
    )
    .unwrap();
    let third = adapter.find_incremental(&known, &mut |_| {}).unwrap();
    let ids: Vec<&str> = third.upserts.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, vec![MAIN_ID]);
    assert_eq!(third.deleted, vec![LEGACY_ID.to_string()]);
}

#[test]
fn resume_runs_gemini_with_the_session_uuid() {
    let (_tmp, adapter) = fixture();
    let s = parse_id(&adapter, MAIN_ID);
    assert!(adapter.supports_yolo());
    assert!(!adapter.supports_fork());
    assert!(adapter.mtime_is_per_session());
    assert_eq!(
        adapter.resume_command(&s, false),
        vec!["gemini", "--resume", MAIN_ID]
    );
    assert_eq!(
        adapter.resume_command(&s, true),
        vec!["gemini", "--yolo", "--resume", MAIN_ID]
    );
    assert!(adapter.fork_command(&s, None, None, false).is_none());
}

//! Fixture tests for the Pi coding agent's session store:
//! `~/.pi/agent/sessions/<workspace>/<timestamp>_<uuid>.jsonl`, or a flat
//! custom directory. The row shapes have not been compared with files written
//! by a real Pi.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use hopback::adapters::pi::{resolve_sessions_dir, PiAdapter};
use hopback::adapters::{mtime_nanos, parse_iso8601_secs, Adapter, KnownSessions};
use hopback::config::AgentId;
use hopback::model::SessionKey;

const MAIN_ID: &str = "11111111-1111-4111-8111-111111111111";
const FILE_ID: &str = "22222222-2222-4222-8222-222222222222";
const HEADER_ID: &str = "33333333-3333-4333-8333-333333333333";
const WORKSPACE: &str = "--work-app--";

/// 2026-07-15T10:00:00Z. Message timestamps in the fixtures are this plus a
/// few seconds, in milliseconds.
const BASE_SECS: i64 = 1_784_109_600;

fn write_lines(path: &Path, lines: &[&str]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let body: String = lines.iter().map(|line| format!("{line}\n")).collect();
    fs::write(path, body).unwrap();
}

fn header(id: &str) -> String {
    format!(
        r#"{{"type":"session","version":3,"id":"{id}","timestamp":"2026-07-15T10:00:00.000Z","cwd":"/work/app"}}"#
    )
}

fn user(text: &str) -> String {
    format!(
        r#"{{"type":"message","id":"u1","parentId":null,"message":{{"role":"user","content":"{text}"}}}}"#
    )
}

/// A session file with a header and one prompt.
fn write_simple(path: &Path, id: &str) {
    write_lines(path, &[&header(id), &user("Prompt")]);
}

fn sorted_ids(adapter: &PiAdapter) -> Vec<String> {
    let mut ids: Vec<String> = adapter.scan().unwrap().into_keys().collect();
    ids.sort();
    ids
}

fn known(entries: &[(&str, i64)]) -> KnownSessions {
    entries
        .iter()
        .map(|(id, mtime)| (SessionKey::new(AgentId::Pi, *id), *mtime))
        .collect()
}

/// A default-layout store holding one session that uses every row type the
/// adapter reads, in the key order Pi writes, plus rows it must ignore.
fn main_fixture() -> (tempfile::TempDir, PiAdapter, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let sessions = tmp.path().join("sessions");
    let path = sessions
        .join(WORKSPACE)
        .join(format!("2026-07-15T10-00-00-000Z_{MAIN_ID}.jsonl"));
    write_lines(
        &path,
        &[
            &header(MAIN_ID),
            r#"{"type":"model_change","id":"m0","parentId":null,"timestamp":"2026-07-15T10:00:00.100Z","provider":"example","modelId":"model-x"}"#,
            r#"{"type":"thinking_level_change","id":"m1","parentId":"m0","timestamp":"2026-07-15T10:00:00.200Z","thinkingLevel":"off"}"#,
            r#"{"type":"message","id":"a1","parentId":"m1","timestamp":"2026-07-15T10:00:01.000Z","message":{"role":"user","content":"Add retry logic to the uploader","timestamp":1784109601000}}"#,
            r#"{"type":"message","id":"a2","parentId":"a1","timestamp":"2026-07-15T10:00:02.000Z","message":{"role":"assistant","content":[{"type":"thinking","thinking":"private reasoning"},{"type":"text","text":"Added a retry loop with backoff."},{"type":"toolCall","id":"call_1","name":"bash","arguments":{"command":"hidden tool arguments"}}],"timestamp":1784109602000}}"#,
            r#"{"type":"message","id":"a3","parentId":"a2","timestamp":"2026-07-15T10:00:03.000Z","message":{"role":"toolResult","toolCallId":"call_1","toolName":"bash","content":[{"type":"text","text":"tool output stays out"}],"isError":false,"timestamp":1784109603000}}"#,
            r#"{"type":"message","id":"a4","parentId":"a3","timestamp":"2026-07-15T10:00:03.100Z","message":{"role":"custom","customType":"note","content":[{"type":"text","text":"visible extension note"}],"display":true,"timestamp":1784109603100}}"#,
            r#"{"type":"message","id":"a5","parentId":"a4","timestamp":"2026-07-15T10:00:03.200Z","message":{"role":"hookMessage","hookName":"legacy","content":"legacy hook note","display":true,"timestamp":1784109603200}}"#,
            r#"{"type":"message","id":"a6","parentId":"a5","timestamp":"2026-07-15T10:00:03.300Z","message":{"role":"custom","customType":"hidden","content":"hidden extension context","display":false,"timestamp":1784109603300}}"#,
            r#"{"type":"custom_message","id":"a7","parentId":"a6","timestamp":"2026-07-15T10:00:04.000Z","customType":"note","content":[{"type":"text","text":"top-level visible note"}],"display":true}"#,
            r#"{"type":"custom_message","id":"a8","parentId":"a7","timestamp":"2026-07-15T10:00:04.500Z","customType":"hidden","content":[{"type":"text","text":"hidden top-level context"}],"display":false}"#,
            r#"{"type":"compaction","id":"a9","parentId":"a8","timestamp":"2026-07-15T10:00:05.000Z","summary":"compacted context summary","firstKeptEntryId":"a2","tokensBefore":1000}"#,
            r#"{"type":"message","id":"a10","parentId":"a9","timestamp":"2026-07-15T10:00:06.000Z","message":{"role":"bashExecution","command":"ls","output":"bash output stays out","exitCode":0,"cancelled":false,"truncated":false,"timestamp":1784109606000}}"#,
            r#"{"type":"custom","id":"a11","parentId":"a10","timestamp":"2026-07-15T10:00:06.500Z","customType":"ext-state","data":{"note":"extension state stays out"}}"#,
            r#"{"type":"label","id":"a12","parentId":"a11","timestamp":"2026-07-15T10:00:06.600Z","targetId":"a2","label":"checkpoint label"}"#,
            r#"{"type":"session_info","id":"a13","parentId":"a12","timestamp":"2026-07-15T10:00:07.000Z","name":"  Uploader retries  "}"#,
        ],
    );
    (tmp, PiAdapter::with_sessions_dir(sessions), path)
}

#[test]
fn scans_only_the_level_pi_lists() {
    let tmp = tempfile::tempdir().unwrap();
    let grouped = tmp.path().join("sessions");
    let project = grouped.join(WORKSPACE);
    write_simple(&project.join("2026-07-15T10-00-00-000Z_main.jsonl"), "main");
    // Subagent extensions nest child sessions under the parent's file stem.
    write_simple(
        &project.join("2026-07-15T10-00-00-000Z_main/run-1/run-0/session.jsonl"),
        "nested",
    );
    write_simple(
        &project.join("2026-07-15T10-00-00-000Z_main/tasks/task.jsonl"),
        "task",
    );
    write_simple(&grouped.join("loose.jsonl"), "loose");
    fs::write(project.join("notes.txt"), "not a transcript").unwrap();
    let custom = tmp.path().join("custom");
    write_simple(&custom.join("2026-07-15T10-00-00-000Z_flat.jsonl"), "flat");
    write_simple(&custom.join("child/session.jsonl"), "child");

    let grouped = PiAdapter::with_sessions_dir(grouped);
    assert!(grouped.is_available());
    assert_eq!(sorted_ids(&grouped), ["main"]);
    assert_eq!(sorted_ids(&PiAdapter::with_custom_dir(custom)), ["flat"]);

    // A nested session indexed by an older build disappears on the next pass.
    let res = grouped
        .find_incremental(&known(&[("nested", 1)]), &mut |_| {})
        .unwrap();
    let ids: Vec<&str> = res.upserts.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, ["main"]);
    assert_eq!(res.deleted, ["nested"]);
}

#[test]
fn parses_messages_and_metadata() {
    let (_tmp, adapter, path) = main_fixture();
    let scan = adapter.scan().unwrap();
    assert_eq!(scan.len(), 1);
    let (scan_path, scan_mtime) = &scan[MAIN_ID];
    assert_eq!(scan_path, &path);
    assert_eq!(*scan_mtime, mtime_nanos(&path).unwrap());

    let s = adapter.parse(scan_path).unwrap().expect("a session");
    assert_eq!(s.id, MAIN_ID);
    assert_eq!(s.agent, AgentId::Pi);
    assert_eq!(s.title, "Uploader retries", "session_info name, trimmed");
    assert_eq!(s.directory, "/work/app");
    assert_eq!(s.message_count, 2, "one prompt and one reply with text");
    assert_eq!(
        s.content,
        [
            "» Add retry logic to the uploader",
            "  Added a retry loop with backoff.",
            "  visible extension note",
            "  legacy hook note",
            "  top-level visible note",
            "  compacted context summary",
        ]
        .join("\n\n")
    );
    // The last user or assistant message; later tool and extension rows do
    // not count as activity.
    assert_eq!(s.timestamp, BASE_SECS + 2);
    assert_eq!(
        s.timestamp,
        parse_iso8601_secs("2026-07-15T10:00:02Z").unwrap()
    );
    assert_eq!(s.mtime, *scan_mtime);
    assert!(!s.yolo);
    assert_eq!(s.source_path.as_deref(), path.to_str());
}

#[test]
fn resumes_by_session_id_without_a_yolo_or_fork_variant() {
    let (_tmp, adapter, path) = main_fixture();
    let s = adapter.parse(&path).unwrap().unwrap();
    assert_eq!(adapter.id(), AgentId::Pi);
    assert_eq!(
        adapter.resume_command(&s, false),
        ["pi", "--session", MAIN_ID]
    );
    assert_eq!(
        adapter.resume_command(&s, true),
        ["pi", "--session", MAIN_ID]
    );
    assert!(!adapter.supports_yolo());
    assert!(!adapter.detects_yolo());
    assert!(!adapter.supports_fork());
    assert!(adapter.fork_command(&s, None, None, false).is_none());
    assert!(adapter.mtime_is_per_session());
}

#[test]
fn scan_ids_match_parsed_ids_for_every_file_name_shape() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    // A renamed or copied file whose name no longer matches its header: the
    // header id is the one `pi --session` takes.
    write_simple(
        &dir.join(format!("2026-07-15T10-00-00-000Z_{FILE_ID}.jsonl")),
        HEADER_ID,
    );
    // No UUID in the name.
    write_simple(&dir.join("session_plain.jsonl"), "from-header");
    // No header row: the id is the file name's suffix.
    write_lines(
        &dir.join("2026-07-15T11-00-00-000Z_abc123.jsonl"),
        &[&user("No header here")],
    );
    // A header without an id, and a blank line before the header.
    write_lines(
        &dir.join("2026-07-15T12-00-00-000Z_noid.jsonl"),
        &[r#"{"type":"session","cwd":"/work/app"}"#, &user("Prompt")],
    );
    write_lines(
        &dir.join("2026-07-15T13-00-00-000Z_blank.jsonl"),
        &["", &header("after-blank"), &user("Prompt")],
    );

    let adapter = PiAdapter::with_custom_dir(dir);
    let scan = adapter.scan().unwrap();
    let mut ids: Vec<&str> = scan.keys().map(String::as_str).collect();
    ids.sort();
    assert_eq!(
        ids,
        [HEADER_ID, "abc123", "after-blank", "from-header", "noid"]
    );
    for (id, (path, mtime)) in &scan {
        let s = adapter.parse(path).unwrap().expect("a session");
        assert_eq!(&s.id, id, "scan key and parsed id for {path:?}");
        assert_eq!(s.mtime, *mtime);
    }

    // So a pass that re-indexes the renamed file does not also delete it.
    let res = adapter
        .find_incremental(&known(&[(HEADER_ID, 1)]), &mut |_| {})
        .unwrap();
    assert!(res.upserts.iter().any(|s| s.id == HEADER_ID));
    assert!(res.deleted.is_empty());
}

#[test]
fn rows_with_unexpected_shapes_are_ignored_field_by_field() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    let path = dir.join("session_shapes.jsonl");
    write_lines(
        &path,
        &[
            r#"{"type":"session","id":"shapes","cwd":7,"timestamp":"2026-07-15T10:00:00.000Z"}"#,
            r#"{"type":"message","timestamp":"2026-07-15T10:00:05.000Z","message":{"role":"user","content":[{"type":"text","text":"kept prompt"},{"type":"text","text":42},"bare",{"type":"text","text":""}],"timestamp":"not a number"}}"#,
            r#"{"type":"message","message":"not an object"}"#,
            r#"{"type":"message","message":{"role":"user","content":{"text":"object content"}}}"#,
            r#"{"type":"custom_message","display":"true","content":"string display flag is not true"}"#,
            r#"{"type":"compaction","summary":"   "}"#,
            r#"{"type":"session_info","name":12}"#,
            r#"["message",{"role":"user","content":"array row"}]"#,
            "null",
            "42",
        ],
    );

    let adapter = PiAdapter::with_custom_dir(dir);
    let s = adapter.parse(&path).unwrap().expect("a session");
    assert_eq!(s.id, "shapes");
    assert_eq!(s.content, "» kept prompt");
    assert_eq!(s.title, "kept prompt");
    assert_eq!(s.directory, "");
    assert_eq!(s.message_count, 1);
    // A non-numeric message timestamp falls back to the row timestamp.
    assert_eq!(s.timestamp, BASE_SECS + 5);
}

#[test]
fn content_is_filtered_by_role_regardless_of_key_order() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    let path = dir.join("session_order.jsonl");
    write_lines(
        &path,
        &[
            r#"{"type":"session","id":"order","cwd":"/work/app"}"#,
            r#"{"type":"message","message":{"role":"user","content":"role first"}}"#,
            r#"{"type":"message","message":{"content":[{"text":"content first","type":"text"}],"role":"assistant"}}"#,
            r#"{"type":"message","message":{"role":"toolResult","content":"tool output role first"}}"#,
            r#"{"type":"message","message":{"content":"tool output content first","role":"toolResult"}}"#,
            r#"{"message":{"role":"user","content":"type after message"},"type":"message"}"#,
            r#"{"content":"type after content","display":true,"type":"custom_message"}"#,
            r#"{"content":"content of another row type","type":"label"}"#,
        ],
    );

    let s = PiAdapter::with_custom_dir(dir)
        .parse(&path)
        .unwrap()
        .unwrap();
    assert_eq!(
        s.content,
        [
            "» role first",
            "  content first",
            "» type after message",
            "  type after content",
        ]
        .join("\n\n")
    );
    assert_eq!(s.message_count, 3);
}

#[test]
fn rows_that_are_not_valid_json_are_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    let path = dir.join("session_torn.jsonl");
    let header = r#"{"type":"session","id":"torn","cwd":"/work/app"}"#;
    let prompt = user("prompt");
    let reply = r#"{"type":"message","message":{"role":"assistant","content":"reply"}}"#;
    let adapter = PiAdapter::with_custom_dir(dir.clone());
    fs::create_dir_all(&dir).unwrap();

    // A last row that Pi is still writing.
    fs::write(&path, format!("{header}\n{prompt}\n{{\"type\":")).unwrap();
    let s = adapter.parse(&path).unwrap().unwrap();
    assert_eq!((s.id.as_str(), s.content.as_str()), ("torn", "» prompt"));

    // A torn row in the middle.
    fs::write(&path, format!("{header}\n{{\"type\":\n{prompt}\n{reply}\n")).unwrap();
    let s = adapter.parse(&path).unwrap().unwrap();
    assert_eq!(s.content, "» prompt\n\n  reply");

    // Invalid UTF-8 loses only its own row.
    let mut bytes = format!("{header}\n{prompt}\n").into_bytes();
    bytes.extend_from_slice(
        b"{\"type\":\"message\",\"message\":{\"role\":\"user\",\"content\":\"\xff\"}}\n",
    );
    bytes.extend_from_slice(format!("{reply}\n").as_bytes());
    fs::write(&path, bytes).unwrap();
    let s = adapter.parse(&path).unwrap().unwrap();
    assert_eq!(s.content, "» prompt\n\n  reply");

    // A garbage first row is skipped when looking for the header id, by scan
    // and by parse alike.
    fs::write(&path, format!("not json\n{header}\n{prompt}\n")).unwrap();
    assert_eq!(sorted_ids(&adapter), ["torn"]);
    assert_eq!(adapter.parse(&path).unwrap().unwrap().id, "torn");
}

#[test]
fn title_prefers_the_latest_name_then_the_first_prompt() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    let adapter = PiAdapter::with_custom_dir(dir.clone());
    let long_prompt = "word ".repeat(40);

    let renamed = dir.join("session_renamed.jsonl");
    write_lines(
        &renamed,
        &[
            &header("renamed"),
            r#"{"type":"session_info","name":"First name"}"#,
            &user("Prompt"),
            r#"{"type":"session_info","name":"Second name"}"#,
        ],
    );
    assert_eq!(
        adapter.parse(&renamed).unwrap().unwrap().title,
        "Second name"
    );

    // A blank name clears the earlier one.
    let cleared = dir.join("session_cleared.jsonl");
    write_lines(
        &cleared,
        &[
            &header("cleared"),
            r#"{"type":"session_info","name":"First name"}"#,
            &user(long_prompt.trim()),
            r#"{"type":"message","message":{"role":"user","content":"Second prompt"}}"#,
            r#"{"type":"session_info","name":"  "}"#,
        ],
    );
    let s = adapter.parse(&cleared).unwrap().unwrap();
    assert!(s.title.starts_with("word word"));
    assert!(s.title.ends_with("..."));
    assert_eq!(s.title.chars().count(), 80);
    assert_eq!(s.message_count, 2);
}

#[test]
fn sessions_without_a_prompt_are_kept_only_when_they_have_text() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    let adapter = PiAdapter::with_custom_dir(dir.clone());

    let reply_only = dir.join("session_reply.jsonl");
    write_lines(
        &reply_only,
        &[
            &header("reply"),
            r#"{"type":"message","message":{"role":"assistant","content":[{"type":"text","text":"Unprompted reply"}]}}"#,
        ],
    );
    let s = adapter.parse(&reply_only).unwrap().unwrap();
    assert_eq!(s.title, "(no messages)");
    assert_eq!(s.message_count, 1);
    assert_eq!(s.content, "  Unprompted reply");

    let summary_only = dir.join("session_summary.jsonl");
    write_lines(
        &summary_only,
        &[
            &header("summary"),
            r#"{"type":"branch_summary","id":"b1","parentId":null,"summary":"what the other branch did"}"#,
        ],
    );
    let s = adapter.parse(&summary_only).unwrap().unwrap();
    assert_eq!(s.message_count, 0);
    assert_eq!(s.content, "  what the other branch did");

    // Opened and closed without any text: listed by scan, never indexed.
    let empty = dir.join("session_empty.jsonl");
    write_lines(
        &empty,
        &[
            &header("empty"),
            r#"{"type":"message","message":{"role":"user","content":[{"type":"image","data":"xxxx","mimeType":"image/png"}]}}"#,
            r#"{"type":"message","message":{"role":"toolResult","content":"only tool output"}}"#,
        ],
    );
    assert!(adapter.parse(&empty).unwrap().is_none());
    assert!(adapter.scan().unwrap().contains_key("empty"));
    let res = adapter.find_incremental(&known(&[]), &mut |_| {}).unwrap();
    let mut ids: Vec<&str> = res.upserts.iter().map(|s| s.id.as_str()).collect();
    ids.sort();
    assert_eq!(ids, ["reply", "summary"]);
    assert!(res.deleted.is_empty());
}

#[test]
fn every_branch_of_the_entry_tree_is_indexed_in_file_order() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    let path = dir.join("session_tree.jsonl");
    write_lines(
        &path,
        &[
            &header("tree"),
            r#"{"type":"message","id":"a1","parentId":null,"message":{"role":"user","content":"Start here"}}"#,
            r#"{"type":"message","id":"a2","parentId":"a1","message":{"role":"assistant","content":"Abandoned answer"}}"#,
            r#"{"type":"message","id":"b2","parentId":"a1","message":{"role":"assistant","content":"Kept answer"}}"#,
        ],
    );
    let s = PiAdapter::with_custom_dir(dir)
        .parse(&path)
        .unwrap()
        .unwrap();
    assert_eq!(
        s.content,
        "» Start here\n\n  Abandoned answer\n\n  Kept answer"
    );
    assert_eq!(s.message_count, 3);
}

#[test]
fn timestamp_falls_back_to_the_header_and_then_to_the_file_mtime() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    let adapter = PiAdapter::with_custom_dir(dir.clone());

    // Message times are compared, not taken in file order.
    let unordered = dir.join("session_unordered.jsonl");
    write_lines(
        &unordered,
        &[
            &header("unordered"),
            r#"{"type":"message","timestamp":"2026-07-15T10:00:09.000Z","message":{"role":"user","content":"Later branch"}}"#,
            r#"{"type":"message","timestamp":"2026-07-15T10:00:01.000Z","message":{"role":"assistant","content":"Earlier","timestamp":1784109604500.7}}"#,
        ],
    );
    let s = adapter.parse(&unordered).unwrap().unwrap();
    assert_eq!(s.timestamp, BASE_SECS + 9);

    let header_only = dir.join("session_header.jsonl");
    write_simple(&header_only, "header");
    let s = adapter.parse(&header_only).unwrap().unwrap();
    assert_eq!(s.timestamp, BASE_SECS);

    let bare = dir.join("session_bare.jsonl");
    write_lines(&bare, &[&user("No timestamps anywhere")]);
    let s = adapter.parse(&bare).unwrap().unwrap();
    assert_eq!(s.timestamp, mtime_nanos(&bare).unwrap() / 1_000_000_000);
}

#[test]
fn incremental_reparses_only_changed_files_and_reports_deletions() {
    let (_tmp, adapter, path) = main_fixture();
    let other = path.with_file_name("2026-07-15T11-00-00-000Z_other.jsonl");
    write_simple(&other, "other");
    let scan = adapter.scan().unwrap();
    let indexed = known(&[(MAIN_ID, scan[MAIN_ID].1), ("other", scan["other"].1)]);

    let res = adapter.find_incremental(&indexed, &mut |_| {}).unwrap();
    assert!(res.upserts.is_empty());
    assert!(res.deleted.is_empty());

    // Pi appends to the transcript.
    let mut body = fs::read_to_string(&other).unwrap();
    body.push_str(
        r#"{"type":"message","message":{"role":"assistant","content":"Appended reply"}}"#,
    );
    body.push('\n');
    fs::write(&other, body).unwrap();
    fs::File::options()
        .write(true)
        .open(&other)
        .unwrap()
        .set_modified(SystemTime::now() + Duration::from_secs(60))
        .unwrap();
    fs::remove_file(&path).unwrap();

    let mut streamed = Vec::new();
    let res = adapter
        .find_incremental(&indexed, &mut |s| streamed.push(s.id.clone()))
        .unwrap();
    assert_eq!(streamed, ["other"]);
    assert_eq!(res.upserts.len(), 1);
    let s = &res.upserts[0];
    assert_eq!(s.content, "» Prompt\n\n  Appended reply");
    assert_eq!(s.mtime, mtime_nanos(&other).unwrap());
    assert!(s.mtime > indexed[&SessionKey::new(AgentId::Pi, "other")]);
    assert_eq!(res.deleted, [MAIN_ID]);
}

#[test]
fn a_missing_store_is_unavailable_and_drops_what_was_indexed() {
    let tmp = tempfile::tempdir().unwrap();
    let adapter = PiAdapter::with_sessions_dir(tmp.path().join("no-such-dir"));
    assert!(!adapter.is_available());
    assert!(adapter.scan().unwrap().is_empty());

    let mut indexed = known(&[(MAIN_ID, 1)]);
    indexed.insert(SessionKey::new(AgentId::Codex, "not-mine"), 1);
    let res = adapter.find_incremental(&indexed, &mut |_| {}).unwrap();
    assert!(res.upserts.is_empty());
    assert_eq!(res.deleted, [MAIN_ID]);
}

#[test]
fn two_files_with_one_header_id_resolve_to_the_newer_file() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("store");
    let older = dir.join("2026-07-15T10-00-00-000Z_copy-a.jsonl");
    let newer = dir.join("2026-07-15T10-00-00-000Z_copy-b.jsonl");
    write_simple(&older, "same");
    write_simple(&newer, "same");
    fs::File::options()
        .write(true)
        .open(&newer)
        .unwrap()
        .set_modified(SystemTime::now() + Duration::from_secs(60))
        .unwrap();

    let adapter = PiAdapter::with_custom_dir(dir);
    let scan = adapter.scan().unwrap();
    assert_eq!(scan.len(), 1);
    assert_eq!(scan["same"].0, newer);
    assert_eq!(scan["same"].1, mtime_nanos(&newer).unwrap());
}

#[test]
fn store_location_follows_the_env_override_then_settings_then_the_default() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let agent_dir = home.join(".pi").join("agent");
    fs::create_dir_all(&agent_dir).unwrap();

    // Nothing configured: the grouped default store.
    assert_eq!(
        resolve_sessions_dir(None, &agent_dir, &home),
        (agent_dir.join("sessions"), false)
    );
    assert_eq!(
        resolve_sessions_dir(Some("  "), &agent_dir, &home),
        (agent_dir.join("sessions"), false)
    );

    // Settings that do not name a directory change nothing.
    for settings in [r#"{"sessionDir":"  "}"#, r#"{"sessionDir":5}"#, "not json"] {
        fs::write(agent_dir.join("settings.json"), settings).unwrap();
        assert_eq!(
            resolve_sessions_dir(None, &agent_dir, &home),
            (agent_dir.join("sessions"), false),
            "{settings}"
        );
    }

    // `sessionDir` in settings.json: a flat custom store.
    fs::write(
        agent_dir.join("settings.json"),
        r#"{"theme":"dark","sessionDir":"~/pi-sessions"}"#,
    )
    .unwrap();
    assert_eq!(
        resolve_sessions_dir(None, &agent_dir, &home),
        (home.join("pi-sessions"), true)
    );

    // The environment variable wins over settings.json.
    assert_eq!(
        resolve_sessions_dir(Some("/srv/pi"), &agent_dir, &home),
        (PathBuf::from("/srv/pi"), true)
    );
    assert_eq!(
        resolve_sessions_dir(Some("~"), &agent_dir, &home),
        (home.clone(), true)
    );
    assert_eq!(
        resolve_sessions_dir(Some("~/elsewhere"), &agent_dir, &home),
        (home.join("elsewhere"), true)
    );
}

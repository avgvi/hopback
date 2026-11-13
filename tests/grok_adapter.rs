//! Fixture tests for the Grok Build session layout:
//! `<root>/sessions/<percent-encoded cwd>/<session id>/` holding
//! `summary.json` beside `updates.jsonl`, an ACP update stream whose message
//! chunks are merged by `promptIndex` (user) and `promptId` (agent).
//!
//! No real Grok data was available, so these fixtures are built from the
//! format as the adapter reads it, not from captured files.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use hopback::adapters::grok::GrokAdapter;
use hopback::adapters::{mtime_nanos, parse_iso8601_secs, Adapter, KnownSessions};
use hopback::config::AgentId;
use hopback::model::SessionKey;
use serde_json::{json, Value};

const WORKSPACE: &str = "%2Fwork%2Fapp";
const MAIN_ID: &str = "019edf9c-0000-7000-8000-000000000001";
const REWIND_ID: &str = "019edf9c-0000-7000-8000-000000000002";

fn session_dir(root: &Path, workspace: &str, dir: &str) -> PathBuf {
    root.join("sessions").join(workspace).join(dir)
}

/// Writes one session directory and returns its `updates.jsonl`, the path
/// `scan` reports. `summary` is written verbatim so tests can break it.
fn write_session_raw(
    root: &Path,
    workspace: &str,
    dir: &str,
    summary: Option<&str>,
    rows: &[Value],
) -> PathBuf {
    let dir = session_dir(root, workspace, dir);
    fs::create_dir_all(&dir).unwrap();
    if let Some(summary) = summary {
        fs::write(dir.join("summary.json"), summary).unwrap();
    }
    let body: String = rows.iter().map(|r| format!("{r}\n")).collect();
    let updates = dir.join("updates.jsonl");
    fs::write(&updates, body).unwrap();
    updates
}

fn write_session(root: &Path, dir: &str, summary: Value, rows: &[Value]) -> PathBuf {
    write_session_raw(root, WORKSPACE, dir, Some(&summary.to_string()), rows)
}

fn summary(id: &str) -> Value {
    json!({"info": {"id": id, "cwd": "/work/app"}, "created_at": "2026-07-17T10:00:00Z"})
}

fn with(mut base: Value, extra: Value) -> Value {
    base.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    base
}

fn user(text: &str, prompt_index: u64) -> Value {
    json!({"method": "session/update", "params": {"update": {
        "sessionUpdate": "user_message_chunk",
        "content": {"type": "text", "text": text},
        "_meta": {"promptIndex": prompt_index}
    }}})
}

fn user_plain(text: &str) -> Value {
    json!({"params": {"update": {
        "sessionUpdate": "user_message_chunk",
        "content": {"type": "text", "text": text}
    }}})
}

fn agent(text: &str, prompt_id: &str) -> Value {
    json!({"method": "session/update", "params": {
        "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": text}},
        "_meta": {"promptId": prompt_id}
    }})
}

fn at(mut row: Value, timestamp: Value) -> Value {
    row["timestamp"] = timestamp;
    row
}

fn rewind(target: u64) -> Value {
    json!({"method": "_x.ai/session/update", "params": {"update": {
        "sessionUpdate": "rewind_marker", "target_prompt_index": target
    }}})
}

fn tool_call() -> Value {
    json!({"params": {"update": {
        "sessionUpdate": "tool_call", "toolCallId": "t1", "title": "Read file", "status": "pending"
    }}})
}

fn set_mtime(path: &Path, secs: u64) {
    let time = SystemTime::UNIX_EPOCH + Duration::from_secs(secs);
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(time)
        .unwrap();
}

fn adapter(root: &Path) -> GrokAdapter {
    GrokAdapter::with_root(root.to_path_buf())
}

fn sorted_ids(adapter: &GrokAdapter) -> Vec<String> {
    let mut ids: Vec<String> = adapter.scan().unwrap().into_keys().collect();
    ids.sort();
    ids
}

fn secs(iso: &str) -> i64 {
    parse_iso8601_secs(iso).unwrap()
}

/// A session streamed in chunks: one prompt and one reply, two chunks each.
fn main_rows() -> Vec<Value> {
    vec![
        at(user("Add Grok ", 0), json!("2026-07-17T10:00:01Z")),
        at(user("support", 0), json!("2026-07-17T10:00:01Z")),
        at(agent("Added ", "p0"), json!("2026-07-17T10:00:02Z")),
        at(agent("the adapter", "p0"), json!("2026-07-17T10:00:03Z")),
    ]
}

fn main_fixture() -> (tempfile::TempDir, GrokAdapter, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let updates = write_session(
        tmp.path(),
        MAIN_ID,
        with(
            summary(MAIN_ID),
            json!({"generated_title": "Grok adapter work"}),
        ),
        &main_rows(),
    );
    let adapter = adapter(tmp.path());
    (tmp, adapter, updates)
}

#[test]
fn adapter_identity_and_capabilities() {
    let tmp = tempfile::tempdir().unwrap();
    let a = adapter(tmp.path());
    assert_eq!(a.id(), AgentId::Grok);
    assert!(a.supports_yolo());
    assert!(!a.detects_yolo());
    assert!(!a.supports_fork());
    assert!(a.mtime_is_per_session());
    assert!(!a.uses_dir_mtime_fastpath());
}

#[test]
fn unavailable_without_a_sessions_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let a = adapter(tmp.path());
    assert!(!a.is_available());
    assert!(a.scan().unwrap().is_empty());

    fs::create_dir_all(tmp.path().join("sessions")).unwrap();
    assert!(a.is_available());
    assert!(a.scan().unwrap().is_empty());
}

#[test]
fn scan_finds_session_directories_exactly_two_levels_down() {
    let (tmp, a, updates) = main_fixture();
    let sessions = tmp.path().join("sessions");

    // None of these is a session directory holding updates.jsonl.
    fs::write(sessions.join("stray.jsonl"), "{}\n").unwrap();
    fs::write(sessions.join(WORKSPACE).join("updates.jsonl"), "{}\n").unwrap();
    let no_updates = session_dir(tmp.path(), WORKSPACE, "no-updates");
    fs::create_dir_all(&no_updates).unwrap();
    fs::write(
        no_updates.join("summary.json"),
        summary("no-updates").to_string(),
    )
    .unwrap();
    write_session_raw(
        tmp.path(),
        WORKSPACE,
        "outer/too-deep",
        Some(&summary("too-deep").to_string()),
        &[user("Prompt", 0)],
    );

    let scan = a.scan().unwrap();
    assert_eq!(scan.len(), 1);
    assert_eq!(scan[MAIN_ID].0, updates);
}

#[test]
fn parses_streamed_messages_and_summary() {
    let (_tmp, a, updates) = main_fixture();
    let scan = a.scan().unwrap();
    let (path, mtime) = &scan[MAIN_ID];
    let s = a.parse(path).unwrap().expect("a session with a prompt");

    assert_eq!(s.id, MAIN_ID);
    assert_eq!(s.agent, AgentId::Grok);
    assert_eq!(s.title, "Grok adapter work");
    assert_eq!(s.directory, "/work/app");
    assert_eq!(s.message_count, 2, "one prompt and one reply");
    assert_eq!(s.content, "» Add Grok support\n\n  Added the adapter");
    assert_eq!(s.timestamp, secs("2026-07-17T10:00:03Z"));
    assert_eq!(s.mtime, *mtime);
    assert!(!s.yolo);
    assert_eq!(s.source_path.as_deref(), updates.to_str());
}

#[test]
fn resume_command_with_and_without_yolo() {
    let (_tmp, a, updates) = main_fixture();
    let s = a.parse(&updates).unwrap().unwrap();
    assert_eq!(
        a.resume_command(&s, false),
        vec!["grok", "--resume", MAIN_ID]
    );
    assert_eq!(
        a.resume_command(&s, true),
        vec!["grok", "--always-approve", "--resume", MAIN_ID]
    );
    assert!(a.fork_command(&s, None, None, false).is_none());
}

#[test]
fn mtime_is_the_newest_of_summary_and_updates() {
    let (_tmp, a, updates) = main_fixture();
    let summary_path = updates.with_file_name("summary.json");

    set_mtime(&updates, 1_800_000_000);
    set_mtime(&summary_path, 1_800_000_500);
    let renamed = mtime_nanos(&summary_path).unwrap();
    assert_eq!(a.scan().unwrap()[MAIN_ID].1, renamed);
    assert_eq!(a.parse(&updates).unwrap().unwrap().mtime, renamed);

    set_mtime(&updates, 1_800_001_000);
    let appended = mtime_nanos(&updates).unwrap();
    assert!(appended > renamed);
    assert_eq!(a.scan().unwrap()[MAIN_ID].1, appended);
    assert_eq!(a.parse(&updates).unwrap().unwrap().mtime, appended);
}

#[test]
fn rewinds_discard_abandoned_messages() {
    let tmp = tempfile::tempdir().unwrap();
    let updates = write_session(
        tmp.path(),
        REWIND_ID,
        summary(REWIND_ID),
        &[
            user("Keep prompt", 0),
            agent("Keep response", "p0"),
            user("Discard prompt", 1),
            agent("Discard response", "p1"),
            rewind(1),
            user("Replacement prompt", 1),
            agent("Replacement response", "p2"),
        ],
    );
    let s = adapter(tmp.path()).parse(&updates).unwrap().unwrap();

    assert_eq!(s.message_count, 4);
    assert_eq!(
        s.content,
        "» Keep prompt\n\n  Keep response\n\n» Replacement prompt\n\n  Replacement response"
    );
    assert_eq!(s.title, "Keep prompt");
}

#[test]
fn a_trailing_rewind_drops_the_prompt_but_still_counts_as_activity() {
    let tmp = tempfile::tempdir().unwrap();
    let mut rows = main_rows();
    rows.push(at(user("Discard me", 1), json!("2026-07-17T10:00:04Z")));
    rows.push(at(
        json!({"method": "_x.ai/session/update", "params": {"update": {
            "sessionUpdate": "rewind_marker", "targetPromptIndex": 1
        }}}),
        json!("2026-07-17T10:00:05Z"),
    ));
    let updates = write_session(tmp.path(), MAIN_ID, summary(MAIN_ID), &rows);
    let s = adapter(tmp.path()).parse(&updates).unwrap().unwrap();

    assert_eq!(s.content, "» Add Grok support\n\n  Added the adapter");
    assert_eq!(s.message_count, 2);
    assert_eq!(s.timestamp, secs("2026-07-17T10:00:05Z"));
}

#[test]
fn a_rewind_to_an_unknown_prompt_changes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let updates = write_session(
        tmp.path(),
        REWIND_ID,
        summary(REWIND_ID),
        &[user("Only prompt", 0), agent("Reply", "p0"), rewind(7)],
    );
    let s = adapter(tmp.path()).parse(&updates).unwrap().unwrap();
    assert_eq!(s.content, "» Only prompt\n\n  Reply");
}

#[test]
fn a_session_rewound_to_nothing_is_not_a_session() {
    let tmp = tempfile::tempdir().unwrap();
    let updates = write_session(
        tmp.path(),
        REWIND_ID,
        summary(REWIND_ID),
        &[user("Only prompt", 0), agent("Reply", "p0"), rewind(0)],
    );
    assert!(adapter(tmp.path()).parse(&updates).unwrap().is_none());
}

#[test]
fn agent_text_keeps_accumulating_across_tool_updates() {
    let tmp = tempfile::tempdir().unwrap();
    let updates = write_session(
        tmp.path(),
        MAIN_ID,
        summary(MAIN_ID),
        &[
            user("Fix the build", 0),
            agent("Looking", "p0"),
            tool_call(),
            agent(" at it.", "p0"),
            agent("   ", "p0"),
            agent("Second answer", "p1"),
        ],
    );
    let s = adapter(tmp.path()).parse(&updates).unwrap().unwrap();
    assert_eq!(
        s.content,
        "» Fix the build\n\n  Looking at it.\n\n  Second answer"
    );
    assert_eq!(s.message_count, 3);
}

#[test]
fn a_tool_update_ends_the_user_message_being_streamed() {
    let tmp = tempfile::tempdir().unwrap();
    let updates = write_session(
        tmp.path(),
        MAIN_ID,
        summary(MAIN_ID),
        &[user("First half", 0), tool_call(), user("Second half", 0)],
    );
    let s = adapter(tmp.path()).parse(&updates).unwrap().unwrap();
    assert_eq!(s.content, "» First half\n\n» Second half");
    assert_eq!(s.message_count, 2);
}

#[test]
fn shell_commands_and_unindexed_chunks_are_not_prompts() {
    let tmp = tempfile::tempdir().unwrap();
    let bash = json!({"params": {"update": {
        "sessionUpdate": "user_message_chunk",
        "content": {"type": "text", "text": "ls -la", "_meta": {"bashCommand": "ls -la"}},
        "_meta": {"promptIndex": 1}
    }}});
    let updates = write_session(
        tmp.path(),
        MAIN_ID,
        summary(MAIN_ID),
        &[
            user("Real prompt", 0),
            agent("Reply", "p0"),
            bash,
            user_plain("injected context"),
            json!({"params": {"update": {"sessionUpdate": "user_message_chunk",
                "content": {"type": "text", "text": ""}, "_meta": {"promptIndex": 2}}}}),
        ],
    );
    let s = adapter(tmp.path()).parse(&updates).unwrap().unwrap();
    assert_eq!(s.content, "» Real prompt\n\n  Reply");
    assert_eq!(s.message_count, 2);
}

#[test]
fn chunks_without_any_prompt_index_merge_into_one_prompt() {
    let tmp = tempfile::tempdir().unwrap();
    let updates = write_session(
        tmp.path(),
        MAIN_ID,
        summary(MAIN_ID),
        &[
            user_plain("Old style "),
            user_plain("prompt"),
            // No promptId at all: chunks still merge under the empty id.
            json!({"params": {"update": {"sessionUpdate": "agent_message_chunk", "content": "Plain "}}}),
            json!({"params": {"update": {"sessionUpdate": "agent_message_chunk",
                "content": [{"type": "text", "text": "string"}, {"type": "image"}, {"type": "text", "text": "parts"}]}}}),
            user_plain("Next prompt"),
        ],
    );
    let s = adapter(tmp.path()).parse(&updates).unwrap().unwrap();
    assert_eq!(
        s.content,
        "» Old style prompt\n\n  Plain string\nparts\n\n» Next prompt"
    );
    assert_eq!(s.message_count, 3);
}

#[test]
fn malformed_and_blank_lines_are_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = session_dir(tmp.path(), WORKSPACE, MAIN_ID);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("summary.json"), summary(MAIN_ID).to_string()).unwrap();
    let updates = dir.join("updates.jsonl");
    // The last line is cut off, as it is while Grok is still writing.
    let body = format!(
        "{}\n\nnot json\n{}\n{{\"params\":{{\"update\":{{\"sessionUpdate\":\"agent_mess",
        user("Prompt", 0),
        agent("Reply", "p0"),
    );
    fs::write(&updates, body).unwrap();

    let s = adapter(tmp.path()).parse(&updates).unwrap().unwrap();
    assert_eq!(s.content, "» Prompt\n\n  Reply");
}

#[test]
fn sessions_without_a_user_prompt_are_not_sessions() {
    let tmp = tempfile::tempdir().unwrap();
    let titled = with(summary("agent-only"), json!({"generated_title": "Titled"}));
    let agent_only = write_session(tmp.path(), "agent-only", titled, &[agent("Hello", "p0")]);
    let blank = write_session(tmp.path(), "blank", summary("blank"), &[user("  \n", 0)]);
    let empty = write_session(tmp.path(), "empty", summary("empty"), &[]);

    let a = adapter(tmp.path());
    for path in [agent_only, blank, empty] {
        assert!(a.parse(&path).unwrap().is_none(), "{}", path.display());
    }
}

#[test]
fn skips_sessions_grok_hides() {
    let tmp = tempfile::tempdir().unwrap();
    for (id, extra) in [
        ("main", json!({})),
        (
            "fork",
            json!({"session_kind": "fork", "parent_session_id": "main"}),
        ),
        ("headless", json!({"session_kind": "headless"})),
        ("worktree", json!({"session_kind": "worktree"})),
        ("subagent", json!({"session_kind": "subagent"})),
        (
            "subagent-fork",
            json!({"session_kind": "subagent_fork", "parent_session_id": "main"}),
        ),
        (
            "subagent-resume",
            json!({"session_kind": "subagent_resume"}),
        ),
        (
            "hidden-fork",
            json!({"session_kind": "fork", "hidden": true}),
        ),
        (
            "shown-subagent",
            json!({"session_kind": "subagent", "hidden": false}),
        ),
    ] {
        write_session(
            tmp.path(),
            id,
            with(json!({"info": {"id": id, "cwd": "/work/app"}}), extra),
            &[user_plain("Prompt")],
        );
    }
    let a = adapter(tmp.path());
    let shown = ["fork", "headless", "main", "shown-subagent", "worktree"];

    assert_eq!(sorted_ids(&a), shown);

    // Asked directly, parse refuses hidden sessions too.
    for id in [
        "subagent",
        "subagent-fork",
        "subagent-resume",
        "hidden-fork",
    ] {
        let path = session_dir(tmp.path(), WORKSPACE, id).join("updates.jsonl");
        assert!(path.is_file());
        assert!(a.parse(&path).unwrap().is_none(), "{id} is hidden");
    }

    // A subagent indexed by an older build must disappear on the next pass.
    let known: KnownSessions = [(SessionKey::new(AgentId::Grok, "subagent"), 1)]
        .into_iter()
        .collect();
    let res = a.find_incremental(&known, &mut |_| {}).unwrap();
    let mut ids: Vec<String> = res.upserts.iter().map(|s| s.id.clone()).collect();
    ids.sort();
    assert_eq!(ids, shown);
    assert_eq!(res.deleted, vec!["subagent".to_string()]);
}

#[test]
fn a_session_hidden_later_drops_out_of_the_index() {
    let (_tmp, a, updates) = main_fixture();
    let known: KnownSessions = [(SessionKey::new(AgentId::Grok, MAIN_ID), 1)]
        .into_iter()
        .collect();
    fs::write(
        updates.with_file_name("summary.json"),
        with(summary(MAIN_ID), json!({"hidden": true})).to_string(),
    )
    .unwrap();

    let res = a.find_incremental(&known, &mut |_| {}).unwrap();
    assert!(res.upserts.is_empty());
    assert_eq!(res.deleted, vec![MAIN_ID.to_string()]);
}

#[test]
fn keeps_indexed_session_while_summary_is_half_written() {
    let tmp = tempfile::tempdir().unwrap();
    let updates = write_session_raw(
        tmp.path(),
        WORKSPACE,
        "main",
        Some(r#"{"info":"#),
        &[user_plain("Prompt")],
    );
    let a = adapter(tmp.path());
    let known: KnownSessions = [(SessionKey::new(AgentId::Grok, "main"), 1)]
        .into_iter()
        .collect();

    assert!(a.parse(&updates).unwrap().is_none());
    let res = a.find_incremental(&known, &mut |_| {}).unwrap();
    assert!(res.upserts.is_empty());
    assert!(res.deleted.is_empty());
}

#[test]
fn a_session_without_a_summary_is_never_indexed() {
    let tmp = tempfile::tempdir().unwrap();
    let updates = write_session_raw(tmp.path(), WORKSPACE, "bare", None, &[user("Prompt", 0)]);
    let a = adapter(tmp.path());

    assert!(a.parse(&updates).unwrap().is_none());
    let res = a
        .find_incremental(&KnownSessions::new(), &mut |_| {})
        .unwrap();
    assert!(res.upserts.is_empty());
    assert!(res.deleted.is_empty());
}

#[test]
fn scan_and_parse_agree_on_the_id_when_it_differs_from_the_directory_name() {
    let tmp = tempfile::tempdir().unwrap();
    write_session(
        tmp.path(),
        "directory-name",
        summary(MAIN_ID),
        &[user("Prompt", 0)],
    );
    // An empty info.id falls back to the directory name.
    write_session(
        tmp.path(),
        "fallback-name",
        json!({"info": {"id": "", "cwd": "/work/app"}}),
        &[user("Prompt", 0)],
    );
    let a = adapter(tmp.path());

    let scan = a.scan().unwrap();
    assert_eq!(sorted_ids(&a), vec![MAIN_ID, "fallback-name"]);
    for (id, (path, _)) in &scan {
        let s = a.parse(path).unwrap().unwrap();
        assert_eq!(&s.id, id);
    }
    let s = a.parse(&scan[MAIN_ID].0).unwrap().unwrap();
    assert_eq!(
        a.resume_command(&s, false),
        vec!["grok", "--resume", MAIN_ID]
    );
}

#[test]
fn the_newest_copy_wins_when_two_workspaces_hold_the_same_id() {
    let tmp = tempfile::tempdir().unwrap();
    let old = write_session_raw(
        tmp.path(),
        "%2Fwork%2Fold",
        MAIN_ID,
        Some(&json!({"info": {"id": MAIN_ID}}).to_string()),
        &[user("Old copy", 0)],
    );
    let new = write_session_raw(
        tmp.path(),
        "%2Fwork%2Fnew",
        MAIN_ID,
        Some(&json!({"info": {"id": MAIN_ID}}).to_string()),
        &[user("New copy", 0)],
    );
    for path in [&old, &new] {
        set_mtime(&path.with_file_name("summary.json"), 1_800_000_000);
    }
    set_mtime(&old, 1_800_000_100);
    set_mtime(&new, 1_800_000_200);

    let a = adapter(tmp.path());
    let scan = a.scan().unwrap();
    assert_eq!(scan.len(), 1);
    assert_eq!(scan[MAIN_ID].0, new);
    let s = a.parse(&scan[MAIN_ID].0).unwrap().unwrap();
    assert_eq!(s.directory, "/work/new");
}

#[test]
fn directory_falls_back_to_the_decoded_workspace_name() {
    let tmp = tempfile::tempdir().unwrap();
    let no_cwd = json!({"info": {"id": "a"}}).to_string();
    let blank_cwd = json!({"info": {"id": "b", "cwd": "  "}}).to_string();
    let rows = [user("Prompt", 0)];
    let encoded = write_session_raw(tmp.path(), "%2Fwork%2Fmy%20app", "a", Some(&no_cwd), &rows);
    let blank = write_session_raw(tmp.path(), "%2Fwork%2Fb", "b", Some(&blank_cwd), &rows);
    let relative = write_session_raw(tmp.path(), "not-a-path", "a", Some(&no_cwd), &rows);

    let a = adapter(tmp.path());
    assert_eq!(
        a.parse(&encoded).unwrap().unwrap().directory,
        "/work/my app"
    );
    assert_eq!(a.parse(&blank).unwrap().unwrap().directory, "/work/b");
    assert_eq!(a.parse(&relative).unwrap().unwrap().directory, "");
}

#[test]
fn title_prefers_generated_title_then_session_summary_then_the_first_prompt() {
    let tmp = tempfile::tempdir().unwrap();
    let rows = [user("  first prompt\nsecond line  ", 0), agent("ok", "p0")];
    let both = with(
        summary("both"),
        json!({"generated_title": "Generated", "session_summary": "Summary"}),
    );
    let only_summary = with(
        summary("only-summary"),
        json!({"generated_title": "  ", "session_summary": "Summary"}),
    );
    let long = with(summary("long"), json!({"generated_title": "x".repeat(200)}));
    let a = adapter(tmp.path());
    let title = |dir: &str, summary: Value| {
        let path = write_session(tmp.path(), dir, summary, &rows);
        a.parse(&path).unwrap().unwrap().title
    };

    assert_eq!(title("both", both), "Generated");
    assert_eq!(title("only-summary", only_summary), "Summary");
    assert_eq!(
        title("neither", summary("neither")),
        "first prompt second line"
    );
    assert_eq!(title("long", long), format!("{}...", "x".repeat(77)));
}

#[test]
fn timestamp_is_the_latest_record_time_in_seconds_or_milliseconds() {
    let tmp = tempfile::tempdir().unwrap();
    let base = secs("2026-07-17T10:00:00Z");
    let updates = write_session(
        tmp.path(),
        MAIN_ID,
        with(
            summary(MAIN_ID),
            json!({"updated_at": "2026-07-18T00:00:00Z"}),
        ),
        &[
            at(user("Prompt", 0), json!(base + 5)),
            // A tool update carries the newest time, in milliseconds.
            at(tool_call(), json!((base + 60) * 1000 + 123)),
            at(agent("Reply", "p0"), json!("2026-07-17T10:00:30.500+00:00")),
            at(agent(" more", "p0"), json!("not a time")),
        ],
    );
    let s = adapter(tmp.path()).parse(&updates).unwrap().unwrap();
    assert_eq!(s.timestamp, base + 60);
}

#[test]
fn timestamp_falls_back_to_the_summary_and_then_to_mtime() {
    let tmp = tempfile::tempdir().unwrap();
    let rows = [user("Prompt", 0)];
    let a = adapter(tmp.path());
    let timestamp = |dir: &str, extra: Value| {
        let path = write_session(
            tmp.path(),
            dir,
            with(json!({"info": {"id": dir, "cwd": "/work/app"}}), extra),
            &rows,
        );
        set_mtime(&path, 1_800_000_000);
        set_mtime(&path.with_file_name("summary.json"), 1_700_000_000);
        a.parse(&path).unwrap().unwrap().timestamp
    };

    assert_eq!(
        timestamp(
            "updated",
            json!({"created_at": "2026-07-17T10:00:00Z", "last_active_at": "2026-07-17T11:00:00Z",
                "updated_at": "2026-07-17T12:00:00Z"})
        ),
        secs("2026-07-17T12:00:00Z")
    );
    assert_eq!(
        timestamp(
            "last-active",
            json!({"created_at": "2026-07-17T10:00:00Z", "last_active_at": "2026-07-17T11:00:00Z",
                "updated_at": "garbage"})
        ),
        secs("2026-07-17T11:00:00Z")
    );
    assert_eq!(
        timestamp("created", json!({"created_at": "2026-07-17T10:00:00Z"})),
        secs("2026-07-17T10:00:00Z")
    );
    assert_eq!(timestamp("mtime", json!({})), 1_800_000_000);
}

#[test]
fn incremental_parses_only_new_and_changed_sessions() {
    let tmp = tempfile::tempdir().unwrap();
    let rows = [user("Prompt", 0), agent("Reply", "p0")];
    for id in ["same", "changed", "new"] {
        write_session(tmp.path(), id, summary(id), &rows);
    }
    let a = adapter(tmp.path());
    let scan = a.scan().unwrap();
    let known: KnownSessions = [
        (SessionKey::new(AgentId::Grok, "same"), scan["same"].1),
        (
            SessionKey::new(AgentId::Grok, "changed"),
            scan["changed"].1 - 1,
        ),
        (SessionKey::new(AgentId::Grok, "gone"), 10),
        (SessionKey::new(AgentId::Codex, "other-agent"), 10),
    ]
    .into_iter()
    .collect();

    let mut streamed = Vec::new();
    let res = a
        .find_incremental(&known, &mut |s| streamed.push(s.id.clone()))
        .unwrap();
    let mut upserted: Vec<&str> = res.upserts.iter().map(|s| s.id.as_str()).collect();
    upserted.sort();
    streamed.sort();

    assert_eq!(upserted, vec!["changed", "new"]);
    assert_eq!(streamed, vec!["changed", "new"]);
    assert_eq!(res.deleted, vec!["gone".to_string()]);
    for s in &res.upserts {
        assert_eq!(s.mtime, scan[&s.id].1, "indexed mtime is the scan mtime");
        assert_eq!(s.agent, AgentId::Grok);
    }
}

#[test]
fn incremental_drops_everything_when_grok_is_not_installed() {
    let tmp = tempfile::tempdir().unwrap();
    let a = adapter(&tmp.path().join("missing"));
    let known: KnownSessions = [
        (SessionKey::new(AgentId::Grok, "a"), 1),
        (SessionKey::new(AgentId::Codex, "b"), 1),
    ]
    .into_iter()
    .collect();
    let res = a.find_incremental(&known, &mut |_| {}).unwrap();
    assert!(res.upserts.is_empty());
    assert_eq!(res.deleted, vec!["a".to_string()]);
}

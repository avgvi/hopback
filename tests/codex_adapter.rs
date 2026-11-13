//! Fixture tests for the Codex rollout format written by current Codex builds:
//! `~/.codex/sessions/YYYY/MM/DD/rollout-<time>-<uuid>.jsonl`, with prompts
//! stored as `response_item` messages and names in `session_index.jsonl`.

use std::fs;
use std::path::{Path, PathBuf};

use hopback::adapters::codex::CodexAdapter;
use hopback::adapters::{Adapter, KnownSessions};
use hopback::config::AgentId;
use hopback::model::SessionKey;
use serde_json::{json, Value};

const MAIN_ID: &str = "01a1cde9-b5f4-7141-b2c9-8a1edb368004";
const FORK_ID: &str = "01a1ce2e-a48b-71e3-9deb-587a0320156c";
const REVIEW_ID: &str = "01a1ce30-0000-7000-8000-000000000001";
const GUARDIAN_ID: &str = "01a1ce31-0000-7000-8000-000000000002";
const SPAWN_ID: &str = "01a1ce32-0000-7000-8000-000000000003";
const EMPTY_ID: &str = "01a1ce33-0000-7000-8000-000000000004";

fn write_rollout(root: &Path, day: &str, time: &str, id: &str, rows: &[Value]) -> PathBuf {
    let dir = root.join("sessions").join(day);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("rollout-{time}-{id}.jsonl"));
    let body: String = rows.iter().map(|r| format!("{r}\n")).collect();
    fs::write(&path, body).unwrap();
    path
}

fn meta(id: &str, source: Value) -> Value {
    json!({
        "timestamp": "2026-11-11T20:00:08.512Z",
        "type": "session_meta",
        "payload": {
            "session_id": id,
            "id": id,
            "timestamp": "2026-11-11T20:00:08.100Z",
            "cwd": "/work/app",
            "originator": "codex-tui",
            "cli_version": "0.0.0",
            "source": source,
            "model_provider": "openai",
            "base_instructions": {"text": "You are a coding agent."},
            "history_mode": "full"
        }
    })
}

fn user(text: &str) -> Value {
    json!({
        "timestamp": "2026-11-11T20:00:09.000Z",
        "type": "response_item",
        "payload": {"type": "message", "role": "user", "content": [{"type": "input_text", "text": text}]}
    })
}

fn assistant(text: &str) -> Value {
    json!({
        "timestamp": "2026-11-11T20:00:12.000Z",
        "type": "response_item",
        "payload": {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": text}]}
    })
}

/// Records a real rollout interleaves with the conversation and that carry no
/// text hopback should index.
fn noise() -> Vec<Value> {
    vec![
        json!({"type": "response_item", "payload": {"type": "message", "role": "developer",
            "content": [{"type": "input_text", "text": "<permissions instructions>sandboxed</permissions instructions>"}]}}),
        json!({"type": "turn_context", "payload": {"cwd": "/work/app", "approval_policy": "on-request",
            "sandbox_policy": {"type": "workspace-write"}, "model": "gpt-x"}}),
        json!({"type": "event_msg", "payload": {"type": "task_started", "turn_id": "t1"}}),
        json!({"type": "event_msg", "payload": {"type": "thread_settings_applied"}}),
        json!({"type": "world_state", "payload": {"cwd": "/work/app"}}),
        json!({"type": "response_item", "payload": {"type": "reasoning", "summary": [], "encrypted_content": "xxxx"}}),
        json!({"type": "response_item", "payload": {"type": "function_call", "name": "shell", "arguments": "{}", "call_id": "c1"}}),
        json!({"type": "response_item", "payload": {"type": "function_call_output", "call_id": "c1", "output": "ok"}}),
        json!({"type": "event_msg", "payload": {"type": "item_completed", "item": {"id": "i1"}}}),
        json!({"type": "event_msg", "payload": {"type": "token_count", "info": {"total": 10}}}),
        json!({"type": "token_usage_record", "payload": {"input_tokens": 5}}),
        json!({"type": "event_msg", "payload": {"type": "task_complete", "turn_id": "t1"}}),
    ]
}

fn fixture() -> (tempfile::TempDir, CodexAdapter) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    let mut main = vec![meta(MAIN_ID, json!("cli"))];
    main.push(user(
        "<environment_context>\n  <cwd>/work/app</cwd>\n</environment_context>",
    ));
    main.push(user("add retry logic to the uploader"));
    main.extend(noise());
    main.push(assistant("Added a retry loop with backoff."));
    main.push(user("now cover it with tests"));
    main.push(assistant("Tests are in place."));
    write_rollout(root, "2026/11/11", "2026-11-11T20-00-08", MAIN_ID, &main);

    // A fork keeps a user source and replays the parent's records, including
    // the parent's own session_meta, after its first line.
    let mut fork_meta = meta(FORK_ID, json!("vscode"));
    fork_meta["payload"]["forked_from_id"] = json!(REVIEW_ID);
    fork_meta["payload"]["cwd"] = json!("/work/fork");
    write_rollout(
        root,
        "2026/11/11",
        "2026-11-11T21-15-25",
        FORK_ID,
        &[
            fork_meta,
            meta(REVIEW_ID, json!({"subagent": "review"})),
            user("continue from the fork"),
            assistant("Continuing."),
        ],
    );

    for (id, source) in [
        (REVIEW_ID, json!({"subagent": "review"})),
        (GUARDIAN_ID, json!({"subagent": {"other": "guardian"}})),
        (
            SPAWN_ID,
            json!({"subagent": {"thread_spawn": {"parent_thread_id": MAIN_ID, "depth": 1}}}),
        ),
    ] {
        write_rollout(
            root,
            "2026/11/10",
            "2026-11-10T09-00-00",
            id,
            &[meta(id, source), user("internal prompt"), assistant("done")],
        );
    }

    // Opened and closed without a prompt: only injected context.
    write_rollout(
        root,
        "2026/11/09",
        "2026-11-09T08-00-00",
        EMPTY_ID,
        &[
            meta(EMPTY_ID, json!("exec")),
            user("<environment_context><cwd>/work/app</cwd></environment_context>"),
        ],
    );

    let index = root.join("session_index.jsonl");
    fs::write(
        &index,
        format!(
            "{}\n{}\n",
            json!({"id": MAIN_ID, "thread_name": "Old name", "updated_at": "2026-11-11T20:05:00.000000Z"}),
            json!({"id": MAIN_ID, "thread_name": "Uploader retries", "updated_at": "2026-11-11T20:30:00.000000Z"}),
        ),
    )
    .unwrap();

    let adapter = CodexAdapter::with_paths(root.join("sessions"), index);
    (tmp, adapter)
}

#[test]
fn scan_lists_user_sessions_and_skips_subagent_rollouts() {
    let (_tmp, adapter) = fixture();
    let mut ids: Vec<String> = adapter.scan().unwrap().into_keys().collect();
    ids.sort();
    assert_eq!(ids, vec![MAIN_ID, FORK_ID, EMPTY_ID]);
}

#[test]
fn parses_prompts_stored_as_response_items() {
    let (_tmp, adapter) = fixture();
    let scan = adapter.scan().unwrap();
    let (path, _) = &scan[MAIN_ID];
    let s = adapter.parse(path).unwrap().expect("session with prompts");

    assert_eq!(s.id, MAIN_ID);
    assert_eq!(s.agent, AgentId::Codex);
    assert_eq!(s.directory, "/work/app");
    assert_eq!(s.title, "Uploader retries", "latest thread name wins");
    assert_eq!(s.message_count, 4, "two prompts and two replies");
    assert!(!s.yolo);
    assert!(s.content.contains("» add retry logic to the uploader"));
    assert!(s.content.contains("  Added a retry loop with backoff."));
    assert!(s.content.contains("» now cover it with tests"));
    assert!(!s.content.contains("environment_context"));
    assert!(!s.content.contains("permissions instructions"));
    assert!(!s.content.contains("encrypted_content"));
    assert_eq!(
        s.timestamp,
        hopback::adapters::parse_iso8601_secs("2026-11-11T20:00:08.100Z").unwrap()
    );
    assert_eq!(s.source_path.as_deref(), path.to_str());
    assert_eq!(
        adapter.resume_command(&s, false),
        vec!["codex", "resume", MAIN_ID]
    );
}

#[test]
fn title_falls_back_to_the_first_prompt_without_a_thread_name() {
    let (_tmp, adapter) = fixture();
    let scan = adapter.scan().unwrap();
    let s = adapter.parse(&scan[FORK_ID].0).unwrap().unwrap();
    assert_eq!(s.title, "continue from the fork");
}

#[test]
fn fork_uses_its_own_meta_not_the_copied_parent_meta() {
    let (_tmp, adapter) = fixture();
    let scan = adapter.scan().unwrap();
    let s = adapter.parse(&scan[FORK_ID].0).unwrap().unwrap();
    assert_eq!(s.id, FORK_ID);
    assert_eq!(s.directory, "/work/fork");
}

#[test]
fn subagent_and_promptless_rollouts_are_not_sessions() {
    let (tmp, adapter) = fixture();
    let day = tmp.path().join("sessions/2026/11/10");
    for id in [REVIEW_ID, GUARDIAN_ID, SPAWN_ID] {
        let path = day.join(format!("rollout-2026-11-10T09-00-00-{id}.jsonl"));
        assert!(path.is_file());
        assert!(
            adapter.parse(&path).unwrap().is_none(),
            "{id} is a subagent"
        );
    }
    let scan = adapter.scan().unwrap();
    assert!(adapter.parse(&scan[EMPTY_ID].0).unwrap().is_none());
}

#[test]
fn incremental_index_finds_two_sessions_and_drops_a_known_subagent() {
    let (_tmp, adapter) = fixture();
    // A subagent indexed by an older build must disappear on the next pass.
    let known: KnownSessions = [(SessionKey::new(AgentId::Codex, REVIEW_ID), 1)]
        .into_iter()
        .collect();
    let res = adapter.find_incremental(&known, &mut |_| {}).unwrap();
    let mut ids: Vec<String> = res.upserts.iter().map(|s| s.id.clone()).collect();
    ids.sort();
    assert_eq!(ids, vec![MAIN_ID, FORK_ID]);
    assert_eq!(res.deleted, vec![REVIEW_ID.to_string()]);
}

#[test]
fn a_rename_in_the_session_index_counts_as_a_change() {
    let (_tmp, adapter) = fixture();
    let scan = adapter.scan().unwrap();
    let renamed_at =
        hopback::adapters::parse_iso8601_secs("2026-11-11T20:30:00Z").unwrap() * 1_000_000_000;
    let file_mtime = hopback::adapters::mtime_nanos(&scan[MAIN_ID].0).unwrap();
    assert_eq!(scan[MAIN_ID].1, file_mtime.max(renamed_at));
}

#[test]
fn older_event_msg_prompts_still_work_and_are_not_double_counted_as_titles() {
    let tmp = tempfile::tempdir().unwrap();
    let path = write_rollout(
        tmp.path(),
        "2026/03/06",
        "2026-03-06T02-23-01",
        "019cc0f4-0000-7000-8000-00000000abcd",
        &[
            json!({"type": "session_meta", "payload": {"id": "019cc0f4-0000-7000-8000-00000000abcd", "cwd": "/w"}}),
            json!({"type": "event_msg", "payload": {"type": "user_message", "message": "the real prompt"}}),
            user("the real prompt"),
            assistant("ok"),
        ],
    );
    let adapter = CodexAdapter::with_paths(
        tmp.path().join("sessions"),
        tmp.path().join("session_index.jsonl"),
    );
    let s = adapter.parse(&path).unwrap().unwrap();
    assert_eq!(s.title, "the real prompt");
    assert_eq!(s.id, "019cc0f4-0000-7000-8000-00000000abcd");
}

//! Titles end to end: what the adapters pick, what the indexer cleans, and
//! that sessions indexed by an older build get re-read.

use std::fs;
use std::path::Path;

use hopback::adapters::claude::ClaudeAdapter;
use hopback::adapters::codex::CodexAdapter;
use hopback::adapters::{mtime_nanos, Adapter};
use hopback::config::AgentId;
use hopback::index;
use hopback::model::{Session, SessionKey};
use hopback::store::Store;
use serde_json::{json, Value};

fn write_lines(path: &Path, rows: &[Value]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let body: String = rows.iter().map(|r| format!("{r}\n")).collect();
    fs::write(path, body).unwrap();
}

fn user(text: &str) -> Value {
    json!({"type": "user", "cwd": "/work/app", "timestamp": "2026-06-01T10:00:00Z",
           "message": {"content": text}})
}

fn assistant(text: &str) -> Value {
    json!({"type": "assistant", "message": {"content": text}})
}

fn claude_fixture(rows: &[Value]) -> (tempfile::TempDir, ClaudeAdapter, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("projects/-work-app/sess-1.jsonl");
    write_lines(&path, rows);
    let adapter = ClaudeAdapter::with_sessions_dir(tmp.path().join("projects"));
    (tmp, adapter, path)
}

fn indexed_title(adapter: Box<dyn Adapter>, id: &str) -> String {
    let agent = adapter.id();
    let mut store = Store::open_in_memory().unwrap();
    index::index_with(&mut store, vec![adapter], false, None).unwrap();
    store
        .get_session(&SessionKey::new(agent, id))
        .unwrap()
        .unwrap_or_else(|| panic!("{id} was not indexed"))
        .title
}

#[test]
fn claude_title_sources_in_order() {
    let base = vec![
        user("please look into the flaky upload test"),
        assistant("Looking."),
    ];

    let (_t, adapter, path) = claude_fixture(&base);
    let s = adapter.parse(&path).unwrap().unwrap();
    assert_eq!(s.title, "please look into the flaky upload test");

    let mut rows = base.clone();
    rows.push(json!({"type": "ai-title", "aiTitle": "Old AI title"}));
    rows.push(json!({"type": "ai-title", "aiTitle": "Flaky upload test"}));
    let (_t, adapter, path) = claude_fixture(&rows);
    assert_eq!(
        adapter.parse(&path).unwrap().unwrap().title,
        "Flaky upload test"
    );

    rows.push(json!({"type": "custom-title", "customTitle": "upload-flake"}));
    rows.push(json!({"type": "custom-title", "customTitle": "  "}));
    rows.push(json!({"type": "custom-title", "customTitle": "Upload flake hunt"}));
    let (tmp, adapter, path) = claude_fixture(&rows);
    assert_eq!(
        adapter.parse(&path).unwrap().unwrap().title,
        "Upload flake hunt"
    );

    // `/rename` writes a sidecar, which wins over everything in the transcript.
    let sidecar = tmp
        .path()
        .join("projects/-work-app/sess-1/custom-title.json");
    fs::create_dir_all(sidecar.parent().unwrap()).unwrap();
    fs::write(
        &sidecar,
        json!({"customTitle": "Renamed by hand"}).to_string(),
    )
    .unwrap();
    assert_eq!(
        adapter.parse(&path).unwrap().unwrap().title,
        "Renamed by hand"
    );

    // Title records are not messages.
    let s = adapter.parse(&path).unwrap().unwrap();
    assert_eq!(s.message_count, 2);
    assert!(!s.content.contains("Upload flake hunt"));
}

#[test]
fn claude_rename_sidecar_counts_as_a_change() {
    let (tmp, adapter, path) =
        claude_fixture(&[user("a prompt that is long enough"), assistant("ok")]);
    let before = adapter.scan().unwrap()["sess-1"].1;
    assert_eq!(before, mtime_nanos(&path).unwrap());

    let sidecar = tmp
        .path()
        .join("projects/-work-app/sess-1/custom-title.json");
    fs::create_dir_all(sidecar.parent().unwrap()).unwrap();
    fs::write(&sidecar, json!({"customTitle": "New name"}).to_string()).unwrap();
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(60);
    fs::File::options()
        .write(true)
        .open(&sidecar)
        .unwrap()
        .set_modified(later)
        .unwrap();

    let after = adapter.scan().unwrap()["sess-1"].1;
    assert!(after > before, "the sidecar's mtime must reach the scan");
    assert_eq!(after, mtime_nanos(&sidecar).unwrap());
}

#[test]
fn claude_pasted_first_prompt_is_cleaned_by_the_indexer() {
    let (_t, adapter, _path) = claude_fixture(&[
        user("<pasted_content id=\"e239\">\n# Task: migrate the store\n\n- step one\n</pasted_content id=\"e239\">"),
        assistant("Starting."),
    ]);
    assert_eq!(
        indexed_title(Box::new(adapter), "sess-1"),
        "Task: migrate the store"
    );
}

#[test]
fn claude_session_opened_with_a_slash_command_is_titled_by_the_first_real_prompt() {
    let (_t, adapter, _path) = claude_fixture(&[
        user("<command-name>/clear</command-name>\n<command-message>clear</command-message>\n<command-args></command-args>"),
        user("<local-command-stdout></local-command-stdout>"),
        user("<system-reminder>\nToday is Tuesday.\n</system-reminder>\n\nadd pagination to the list endpoint"),
        assistant("On it."),
    ]);
    assert_eq!(
        indexed_title(Box::new(adapter), "sess-1"),
        "add pagination to the list endpoint"
    );
}

const CODEX_ID: &str = "11111111-2222-4333-8444-555555555555";

fn codex_fixture(extra_user_parts: Value, with_item: bool) -> (tempfile::TempDir, CodexAdapter) {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join(format!(
        "sessions/2026/11/11/rollout-2026-11-11T20-00-08-{CODEX_ID}.jsonl"
    ));
    let mut rows = vec![
        json!({"type": "session_meta", "payload": {"id": CODEX_ID, "cwd": "/work/app", "source": "cli",
               "timestamp": "2026-11-11T20:00:08.000Z"}}),
        json!({"type": "response_item", "payload": {"type": "message", "role": "user",
               "content": [{"type": "input_text", "text": "<environment_context>\n<cwd>/work/app</cwd>\n</environment_context>"}]}}),
        json!({"type": "response_item", "payload": {"type": "message", "role": "user", "content": extra_user_parts}}),
    ];
    if with_item {
        rows.push(
            json!({"type": "event_msg", "payload": {"type": "item_completed", "turn_id": "t1",
            "item": {"type": "UserMessage", "id": "i1", "content": [
                {"type": "text", "text": "what is wrong in this screenshot", "text_elements": []},
                {"type": "local_image", "path": "/tmp/shot.png"}
            ]}}}),
        );
    }
    rows.push(
        json!({"type": "response_item", "payload": {"type": "message", "role": "assistant",
        "content": [{"type": "output_text", "text": "The header overlaps."}]}}),
    );
    rows.push(json!({"type": "event_msg", "payload": {"type": "item_completed", "turn_id": "t1",
        "item": {"type": "AgentMessage", "id": "i2", "content": [{"type": "Text", "text": "The header overlaps."}]}}}));
    write_lines(&path, &rows);
    let adapter = CodexAdapter::with_paths(
        tmp.path().join("sessions"),
        tmp.path().join("session_index.jsonl"),
    );
    (tmp, adapter)
}

fn image_prompt_parts() -> Value {
    json!([
        {"type": "input_text", "text": "<image name=[Image #1]>"},
        {"type": "input_image", "image_url": "data:image/png;base64,AAAA"},
        {"type": "input_text", "text": "</image>"},
        {"type": "input_text", "text": "[Image #1] what is wrong in this screenshot"}
    ])
}

#[test]
fn codex_image_prompt_is_titled_by_the_user_message_item() {
    let (_t, adapter) = codex_fixture(image_prompt_parts(), true);
    let scan = adapter.scan().unwrap();
    let s = adapter.parse(&scan[CODEX_ID].0).unwrap().unwrap();
    assert_eq!(s.title, "what is wrong in this screenshot");
    assert!(!s.content.contains("<image"), "image labels are not text");
    assert!(!s.content.contains("</image>"));
    assert!(!s.content.contains("environment_context"));
    assert_eq!(s.message_count, 2, "one prompt, one reply");
}

#[test]
fn codex_without_items_falls_back_to_the_first_readable_part() {
    let (_t, adapter) = codex_fixture(image_prompt_parts(), false);
    assert_eq!(
        indexed_title(Box::new(adapter), CODEX_ID),
        "what is wrong in this screenshot"
    );
}

#[test]
fn codex_prompt_wrapped_by_another_program_gets_a_readable_title() {
    let parts = json!([{"type": "input_text",
        "text": "<previous_cards>\n- stand-up notes\n</previous_cards>\n<observations>\nmostly email\n</observations>"}]);
    let (_t, adapter) = codex_fixture(parts, false);
    // The last block wraps the rest of the prompt, so its content is used.
    assert_eq!(indexed_title(Box::new(adapter), CODEX_ID), "mostly email");
}

#[test]
fn a_new_index_format_rereads_sessions_and_keeps_pins() {
    let (_t, adapter, path) = claude_fixture(&[
        user("<pasted_content id=1>\nport the parser to the new api\n</pasted_content>"),
        assistant("ok"),
    ]);
    let key = SessionKey::new(AgentId::Claude, "sess-1");

    // What an older build left behind: the raw title, at the file's mtime, so
    // an ordinary incremental pass would skip it.
    let mut store = Store::open_in_memory().unwrap();
    store
        .upsert_sessions(&[Session {
            id: "sess-1".into(),
            agent: AgentId::Claude,
            title: "<pasted_content id=1>".into(),
            directory: "/work/app".into(),
            timestamp: 1,
            content: String::new(),
            message_count: 2,
            mtime: mtime_nanos(&path).unwrap(),
            yolo: false,
            source_path: None,
        }])
        .unwrap();
    store.set_pinned(&key, true).unwrap();

    let stats = index::index_with(&mut store, vec![Box::new(adapter)], false, None).unwrap();
    assert_eq!(stats.upserted, 1, "the format change forces a re-read");
    assert_eq!(
        store.get_session(&key).unwrap().unwrap().title,
        "port the parser to the new api"
    );
    assert!(store.is_pinned(&key).unwrap(), "pins survive the rebuild");
    assert_eq!(
        store.get_setting("index_format").unwrap().as_deref(),
        Some(index::INDEX_FORMAT)
    );

    // The next pass is incremental again.
    let (_t2, adapter2, _p) =
        claude_fixture(&[user("another long enough prompt"), assistant("ok")]);
    let stats = index::index_with(&mut store, vec![Box::new(adapter2)], false, None).unwrap();
    assert_eq!(stats.upserted, 1);
}

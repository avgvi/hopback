//! Fixture tests for Kimi Code's session layout:
//! `<home>/sessions/wd_<slug>_<hash>/session_<uuid>/state.json` with the main
//! agent's transcript in `agents/main/wire.jsonl`, plus `session_index.jsonl`
//! next to `sessions/`.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use hopback::adapters::kimi::KimiAdapter;
use hopback::adapters::{mtime_nanos, Adapter, KnownSessions};
use hopback::config::AgentId;
use hopback::model::SessionKey;
use serde_json::{json, Value};

const WORKSPACE: &str = "wd_app_0123456789ab";
const MAIN_ID: &str = "session_11111111-2222-4333-8444-555555555555";
const EMPTY_ID: &str = "session_22222222-2222-4333-8444-555555555555";
const OTHER_ID: &str = "session_33333333-2222-4333-8444-555555555555";

const NANOS: i64 = 1_000_000_000;

struct Fixture {
    tmp: tempfile::TempDir,
    adapter: KimiAdapter,
}

impl Fixture {
    fn new() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("sessions")).unwrap();
        let adapter = KimiAdapter::with_root(tmp.path().to_path_buf());
        Fixture { tmp, adapter }
    }

    fn root(&self) -> &Path {
        self.tmp.path()
    }

    fn session_dir(&self, id: &str) -> PathBuf {
        self.root().join("sessions").join(WORKSPACE).join(id)
    }

    /// Writes `state.json` and, when `wire` is given, `agents/main/wire.jsonl`.
    fn session(&self, id: &str, state: Value, wire: Option<&[Value]>) -> PathBuf {
        let dir = self.session_dir(id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("state.json"), state.to_string()).unwrap();
        if let Some(rows) = wire {
            write_wire(&dir, rows);
        }
        dir
    }

    fn index(&self, rows: &[Value]) {
        let body: String = rows.iter().map(|r| format!("{r}\n")).collect();
        fs::write(self.root().join("session_index.jsonl"), body).unwrap();
    }

    fn parse(&self, id: &str) -> Option<hopback::model::Session> {
        let scan = self.adapter.scan().unwrap();
        let (path, _) = scan.get(id).unwrap_or_else(|| panic!("{id} not in scan"));
        self.adapter.parse(path).unwrap()
    }
}

fn write_wire(session_dir: &Path, rows: &[Value]) {
    let dir = session_dir.join("agents").join("main");
    fs::create_dir_all(&dir).unwrap();
    let body: String = rows.iter().map(|r| format!("{r}\n")).collect();
    fs::write(dir.join("wire.jsonl"), body).unwrap();
}

fn set_mtime(path: &Path, secs: u64) {
    let file = fs::File::options().write(true).open(path).unwrap();
    file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
        .unwrap();
}

fn state(id: &str) -> Value {
    json!({
        "id": id,
        "version": 2,
        "cwd": "/work/app",
        "createdAt": 1784110800000i64,
        "updatedAt": 1784110807000i64,
        "archived": false,
        "isCustomTitle": false,
        "title": "Add a health check",
        "titleKind": "replaceable",
        "lastPrompt": "Add a health check endpoint",
        "lastTurnReason": "completed",
        "custom": {},
        "agents": {"main": {"type": "main", "homedir": "/work/app"}}
    })
}

/// State of a session that was opened and never prompted.
fn bare_state(id: &str) -> Value {
    json!({
        "id": id,
        "version": 2,
        "cwd": "/work/app",
        "createdAt": 1784110800000i64,
        "updatedAt": 1784110800000i64,
        "archived": false,
        "isCustomTitle": false,
        "custom": {},
        "agents": {"main": {"type": "main", "homedir": "/work/app"}}
    })
}

fn metadata() -> Value {
    json!({"type": "metadata", "protocol_version": "1.4", "created_at": 1784110800000i64})
}

fn message(role: &str, content: Value, origin: Value) -> Value {
    json!({
        "type": "context.append_message",
        "agentId": "main",
        "time": 1784110801000i64,
        "message": {"role": role, "content": content, "origin": origin, "toolCalls": []}
    })
}

fn user(text: &str) -> Value {
    message(
        "user",
        json!([{"type": "text", "text": text}]),
        json!({"kind": "user"}),
    )
}

fn injection(text: &str) -> Value {
    message(
        "user",
        json!([{"type": "text", "text": text}]),
        json!({"kind": "injection", "variant": "system_reminder"}),
    )
}

fn task_notification(text: &str) -> Value {
    message(
        "user",
        json!([{"type": "text", "text": text}]),
        json!({"kind": "task", "taskId": "bash-1", "status": "completed"}),
    )
}

fn event(event: Value) -> Value {
    json!({"type": "context.append_loop_event", "agentId": "main", "time": 1784110802000i64, "event": event})
}

fn step_begin(step: &str) -> Value {
    event(json!({"type": "step.begin", "uuid": step, "turnId": "turn-1", "step": 0}))
}

fn step_end(step: &str) -> Value {
    event(
        json!({"type": "step.end", "uuid": step, "turnId": "turn-1", "step": 0, "finishReason": "stop"}),
    )
}

fn text_part(step: &str, text: &str) -> Value {
    event(
        json!({"type": "content.part", "uuid": "p", "stepUuid": step, "turnId": "turn-1",
        "step": 0, "part": {"type": "text", "text": text}}),
    )
}

fn think_part(step: &str, text: &str) -> Value {
    event(
        json!({"type": "content.part", "uuid": "p", "stepUuid": step, "turnId": "turn-1",
        "step": 0, "part": {"type": "think", "think": text}}),
    )
}

fn tool_call(step: &str, call: &str) -> Value {
    event(
        json!({"type": "tool.call", "uuid": "c", "stepUuid": step, "turnId": "turn-1",
        "step": 0, "toolCallId": call, "name": "Read", "args": {"path": "tool-argument"}}),
    )
}

fn tool_result(call: &str) -> Value {
    event(
        json!({"type": "tool.result", "toolCallId": call, "parentUuid": "c",
        "result": {"output": "tool-output"}}),
    )
}

/// One prompt and one streamed reply, with the records a real wire file puts
/// around them.
fn main_wire() -> Vec<Value> {
    vec![
        metadata(),
        json!({"type": "config.update", "time": 1784110800100i64, "agentId": "main"}),
        injection("hidden reminder"),
        json!({"type": "turn.prompt", "time": 1784110800900i64, "agentId": "main",
            "input": "turn-prompt-copy", "origin": {"kind": "user"}}),
        user("Add a health check endpoint"),
        step_begin("step-1"),
        think_part("step-1", "private reasoning"),
        text_part("step-1", "Added "),
        text_part("step-1", "the endpoint."),
        tool_call("step-1", "call-1"),
        tool_result("call-1"),
        step_end("step-1"),
        json!({"type": "usage.record", "time": 1784110806500i64, "agentId": "main"}),
        json!({"type": "turn.ended", "time": 1784110807000i64, "agentId": "main",
            "turnId": "turn-1", "reason": "completed", "durationMs": 6000}),
    ]
}

fn main_fixture() -> Fixture {
    let f = Fixture::new();
    f.session(MAIN_ID, state(MAIN_ID), Some(&main_wire()));
    f
}

#[test]
fn scan_keys_sessions_by_directory_name() {
    let f = main_fixture();
    f.session(EMPTY_ID, bare_state(EMPTY_ID), Some(&[metadata()]));
    // A subagent's wire and a stray file are not sessions of their own.
    let agent_dir = f.session_dir(MAIN_ID).join("agents").join("agent-0");
    fs::create_dir_all(&agent_dir).unwrap();
    fs::write(agent_dir.join("wire.jsonl"), format!("{}\n", metadata())).unwrap();
    fs::write(f.root().join("sessions").join("notes.json"), "{}").unwrap();

    let scan = f.adapter.scan().unwrap();
    let mut ids: Vec<&str> = scan.keys().map(String::as_str).collect();
    ids.sort();
    assert_eq!(ids, vec![MAIN_ID, EMPTY_ID]);
    assert_eq!(scan[MAIN_ID].0, f.session_dir(MAIN_ID).join("state.json"));
    assert!(f.adapter.is_available());
}

#[test]
fn scan_mtime_is_the_newest_of_state_and_wire() {
    let f = main_fixture();
    let dir = f.session_dir(MAIN_ID);
    let state_file = dir.join("state.json");
    let wire_file = dir.join("agents/main/wire.jsonl");

    set_mtime(&state_file, 1_000);
    set_mtime(&wire_file, 2_000);
    assert_eq!(f.adapter.scan().unwrap()[MAIN_ID].1, 2_000 * NANOS);
    assert_eq!(f.parse(MAIN_ID).unwrap().mtime, 2_000 * NANOS);

    set_mtime(&state_file, 3_000);
    assert_eq!(f.adapter.scan().unwrap()[MAIN_ID].1, 3_000 * NANOS);

    // No wire file yet: the state file alone dates the session.
    f.session(OTHER_ID, state(OTHER_ID), None);
    let other_state = f.session_dir(OTHER_ID).join("state.json");
    set_mtime(&other_state, 4_000);
    assert_eq!(f.adapter.scan().unwrap()[OTHER_ID].1, 4_000 * NANOS);
}

#[test]
fn parses_state_and_wire_into_a_session() {
    let f = main_fixture();
    let scan = f.adapter.scan().unwrap();
    let (path, mtime) = &scan[MAIN_ID];
    let s = f
        .adapter
        .parse(path)
        .unwrap()
        .expect("session with a prompt");

    assert_eq!(s.id, MAIN_ID);
    assert_eq!(s.agent, AgentId::Kimi);
    assert_eq!(s.title, "Add a health check");
    assert_eq!(s.directory, "/work/app");
    assert_eq!(s.timestamp, 1_784_110_807);
    assert_eq!(s.message_count, 2, "one prompt and one reply");
    assert_eq!(
        s.content, "» Add a health check endpoint\n\n  Added the endpoint.",
        "no reminder, reasoning or tool data"
    );
    assert_eq!(s.mtime, *mtime);
    assert!(!s.yolo);
    assert_eq!(s.source_path.as_deref(), path.to_str());
}

#[test]
fn resume_command_passes_the_full_session_id() {
    let f = main_fixture();
    let s = f.parse(MAIN_ID).unwrap();
    assert_eq!(
        f.adapter.resume_command(&s, false),
        vec!["kimi", "--session", MAIN_ID]
    );
    assert_eq!(
        f.adapter.resume_command(&s, true),
        vec!["kimi", "--yolo", "--session", MAIN_ID]
    );
    assert!(f.adapter.supports_yolo());
    assert!(!f.adapter.supports_fork());
    assert!(f.adapter.fork_command(&s, None, None, false).is_none());
    assert!(f.adapter.mtime_is_per_session());
    assert_eq!(f.adapter.id(), AgentId::Kimi);
}

#[test]
fn parsed_id_is_the_scan_key_even_when_state_names_another_id() {
    let f = Fixture::new();
    let mut stale = state("session_stale-id-from-a-copied-state");
    stale["title"] = json!("Copied session");
    f.session(MAIN_ID, stale, Some(&main_wire()));

    let scan = f.adapter.scan().unwrap();
    assert_eq!(scan.len(), 1);
    for (id, (path, _)) in &scan {
        assert_eq!(&f.adapter.parse(path).unwrap().unwrap().id, id);
    }
}

#[test]
fn sessions_without_a_prompt_are_not_sessions() {
    let f = Fixture::new();
    // Opened and closed: state without title or lastPrompt, wire with setup only.
    f.session(
        EMPTY_ID,
        bare_state(EMPTY_ID),
        Some(&[metadata(), injection("hidden reminder")]),
    );
    // Only a background notification, and a title that names no prompt.
    let mut titled = bare_state(OTHER_ID);
    titled["title"] = json!("Internal events only");
    f.session(
        OTHER_ID,
        titled,
        Some(&[metadata(), task_notification("Task finished")]),
    );
    // No wire file at all.
    f.session(MAIN_ID, bare_state(MAIN_ID), None);

    for id in [EMPTY_ID, OTHER_ID, MAIN_ID] {
        assert!(f.parse(id).is_none(), "{id} has no user turn");
    }
    let res = f
        .adapter
        .find_incremental(&KnownSessions::new(), &mut |_| {})
        .unwrap();
    assert!(res.upserts.is_empty());
    assert!(res.deleted.is_empty());
}

#[test]
fn last_prompt_recovers_a_session_whose_prompt_is_not_in_the_wire() {
    let f = Fixture::new();
    let mut no_wire = bare_state(MAIN_ID);
    no_wire["lastPrompt"] = json!("Resume the migration");
    f.session(MAIN_ID, no_wire, None);

    let mut notified = bare_state(OTHER_ID);
    notified["lastPrompt"] = json!("Recovered prompt");
    f.session(
        OTHER_ID,
        notified,
        Some(&[metadata(), task_notification("Task finished")]),
    );

    let s = f.parse(MAIN_ID).unwrap();
    assert_eq!(s.title, "Resume the migration");
    assert_eq!(s.content, "» Resume the migration");
    assert_eq!(s.message_count, 1);

    let s = f.parse(OTHER_ID).unwrap();
    assert_eq!(s.title, "Recovered prompt");
    assert_eq!(s.content, "» Task finished\n\n» Recovered prompt");
    assert_eq!(s.message_count, 2);
}

#[test]
fn title_prefers_state_title_then_last_prompt_then_first_prompt() {
    let f = Fixture::new();
    let wire = [
        metadata(),
        injection("hidden reminder"),
        user("first prompt in the wire"),
        user("second prompt in the wire"),
    ];

    let mut with_last = bare_state(MAIN_ID);
    with_last["lastPrompt"] = json!("second prompt in the wire");
    f.session(MAIN_ID, with_last, Some(&wire));
    assert_eq!(f.parse(MAIN_ID).unwrap().title, "second prompt in the wire");

    f.session(OTHER_ID, bare_state(OTHER_ID), Some(&wire));
    assert_eq!(f.parse(OTHER_ID).unwrap().title, "first prompt in the wire");

    // Blank titles do not count, and long ones are cut to 80 characters.
    let long = format!("fix   the\nparser {}", "x".repeat(100));
    let mut blank_title = bare_state(EMPTY_ID);
    blank_title["title"] = json!("   ");
    blank_title["lastPrompt"] = json!(long);
    f.session(EMPTY_ID, blank_title, Some(&wire));
    let title = f.parse(EMPTY_ID).unwrap().title;
    assert_eq!(title.chars().count(), 80);
    assert!(title.starts_with("fix the parser xxx"));
    assert!(title.ends_with("..."));
}

#[test]
fn legacy_custom_title_wins_only_without_the_modern_title_flag() {
    let f = Fixture::new();
    let wire = [metadata(), user("a prompt")];

    let legacy = json!({"workDir": "/work/legacy", "title": "Generated title",
        "customTitle": "Pinned title", "createdAt": "2026-07-15T10:00:00Z"});
    f.session(MAIN_ID, legacy, Some(&wire));
    let s = f.parse(MAIN_ID).unwrap();
    assert_eq!(s.title, "Pinned title");
    assert_eq!(s.directory, "/work/legacy");
    assert_eq!(
        s.timestamp,
        hopback::adapters::parse_iso8601_secs("2026-07-15T10:00:00Z").unwrap()
    );

    let mut modern = state(OTHER_ID);
    modern["customTitle"] = json!("Stale pinned title");
    f.session(OTHER_ID, modern, Some(&wire));
    assert_eq!(f.parse(OTHER_ID).unwrap().title, "Add a health check");
}

#[test]
fn timestamp_falls_back_to_created_at_then_file_time() {
    let f = Fixture::new();
    let wire = [metadata(), user("a prompt")];

    let mut created_only = state(MAIN_ID);
    created_only.as_object_mut().unwrap().remove("updatedAt");
    f.session(MAIN_ID, created_only, Some(&wire));
    assert_eq!(f.parse(MAIN_ID).unwrap().timestamp, 1_784_110_800);

    let mut undated = state(OTHER_ID);
    undated.as_object_mut().unwrap().remove("updatedAt");
    undated.as_object_mut().unwrap().remove("createdAt");
    let dir = f.session(OTHER_ID, undated, Some(&wire));
    set_mtime(&dir.join("state.json"), 5_000);
    set_mtime(&dir.join("agents/main/wire.jsonl"), 6_000);
    assert_eq!(f.parse(OTHER_ID).unwrap().timestamp, 5_000);
}

#[test]
fn undo_clear_and_compaction_are_replayed() {
    let f = Fixture::new();
    let assistant = |text: &str| message("assistant", json!(text), json!({}));
    f.session(
        MAIN_ID,
        state(MAIN_ID),
        Some(&[
            metadata(),
            user("Keep prompt"),
            assistant("Keep response"),
            json!({"type": "context.apply_compaction", "summary": "Compacted summary", "compactedCount": 2}),
            user("Discard before clear"),
            assistant("Discarded response"),
            // Removes the prompt and its reply, but not the summary.
            json!({"type": "context.undo", "count": 1}),
            json!({"type": "context.clear"}),
            user("Discard after clear"),
            // Cannot reach past the clear.
            json!({"type": "context.undo", "count": 2}),
        ]),
    );

    let s = f.parse(MAIN_ID).unwrap();
    assert_eq!(
        s.content,
        "» Keep prompt\n\n  Keep response\n\n» Compacted summary"
    );
    assert_eq!(s.message_count, 3);
}

#[test]
fn a_fully_undone_prompt_is_not_restored_from_last_prompt() {
    let f = Fixture::new();
    let mut undone = bare_state(MAIN_ID);
    undone["lastPrompt"] = json!("Undone prompt");
    f.session(
        MAIN_ID,
        undone,
        Some(&[
            metadata(),
            user("Undone prompt"),
            json!({"type": "context.undo", "count": 1}),
        ]),
    );
    assert!(f.parse(MAIN_ID).is_none());
}

#[test]
fn slash_commands_and_shell_input_show_what_the_user_typed() {
    let f = Fixture::new();
    let text = |t: &str| json!([{"type": "text", "text": t}]);
    f.session(
        MAIN_ID,
        bare_state(MAIN_ID),
        Some(&[
            metadata(),
            message(
                "user",
                text("EXPANDED_SKILL_PROMPT"),
                json!({"kind": "skill_activation", "skillName": "review", "skillArgs": "  src  ", "trigger": "user-slash"}),
            ),
            message(
                "user",
                text("EXPANDED_PLUGIN_PROMPT"),
                json!({"kind": "plugin_command", "pluginId": "github", "commandName": "issue", "commandArgs": " 42 ", "trigger": "user-slash"}),
            ),
            message(
                "user",
                text("EXPANDED_MODEL_SKILL"),
                json!({"kind": "skill_activation", "skillName": "lint", "trigger": "model-tool"}),
            ),
            message(
                "user",
                text("<bash-input>\nprintf '&lt;ok&gt; &amp; done'\n</bash-input>"),
                json!({"kind": "shell_command", "phase": "input"}),
            ),
            message(
                "user",
                text("captured shell output"),
                json!({"kind": "shell_command", "phase": "output"}),
            ),
        ]),
    );

    let s = f.parse(MAIN_ID).unwrap();
    assert_eq!(
        s.content,
        "» /review src\n\n» /github:issue 42\n\n» Activated skill: lint\n\n» printf '<ok> & done'"
    );
    assert_eq!(s.title, "/review src");
    assert_eq!(s.message_count, 4);
}

#[test]
fn a_model_activated_skill_alone_is_not_a_user_turn() {
    let f = Fixture::new();
    f.session(
        MAIN_ID,
        bare_state(MAIN_ID),
        Some(&[
            metadata(),
            message(
                "user",
                json!([{"type": "text", "text": "EXPANDED_MODEL_SKILL"}]),
                json!({"kind": "skill_activation", "skillName": "lint", "trigger": "model-tool"}),
            ),
        ]),
    );
    assert!(f.parse(MAIN_ID).is_none());
}

#[test]
fn streamed_reply_stays_whole_around_an_interleaved_notification() {
    let f = Fixture::new();
    f.session(
        MAIN_ID,
        state(MAIN_ID),
        Some(&[
            metadata(),
            user("Run the background task"),
            step_begin("step-1"),
            text_part("step-1", "Before "),
            tool_call("step-1", "call-1"),
            task_notification("Task finished"),
            text_part("step-1", "and after."),
            tool_result("call-1"),
            step_end("step-1"),
            // A part for a step that is not open is dropped.
            text_part("step-1", "late text"),
            step_begin("step-2"),
            text_part("step-2", "Second step."),
            step_end("step-2"),
        ]),
    );

    let s = f.parse(MAIN_ID).unwrap();
    assert_eq!(
        s.content,
        "» Run the background task\n\n  Before and after.\n\n» Task finished\n\n  Second step."
    );
    assert_eq!(s.message_count, 4);
}

#[test]
fn image_parts_and_string_content_are_handled() {
    let f = Fixture::new();
    f.session(
        MAIN_ID,
        bare_state(MAIN_ID),
        Some(&[
            metadata(),
            message(
                "user",
                json!([{"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA"}},
                       {"type": "text", "text": "what is in this picture"}]),
                json!({"kind": "user"}),
            ),
            message("assistant", json!("A diagram."), json!({})),
            message("system", json!("system text"), json!({})),
            // No origin at all still counts as the user typing.
            json!({"type": "context.append_message", "message": {"role": "user", "content": "plain string prompt"}}),
        ]),
    );

    let s = f.parse(MAIN_ID).unwrap();
    assert_eq!(
        s.content,
        "» what is in this picture\n\n  A diagram.\n\n» plain string prompt"
    );
    assert_eq!(s.title, "what is in this picture");
    assert_eq!(s.message_count, 3);
}

#[test]
fn malformed_wire_lines_are_skipped() {
    let f = Fixture::new();
    let dir = f.session(MAIN_ID, state(MAIN_ID), None);
    let wire_dir = dir.join("agents/main");
    fs::create_dir_all(&wire_dir).unwrap();
    let body = format!(
        "{}\nnot json at all\n\n{}\n{}\n{}\n{{\"type\":\"context.append_message\",\"mess",
        metadata(),
        user("a prompt before the damage"),
        step_begin("step-1"),
        text_part("step-1", "A reply."),
    );
    fs::write(wire_dir.join("wire.jsonl"), body).unwrap();

    let s = f.parse(MAIN_ID).unwrap();
    assert_eq!(s.content, "» a prompt before the damage\n\n  A reply.");
}

#[test]
fn legacy_session_meta_layout_is_read_and_the_current_layout_wins() {
    let f = Fixture::new();
    let wire = [metadata(), user("Nested state prompt")];

    // Legacy only: <session dir>/session-meta/state.json.
    let legacy_dir = f.session_dir(MAIN_ID);
    fs::create_dir_all(legacy_dir.join("session-meta")).unwrap();
    let mut legacy_state = state(MAIN_ID);
    legacy_state["title"] = json!("Legacy metadata");
    legacy_state["cwd"] = json!("/work/legacy");
    fs::write(
        legacy_dir.join("session-meta/state.json"),
        legacy_state.to_string(),
    )
    .unwrap();
    write_wire(&legacy_dir, &wire);

    // Both layouts for one id.
    let both_dir = f.session(OTHER_ID, state(OTHER_ID), Some(&wire));
    fs::create_dir_all(both_dir.join("session-meta")).unwrap();
    fs::write(
        both_dir.join("session-meta/state.json"),
        legacy_state.to_string(),
    )
    .unwrap();

    let scan = f.adapter.scan().unwrap();
    let mut ids: Vec<&str> = scan.keys().map(String::as_str).collect();
    ids.sort();
    assert_eq!(ids, vec![MAIN_ID, OTHER_ID]);

    let s = f.parse(MAIN_ID).unwrap();
    assert_eq!(s.id, MAIN_ID);
    assert_eq!(s.title, "Legacy metadata");
    assert_eq!(s.directory, "/work/legacy");
    assert_eq!(s.content, "» Nested state prompt");

    assert_eq!(scan[OTHER_ID].0, both_dir.join("state.json"));
    assert_eq!(f.parse(OTHER_ID).unwrap().title, "Add a health check");
}

#[test]
fn directory_prefers_state_and_falls_back_to_the_session_index() {
    let f = Fixture::new();
    let wire = [metadata(), user("a prompt")];
    let without_cwd = |id: &str| {
        let mut s = state(id);
        s.as_object_mut().unwrap().remove("cwd");
        s
    };

    let main_dir = f.session(MAIN_ID, state(MAIN_ID), Some(&wire));
    let other_dir = f.session(OTHER_ID, without_cwd(OTHER_ID), Some(&wire));
    let empty_dir = f.session(EMPTY_ID, without_cwd(EMPTY_ID), Some(&wire));
    let outside = f.root().join("elsewhere").join(OTHER_ID);

    f.index(&[
        json!({"sessionId": MAIN_ID, "sessionDir": main_dir, "workDir": "/work/stale-index"}),
        json!({"sessionId": OTHER_ID, "sessionDir": other_dir, "workDir": "/work/old"}),
        json!({"sessionId": OTHER_ID, "sessionDir": other_dir, "workDir": "/work/from-index"}),
        // Not this session's directory: ignored, the earlier entry stands.
        json!({"sessionId": OTHER_ID, "sessionDir": outside, "workDir": "/work/outside"}),
        json!({"sessionId": EMPTY_ID, "sessionDir": empty_dir, "workDir": "/work/removed"}),
        json!({"sessionId": EMPTY_ID, "deleted": true}),
    ]);
    // A damaged line must not hide the rest.
    let index_file = f.root().join("session_index.jsonl");
    let body = fs::read_to_string(&index_file).unwrap();
    fs::write(&index_file, format!("{{\n{body}")).unwrap();

    assert_eq!(f.parse(MAIN_ID).unwrap().directory, "/work/app");
    assert_eq!(f.parse(OTHER_ID).unwrap().directory, "/work/from-index");
    assert_eq!(f.parse(EMPTY_ID).unwrap().directory, "");
}

#[test]
fn incremental_reparses_on_a_new_wire_record_and_reports_deletions() {
    let f = main_fixture();
    f.session(EMPTY_ID, bare_state(EMPTY_ID), Some(&[metadata()]));
    let dir = f.session_dir(MAIN_ID);
    let wire_file = dir.join("agents/main/wire.jsonl");
    set_mtime(&dir.join("state.json"), 1_000);
    set_mtime(&wire_file, 1_000);

    let mut streamed = Vec::new();
    let first = f
        .adapter
        .find_incremental(&KnownSessions::new(), &mut |s| streamed.push(s.id.clone()))
        .unwrap();
    assert_eq!(first.upserts.len(), 1, "the promptless session is left out");
    assert_eq!(first.upserts[0].id, MAIN_ID);
    assert_eq!(first.upserts[0].mtime, 1_000 * NANOS);
    assert_eq!(streamed, vec![MAIN_ID]);
    assert!(first.deleted.is_empty());

    let mut known: KnownSessions = first.upserts.iter().map(|s| (s.key(), s.mtime)).collect();
    known.insert(SessionKey::new(AgentId::Claude, "not-ours"), 1);

    let unchanged = f.adapter.find_incremental(&known, &mut |_| {}).unwrap();
    assert!(unchanged.upserts.is_empty());
    assert!(unchanged.deleted.is_empty());

    // Kimi appends to the wire file while a turn runs; state.json is untouched.
    let mut rows = main_wire();
    rows.push(user("now add a readiness probe"));
    write_wire(&dir, &rows);
    set_mtime(&wire_file, 2_000);
    let changed = f.adapter.find_incremental(&known, &mut |_| {}).unwrap();
    assert_eq!(changed.upserts.len(), 1);
    assert_eq!(changed.upserts[0].mtime, 2_000 * NANOS);
    assert_eq!(changed.upserts[0].message_count, 3);
    assert!(changed.upserts[0]
        .content
        .ends_with("» now add a readiness probe"));
    assert!(changed.deleted.is_empty());

    fs::remove_dir_all(&dir).unwrap();
    let gone = f.adapter.find_incremental(&known, &mut |_| {}).unwrap();
    assert!(gone.upserts.is_empty());
    assert_eq!(gone.deleted, vec![MAIN_ID.to_string()]);
}

#[test]
fn a_half_written_state_file_keeps_the_indexed_session() {
    let f = main_fixture();
    let state_file = f.session_dir(MAIN_ID).join("state.json");
    let known: KnownSessions = [(SessionKey::new(AgentId::Kimi, MAIN_ID), 1)]
        .into_iter()
        .collect();

    fs::write(&state_file, "{\"id\":\"session_1111").unwrap();
    assert!(f.adapter.scan().unwrap().contains_key(MAIN_ID));
    assert!(f.adapter.parse(&state_file).unwrap().is_none());
    let res = f.adapter.find_incremental(&known, &mut |_| {}).unwrap();
    assert!(res.upserts.is_empty());
    assert!(res.deleted.is_empty(), "nothing indexed is thrown away");

    // Once the write completes the session is picked up again.
    fs::write(&state_file, state(MAIN_ID).to_string()).unwrap();
    let res = f.adapter.find_incremental(&known, &mut |_| {}).unwrap();
    assert_eq!(res.upserts.len(), 1);
    assert_eq!(res.upserts[0].mtime, mtime_nanos(&state_file).unwrap());
}

#[test]
fn a_missing_home_is_unavailable_and_drops_known_sessions() {
    let tmp = tempfile::tempdir().unwrap();
    let adapter = KimiAdapter::with_root(tmp.path().join("no-such-home"));
    assert!(!adapter.is_available());
    assert!(adapter.scan().unwrap().is_empty());

    let known: KnownSessions = [
        (SessionKey::new(AgentId::Kimi, MAIN_ID), 1),
        (SessionKey::new(AgentId::Codex, "not-ours"), 1),
    ]
    .into_iter()
    .collect();
    let res = adapter.find_incremental(&known, &mut |_| {}).unwrap();
    assert_eq!(res.deleted, vec![MAIN_ID.to_string()]);
}

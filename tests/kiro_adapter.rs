//! Fixture tests for Kiro CLI's session store under `~/.kiro/sessions`. It
//! has two layouts: `cli/<uuid>.json` beside `cli/<uuid>.jsonl`, and
//! `<workspace hash>/sess_<uuid>/` holding `session.json` and
//! `messages.jsonl`. The record shapes follow files written by Kiro CLI 2.22.
//! All text here is made up.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use hopback::adapters::kiro::KiroAdapter;
use hopback::adapters::{mtime_nanos, parse_iso8601_secs, Adapter, KnownSessions};
use hopback::config::AgentId;
use hopback::model::SessionKey;

const CLI_ID: &str = "11111111-1111-4111-8111-111111111111";
const DIR_ID: &str = "sess_22222222-2222-4222-8222-222222222222";
const WORKSPACE: &str = "0123456789abcdef";

fn write(path: &Path, body: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

fn write_lines(path: &Path, lines: &[&str]) {
    let body: String = lines.iter().map(|line| format!("{line}\n")).collect();
    write(path, &body);
}

fn cli_meta(id: &str, title: &str) -> String {
    format!(
        r#"{{"session_id":"{id}","cwd":"/work/app","created_at":"2026-07-15T10:00:00.323288Z","updated_at":"2026-07-15T10:00:04.924237Z","title":"{title}","session_created_reason":"subagent","session_state":{{"version":"v1","agent_name":"kiro_default","permissions":{{"trusted_tools":[]}}}}}}"#
    )
}

fn cli_prompt(text: &str) -> String {
    format!(
        r#"{{"version":"v1","kind":"Prompt","data":{{"message_id":"p1","content":[{{"kind":"text","data":"{text}"}}],"meta":{{"timestamp":1784109602}}}}}}"#
    )
}

/// A first-layout session with one prompt.
fn write_cli(root: &Path, id: &str, title: &str, prompt: &str) -> PathBuf {
    let meta = root.join("cli").join(format!("{id}.json"));
    write(&meta, &cli_meta(id, title));
    write_lines(&meta.with_extension("jsonl"), &[&cli_prompt(prompt)]);
    meta
}

fn dir_meta(id: &str, title: &str) -> String {
    format!(
        r#"{{"schemaVersion":"1.0.0","dataModelVersion":1,"id":"{id}","title":"{title}","agentMode":"default","workspacePaths":["/work/site"],"rootPaths":["/work/site"],"createdAt":"2026-08-01T12:00:00.416Z","lastModifiedAt":"2026-08-01T12:00:09.873Z","modelId":"auto","autopilot":true,"status":"idle"}}"#
    )
}

fn dir_user(text: &str) -> String {
    format!(
        r#"{{"id":"u1","timestamp":"2026-08-01T12:00:01.582Z","payload":{{"type":"user","content":"{text}","images":[],"documents":[]}}}}"#
    )
}

/// A second-layout session with one prompt.
fn write_dir(root: &Path, id: &str, title: &str, prompt: &str) -> PathBuf {
    let dir = root.join(WORKSPACE).join(id);
    write(&dir.join("session.json"), &dir_meta(id, title));
    write_lines(&dir.join("messages.jsonl"), &[&dir_user(prompt)]);
    dir
}

fn sorted_ids(adapter: &KiroAdapter) -> Vec<String> {
    let mut ids: Vec<String> = adapter.scan().unwrap().into_keys().collect();
    ids.sort();
    ids
}

fn set_mtime(path: &Path, secs_after_epoch: u64) {
    let time = SystemTime::UNIX_EPOCH + Duration::from_secs(secs_after_epoch);
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(time)
        .unwrap();
}

#[test]
fn scan_finds_both_layouts_and_nothing_else() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("sessions");
    let cli_path = write_cli(&root, CLI_ID, "Session", "Prompt");
    let dir_path = write_dir(&root, DIR_ID, "Session", "Prompt");

    // Files Kiro keeps beside its sessions.
    write(&root.join("cli").join(format!("{CLI_ID}.lock")), "{}");
    write(
        &root.join("cli").join(format!("{CLI_ID}.history")),
        "Prompt\n/exit\n",
    );
    write(&root.join("cli").join("orphan.jsonl"), "");
    write(&root.join(WORKSPACE).join(".index").join(".lock"), "");
    write(
        &root.join(WORKSPACE).join("notes").join("session.json"),
        "{}",
    );
    write(
        &root
            .join(WORKSPACE)
            .join("sess_no-metadata")
            .join("messages.jsonl"),
        "",
    );
    write(&root.join("stray.json"), "{}");
    // A folder in `cli` named like a second-layout session is not one.
    write(
        &root.join("cli").join("sess_nested").join("session.json"),
        "{}",
    );

    let adapter = KiroAdapter::with_sessions_dir(root);
    assert!(adapter.is_available());
    assert_eq!(sorted_ids(&adapter), [CLI_ID, DIR_ID]);

    let scan = adapter.scan().unwrap();
    assert_eq!(scan[CLI_ID].0, cli_path);
    assert_eq!(scan[DIR_ID].0, dir_path);
}

#[test]
fn a_missing_store_is_unavailable() {
    let tmp = tempfile::tempdir().unwrap();
    let adapter = KiroAdapter::with_sessions_dir(tmp.path().join("sessions"));
    assert!(!adapter.is_available());
    assert!(adapter.scan().unwrap().is_empty());

    let known: KnownSessions = [(SessionKey::new(AgentId::Kiro, "a"), 1)]
        .into_iter()
        .collect();
    let result = adapter.find_incremental(&known, &mut |_| {}).unwrap();
    assert_eq!(result.deleted, ["a"]);
}

#[test]
fn parses_the_first_layout() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("sessions");
    let meta = root.join("cli").join(format!("{CLI_ID}.json"));
    write(&meta, &cli_meta(CLI_ID, "Uploader retries"));
    write_lines(
        &meta.with_extension("jsonl"),
        &[
            &cli_prompt("Add retry logic to the uploader"),
            r#"{"version":"v1","kind":"AssistantMessage","data":{"message_id":"a1","content":[{"kind":"thinking","data":{"text":"private reasoning","signature":"sig","redactedContent":null,"modelId":"auto"}},{"kind":"text","data":"Added a retry loop with backoff."},{"kind":"toolUse","data":{"toolUseId":"t1","name":"fs_write","input":{"path":"hidden arguments"}}}]}}"#,
            r#"{"version":"v1","kind":"ToolResults","data":{"message_id":"r1","content":[{"kind":"toolResult","data":{"toolUseId":"t1","content":"tool output stays out"}}]}}"#,
            "this line is not JSON",
            "",
            &cli_prompt("Now cap it at five tries"),
        ],
    );

    let adapter = KiroAdapter::with_sessions_dir(root);
    let scan = adapter.scan().unwrap();
    let (path, mtime) = &scan[CLI_ID];
    let session = adapter.parse(path).unwrap().expect("a session");

    assert_eq!(session.id, CLI_ID);
    assert_eq!(session.agent, AgentId::Kiro);
    assert_eq!(session.title, "Uploader retries");
    assert_eq!(session.directory, "/work/app");
    assert_eq!(
        Some(session.timestamp),
        parse_iso8601_secs("2026-07-15T10:00:04Z"),
        "updated_at, not created_at"
    );
    assert_eq!(session.message_count, 3);
    assert_eq!(session.mtime, *mtime);
    assert!(!session.yolo);
    assert_eq!(
        session.source_path.as_deref(),
        Some(meta.to_string_lossy().as_ref())
    );
    assert_eq!(
        session.content,
        "» Add retry logic to the uploader\n\n  Added a retry loop with backoff. [calling fs_write]\n\n» Now cap it at five tries"
    );
    for hidden in ["private reasoning", "hidden arguments", "tool output"] {
        assert!(!session.content.contains(hidden), "{hidden} leaked");
    }
}

#[test]
fn parses_the_second_layout() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("sessions");
    let dir = root.join(WORKSPACE).join(DIR_ID);
    write(
        &dir.join("session.json"),
        &dir_meta(DIR_ID, "Landing page copy"),
    );
    write_lines(
        &dir.join("messages.jsonl"),
        &[
            &dir_user("Tighten the landing page copy"),
            r#"{"id":"e1-turn-start","timestamp":"2026-08-01T12:00:01.589Z","payload":{"type":"turn_start","executionId":"e1"}}"#,
            r#"{"id":"m1-say","timestamp":"2026-08-01T12:00:04.406Z","payload":{"type":"assistant","content":"private reasoning","operationType":"Reasoning","executionId":"e1","reasoningSignature":"sig"}}"#,
            r#"{"id":"m2-say","timestamp":"2026-08-01T12:00:04.860Z","payload":{"type":"assistant","content":"Shortened the headline and the intro.","operationType":"Say","executionId":"e1"}}"#,
            r#"{"id":"s1","timestamp":"2026-08-01T12:00:04.860Z","payload":{"type":"session_metadata","key":"contextUsage","value":{"usagePercentage":9.7},"executionId":"e1"}}"#,
            r#"{"id":"e1-usage","timestamp":"2026-08-01T12:00:04.870Z","payload":{"type":"usage_summary","elapsedTime":9281,"status":"success","executionId":"e1"}}"#,
            r#"{"id":"e1-turn-end","timestamp":"2026-08-01T12:00:04.870Z","payload":{"type":"turn_end","stopReason":"end_turn","executionId":"e1"}}"#,
            r#"{"id":"session_start","timestamp":"2026-08-01T12:00:04.873Z","payload":{"type":"session_start","agentType":"default","content":"operating instructions stay out","forcedRole":true}}"#,
        ],
    );
    write(&dir.join("publish.cursor"), "7");

    let adapter = KiroAdapter::with_sessions_dir(root);
    let scan = adapter.scan().unwrap();
    let (path, mtime) = &scan[DIR_ID];
    assert_eq!(path, &dir);
    let session = adapter.parse(path).unwrap().expect("a session");

    assert_eq!(session.id, DIR_ID);
    assert_eq!(session.agent, AgentId::Kiro);
    assert_eq!(session.title, "Landing page copy");
    assert_eq!(session.directory, "/work/site");
    assert_eq!(
        Some(session.timestamp),
        parse_iso8601_secs("2026-08-01T12:00:09Z"),
        "lastModifiedAt, not createdAt"
    );
    assert_eq!(session.message_count, 2);
    assert_eq!(session.mtime, *mtime);
    assert_eq!(
        session.source_path.as_deref(),
        Some(dir.to_string_lossy().as_ref())
    );
    assert_eq!(
        session.content,
        "» Tighten the landing page copy\n\n  Shortened the headline and the intro."
    );
}

#[test]
fn drops_sessions_without_a_prompt_and_titles_the_untitled() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("sessions");

    // Kiro writes both files as soon as a session opens.
    let empty = root.join("cli").join("empty.json");
    write(&empty, &cli_meta("empty", ""));
    write(&empty.with_extension("jsonl"), "");

    // Metadata whose transcript was never written.
    let alone = root.join("cli").join("alone.json");
    write(&alone, &cli_meta("alone", "Has a title"));

    let broken = root.join("cli").join("broken.json");
    write(&broken, "{ not json");
    write_lines(&broken.with_extension("jsonl"), &[&cli_prompt("Prompt")]);

    let empty_dir = root.join(WORKSPACE).join("sess_empty");
    write(
        &empty_dir.join("session.json"),
        &dir_meta("sess_empty", "Titled"),
    );
    write(&empty_dir.join("messages.jsonl"), "");

    let untitled = write_cli(&root, "untitled", "", "the first thing I asked");
    let untitled_dir = write_dir(&root, "sess_untitled", "", "another first prompt");

    let adapter = KiroAdapter::with_sessions_dir(root);
    for dropped in [&empty, &alone, &broken, &empty_dir] {
        assert!(
            adapter.parse(dropped).unwrap().is_none(),
            "{}",
            dropped.display()
        );
    }
    assert_eq!(
        adapter.parse(&untitled).unwrap().unwrap().title,
        "the first thing I asked"
    );
    assert_eq!(
        adapter.parse(&untitled_dir).unwrap().unwrap().title,
        "another first prompt"
    );

    let mut upserts: Vec<String> = adapter
        .find_incremental(&KnownSessions::new(), &mut |_| {})
        .unwrap()
        .upserts
        .into_iter()
        .map(|s| s.id)
        .collect();
    upserts.sort();
    assert_eq!(upserts, ["sess_untitled", "untitled"]);
}

#[test]
fn the_id_is_the_file_name_even_when_the_metadata_disagrees() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("sessions");
    let meta = root.join("cli").join("on-disk.json");
    write(&meta, &cli_meta("something-else", "Session"));
    write_lines(&meta.with_extension("jsonl"), &[&cli_prompt("Prompt")]);

    let adapter = KiroAdapter::with_sessions_dir(root);
    assert_eq!(sorted_ids(&adapter), ["on-disk"]);
    assert_eq!(adapter.parse(&meta).unwrap().unwrap().id, "on-disk");
}

#[test]
fn a_new_message_in_either_file_marks_the_session_changed() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("sessions");
    let meta = write_cli(&root, CLI_ID, "Session", "Prompt");
    let transcript = meta.with_extension("jsonl");
    let dir = write_dir(&root, DIR_ID, "Session", "Prompt");
    for path in [&meta, &transcript] {
        set_mtime(path, 1_000);
    }
    set_mtime(&dir.join("session.json"), 1_000);
    set_mtime(&dir.join("messages.jsonl"), 1_000);

    let adapter = KiroAdapter::with_sessions_dir(root);
    let before = adapter.scan().unwrap();
    assert_eq!(before[CLI_ID].1, mtime_nanos(&meta).unwrap());

    let known: KnownSessions = before
        .iter()
        .map(|(id, (_, mtime))| (SessionKey::new(AgentId::Kiro, id.clone()), *mtime))
        .chain([(SessionKey::new(AgentId::Kiro, "gone"), 1)])
        .collect();
    let result = adapter.find_incremental(&known, &mut |_| {}).unwrap();
    assert!(result.upserts.is_empty(), "nothing changed yet");
    assert_eq!(result.deleted, ["gone"]);

    // Only the transcripts move on; the metadata files keep their old time.
    set_mtime(&transcript, 2_000);
    set_mtime(&dir.join("messages.jsonl"), 3_000);

    let after = adapter.scan().unwrap();
    assert_eq!(after[CLI_ID].1, mtime_nanos(&transcript).unwrap());
    assert_eq!(
        after[DIR_ID].1,
        mtime_nanos(&dir.join("messages.jsonl")).unwrap()
    );

    let result = adapter.find_incremental(&known, &mut |_| {}).unwrap();
    let mut changed: Vec<(String, i64)> = result
        .upserts
        .into_iter()
        .map(|s| (s.id, s.mtime))
        .collect();
    changed.sort();
    assert_eq!(
        changed,
        [
            (CLI_ID.to_string(), after[CLI_ID].1),
            (DIR_ID.to_string(), after[DIR_ID].1),
        ]
    );
}

#[test]
fn resume_command_with_and_without_trusting_all_tools() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("sessions");
    let meta = write_cli(&root, CLI_ID, "Session", "Prompt");
    let dir = write_dir(&root, DIR_ID, "Session", "Prompt");
    let adapter = KiroAdapter::with_sessions_dir(root);

    let cli = adapter.parse(&meta).unwrap().unwrap();
    assert_eq!(
        adapter.resume_command(&cli, false),
        ["kiro-cli", "chat", "--resume-id", CLI_ID]
    );
    assert_eq!(
        adapter.resume_command(&cli, true),
        [
            "kiro-cli",
            "chat",
            "--trust-all-tools",
            "--resume-id",
            CLI_ID
        ]
    );

    let dir_session = adapter.parse(&dir).unwrap().unwrap();
    assert_eq!(
        adapter.resume_command(&dir_session, false),
        ["kiro-cli", "chat", "--resume-id", DIR_ID]
    );

    assert!(adapter.supports_yolo());
    assert!(!adapter.detects_yolo());
    assert!(!adapter.supports_fork());
    assert!(adapter.fork_command(&cli, None, None, false).is_none());
}

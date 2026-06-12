use std::fs;
use std::path::PathBuf;

use aiz::adapters::claude::ClaudeAdapter;
use aiz::adapters::Adapter;

fn write(path: &std::path::Path, contents: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

#[test]
fn discovers_and_parses_sessions_skipping_agent_files() {
    let tmp = tempfile::tempdir().unwrap();
    let root: PathBuf = tmp.path().join("projects");

    let proj = root.join("-home-u-proj");
    write(
        &proj.join("aaaa-1111.jsonl"),
        "{\"type\":\"user\",\"cwd\":\"/home/u/proj\",\"message\":{\"content\":\"please review the parser code\"}}\n{\"type\":\"assistant\",\"message\":{\"content\":\"On it.\"}}\n",
    );
    write(
        &proj.join("agent-subprocess.jsonl"),
        "{\"type\":\"user\",\"message\":{\"content\":\"should be skipped entirely\"}}\n{\"type\":\"assistant\",\"message\":{\"content\":\"x\"}}\n",
    );
    write(&proj.join("notes.txt"), "ignore me");

    write(
        &proj.join("sessions-index.json"),
        r#"{"entries":[{"sessionId":"aaaa-1111","summary":"Parser Review","modified":"2026-06-03T16:11:02.882Z"}]}"#,
    );

    let adapter = ClaudeAdapter::with_sessions_dir(root.clone());
    assert!(adapter.is_available());

    let scanned = adapter.scan().unwrap();

    assert_eq!(scanned.len(), 1);
    assert!(scanned.contains_key("aaaa-1111"));

    let (path, _mtime) = scanned.get("aaaa-1111").unwrap();
    let session = adapter.parse(path).unwrap().unwrap();
    assert_eq!(session.id, "aaaa-1111");
    assert_eq!(session.directory, "/home/u/proj");

    assert_eq!(session.title, "Parser Review");
    assert_eq!(session.message_count, 2);
}

#[test]
fn unavailable_when_dir_missing() {
    let adapter = ClaudeAdapter::with_sessions_dir(PathBuf::from("/definitely/not/here"));
    assert!(!adapter.is_available());
    assert!(adapter.scan().unwrap().is_empty());
}

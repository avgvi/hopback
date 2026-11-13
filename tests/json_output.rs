#![cfg(unix)]
//! `--json` on fixture histories: one JSON object on stdout, the documented
//! schema, paging, and the flags that shape the result.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use hopback::config::AgentId;
use hopback::model::{ForkLink, ForkMode, PermissionPolicy, SessionKey};
use hopback::store::Store;
use serde_json::Value;

struct Sandbox {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    cache: PathBuf,
}

fn write(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn claude_session(home: &Path, id: &str, ts: &str, prompt: &str) {
    write(
        &home.join(format!(".claude/projects/-work-app/{id}.jsonl")),
        &format!(
            "{{\"type\":\"user\",\"timestamp\":\"{ts}\",\"cwd\":\"/work/app\",\"message\":{{\"content\":\"{prompt}\"}}}}\n\
             {{\"type\":\"assistant\",\"timestamp\":\"{ts}\",\"message\":{{\"content\":\"On it.\"}}}}\n"
        ),
    );
}

fn codex_session(home: &Path, id: &str, ts: &str, prompt: &str) {
    write(
        &home.join(format!(
            ".codex/sessions/2026/06/01/rollout-2026-06-01T10-00-00-{id}.jsonl"
        )),
        &format!(
            "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\",\"timestamp\":\"{ts}\",\"cwd\":\"/work/api\",\"source\":\"cli\"}}}}\n\
             {{\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"user\",\"content\":[{{\"type\":\"input_text\",\"text\":\"{prompt}\"}}]}}}}\n\
             {{\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{{\"type\":\"output_text\",\"text\":\"Done.\"}}]}}}}\n"
        ),
    );
}

/// Five sessions; by time, newest first: c5, x4, c3, x2, c1.
fn sandbox() -> Sandbox {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let cache = tmp.path().join("cache");
    fs::create_dir_all(&cache).unwrap();
    claude_session(
        &home,
        "c1",
        "2026-06-01T10:00:00Z",
        "fix the login redirect",
    );
    codex_session(
        &home,
        "x2",
        "2026-06-02T10:00:00Z",
        "refactor the api client",
    );
    claude_session(&home, "c3", "2026-06-03T10:00:00Z", "write release notes");
    codex_session(
        &home,
        "x4",
        "2026-06-04T10:00:00Z",
        "speed up the test suite",
    );
    claude_session(
        &home,
        "c5",
        "2026-06-05T10:00:00Z",
        "review the api client refactor",
    );
    Sandbox {
        _tmp: tmp,
        home,
        cache,
    }
}

impl Sandbox {
    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_hb"))
            .args(args)
            .env("HOME", &self.home)
            .env("XDG_CACHE_HOME", &self.cache)
            .env_remove("XDG_CONFIG_HOME")
            .output()
            .expect("spawn hb")
    }

    fn json(&self, args: &[&str]) -> Value {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "hb {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8(out.stdout).unwrap();
        assert_eq!(stdout.lines().count(), 1, "exactly one line:\n{stdout}");
        serde_json::from_str(&stdout).expect("stdout is one JSON value")
    }

    fn store(&self) -> Store {
        Store::open(&self.cache.join("hopback/hopback.db")).unwrap()
    }
}

fn ids(v: &Value) -> Vec<&str> {
    v["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap())
        .collect()
}

#[test]
fn prints_one_object_with_the_v1_schema() {
    let sb = sandbox();
    let v = sb.json(&["--json"]);

    assert_eq!(v["schema_version"], 1);
    assert_eq!(ids(&v), vec!["c5", "x4", "c3", "x2", "c1"]);
    assert_eq!(
        v["meta"],
        serde_json::json!({
            "state": "complete", "total": 5, "offset": 0, "limit": 50,
            "returned": 5, "next_offset": null
        })
    );

    let s = &v["sessions"][1];
    assert_eq!(s["id"], "x4");
    assert_eq!(s["agent"], "codex");
    assert_eq!(s["title"], "speed up the test suite");
    assert_eq!(s["original_title"], "speed up the test suite");
    assert_eq!(s["directory"], "/work/api");
    assert_eq!(s["timestamp"], "2026-06-04T10:00:00Z");
    assert_eq!(s["message_count"], 2);
    assert_eq!(s["pinned"], false);
    assert_eq!(s["hidden"], false);
    assert!(s["forked_from"].is_null());
    assert_eq!(
        s["resume_command"],
        serde_json::json!(["codex", "resume", "x4"])
    );
    assert_eq!(
        v["sessions"][0]["resume_command"],
        serde_json::json!(["claude", "--resume", "c5"])
    );
    assert_eq!(s.as_object().unwrap().len(), 11, "no undocumented fields");
}

#[test]
fn pages_with_limit_and_offset_until_complete_then_past_end() {
    let sb = sandbox();

    let first = sb.json(&["--json", "--limit", "2"]);
    assert_eq!(ids(&first), vec!["c5", "x4"]);
    assert_eq!(first["meta"]["state"], "more");
    assert_eq!(first["meta"]["total"], 5);
    assert_eq!(first["meta"]["limit"], 2);
    assert_eq!(first["meta"]["returned"], 2);
    assert_eq!(first["meta"]["next_offset"], 2);

    let second = sb.json(&["--json", "--limit", "2", "--offset", "2"]);
    assert_eq!(ids(&second), vec!["c3", "x2"]);
    assert_eq!(second["meta"]["state"], "more");
    assert_eq!(second["meta"]["next_offset"], 4);

    let last = sb.json(&["--json", "--limit", "2", "--offset", "4"]);
    assert_eq!(ids(&last), vec!["c1"]);
    assert_eq!(last["meta"]["state"], "complete");
    assert!(last["meta"]["next_offset"].is_null());

    let past = sb.json(&["--json", "--limit", "2", "--offset", "9"]);
    assert!(ids(&past).is_empty());
    assert_eq!(past["meta"]["state"], "past_end");
    assert_eq!(past["meta"]["returned"], 0);
    assert_eq!(past["meta"]["total"], 5);
}

#[test]
fn all_returns_everything_from_the_offset() {
    let sb = sandbox();
    let v = sb.json(&["--json", "--all", "--offset", "1"]);
    assert_eq!(ids(&v), vec!["x4", "c3", "x2", "c1"]);
    assert_eq!(v["meta"]["state"], "complete");
    assert_eq!(v["meta"]["limit"], 4);
    assert_eq!(v["meta"]["offset"], 1);
}

#[test]
fn query_and_agent_filters_apply_before_paging() {
    let sb = sandbox();
    let v = sb.json(&["--json", "api client"]);
    let mut got = ids(&v);
    got.sort();
    assert_eq!(got, vec!["c5", "x2"]);
    assert_eq!(v["meta"]["total"], 2);

    let codex = sb.json(&["--json", "--agent", "codex"]);
    assert_eq!(ids(&codex), vec!["x4", "x2"]);

    let none = sb.json(&["--json", "zzz-no-such-text"]);
    assert!(ids(&none).is_empty());
    assert_eq!(none["meta"]["state"], "complete");
    assert_eq!(none["meta"]["total"], 0);
}

#[test]
fn pins_renames_hidden_and_forks_show_in_the_output() {
    let sb = sandbox();
    sb.json(&["--json"]);
    {
        let store = sb.store();
        store
            .set_pinned(&SessionKey::new(AgentId::Claude, "c1"), true)
            .unwrap();
        store
            .set_title_override(&SessionKey::new(AgentId::Codex, "x2"), Some("API client"))
            .unwrap();
        store
            .set_hidden(&SessionKey::new(AgentId::Claude, "c3"), true)
            .unwrap();
        store
            .insert_fork_link(&ForkLink {
                fork_session_id: Some("x4".into()),
                parent_session_id: "c5".into(),
                parent_agent: AgentId::Claude,
                fork_agent: AgentId::Codex,
                mode: ForkMode::Translated,
                directory: "/work/api".into(),
                created: 0,
            })
            .unwrap();
    }

    let v = sb.json(&["--json"]);
    assert_eq!(
        ids(&v),
        vec!["c1", "c5", "x4", "x2"],
        "pinned first, hidden left out"
    );
    assert_eq!(v["meta"]["total"], 4);
    assert_eq!(v["sessions"][0]["pinned"], true);
    assert_eq!(
        v["sessions"][2]["forked_from"],
        serde_json::json!({"agent": "claude", "id": "c5"})
    );
    assert_eq!(v["sessions"][3]["title"], "API client");
    assert_eq!(
        v["sessions"][3]["original_title"],
        "refactor the api client"
    );

    let with_hidden = sb.json(&["--json", "--include-hidden"]);
    assert_eq!(ids(&with_hidden), vec!["c1", "c5", "x4", "c3", "x2"]);
    assert_eq!(with_hidden["sessions"][3]["hidden"], true);
    assert_eq!(with_hidden["meta"]["total"], 5);
}

#[test]
fn resume_command_follows_the_yolo_flag_and_the_stored_policy() {
    let sb = sandbox();
    let plain = sb.json(&["--json", "--agent", "codex", "--limit", "1"]);
    assert_eq!(
        plain["sessions"][0]["resume_command"],
        serde_json::json!(["codex", "resume", "x4"])
    );

    let flagged = sb.json(&["--json", "--yolo", "--agent", "codex", "--limit", "1"]);
    assert_eq!(
        flagged["sessions"][0]["resume_command"],
        serde_json::json!([
            "codex",
            "--dangerously-bypass-approvals-and-sandbox",
            "resume",
            "x4"
        ])
    );

    sb.store()
        .set_permission_policy(PermissionPolicy::AlwaysYolo)
        .unwrap();
    let by_policy = sb.json(&["--json", "--agent", "claude", "--limit", "1"]);
    assert_eq!(
        by_policy["sessions"][0]["resume_command"],
        serde_json::json!(["claude", "--dangerously-skip-permissions", "--resume", "c5"])
    );
}

#[test]
fn no_refresh_serves_the_index_as_it_is() {
    let sb = sandbox();
    assert_eq!(sb.json(&["--json"])["meta"]["total"], 5);

    claude_session(
        &sb.home,
        "c6",
        "2026-06-06T10:00:00Z",
        "a brand new session",
    );
    assert_eq!(sb.json(&["--json", "--no-refresh"])["meta"]["total"], 5);
    let refreshed = sb.json(&["--json"]);
    assert_eq!(refreshed["meta"]["total"], 6);
    assert_eq!(ids(&refreshed)[0], "c6");
}

#[test]
fn diagnostics_go_to_stderr_and_stdout_stays_json() {
    let sb = sandbox();
    // An old aiz store makes the first run print its import notice.
    Store::open(&sb.cache.join("aiz/aiz.db"))
        .unwrap()
        .set_pinned(&SessionKey::new(AgentId::Claude, "c1"), true)
        .unwrap();

    let out = sb.run(&["--json", "--rebuild"]);
    assert!(out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("from aiz"), "import notice: {stderr}");
    assert!(stderr.contains("Index rebuilt"), "rebuild banner: {stderr}");
    let v: Value = serde_json::from_slice(&out.stdout).expect("stdout is only JSON");
    assert_eq!(v["meta"]["total"], 5);
    assert_eq!(ids(&v)[0], "c1", "the imported pin floats to the top");
}

#[test]
fn bad_flag_combinations_fail_without_printing_json() {
    let sb = sandbox();
    for args in [
        &["--json", "--all", "--limit", "3"][..],
        &["--json", "--limit", "0"][..],
        &["--limit", "3"][..],
        &["--offset", "3"][..],
    ] {
        let out = sb.run(args);
        assert!(!out.status.success(), "{args:?} should fail");
        assert!(out.stdout.is_empty(), "{args:?} printed to stdout");
        assert!(!out.stderr.is_empty(), "{args:?} should explain on stderr");
    }
}

#[test]
fn list_honours_the_same_paging_flags() {
    let sb = sandbox();
    let out = sb.run(&["--list", "--limit", "2", "--offset", "1"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].contains("speed up the test suite"));
    assert!(lines[1].contains("write release notes"));
}

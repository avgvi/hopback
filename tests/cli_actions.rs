#![cfg(unix)]
//! `hopback pin|unpin|rename|hide|unhide <selector>` and `fork --json`, run
//! non-interactively against fixture histories in a temp HOME. Forks are only
//! ever made inside that temp HOME or with `--dry-run`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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

/// A Claude transcript with the uuid chain a translated fork needs.
fn claude_session(home: &Path, id: &str, ts: &str, prompt: &str) {
    write(
        &home.join(format!(".claude/projects/-work-app/{id}.jsonl")),
        &format!(
            "{{\"type\":\"user\",\"uuid\":\"u1\",\"parentUuid\":null,\"timestamp\":\"{ts}\",\"sessionId\":\"{id}\",\"cwd\":\"/work/app\",\"message\":{{\"role\":\"user\",\"content\":\"{prompt}\"}}}}\n\
             {{\"type\":\"assistant\",\"uuid\":\"a1\",\"parentUuid\":\"u1\",\"timestamp\":\"{ts}\",\"sessionId\":\"{id}\",\"cwd\":\"/work/app\",\"message\":{{\"id\":\"m1\",\"role\":\"assistant\",\"model\":\"claude\",\"content\":[{{\"type\":\"text\",\"text\":\"On it.\"}}]}}}}\n"
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

/// Newest first: `c3` (claude), `x2` (codex), `c1` (claude), plus `dup`, an id
/// that exists for both agents.
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
    claude_session(&home, "dup", "2026-05-01T10:00:00Z", "claude side of dup");
    codex_session(&home, "dup", "2026-05-02T10:00:00Z", "codex side of dup");
    Sandbox {
        _tmp: tmp,
        home,
        cache,
    }
}

impl Sandbox {
    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_hopback"))
            .args(args)
            .env("HOME", &self.home)
            .env("XDG_CACHE_HOME", &self.cache)
            .env_remove("XDG_CONFIG_HOME")
            .output()
            .expect("spawn hopback")
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "hopback {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    fn json(&self, args: &[&str]) -> Value {
        let stdout = self.ok(args);
        assert_eq!(stdout.lines().count(), 1, "exactly one line:\n{stdout}");
        serde_json::from_str(&stdout).expect("stdout is one JSON value")
    }

    fn fails(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert_eq!(out.status.code(), Some(1), "hopback {args:?} should fail");
        assert!(out.stdout.is_empty(), "nothing on stdout for {args:?}");
        String::from_utf8_lossy(&out.stderr).to_string()
    }

    fn session(&self, agent: &str, id: &str) -> Value {
        let list = self.json(&["--json", "--all", "--include-hidden"]);
        list["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["agent"] == agent && s["id"] == id)
            .unwrap_or_else(|| panic!("{agent}:{id} not listed"))
            .clone()
    }

    fn listed_ids(&self) -> Vec<String> {
        self.json(&["--json", "--all"])["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| {
                format!(
                    "{}:{}",
                    s["agent"].as_str().unwrap(),
                    s["id"].as_str().unwrap()
                )
            })
            .collect()
    }
}

#[test]
fn pin_and_unpin() {
    let sb = sandbox();
    assert_eq!(sb.ok(&["pin", "claude:c1"]), "pinned claude:c1\n");
    assert_eq!(sb.session("claude", "c1")["pinned"], true);
    assert_eq!(
        sb.listed_ids()[0],
        "claude:c1",
        "pinned sessions list first"
    );

    assert_eq!(sb.ok(&["unpin", "claude:c1"]), "unpinned claude:c1\n");
    assert_eq!(sb.session("claude", "c1")["pinned"], false);
    assert_eq!(sb.listed_ids()[0], "claude:c3");
}

#[test]
fn a_bare_id_works_when_it_is_unique() {
    let sb = sandbox();
    assert_eq!(sb.ok(&["pin", "x2"]), "pinned codex:x2\n");
    assert_eq!(sb.session("codex", "x2")["pinned"], true);
}

#[test]
fn ambiguous_and_unknown_selectors_are_errors() {
    let sb = sandbox();
    let err = sb.fails(&["pin", "dup"]);
    assert!(err.contains("ambiguous session id 'dup'"), "{err}");
    assert!(
        err.contains("claude:dup") && err.contains("codex:dup"),
        "{err}"
    );
    assert_eq!(sb.session("claude", "dup")["pinned"], false);

    let err = sb.fails(&["hide", "nope"]);
    assert!(err.contains("no session with id 'nope'"), "{err}");
    let err = sb.fails(&["rename", "claude:nope", "x"]);
    assert!(err.contains("no session with key 'claude:nope'"), "{err}");
    let err = sb.fails(&["pin", "martian:c1"]);
    assert!(err.contains("no session"), "{err}");
}

#[test]
fn rename_sets_a_local_title_and_reset_removes_it() {
    let sb = sandbox();
    let out = sb.ok(&["rename", "codex:x2", "API client cleanup"]);
    assert_eq!(out, "renamed to \"API client cleanup\": codex:x2\n");
    let s = sb.session("codex", "x2");
    assert_eq!(s["title"], "API client cleanup");
    assert_eq!(s["original_title"], "refactor the api client");

    // The agent's own file is untouched.
    let raw = fs::read_to_string(
        sb.home
            .join(".codex/sessions/2026/06/01/rollout-2026-06-01T10-00-00-x2.jsonl"),
    )
    .unwrap();
    assert!(!raw.contains("API client cleanup"));

    assert_eq!(
        sb.ok(&["rename", "codex:x2", "--reset"]),
        "rename removed: codex:x2\n"
    );
    assert_eq!(
        sb.session("codex", "x2")["title"],
        "refactor the api client"
    );
}

#[test]
fn rename_needs_a_title_or_reset() {
    let sb = sandbox();
    let out = sb.run(&["rename", "codex:x2"]);
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());

    let err = sb.fails(&["rename", "codex:x2", "   "]);
    assert!(err.contains("title is empty"), "{err}");

    let out = sb.run(&["rename", "codex:x2", "new", "--reset"]);
    assert!(!out.status.success(), "a title and --reset conflict");
    assert_eq!(
        sb.session("codex", "x2")["title"],
        "refactor the api client"
    );
}

#[test]
fn hide_and_unhide() {
    let sb = sandbox();
    assert_eq!(sb.ok(&["hide", "claude:c3"]), "hidden claude:c3\n");
    assert!(!sb.listed_ids().contains(&"claude:c3".to_string()));
    assert_eq!(sb.session("claude", "c3")["hidden"], true);

    // A hidden session can still be addressed, by key or bare id.
    assert_eq!(sb.ok(&["pin", "c3"]), "pinned claude:c3\n");
    assert_eq!(sb.ok(&["hide", "claude:c3"]), "hidden claude:c3\n");

    assert_eq!(sb.ok(&["unhide", "c3"]), "unhidden claude:c3\n");
    let ids = sb.listed_ids();
    assert_eq!(ids[0], "claude:c3", "back, and still pinned");
    assert_eq!(sb.session("claude", "c3")["hidden"], false);
}

#[test]
fn json_prints_the_session_after_the_change() {
    let sb = sandbox();
    let v = sb.json(&["pin", "claude:c1", "--json"]);
    assert_eq!(v["schema_version"], 1);
    let s = &v["session"];
    assert_eq!(s["id"], "c1");
    assert_eq!(s["agent"], "claude");
    assert_eq!(s["pinned"], true);
    assert_eq!(s["hidden"], false);
    assert_eq!(s["directory"], "/work/app");
    assert_eq!(
        s["resume_command"],
        serde_json::json!(["claude", "--resume", "c1"])
    );
    assert_eq!(s.as_object().unwrap().len(), 11);

    let v = sb.json(&["rename", "--json", "claude:c1", "Login redirect"]);
    assert_eq!(v["session"]["title"], "Login redirect");
    assert_eq!(v["session"]["original_title"], "fix the login redirect");
    assert_eq!(v["session"]["pinned"], true, "earlier changes are kept");

    let v = sb.json(&["hide", "claude:c1", "--json"]);
    assert_eq!(v["session"]["hidden"], true);
    let v = sb.json(&["unhide", "claude:c1", "--json", "--yolo"]);
    assert_eq!(v["session"]["hidden"], false);
    assert_eq!(
        v["session"]["resume_command"],
        serde_json::json!(["claude", "--dangerously-skip-permissions", "--resume", "c1"])
    );
}

#[test]
fn no_refresh_uses_the_index_as_it_is() {
    let sb = sandbox();
    // Nothing indexed yet, and told not to look.
    let err = sb.fails(&["pin", "claude:c1", "--no-refresh"]);
    assert!(err.contains("no session with key 'claude:c1'"), "{err}");

    sb.ok(&["--list"]);
    assert_eq!(
        sb.ok(&["--no-refresh", "pin", "claude:c1"]),
        "pinned claude:c1\n"
    );
}

#[test]
fn native_fork_json_dry_run_describes_a_claude_fork_without_creating_it() {
    let sb = sandbox();
    let before = sb.listed_ids();
    let v = sb.json(&["fork", "--json", "--dry-run", "claude:c1"]);

    assert_eq!(v["schema_version"], 1);
    assert_eq!(v["mode"], "native");
    assert_eq!(v["dry_run"], true);
    assert_eq!(v["created"], false);
    let s = &v["session"];
    let new_id = s["id"].as_str().unwrap();
    assert_eq!(new_id.len(), 36, "a fresh uuid: {new_id}");
    assert_ne!(new_id, "c1");
    assert_eq!(s["agent"], "claude");
    assert_eq!(s["directory"], "/work/app");
    assert_eq!(s["title"], "fix the login redirect");
    assert_eq!(
        s["forked_from"],
        serde_json::json!({"agent": "claude", "id": "c1"})
    );
    assert_eq!(
        s["resume_command"],
        serde_json::json!(["claude", "--resume", new_id])
    );
    assert_eq!(
        v["launch_command"],
        serde_json::json!([
            "claude",
            "--resume",
            "c1",
            "--fork-session",
            "--session-id",
            new_id
        ])
    );

    assert_eq!(sb.listed_ids(), before, "a dry run changes nothing");
    assert!(
        !sb.home
            .join(format!(".claude/projects/-work-app/{new_id}.jsonl"))
            .exists(),
        "no session file is written"
    );
}

#[test]
fn native_fork_json_for_codex_has_a_command_but_no_session_yet() {
    let sb = sandbox();
    let v = sb.json(&["fork", "--json", "--dry-run", "--yolo", "codex:x2"]);
    assert_eq!(v["mode"], "native");
    assert!(v["session"].is_null(), "Codex picks the new id itself");
    assert_eq!(
        v["launch_command"],
        serde_json::json!(["codex", "fork", "x2"])
    );
    assert_eq!(v["created"], false);
}

#[test]
fn translated_fork_json_dry_run_writes_nothing() {
    let sb = sandbox();
    let v = sb.json(&["fork", "--json", "--dry-run", "--to", "codex", "claude:c1"]);
    assert_eq!(v["mode"], "translated");
    assert_eq!(v["dry_run"], true);
    assert_eq!(v["created"], false);
    assert_eq!(v["session"]["agent"], "codex");
    assert_eq!(
        v["session"]["forked_from"],
        serde_json::json!({"agent": "claude", "id": "c1"})
    );
    assert_eq!(v["launch_command"], v["session"]["resume_command"]);

    let ids = sb.listed_ids();
    assert_eq!(ids.len(), 5, "no new session after a dry run: {ids:?}");
    let new_id = v["session"]["id"].as_str().unwrap();
    assert!(!ids.contains(&format!("codex:{new_id}")));
}

#[test]
fn translated_fork_json_creates_a_codex_session_from_a_claude_one() {
    let sb = sandbox();
    let v = sb.json(&[
        "fork",
        "--json",
        "--to",
        "codex",
        "--no-register-codex-index",
        "claude:c1",
    ]);
    assert_eq!(v["schema_version"], 1);
    assert_eq!(v["mode"], "translated");
    assert_eq!(v["dry_run"], false);
    assert_eq!(v["created"], true);

    let s = &v["session"];
    let new_id = s["id"].as_str().unwrap().to_string();
    assert_eq!(s["agent"], "codex");
    assert_eq!(s["directory"], "/work/app");
    assert_eq!(
        s["forked_from"],
        serde_json::json!({"agent": "claude", "id": "c1"})
    );
    assert_eq!(
        s["resume_command"],
        serde_json::json!(["codex", "resume", new_id])
    );
    assert_eq!(v["launch_command"], s["resume_command"]);
    assert!(s["message_count"].as_u64().unwrap() >= 1);

    // The new Codex session exists inside the temp HOME only, and the next
    // listing shows it as a fork of the Claude session.
    let rollouts: Vec<PathBuf> = walk(&sb.home.join(".codex/sessions"))
        .into_iter()
        .filter(|p| p.to_string_lossy().contains(&new_id))
        .collect();
    assert_eq!(rollouts.len(), 1, "one new rollout file: {rollouts:?}");
    assert!(rollouts[0].starts_with(&sb.home));

    let listed = sb.session("codex", &new_id);
    assert_eq!(
        listed["forked_from"],
        serde_json::json!({"agent": "claude", "id": "c1"})
    );
    assert_eq!(sb.listed_ids().len(), 6);

    // The source transcript is unchanged.
    let source = fs::read_to_string(sb.home.join(".claude/projects/-work-app/c1.jsonl")).unwrap();
    assert_eq!(source.lines().count(), 2);
}

#[test]
fn fork_json_errors_keep_stdout_empty() {
    let sb = sandbox();
    let err = sb.fails(&["fork", "--json", "--dry-run", "claude:nope"]);
    assert!(err.contains("no session"), "{err}");
    let err = sb.fails(&["fork", "--json", "--dry-run", "--to", "kimi", "claude:c1"]);
    assert!(
        err.contains("translated forks are supported only between"),
        "{err}"
    );
    let err = sb.fails(&[
        "fork",
        "--json",
        "--dry-run",
        "--target-cwd",
        "/tmp",
        "claude:c1",
    ]);
    assert!(err.contains("--target-cwd is only valid"), "{err}");
}

#[test]
fn fork_without_json_still_prints_the_plain_dry_run_line() {
    let sb = sandbox();
    let out = sb.ok(&["fork", "--dry-run", "claude:c1"]);
    assert_eq!(
        out,
        "Would run native fork command: claude --resume c1 --fork-session\n"
    );
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(path);
        }
    }
    out
}

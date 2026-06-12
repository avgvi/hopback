#![cfg(unix)]

use std::fs;
use std::path::Path;
use std::process::Command;

use aiz::config::AgentId;
use aiz::model::SessionKey;
use aiz::store::Store;

fn write(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn sandbox() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let cache = tmp.path().join("cache");
    fs::create_dir_all(&cache).unwrap();

    write(
        &home.join(".claude/projects/-proj/claude-sess-1.jsonl"),
        "{\"type\":\"user\",\"cwd\":\"/work/backend\",\"message\":{\"content\":\"fix the auth middleware bug\"}}\n{\"type\":\"assistant\",\"message\":{\"content\":\"Looking now.\"}}\n",
    );

    write(
        &home.join(".codex/sessions/2026/06/01/rollout-2026-06-01T10-00-00-codex-sess-1.jsonl"),
        "{\"type\":\"session_meta\",\"payload\":{\"id\":\"codex-sess-1\",\"cwd\":\"/work/frontend\"}}\n{\"type\":\"event_msg\",\"payload\":{\"type\":\"user_message\",\"message\":\"refactor the react components\"}}\n{\"type\":\"response_item\",\"payload\":{\"role\":\"assistant\",\"content\":[{\"text\":\"On it.\"}]}}\n",
    );

    tmp
}

fn run(home: &Path, cache: &Path, args: &[&str]) -> std::process::Output {
    Command::new(aiz_bin())
        .args(args)
        .env("HOME", home)
        .env("XDG_CACHE_HOME", cache)
        .env_remove("XDG_CONFIG_HOME")
        .output()
        .expect("spawn aiz")
}

fn aiz_bin() -> std::path::PathBuf {
    let cargo_bin = std::path::PathBuf::from(env!("CARGO_BIN_EXE_aiz"));
    if cargo_bin.components().count() > 1 {
        return cargo_bin;
    }

    let mut path = std::env::current_exe().expect("current test exe");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.push("aiz");
    path
}

#[test]
fn no_tui_lists_all_sessions_and_exits_zero() {
    let tmp = sandbox();
    let home = tmp.path().join("home");
    let cache = tmp.path().join("cache");

    let out = run(&home, &cache, &["--no-tui"]);
    assert!(out.status.success(), "should exit 0");
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(stdout.contains("claude"), "claude row missing:\n{stdout}");
    assert!(stdout.contains("codex"), "codex row missing:\n{stdout}");
    assert!(stdout.contains("fix the auth middleware bug"));
    assert!(stdout.contains("refactor the react components"));
}

#[test]
fn no_tui_with_query_filters() {
    let tmp = sandbox();
    let home = tmp.path().join("home");
    let cache = tmp.path().join("cache");

    let out = run(&home, &cache, &["--no-tui", "auth"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("fix the auth middleware bug"));

    assert!(
        !stdout.contains("refactor the react components"),
        "query should filter out non-matches:\n{stdout}"
    );
}

#[test]
fn no_tui_with_agent_filter() {
    let tmp = sandbox();
    let home = tmp.path().join("home");
    let cache = tmp.path().join("cache");

    let out = run(&home, &cache, &["--no-tui", "-a", "codex"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("refactor the react components"));
    assert!(!stdout.contains("fix the auth middleware bug"));
}

#[test]
fn no_tui_with_agent_keyword_in_query() {
    let tmp = sandbox();
    let home = tmp.path().join("home");
    let cache = tmp.path().join("cache");

    let out = run(&home, &cache, &["--no-tui", "agent:claude auth"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("fix the auth middleware bug"),
        "claude auth session missing:\n{stdout}"
    );
    assert!(
        !stdout.contains("refactor the react components"),
        "codex session should be filtered out:\n{stdout}"
    );
}

#[test]
fn no_tui_with_dir_keyword_in_query() {
    let tmp = sandbox();
    let home = tmp.path().join("home");
    let cache = tmp.path().join("cache");

    let out = run(&home, &cache, &["--no-tui", "dir:frontend"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("refactor the react components"),
        "frontend session missing:\n{stdout}"
    );
    assert!(
        !stdout.contains("fix the auth middleware bug"),
        "backend session should be filtered out:\n{stdout}"
    );
}

#[test]
fn no_tui_with_date_keyword_filters_deterministically() {
    let tmp = sandbox();
    let home = tmp.path().join("home");
    let cache = tmp.path().join("cache");

    let old_only = run(&home, &cache, &["--no-tui", "date:>1d"]);
    assert!(old_only.status.success());
    assert!(
        old_only.stdout.is_empty(),
        "freshly-written fixtures are not 'older than a day':\n{}",
        String::from_utf8_lossy(&old_only.stdout)
    );

    let all = run(&home, &cache, &["--no-tui", "date:<100y"]);
    assert!(all.status.success());
    let stdout = String::from_utf8_lossy(&all.stdout);
    assert!(stdout.contains("auth middleware") && stdout.contains("react components"));
}

#[test]
fn list_flag_behaves_like_no_tui() {
    let tmp = sandbox();
    let home = tmp.path().join("home");
    let cache = tmp.path().join("cache");

    let out = run(&home, &cache, &["--list"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("claude") && stdout.contains("codex"));
}

#[test]
fn rebuild_reconstructs_store_from_files() {
    let tmp = sandbox();
    let home = tmp.path().join("home");
    let cache = tmp.path().join("cache");

    let first = run(&home, &cache, &["--no-tui"]);
    assert!(first.status.success());
    assert!(cache.join("aiz/aiz.db").exists(), "store should be created");

    let out = run(&home, &cache, &["--rebuild", "--list"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Index rebuilt"),
        "rebuild banner missing:\n{stdout}"
    );

    assert!(stdout.contains("claude") && stdout.contains("codex"));
}

#[test]
fn no_sessions_prints_nothing_and_exits_zero() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("empty-home");
    let cache = tmp.path().join("cache");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&cache).unwrap();

    let out = run(&home, &cache, &["--no-tui", "anything"]);
    assert!(out.status.success(), "empty result still exits 0");
    assert!(out.stdout.is_empty(), "no rows when there are no sessions");
}

#[test]
fn config_permission_get_set_and_reset() {
    let tmp = sandbox();
    let home = tmp.path().join("home");
    let cache = tmp.path().join("cache");

    let initial = run(&home, &cache, &["config", "permission", "get"]);
    assert!(initial.status.success());
    assert_eq!(String::from_utf8_lossy(&initial.stdout).trim(), "ask");

    let set = run(&home, &cache, &["config", "permission", "set", "yolo"]);
    assert!(set.status.success());
    assert_eq!(
        String::from_utf8_lossy(&set.stdout).trim(),
        "permission_policy=always_yolo"
    );

    let get = run(&home, &cache, &["config", "permission", "get"]);
    assert!(get.status.success());
    assert_eq!(String::from_utf8_lossy(&get.stdout).trim(), "always_yolo");

    let reset = run(&home, &cache, &["config", "permission", "reset"]);
    assert!(reset.status.success());
    assert_eq!(
        String::from_utf8_lossy(&reset.stdout).trim(),
        "permission_policy=ask"
    );
}

#[test]
fn config_hidden_list_unhide_and_reset() {
    let tmp = sandbox();
    let home = tmp.path().join("home");
    let cache = tmp.path().join("cache");

    let initial = run(&home, &cache, &["--no-tui"]);
    assert!(initial.status.success());

    let store_path = cache.join("aiz/aiz.db");
    let store = Store::open(&store_path).unwrap();
    store
        .set_hidden(&SessionKey::new(AgentId::Claude, "claude-sess-1"), true)
        .unwrap();

    let hidden_list = run(&home, &cache, &["config", "hidden", "list"]);
    assert!(hidden_list.status.success());
    let stdout = String::from_utf8_lossy(&hidden_list.stdout);
    assert!(stdout.contains("claude:claude-sess-1"));
    assert!(stdout.contains("fix the auth middleware bug"));

    let listed = run(&home, &cache, &["--list"]);
    assert!(listed.status.success());
    let stdout = String::from_utf8_lossy(&listed.stdout);
    assert!(!stdout.contains("fix the auth middleware bug"));
    assert!(stdout.contains("refactor the react components"));

    let unhide = run(
        &home,
        &cache,
        &["config", "hidden", "unhide", "claude:claude-sess-1"],
    );
    assert!(unhide.status.success());
    assert_eq!(
        String::from_utf8_lossy(&unhide.stdout).trim(),
        "hidden=false claude:claude-sess-1"
    );

    let listed = run(&home, &cache, &["--list"]);
    assert!(listed.status.success());
    assert!(String::from_utf8_lossy(&listed.stdout).contains("fix the auth middleware bug"));

    let store = Store::open(&store_path).unwrap();
    store
        .set_hidden(&SessionKey::new(AgentId::Claude, "claude-sess-1"), true)
        .unwrap();
    store
        .set_hidden(&SessionKey::new(AgentId::Codex, "codex-sess-1"), true)
        .unwrap();
    let reset = run(&home, &cache, &["config", "hidden", "reset"]);
    assert!(reset.status.success());
    assert_eq!(
        String::from_utf8_lossy(&reset.stdout).trim(),
        "hidden=reset"
    );

    let listed = run(&home, &cache, &["--list"]);
    assert!(listed.status.success());
    let stdout = String::from_utf8_lossy(&listed.stdout);
    assert!(stdout.contains("fix the auth middleware bug"));
    assert!(stdout.contains("refactor the react components"));
}

#[test]
fn yolo_flag_reaches_native_fork_dry_run() {
    let tmp = sandbox();
    let home = tmp.path().join("home");
    let cache = tmp.path().join("cache");

    let out = run(
        &home,
        &cache,
        &["--yolo", "fork", "--dry-run", "claude-sess-1"],
    );
    assert!(out.status.success(), "dry-run fork exits 0");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("--dangerously-skip-permissions"),
        "--yolo should reach Claude fork command:\n{stdout}"
    );
}

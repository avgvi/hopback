#![cfg(unix)]
//! `hb .`: resume the newest session of the current folder, from any agent.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Sandbox {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    cache: PathBuf,
    /// A project folder with two sessions: an older Claude one and a newer
    /// Codex one.
    app: PathBuf,
    /// A project folder with one Claude session.
    solo: PathBuf,
    /// A folder no agent ever ran in, outside every project.
    empty: PathBuf,
    bin: PathBuf,
}

fn write(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn claude_session(home: &Path, id: &str, ts: &str, cwd: &Path) {
    write(
        &home.join(format!(".claude/projects/-p/{id}.jsonl")),
        &format!(
            "{{\"type\":\"user\",\"timestamp\":\"{ts}\",\"cwd\":\"{}\",\"message\":{{\"content\":\"prompt for {id}\"}}}}\n\
             {{\"type\":\"assistant\",\"message\":{{\"content\":\"ok\"}}}}\n",
            cwd.display()
        ),
    );
}

fn codex_session(home: &Path, id: &str, ts: &str, cwd: &Path) {
    write(
        &home.join(format!(
            ".codex/sessions/2026/06/01/rollout-2026-06-01T10-00-00-{id}.jsonl"
        )),
        &format!(
            "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\",\"timestamp\":\"{ts}\",\"cwd\":\"{}\",\"source\":\"cli\"}}}}\n\
             {{\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"user\",\"content\":[{{\"type\":\"input_text\",\"text\":\"prompt for {id}\"}}]}}}}\n",
            cwd.display()
        ),
    );
}

/// Makes every session file as old as its conversation, so "newest" is decided
/// by the fixture's timestamps and not by the order the files were written in.
fn age_files(home: &Path) {
    let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
    let mut stack = vec![home.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                fs::File::options()
                    .write(true)
                    .open(&path)
                    .unwrap()
                    .set_modified(old)
                    .unwrap();
            }
        }
    }
}

fn sandbox() -> Sandbox {
    let tmp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(tmp.path()).unwrap();
    let home = root.join("home");
    let cache = root.join("cache");
    let app = root.join("work/app");
    let solo = root.join("work/solo");
    let empty = root.join("elsewhere/empty");
    let bin = root.join("bin");
    for dir in [&cache, &app.join("src/deep"), &solo, &empty, &bin] {
        fs::create_dir_all(dir).unwrap();
    }

    claude_session(&home, "claude-app", "2026-06-01T10:00:00Z", &app);
    codex_session(&home, "codex-app", "2026-06-03T10:00:00Z", &app);
    claude_session(&home, "claude-solo", "2026-06-05T10:00:00Z", &solo);
    age_files(&home);

    // Stand-ins for the agents: print the folder and the arguments.
    for agent in ["claude", "codex"] {
        let path = bin.join(agent);
        fs::write(
            &path,
            format!("#!/bin/sh\necho \"{agent} ran in $(pwd) with: $*\"\n"),
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    Sandbox {
        _tmp: tmp,
        home,
        cache,
        app,
        solo,
        empty,
        bin,
    }
}

impl Sandbox {
    fn hb(&self, cwd: &Path, args: &[&str]) -> Output {
        let path = format!(
            "{}:{}",
            self.bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        Command::new(env!("CARGO_BIN_EXE_hb"))
            .args(args)
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env("XDG_CACHE_HOME", &self.cache)
            .env("PATH", path)
            .env_remove("XDG_CONFIG_HOME")
            .output()
            .expect("spawn hb")
    }
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

#[test]
fn print_shows_the_folder_and_command_of_the_newest_session() {
    let sb = sandbox();
    let out = sb.hb(&sb.app, &[".", "--print"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        format!("{}\ncodex resume codex-app\n", sb.app.display()),
        "the Codex session is newer than the Claude one"
    );
    assert!(stderr(&out).is_empty());
}

#[test]
fn agent_narrows_the_pick() {
    let sb = sandbox();
    let out = sb.hb(&sb.app, &[".", "--print", "--agent", "claude"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        format!("{}\nclaude --resume claude-app\n", sb.app.display())
    );
}

#[test]
fn a_subfolder_falls_back_to_the_closest_parent() {
    let sb = sandbox();
    let deep = sb.app.join("src/deep");
    let out = sb.hb(&deep, &[".", "--print"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        format!("{}\ncodex resume codex-app\n", sb.app.display())
    );
    let err = stderr(&out);
    assert!(err.contains("no session in"), "notice missing: {err}");
    assert!(err.contains(&sb.app.display().to_string()));
}

#[test]
fn no_session_anywhere_is_a_clear_error_and_exit_1() {
    let sb = sandbox();
    let out = sb.hb(&sb.empty, &["."]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stdout(&out).is_empty());
    let err = stderr(&out);
    assert!(err.contains("no session found for"), "{err}");
    assert!(err.contains(&sb.empty.display().to_string()), "{err}");

    // Sessions exist in the folder, but none from the requested agent.
    let out = sb.hb(&sb.solo, &[".", "--agent", "codex", "--print"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stdout(&out).is_empty());
    assert!(stderr(&out).contains("no codex session found for"));
}

#[test]
fn unknown_agent_is_rejected() {
    let sb = sandbox();
    let out = sb.hb(&sb.app, &[".", "--agent", "nope"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("unknown agent 'nope'"));
}

#[test]
fn yolo_reaches_the_printed_command() {
    let sb = sandbox();
    let out = sb.hb(&sb.app, &[".", "--print", "--yolo"]);
    assert!(out.status.success());
    assert_eq!(
        stdout(&out),
        format!(
            "{}\ncodex --dangerously-bypass-approvals-and-sandbox resume codex-app\n",
            sb.app.display()
        )
    );
}

#[test]
fn without_print_it_runs_the_agent_in_the_session_folder() {
    let sb = sandbox();
    // Started from a subfolder: the agent must still run in the session's own.
    let out = sb.hb(&sb.app.join("src"), &["."]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        format!("codex ran in {} with: resume codex-app\n", sb.app.display())
    );

    let out = sb.hb(&sb.solo, &["."]);
    assert_eq!(
        stdout(&out),
        format!(
            "claude ran in {} with: --resume claude-solo\n",
            sb.solo.display()
        )
    );
}

#[test]
fn the_most_recently_written_session_wins_over_a_later_started_one() {
    let sb = sandbox();
    // Append to the older Claude session, as continuing it would.
    let path = sb.home.join(".claude/projects/-p/claude-app.jsonl");
    let mut body = fs::read_to_string(&path).unwrap();
    body.push_str("{\"type\":\"user\",\"message\":{\"content\":\"one more thing\"}}\n");
    fs::write(&path, body).unwrap();

    let out = sb.hb(&sb.app, &[".", "--print"]);
    assert_eq!(
        stdout(&out),
        format!("{}\nclaude --resume claude-app\n", sb.app.display())
    );
}

#[test]
fn dot_with_a_listing_flag_is_still_a_search() {
    let sb = sandbox();
    let out = sb.hb(&sb.app, &[".", "--list"]);
    assert!(out.status.success());
    assert!(!stdout(&out).contains("ran in"));
}

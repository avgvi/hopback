#![cfg(unix)]

use std::process::Command;

#[test]
fn resume_handoff_chdirs_and_execs() {
    let bin = aiz_bin();
    let dir = tempfile::tempdir().unwrap();

    let canon = std::fs::canonicalize(dir.path()).unwrap();

    let out = Command::new(bin)
        .env("AIZ_EXEC_TEST", canon.to_str().unwrap())
        .output()
        .expect("spawn aiz");

    assert!(out.status.success(), "handoff process should exit cleanly");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let printed = stdout.trim();

    assert_eq!(
        std::fs::canonicalize(printed).unwrap(),
        canon,
        "exec'd pwd should report the handoff directory"
    );

    assert!(
        !stdout.contains("aiz"),
        "aiz banner must not appear; the process should have been replaced"
    );
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

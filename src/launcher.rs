use std::path::Path;

use anyhow::{anyhow, Result};

use crate::adapters::{registry, Adapter};
use crate::config::AgentId;
use crate::model::{PermissionPolicy, Session};

fn adapter_for(session: &Session) -> Option<Box<dyn Adapter>> {
    adapter_for_agent(session.agent)
}

fn adapter_for_agent(agent: AgentId) -> Option<Box<dyn Adapter>> {
    registry::all().into_iter().find(|a| a.id() == agent)
}

pub fn resume_argv(session: &Session, yolo: bool) -> Result<Vec<String>> {
    let adapter = adapter_for(session)
        .ok_or_else(|| anyhow!("no adapter registered for agent {}", session.agent.as_str()))?;
    Ok(adapter.resume_command(session, yolo))
}

pub fn resume(session: &Session, yolo: bool) -> Result<std::convert::Infallible> {
    let argv = resume_argv(session, yolo)?;
    exec_in_dir(&argv, &session.directory)
}

pub fn can_fork(session: &Session) -> bool {
    adapter_for(session)
        .map(|a| a.supports_fork())
        .unwrap_or(false)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YoloDecision {
    Off,

    On,

    Prompt,
}

pub fn launch_permission_decision(
    session: &Session,
    flag_yolo: bool,
    policy: Option<PermissionPolicy>,
) -> YoloDecision {
    if flag_yolo {
        return YoloDecision::On;
    }
    let Some(adapter) = adapter_for(session) else {
        return YoloDecision::Off;
    };
    if !adapter.supports_yolo() {
        return YoloDecision::Off;
    }
    match policy {
        None => YoloDecision::Prompt,
        Some(PermissionPolicy::AlwaysNormal) => YoloDecision::Off,
        Some(PermissionPolicy::AlwaysYolo) => YoloDecision::On,
        Some(PermissionPolicy::InheritKnown) => inherited_yolo_decision(session),
    }
}

pub fn launch_permission_decision_for_agent(
    agent: AgentId,
    flag_yolo: bool,
    policy: Option<PermissionPolicy>,
) -> YoloDecision {
    if flag_yolo {
        return YoloDecision::On;
    }
    let Some(adapter) = adapter_for_agent(agent) else {
        return YoloDecision::Off;
    };
    if !adapter.supports_yolo() {
        return YoloDecision::Off;
    }
    match policy {
        None => YoloDecision::Prompt,
        Some(PermissionPolicy::AlwaysNormal) => YoloDecision::Off,
        Some(PermissionPolicy::AlwaysYolo) => YoloDecision::On,
        Some(PermissionPolicy::InheritKnown) => {
            if adapter.detects_yolo() {
                YoloDecision::Off
            } else {
                YoloDecision::Prompt
            }
        }
    }
}

pub fn inherited_yolo_decision(session: &Session) -> YoloDecision {
    let Some(adapter) = adapter_for(session) else {
        return YoloDecision::Off;
    };
    if !adapter.supports_yolo() {
        return YoloDecision::Off;
    }
    if adapter.detects_yolo() {
        if session.yolo {
            YoloDecision::On
        } else {
            YoloDecision::Off
        }
    } else {
        YoloDecision::Prompt
    }
}

pub fn supports_yolo(agent: AgentId) -> bool {
    adapter_for_agent(agent)
        .map(|a| a.supports_yolo())
        .unwrap_or(false)
}

pub fn fork_argv(
    session: &Session,
    new_id: Option<&str>,
    name: Option<&str>,
    yolo: bool,
) -> Result<Vec<String>> {
    let adapter = adapter_for(session)
        .ok_or_else(|| anyhow!("no adapter registered for agent {}", session.agent.as_str()))?;
    adapter
        .fork_command(session, new_id, name, yolo)
        .ok_or_else(|| anyhow!("{} has no native fork", session.agent.as_str()))
}

pub fn fork(session: &Session, name: Option<&str>, yolo: bool) -> Result<std::convert::Infallible> {
    let argv = fork_argv(session, None, name, yolo)?;
    exec_in_dir(&argv, &session.directory)
}

pub fn exec_in_dir(argv: &[String], dir: &str) -> Result<std::convert::Infallible> {
    let (program, rest) = argv
        .split_first()
        .ok_or_else(|| anyhow!("empty resume command"))?;

    if !dir.is_empty() {
        let path = Path::new(dir);
        if path.is_dir() {
            std::env::set_current_dir(path).map_err(|e| anyhow!("chdir to {dir}: {e}"))?;
        }
    }

    do_exec(program, rest)
}

#[cfg(unix)]
fn do_exec(program: &str, args: &[String]) -> Result<std::convert::Infallible> {
    use std::os::unix::process::CommandExt;

    let err = std::process::Command::new(program).args(args).exec();
    Err(anyhow!("exec {program}: {err}"))
}

#[cfg(not(unix))]
fn do_exec(program: &str, args: &[String]) -> Result<std::convert::Infallible> {
    let _ = (program, args);
    Err(anyhow!(
        "process handoff is only implemented on Unix (Windows support is P3.3)"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AgentId;

    fn sess(agent: AgentId, id: &str, dir: &str) -> Session {
        Session {
            id: id.into(),
            agent,
            title: "t".into(),
            directory: dir.into(),
            timestamp: 0,
            content: String::new(),
            message_count: 0,
            mtime: 0,
            yolo: false,
            source_path: None,
        }
    }

    #[test]
    fn resume_argv_uses_the_owning_adapter() {
        let c = sess(AgentId::Claude, "abc", "/tmp");
        assert_eq!(
            resume_argv(&c, false).unwrap(),
            vec!["claude", "--resume", "abc"]
        );
        assert_eq!(
            resume_argv(&c, true).unwrap(),
            vec![
                "claude",
                "--dangerously-skip-permissions",
                "--resume",
                "abc"
            ]
        );

        let x = sess(AgentId::Codex, "z9", "/tmp");
        assert_eq!(
            resume_argv(&x, false).unwrap(),
            vec!["codex", "resume", "z9"]
        );
    }

    #[test]
    fn empty_command_is_an_error() {
        let r = exec_in_dir(&[], "/tmp");
        assert!(r.is_err());
    }

    fn sess_yolo(agent: AgentId, yolo: bool) -> Session {
        let mut s = sess(agent, "id", "/tmp");
        s.yolo = yolo;
        s
    }

    #[test]
    fn launch_permission_flag_forces_on() {
        assert_eq!(
            launch_permission_decision(&sess_yolo(AgentId::Claude, false), true, None),
            YoloDecision::On
        );
        assert_eq!(
            launch_permission_decision(&sess_yolo(AgentId::Codex, false), true, None),
            YoloDecision::On
        );
    }

    #[test]
    fn launch_permission_no_policy_prompts_for_yolo_capable_agents() {
        assert_eq!(
            launch_permission_decision(&sess_yolo(AgentId::Claude, false), false, None),
            YoloDecision::Prompt
        );
        assert_eq!(
            launch_permission_decision(&sess_yolo(AgentId::Codex, false), false, None),
            YoloDecision::Prompt
        );
    }

    #[test]
    fn launch_permission_always_policies_do_not_prompt() {
        assert_eq!(
            launch_permission_decision(
                &sess_yolo(AgentId::Claude, false),
                false,
                Some(PermissionPolicy::AlwaysYolo)
            ),
            YoloDecision::On
        );
        assert_eq!(
            launch_permission_decision(
                &sess_yolo(AgentId::Codex, true),
                false,
                Some(PermissionPolicy::AlwaysNormal)
            ),
            YoloDecision::Off
        );
    }

    #[test]
    fn launch_permission_inherit_known_uses_stored_for_detecting_agents() {
        assert_eq!(
            launch_permission_decision(
                &sess_yolo(AgentId::Codex, true),
                false,
                Some(PermissionPolicy::InheritKnown)
            ),
            YoloDecision::On
        );
        assert_eq!(
            launch_permission_decision(
                &sess_yolo(AgentId::Codex, false),
                false,
                Some(PermissionPolicy::InheritKnown)
            ),
            YoloDecision::Off
        );
        assert_eq!(
            launch_permission_decision(
                &sess_yolo(AgentId::Claude, false),
                false,
                Some(PermissionPolicy::InheritKnown)
            ),
            YoloDecision::Prompt
        );
    }

    #[test]
    fn launch_permission_off_for_non_yolo_agents() {
        assert_eq!(
            launch_permission_decision(&sess_yolo(AgentId::OpenCode, false), false, None),
            YoloDecision::Off
        );
        assert_eq!(
            launch_permission_decision(&sess_yolo(AgentId::CopilotVscode, false), false, None),
            YoloDecision::Off
        );
    }

    #[test]
    fn can_fork_reflects_adapter_support() {
        assert!(can_fork(&sess(AgentId::Claude, "a", "/tmp")));
        assert!(can_fork(&sess(AgentId::Codex, "b", "/tmp")));
    }

    #[test]
    fn fork_argv_builds_native_fork_command() {
        let c = sess(AgentId::Claude, "abc", "/tmp");
        assert_eq!(
            fork_argv(&c, None, None, false).unwrap(),
            vec!["claude", "--resume", "abc", "--fork-session"]
        );
        assert_eq!(
            fork_argv(&c, None, Some("mybranch"), false).unwrap(),
            vec![
                "claude",
                "--resume",
                "abc",
                "--fork-session",
                "--name",
                "mybranch"
            ]
        );

        let x = sess(AgentId::Codex, "z9", "/tmp");
        assert_eq!(
            fork_argv(&x, None, None, false).unwrap(),
            vec!["codex", "fork", "z9"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn exec_of_missing_binary_errors_after_chdir() {
        let original = std::env::current_dir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let canon = std::fs::canonicalize(dir.path()).unwrap();

        let r = exec_in_dir(
            &["definitely-not-a-real-binary-xyzzy".to_string()],
            canon.to_str().unwrap(),
        );
        assert!(r.is_err(), "exec of a missing binary must return Err");
        assert_eq!(
            std::fs::canonicalize(std::env::current_dir().unwrap()).unwrap(),
            canon,
            "chdir should have happened before the failed exec"
        );

        std::env::set_current_dir(&original).unwrap();
    }
}

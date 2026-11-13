//! What hopback ships for other programs: the agent skill, shell completions
//! and the man page. All three are generated from, or bundled into, the
//! binary, so they always match the version that is installed.

use std::io::Write;

use clap::CommandFactory;

use crate::cli::Cli;

/// The Agent Skill printed by `--agent-context`.
pub const SKILL: &str = include_str!("../skills/hopback/SKILL.md");

/// The command names hopback is installed under.
pub const COMMAND_NAMES: [&str; 2] = ["hopback", "hb"];

/// The name this process was started as, when it is one of ours.
pub fn invoked_name() -> String {
    std::env::args_os()
        .next()
        .and_then(|arg0| {
            std::path::Path::new(&arg0)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
        })
        .filter(|name| COMMAND_NAMES.contains(&name.as_str()))
        .unwrap_or_else(|| COMMAND_NAMES[0].to_string())
}

/// Writes a completion script for `shell` that completes the command `name`.
pub fn write_completions(shell: clap_complete::Shell, name: &str, out: &mut dyn Write) {
    let mut command = Cli::command();
    clap_complete::generate(shell, &mut command, name, out);
}

/// Writes the man page for `hopback(1)` in roff format.
pub fn write_man_page(out: &mut dyn Write) -> std::io::Result<()> {
    let command = Cli::command();
    clap_mangen::Man::new(command).render(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn completions(shell: clap_complete::Shell, name: &str) -> String {
        let mut out = Vec::new();
        write_completions(shell, name, &mut out);
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn skill_has_frontmatter_and_names_the_commands_it_relies_on() {
        assert!(SKILL.starts_with("---\nname: hopback\n"));
        let end = SKILL[4..].find("\n---\n").expect("frontmatter closes");
        assert!(SKILL[..end + 4].contains("description:"));
        for needle in [
            "hb --json",
            "--limit",
            "--offset",
            "meta.state",
            "next_offset",
            "resume_command",
            "--no-refresh",
            "directory",
        ] {
            assert!(SKILL.contains(needle), "{needle} missing from the skill");
        }
    }

    #[test]
    fn skill_only_mentions_flags_that_exist() {
        let command = Cli::command();
        let mut known: Vec<String> = Vec::new();
        fn collect(cmd: &clap::Command, known: &mut Vec<String>) {
            for arg in cmd.get_arguments() {
                if let Some(long) = arg.get_long() {
                    known.push(format!("--{long}"));
                }
            }
            for sub in cmd.get_subcommands() {
                collect(sub, known);
            }
        }
        collect(&command, &mut known);
        known.extend(["--help".to_string(), "--version".to_string()]);

        for word in SKILL.split(|c: char| c.is_whitespace() || matches!(c, '`' | ',' | '(' | ')')) {
            let flag = word.trim_end_matches(['.', ':', ';']);
            let is_flag = flag
                .strip_prefix("--")
                .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_lowercase()));
            if is_flag {
                assert!(
                    known.contains(&flag.to_string()),
                    "{flag} is not a hopback flag"
                );
            }
        }
    }

    #[test]
    fn completions_are_generated_for_each_shell_and_both_names() {
        use clap_complete::Shell;
        let zsh = completions(Shell::Zsh, "hopback");
        assert!(zsh.starts_with("#compdef hopback"));
        assert!(zsh.contains("--json") && zsh.contains("--no-refresh"));
        assert!(zsh.contains("rename") && zsh.contains("completions"));

        let zsh_hb = completions(Shell::Zsh, "hb");
        assert!(zsh_hb.starts_with("#compdef hb"));

        let bash = completions(Shell::Bash, "hb");
        assert!(bash.contains("_hb()"));
        assert!(bash.contains("complete -F _hb"));

        let fish = completions(Shell::Fish, "hopback");
        assert!(fish.contains("complete -c hopback"));
        assert!(fish.contains("unhide"));
    }

    #[test]
    fn man_page_is_roff_and_covers_the_commands() {
        let mut out = Vec::new();
        write_man_page(&mut out).unwrap();
        let man = String::from_utf8(out).unwrap();
        assert!(man.starts_with(".ie") || man.contains(".TH hopback 1"));
        assert!(man.contains(".TH hopback 1"));
        assert!(man.contains(".SH NAME"));
        assert!(man.contains(".SH SYNOPSIS"));
        for needle in [
            "\\-\\-json",
            "\\-\\-agent\\-context",
            "pin",
            "fork",
            "completions",
        ] {
            assert!(man.contains(needle), "{needle} missing from the man page");
        }
    }

    #[test]
    fn committed_man_page_matches_the_generated_one() {
        let mut out = Vec::new();
        write_man_page(&mut out).unwrap();
        let generated = String::from_utf8(out).unwrap();
        let committed = include_str!("../man/hopback.1");
        assert_eq!(
            committed, generated,
            "man/hopback.1 is stale; regenerate it with `cargo run --bin hopback -- man > man/hopback.1`"
        );
    }
}

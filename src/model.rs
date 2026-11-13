use std::fmt;

use crate::config::AgentId;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionKey {
    pub agent: AgentId,
    pub id: String,
}

impl SessionKey {
    pub fn new(agent: AgentId, id: impl Into<String>) -> SessionKey {
        SessionKey {
            agent,
            id: id.into(),
        }
    }

    pub fn parse(s: &str) -> Option<SessionKey> {
        let (agent, id) = s.split_once(':')?;
        if id.is_empty() {
            return None;
        }
        Some(SessionKey::new(AgentId::resolve(agent)?, id))
    }
}

impl fmt::Display for SessionKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.agent.as_str(), self.id)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    pub id: String,
    pub agent: AgentId,
    pub title: String,
    pub directory: String,
    pub timestamp: i64,
    pub content: String,
    pub message_count: u32,
    pub mtime: i64,
    pub yolo: bool,
    pub source_path: Option<String>,
}

impl Session {
    pub fn key(&self) -> SessionKey {
        SessionKey::new(self.agent, self.id.clone())
    }
}

impl From<&Session> for SessionKey {
    fn from(value: &Session) -> Self {
        value.key()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForkMode {
    Native,
    Translated,
}

impl ForkMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            ForkMode::Native => "native",
            ForkMode::Translated => "translated",
        }
    }
}

impl std::str::FromStr for ForkMode {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "native" => Ok(ForkMode::Native),
            "translated" => Ok(ForkMode::Translated),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ForkLink {
    pub fork_session_id: Option<String>,
    pub parent_session_id: String,
    pub parent_agent: AgentId,
    pub fork_agent: AgentId,
    pub mode: ForkMode,
    pub directory: String,
    pub created: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionPolicy {
    AlwaysNormal,
    AlwaysYolo,
    InheritKnown,
}

impl PermissionPolicy {
    pub const fn as_str(self) -> &'static str {
        match self {
            PermissionPolicy::AlwaysNormal => "always_normal",
            PermissionPolicy::AlwaysYolo => "always_yolo",
            PermissionPolicy::InheritKnown => "inherit_known",
        }
    }

    pub const fn display(self) -> &'static str {
        match self {
            PermissionPolicy::AlwaysNormal => "always normal",
            PermissionPolicy::AlwaysYolo => "always yolo",
            PermissionPolicy::InheritKnown => "inherit known",
        }
    }

    pub fn from_cli_value(s: &str) -> Option<PermissionPolicy> {
        match s {
            "normal" | "always_normal" => Some(PermissionPolicy::AlwaysNormal),
            "yolo" | "always_yolo" => Some(PermissionPolicy::AlwaysYolo),
            "inherit" | "inherit_known" => Some(PermissionPolicy::InheritKnown),
            _ => None,
        }
    }
}

impl std::str::FromStr for PermissionPolicy {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "always_normal" => Ok(PermissionPolicy::AlwaysNormal),
            "always_yolo" => Ok(PermissionPolicy::AlwaysYolo),
            "inherit_known" => Ok(PermissionPolicy::InheritKnown),
            _ => Err(()),
        }
    }
}

pub const USER_PREFIX: &str = "» ";
pub const ASSISTANT_PREFIX: &str = "  ";

pub fn join_content(messages: &[String]) -> String {
    messages.join("\n\n")
}

/// The messages in an indexed `content` string, oldest first, each with
/// whether the user wrote it. A paragraph with neither prefix continues the
/// message before it.
pub fn split_content(content: &str) -> Vec<(bool, String)> {
    let mut out: Vec<(bool, String)> = Vec::new();
    for block in content.split("\n\n") {
        if let Some(text) = block.strip_prefix(USER_PREFIX) {
            out.push((true, text.to_string()));
        } else if let Some(text) = block.strip_prefix(ASSISTANT_PREFIX) {
            out.push((false, text.to_string()));
        } else if let Some((_, last)) = out.last_mut() {
            last.push_str("\n\n");
            last.push_str(block);
        }
    }
    out
}

pub fn truncate_title(text: &str, max_len: usize, word_break: bool) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let text = collapsed.as_str();
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max_len {
        return text.to_string();
    }

    let budget = max_len.saturating_sub(3).max(1);
    let mut cut = budget;
    if word_break {
        if let Some(pos) = chars[..budget].iter().rposition(|c| c.is_whitespace()) {
            if pos > 0 {
                cut = pos;
            }
        }
    }
    let head: String = chars[..cut].iter().collect();
    format!("{}...", head.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_short_title_unchanged() {
        assert_eq!(truncate_title("hello", 100, true), "hello");
        assert_eq!(truncate_title("  hello  ", 100, true), "hello");
    }

    #[test]
    fn truncate_breaks_on_word_boundary() {
        let t = truncate_title("the quick brown fox jumps", 15, true);
        assert!(t.ends_with("..."));

        assert!(!t.contains("brow.") && t.len() <= 15 + 2);
        assert_eq!(t, "the quick...");
    }

    #[test]
    fn truncate_hard_cut_without_word_break() {
        let t = truncate_title("abcdefghijklmnop", 8, false);
        assert_eq!(t, "abcde...");
    }

    #[test]
    fn truncate_is_unicode_safe() {
        let t = truncate_title("ααααα ββββββββββ γγγγγ", 10, true);
        assert!(t.ends_with("..."));
        assert!(t.chars().count() <= 10);
    }

    #[test]
    fn join_content_uses_blank_line() {
        let c = join_content(&["» hi".into(), "  hello".into()]);
        assert_eq!(c, "» hi\n\n  hello");
    }
}

use std::path::PathBuf;

pub const SCHEMA_VERSION: i64 = 8;

pub const APP_DIR: &str = "hopback";
pub const DB_FILE: &str = "hopback.db";

const LEGACY_APP_DIR: &str = "aiz";
const LEGACY_DB_FILE: &str = "aiz.db";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    const fn from_hex(hex: &str) -> Rgb {
        let b = hex.as_bytes();
        assert!(b.len() == 7 && b[0] == b'#', "expected #RRGGBB");
        Rgb(
            hex_byte(b[1], b[2]),
            hex_byte(b[3], b[4]),
            hex_byte(b[5], b[6]),
        )
    }
}

const fn hex_digit(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        b'A'..=b'F' => c - b'A' + 10,
        _ => panic!("invalid hex digit"),
    }
}

const fn hex_byte(hi: u8, lo: u8) -> u8 {
    hex_digit(hi) * 16 + hex_digit(lo)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentId {
    Antigravity,
    Claude,
    Codex,
    CopilotCli,
    CopilotVscode,
    Crush,
    Cursor,
    Gemini,
    Goose,
    Grok,
    Kimi,
    Kiro,
    OpenCode,
    Pi,
    Vibe,
}

impl AgentId {
    pub const ALL: [AgentId; 15] = [
        AgentId::Antigravity,
        AgentId::Claude,
        AgentId::Codex,
        AgentId::CopilotCli,
        AgentId::CopilotVscode,
        AgentId::Crush,
        AgentId::Cursor,
        AgentId::Gemini,
        AgentId::Goose,
        AgentId::Grok,
        AgentId::Kimi,
        AgentId::Kiro,
        AgentId::OpenCode,
        AgentId::Pi,
        AgentId::Vibe,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            AgentId::Antigravity => "antigravity",
            AgentId::Claude => "claude",
            AgentId::Codex => "codex",
            AgentId::CopilotCli => "copilot-cli",
            AgentId::CopilotVscode => "copilot-vscode",
            AgentId::Crush => "crush",
            AgentId::Cursor => "cursor",
            AgentId::Gemini => "gemini",
            AgentId::Goose => "goose",
            AgentId::Grok => "grok",
            AgentId::Kimi => "kimi",
            AgentId::Kiro => "kiro",
            AgentId::OpenCode => "opencode",
            AgentId::Pi => "pi",
            AgentId::Vibe => "vibe",
        }
    }

    pub const fn badge(self) -> &'static str {
        match self {
            AgentId::Antigravity => "agy",
            AgentId::Claude => "claude",
            AgentId::Codex => "codex",
            AgentId::CopilotCli => "copilot",
            AgentId::CopilotVscode => "vscode",
            AgentId::Crush => "crush",
            AgentId::Cursor => "cursor",
            AgentId::Gemini => "gemini",
            AgentId::Goose => "goose",
            AgentId::Grok => "grok",
            AgentId::Kimi => "kimi",
            AgentId::Kiro => "kiro",
            AgentId::OpenCode => "opencode",
            AgentId::Pi => "pi",
            AgentId::Vibe => "vibe",
        }
    }

    pub const fn color(self) -> Rgb {
        match self {
            AgentId::Antigravity => Rgb::from_hex("#4285F4"),
            AgentId::Claude => Rgb::from_hex("#E87B35"),
            AgentId::Codex => Rgb::from_hex("#00A67E"),
            AgentId::CopilotCli => Rgb::from_hex("#9CA3AF"),
            AgentId::CopilotVscode => Rgb::from_hex("#007ACC"),
            AgentId::Crush => Rgb::from_hex("#6B51FF"),
            AgentId::Cursor => Rgb::from_hex("#FFFFFF"),
            AgentId::Gemini => Rgb::from_hex("#C3677F"),
            AgentId::Goose => Rgb::from_hex("#E5C07B"),
            AgentId::Grok => Rgb::from_hex("#B4B8C0"),
            AgentId::Kimi => Rgb::from_hex("#38BDF8"),
            AgentId::Kiro => Rgb::from_hex("#D17BFF"),
            AgentId::OpenCode => Rgb::from_hex("#CFCECD"),
            AgentId::Pi => Rgb::from_hex("#9776FF"),
            AgentId::Vibe => Rgb::from_hex("#FF6B35"),
        }
    }

    pub fn from_id(s: &str) -> Option<AgentId> {
        AgentId::ALL.into_iter().find(|a| a.as_str() == s)
    }

    pub fn resolve(s: &str) -> Option<AgentId> {
        AgentId::ALL
            .into_iter()
            .find(|a| a.as_str() == s || a.badge() == s)
    }
}

fn xdg_base(override_val: Option<&str>, home: &str, default_rel: &str) -> PathBuf {
    match override_val {
        Some(v) if !v.is_empty() && std::path::Path::new(v).is_absolute() => PathBuf::from(v),
        _ => PathBuf::from(home).join(default_rel),
    }
}

pub fn cache_dir_from(xdg_cache_home: Option<&str>, home: &str) -> PathBuf {
    xdg_base(xdg_cache_home, home, ".cache").join(APP_DIR)
}

pub fn config_dir_from(xdg_config_home: Option<&str>, home: &str) -> PathBuf {
    xdg_base(xdg_config_home, home, ".config").join(APP_DIR)
}

fn env_opt(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.is_empty())
}

fn home_dir() -> PathBuf {
    if let Some(h) = env_opt("HOME") {
        return PathBuf::from(h);
    }

    PathBuf::from(".")
}

pub fn cache_dir() -> PathBuf {
    cache_dir_from(
        env_opt("XDG_CACHE_HOME").as_deref(),
        &home_dir().to_string_lossy(),
    )
}

pub fn config_dir() -> PathBuf {
    config_dir_from(
        env_opt("XDG_CONFIG_HOME").as_deref(),
        &home_dir().to_string_lossy(),
    )
}

pub fn db_path() -> PathBuf {
    cache_dir().join(DB_FILE)
}

pub fn legacy_db_path_from(xdg_cache_home: Option<&str>, home: &str) -> PathBuf {
    xdg_base(xdg_cache_home, home, ".cache")
        .join(LEGACY_APP_DIR)
        .join(LEGACY_DB_FILE)
}

pub fn legacy_db_path() -> PathBuf {
    legacy_db_path_from(
        env_opt("XDG_CACHE_HOME").as_deref(),
        &home_dir().to_string_lossy(),
    )
}

pub fn parse_error_log() -> PathBuf {
    cache_dir().join("parse-errors.log")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_ids_round_trip() {
        for a in AgentId::ALL {
            assert_eq!(AgentId::from_id(a.as_str()), Some(a));
        }
        assert_eq!(AgentId::from_id("nope"), None);

        assert_eq!(AgentId::CopilotCli.as_str(), "copilot-cli");
        assert_eq!(AgentId::CopilotVscode.as_str(), "copilot-vscode");
    }

    #[test]
    fn badges_and_colors_match_registry() {
        assert_eq!(AgentId::CopilotCli.badge(), "copilot");
        assert_eq!(AgentId::Antigravity.badge(), "agy");
        assert_eq!(AgentId::resolve("agy"), Some(AgentId::Antigravity));
        assert_eq!(AgentId::resolve("antigravity"), Some(AgentId::Antigravity));
        let mut ids: Vec<&str> = AgentId::ALL.iter().map(|a| a.as_str()).collect();
        let sorted = {
            let mut v = ids.clone();
            v.sort();
            v
        };
        assert_eq!(ids, sorted, "ALL stays in alphabetical order");
        ids.dedup();
        assert_eq!(ids.len(), AgentId::ALL.len());
        assert_eq!(AgentId::CopilotVscode.badge(), "vscode");
        assert_eq!(AgentId::Claude.color(), Rgb(0xE8, 0x7B, 0x35));
        assert_eq!(AgentId::Codex.color(), Rgb(0x00, 0xA6, 0x7E));
        assert_eq!(AgentId::Crush.color(), Rgb(0x6B, 0x51, 0xFF));
    }

    #[test]
    fn xdg_override_used_when_absolute() {
        let p = cache_dir_from(Some("/custom/cache"), "/home/u");
        assert_eq!(p, PathBuf::from("/custom/cache/hopback"));
        let c = config_dir_from(Some("/custom/cfg"), "/home/u");
        assert_eq!(c, PathBuf::from("/custom/cfg/hopback"));
    }

    #[test]
    fn falls_back_to_home_without_xdg() {
        let p = cache_dir_from(None, "/home/u");
        assert_eq!(p, PathBuf::from("/home/u/.cache/hopback"));
        let c = config_dir_from(None, "/home/u");
        assert_eq!(c, PathBuf::from("/home/u/.config/hopback"));
    }

    #[test]
    fn legacy_db_sits_next_to_the_new_cache_dir() {
        assert_eq!(
            legacy_db_path_from(None, "/home/u"),
            PathBuf::from("/home/u/.cache/aiz/aiz.db")
        );
        assert_eq!(
            legacy_db_path_from(Some("/custom/cache"), "/home/u"),
            PathBuf::from("/custom/cache/aiz/aiz.db")
        );
    }

    #[test]
    fn relative_xdg_value_is_ignored() {
        let p = cache_dir_from(Some("relative/path"), "/home/u");
        assert_eq!(p, PathBuf::from("/home/u/.cache/hopback"));
        let e = cache_dir_from(Some(""), "/home/u");
        assert_eq!(e, PathBuf::from("/home/u/.cache/hopback"));
    }
}

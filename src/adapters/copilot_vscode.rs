use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::Value;

use super::{mtime_nanos, nanos_to_secs, Adapter};
use crate::config::AgentId;
use crate::model::{join_content, truncate_title, Session, ASSISTANT_PREFIX, USER_PREFIX};

const TITLE_MAX: usize = 100;

pub struct CopilotVscodeAdapter {
    workspace_storage: PathBuf,

    empty_window_dir: PathBuf,
}

impl CopilotVscodeAdapter {
    pub fn new() -> CopilotVscodeAdapter {
        let base = vscode_user_dir();
        CopilotVscodeAdapter {
            workspace_storage: base.join("workspaceStorage"),
            empty_window_dir: base.join("globalStorage").join("emptyWindowChatSessions"),
        }
    }

    pub fn with_paths(
        workspace_storage: PathBuf,
        empty_window_dir: PathBuf,
    ) -> CopilotVscodeAdapter {
        CopilotVscodeAdapter {
            workspace_storage,
            empty_window_dir,
        }
    }

    fn session_files(&self) -> Vec<(PathBuf, String)> {
        let mut out = Vec::new();

        if let Ok(hashes) = std::fs::read_dir(&self.workspace_storage) {
            for h in hashes.flatten() {
                let hash_dir = h.path();
                if !hash_dir.is_dir() {
                    continue;
                }
                let chat_dir = hash_dir.join("chatSessions");
                if !chat_dir.is_dir() {
                    continue;
                }
                let dir = workspace_dir(&hash_dir.join("workspace.json"));
                if let Ok(files) = std::fs::read_dir(&chat_dir) {
                    for f in files.flatten() {
                        let p = f.path();
                        if p.extension().and_then(|x| x.to_str()) == Some("json") {
                            out.push((p, dir.clone()));
                        }
                    }
                }
            }
        }

        if let Ok(files) = std::fs::read_dir(&self.empty_window_dir) {
            for f in files.flatten() {
                let p = f.path();
                if p.extension().and_then(|x| x.to_str()) == Some("json") {
                    out.push((p, String::new()));
                }
            }
        }

        out
    }
}

impl Default for CopilotVscodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

pub fn vscode_user_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home)
        .join("Library")
        .join("Application Support")
        .join("Code")
        .join("User")
}

fn workspace_dir(workspace_json: &Path) -> String {
    let Ok(bytes) = std::fs::read(workspace_json) else {
        return String::new();
    };
    let Ok(json) = serde_json::from_slice::<Value>(&bytes) else {
        return String::new();
    };
    let Some(folder) = json.get("folder").and_then(|v| v.as_str()) else {
        return String::new();
    };
    decode_file_uri(folder)
}

fn decode_file_uri(uri: &str) -> String {
    let Some(rest) = uri.strip_prefix("file://") else {
        return String::new();
    };

    let path = match rest.find('/') {
        Some(i) => &rest[i..],
        None => rest,
    };
    let decoded = percent_decode(path);

    if decoded.len() >= 3
        && decoded.as_bytes()[0] == b'/'
        && decoded.as_bytes()[2] == b':'
        && decoded.as_bytes()[1].is_ascii_alphabetic()
    {
        decoded[1..].to_string()
    } else {
        decoded
    }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn parse_session(
    bytes: &[u8],
    id_fallback: &str,
    directory: &str,
    mtime_ns: i64,
) -> Option<Session> {
    let json: Value = serde_json::from_slice(bytes).ok()?;

    let session_id = json
        .get("sessionId")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or(id_fallback)
        .to_string();

    let requests = json.get("requests").and_then(|v| v.as_array());
    let requests = match requests {
        Some(r) if !r.is_empty() => r,
        _ => return None,
    };

    let mut messages: Vec<String> = Vec::new();
    let mut first_user = String::new();
    let mut turn_count: u32 = 0;

    for req in requests {
        if let Some(text) = req
            .get("message")
            .and_then(|m| m.get("text"))
            .and_then(|v| v.as_str())
        {
            if !text.is_empty() {
                messages.push(format!("{USER_PREFIX}{text}"));
                turn_count += 1;
                if first_user.is_empty() {
                    first_user = text.to_string();
                }
            }
        }

        if let Some(resp) = req.get("response").and_then(|v| v.as_array()) {
            let mut chunks = Vec::new();
            for part in resp {
                if let Some(v) = part.get("value").and_then(|v| v.as_str()) {
                    if !v.is_empty() {
                        chunks.push(v);
                    }
                }
            }
            if !chunks.is_empty() {
                messages.push(format!("{ASSISTANT_PREFIX}{}", chunks.join("")));
                turn_count += 1;
            }
        }
    }

    if messages.is_empty() {
        return None;
    }

    let custom = json
        .get("customTitle")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty());
    let title_source = custom.unwrap_or(&first_user);

    let timestamp = json
        .get("lastMessageDate")
        .and_then(|v| v.as_i64())
        .filter(|&v| v > 0)
        .or_else(|| {
            json.get("creationDate")
                .and_then(|v| v.as_i64())
                .filter(|&v| v > 0)
        })
        .map(|ms| ms / 1000)
        .unwrap_or_else(|| nanos_to_secs(mtime_ns));

    Some(Session {
        id: session_id,
        agent: AgentId::CopilotVscode,
        title: truncate_title(title_source, TITLE_MAX, true),
        directory: directory.to_string(),
        timestamp,
        content: join_content(&messages),
        message_count: turn_count,
        mtime: mtime_ns,
        yolo: false,
        source_path: None,
    })
}

impl Adapter for CopilotVscodeAdapter {
    fn id(&self) -> AgentId {
        AgentId::CopilotVscode
    }

    fn is_available(&self) -> bool {
        self.workspace_storage.is_dir() || self.empty_window_dir.is_dir()
    }

    fn scan(&self) -> Result<HashMap<String, (PathBuf, i64)>> {
        let mut out = HashMap::new();
        for (path, _dir) in self.session_files() {
            let Ok(mtime) = mtime_nanos(&path) else {
                continue;
            };
            let id = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            if !id.is_empty() {
                out.insert(id, (path, mtime));
            }
        }
        Ok(out)
    }

    fn parse(&self, path: &Path) -> Result<Option<Session>> {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(_) => return Ok(None),
        };
        let mtime = mtime_nanos(path).unwrap_or(0);
        let id_fallback = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");

        let directory = path
            .parent()
            .and_then(|chat| chat.parent())
            .map(|hash| workspace_dir(&hash.join("workspace.json")))
            .unwrap_or_default();
        let mut session = parse_session(&bytes, id_fallback, &directory, mtime);
        if let Some(s) = &mut session {
            s.source_path = Some(path.to_string_lossy().to_string());
        }
        Ok(session)
    }

    fn resume_command(&self, session: &Session, _yolo: bool) -> Vec<String> {
        if session.directory.is_empty() {
            vec!["code".to_string()]
        } else {
            vec!["code".to_string(), session.directory.clone()]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SESSION: &str = r#"{
        "version": 3,
        "sessionId": "vsc-1",
        "creationDate": 1700000000000,
        "lastMessageDate": 1700000500000,
        "requests": [
            {"message": {"text": "fix the build error"},
             "response": [{"value": "Looking at "}, {"value": "the logs now."}]},
            {"message": {"text": "thanks"}, "response": []}
        ]
    }"#;

    #[test]
    fn parses_requests_into_turns() {
        let s = parse_session(SESSION.as_bytes(), "fallback", "/work/app", 9_000_000_000).unwrap();
        assert_eq!(s.id, "vsc-1");
        assert_eq!(s.agent, AgentId::CopilotVscode);
        assert_eq!(s.directory, "/work/app");
        assert_eq!(s.title, "fix the build error");

        assert_eq!(s.message_count, 3);
        assert!(s.content.contains("» fix the build error"));
        assert!(s.content.contains("  Looking at the logs now."));
        assert!(s.content.contains("» thanks"));
        assert_eq!(s.timestamp, 1_700_000_500);
        assert!(!s.yolo);
    }

    #[test]
    fn custom_title_wins() {
        let f = r#"{"sessionId":"x","customTitle":"My Chat","creationDate":1700000000000,
                    "requests":[{"message":{"text":"hello there friend"}}]}"#;
        let s = parse_session(f.as_bytes(), "fb", "", 0).unwrap();
        assert_eq!(s.title, "My Chat");
    }

    #[test]
    fn empty_requests_returns_none() {
        let f = r#"{"sessionId":"x","requests":[]}"#;
        assert!(parse_session(f.as_bytes(), "fb", "", 0).is_none());
    }

    #[test]
    fn timestamp_falls_back_to_creation_then_mtime() {
        let f = r#"{"sessionId":"x","creationDate":1700000000000,
                    "requests":[{"message":{"text":"a question here"}}]}"#;
        assert_eq!(
            parse_session(f.as_bytes(), "fb", "", 0).unwrap().timestamp,
            1_700_000_000
        );

        let g = r#"{"sessionId":"x","requests":[{"message":{"text":"a question here"}}]}"#;
        assert_eq!(
            parse_session(g.as_bytes(), "fb", "", 7_000_000_000)
                .unwrap()
                .timestamp,
            7
        );
    }

    #[test]
    fn decode_file_uri_handles_paths() {
        assert_eq!(decode_file_uri("file:///Users/u/proj"), "/Users/u/proj");
        assert_eq!(
            decode_file_uri("file:///Users/u/my%20proj"),
            "/Users/u/my proj"
        );
        assert_eq!(decode_file_uri("file:///C:/code/app"), "C:/code/app");
        assert_eq!(decode_file_uri("not-a-uri"), "");
    }

    #[test]
    fn resume_opens_vscode_in_directory() {
        let a = CopilotVscodeAdapter::with_paths(PathBuf::from("/nope"), PathBuf::from("/nope2"));
        let s = parse_session(SESSION.as_bytes(), "fb", "/work/app", 0).unwrap();
        assert_eq!(a.resume_command(&s, false), vec!["code", "/work/app"]);

        let empty = parse_session(SESSION.as_bytes(), "fb", "", 0).unwrap();
        assert_eq!(a.resume_command(&empty, false), vec!["code"]);
        assert!(!a.supports_fork());
        assert!(!a.supports_yolo());
    }
}

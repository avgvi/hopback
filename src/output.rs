// The `--json` output: an envelope (`schema_version`, `sessions`, `meta`)
// with paging states around the sessions. docs/json.md is the contract.

use std::io::{self, Write};

use anyhow::Result;
use serde::Serialize;

use crate::launcher::{self, YoloDecision};
use crate::model::{PermissionPolicy, SessionKey};
use crate::store::sessions::SessionRecord;

pub const JSON_SCHEMA_VERSION: u32 = 1;
pub const DEFAULT_JSON_LIMIT: usize = 50;

#[derive(Debug, Serialize, PartialEq)]
pub struct SessionRef {
    pub agent: &'static str,
    pub id: String,
}

impl From<&SessionKey> for SessionRef {
    fn from(key: &SessionKey) -> Self {
        SessionRef {
            agent: key.agent.as_str(),
            id: key.id.clone(),
        }
    }
}

#[derive(Debug, Serialize, PartialEq)]
pub struct SessionOutput {
    pub id: String,
    pub agent: &'static str,
    pub title: String,
    pub original_title: String,
    pub directory: String,
    pub timestamp: String,
    pub message_count: u32,
    pub pinned: bool,
    pub hidden: bool,
    pub forked_from: Option<SessionRef>,
    pub resume_command: Vec<String>,
    /// Only with `--include-messages`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub messages: Option<Vec<MessageOutput>>,
}

/// One prompt or reply of a session, as hopback indexed it.
#[derive(Debug, Serialize, PartialEq)]
pub struct MessageOutput {
    pub role: &'static str,
    pub text: String,
}

/// How `resume_command` is built: the same decision the launcher makes.
/// `--yolo` and an `always_yolo` policy add the agent's skip-permission
/// flags, `always_normal` leaves them out, and otherwise a session keeps the
/// mode it was started with, where its agent records that.
#[derive(Debug, Clone, Copy)]
pub struct ResumeContext {
    pub flag_yolo: bool,
    pub policy: Option<PermissionPolicy>,
}

impl ResumeContext {
    pub fn yolo_for(&self, record: &SessionRecord) -> bool {
        matches!(
            launcher::launch_permission_decision(&record.session, self.flag_yolo, self.policy),
            YoloDecision::On
        )
    }
}

impl SessionOutput {
    pub fn new(record: &SessionRecord, ctx: ResumeContext) -> SessionOutput {
        let s = &record.session;
        SessionOutput {
            id: s.id.clone(),
            agent: s.agent.as_str(),
            title: s.title.clone(),
            original_title: record.original_title.clone(),
            directory: s.directory.clone(),
            timestamp: rfc3339_utc(s.timestamp),
            message_count: s.message_count,
            pinned: record.pinned,
            hidden: record.hidden,
            forked_from: record.forked_from.as_ref().map(SessionRef::from),
            resume_command: launcher::resume_argv(s, ctx.yolo_for(record)).unwrap_or_default(),
            messages: None,
        }
    }

    /// The same session with its conversation text.
    pub fn with_messages(mut self, record: &SessionRecord) -> SessionOutput {
        let messages = crate::model::split_content(&record.session.content)
            .into_iter()
            .map(|(from_user, text)| MessageOutput {
                role: if from_user { "user" } else { "assistant" },
                text,
            })
            .collect();
        self.messages = Some(messages);
        self
    }
}

#[derive(Debug, Serialize, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum PageState {
    More,
    Complete,
    PastEnd,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct PageMeta {
    pub state: PageState,
    pub total: usize,
    pub offset: usize,
    pub limit: usize,
    pub returned: usize,
    pub next_offset: Option<usize>,
}

impl PageMeta {
    pub fn new(total: usize, offset: usize, limit: usize, returned: usize) -> PageMeta {
        let end = offset.saturating_add(returned);
        let has_more = end < total;
        let state = if total > 0 && offset >= total {
            PageState::PastEnd
        } else if has_more {
            PageState::More
        } else {
            PageState::Complete
        };
        PageMeta {
            state,
            total,
            offset,
            limit,
            returned,
            next_offset: has_more.then_some(end),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct SessionListOutput {
    pub schema_version: u32,
    pub sessions: Vec<SessionOutput>,
    pub meta: PageMeta,
}

/// What the `pin`, `unpin`, `rename`, `hide` and `unhide` commands print with
/// `--json`: the session as it is after the change.
#[derive(Debug, Serialize)]
pub struct SessionChangeOutput {
    pub schema_version: u32,
    pub session: SessionOutput,
}

impl SessionChangeOutput {
    pub fn new(record: &SessionRecord, ctx: ResumeContext) -> SessionChangeOutput {
        SessionChangeOutput {
            schema_version: JSON_SCHEMA_VERSION,
            session: SessionOutput::new(record, ctx),
        }
    }
}

/// What `fork --json` prints.
#[derive(Debug, Serialize)]
pub struct ForkOutput {
    pub schema_version: u32,
    /// `translated` or `native`.
    pub mode: &'static str,
    pub dry_run: bool,
    /// True when the new session's files exist now. A native fork is created
    /// by the agent when `launch_command` runs, so it is false there.
    pub created: bool,
    /// The new session, or `null` when its id is only known once the agent
    /// has created it.
    pub session: Option<SessionOutput>,
    /// The argv that opens the fork, to be run in the session's directory.
    pub launch_command: Vec<String>,
    pub warnings: Vec<String>,
}

/// The slice of `total` matches that a page covers.
pub fn page_bounds(total: usize, offset: usize, limit: usize) -> std::ops::Range<usize> {
    let start = offset.min(total);
    start..start.saturating_add(limit).min(total)
}

pub fn session_list(
    matches: &[SessionRecord],
    offset: usize,
    limit: usize,
    ctx: ResumeContext,
    include_messages: bool,
) -> SessionListOutput {
    let total = matches.len();
    let page = &matches[page_bounds(total, offset, limit)];
    let session = |r: &SessionRecord| {
        let out = SessionOutput::new(r, ctx);
        if include_messages {
            out.with_messages(r)
        } else {
            out
        }
    };
    SessionListOutput {
        schema_version: JSON_SCHEMA_VERSION,
        sessions: page.iter().map(session).collect(),
        meta: PageMeta::new(total, offset, limit, page.len()),
    }
}

/// Writes exactly one JSON value and a newline to stdout.
pub fn print_json<T: Serialize>(value: &T) -> Result<()> {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    serde_json::to_writer(&mut out, value)?;
    writeln!(out)?;
    Ok(())
}

/// Unix seconds as an RFC 3339 timestamp in UTC, e.g. `2026-11-11T20:00:08Z`.
pub fn rfc3339_utc(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::parse_iso8601_secs;
    use crate::config::AgentId;
    use crate::model::Session;

    fn record(id: &str, agent: AgentId, yolo: bool) -> SessionRecord {
        SessionRecord {
            session: Session {
                id: id.into(),
                agent,
                title: format!("shown {id}"),
                directory: "/work".into(),
                timestamp: 1_700_000_000,
                content: "» secret conversation text\n\n  a reply\n\nin two paragraphs".into(),
                message_count: 3,
                mtime: 0,
                yolo,
                source_path: Some("/files/x.jsonl".into()),
            },
            original_title: format!("parsed {id}"),
            pinned: false,
            hidden: false,
            forked_from: None,
        }
    }

    const NO_YOLO: ResumeContext = ResumeContext {
        flag_yolo: false,
        policy: None,
    };

    #[test]
    fn rfc3339_round_trips_through_the_iso_parser() {
        assert_eq!(rfc3339_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339_utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(rfc3339_utc(-1), "1969-12-31T23:59:59Z");
        for secs in [1_700_000_000, 1_794_513_608, 4_102_444_799, 86_399] {
            assert_eq!(parse_iso8601_secs(&rfc3339_utc(secs)), Some(secs));
        }
    }

    #[test]
    fn page_meta_states() {
        assert_eq!(
            PageMeta::new(24, 0, 10, 10),
            PageMeta {
                state: PageState::More,
                total: 24,
                offset: 0,
                limit: 10,
                returned: 10,
                next_offset: Some(10)
            }
        );
        let last = PageMeta::new(24, 20, 10, 4);
        assert_eq!(last.state, PageState::Complete);
        assert_eq!(last.next_offset, None);
        assert_eq!(PageMeta::new(24, 24, 10, 0).state, PageState::PastEnd);
        assert_eq!(PageMeta::new(24, 99, 10, 0).state, PageState::PastEnd);
        assert_eq!(PageMeta::new(0, 0, 10, 0).state, PageState::Complete);
        assert_eq!(PageMeta::new(0, 5, 10, 0).state, PageState::Complete);
    }

    #[test]
    fn page_bounds_never_run_past_the_matches() {
        assert_eq!(page_bounds(5, 0, 2), 0..2);
        assert_eq!(page_bounds(5, 4, 2), 4..5);
        assert_eq!(page_bounds(5, 9, 2), 5..5);
        assert_eq!(page_bounds(5, 0, usize::MAX), 0..5);
    }

    #[test]
    fn session_object_has_the_documented_fields_and_no_content() {
        let mut r = record("abc", AgentId::Codex, false);
        r.pinned = true;
        r.forked_from = Some(SessionKey::new(AgentId::Claude, "parent"));
        let with_text =
            serde_json::to_value(session_list(std::slice::from_ref(&r), 0, 50, NO_YOLO, true))
                .unwrap();
        assert_eq!(
            with_text["sessions"][0]["messages"],
            serde_json::json!([
                {"role": "user", "text": "secret conversation text"},
                {"role": "assistant", "text": "a reply\n\nin two paragraphs"}
            ])
        );
        let json = serde_json::to_value(session_list(&[r], 0, 50, NO_YOLO, false)).unwrap();

        assert_eq!(json["schema_version"], 1);
        let s = &json["sessions"][0];
        let mut keys: Vec<&str> = s.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                "agent",
                "directory",
                "forked_from",
                "hidden",
                "id",
                "message_count",
                "original_title",
                "pinned",
                "resume_command",
                "timestamp",
                "title"
            ]
        );
        assert_eq!(s["title"], "shown abc");
        assert_eq!(s["original_title"], "parsed abc");
        assert_eq!(s["timestamp"], "2023-11-14T22:13:20Z");
        assert_eq!(s["pinned"], true);
        assert_eq!(s["hidden"], false);
        assert_eq!(
            s["forked_from"],
            serde_json::json!({"agent": "claude", "id": "parent"})
        );
        assert_eq!(
            s["resume_command"],
            serde_json::json!(["codex", "resume", "abc"])
        );
        assert!(!json.to_string().contains("secret conversation text"));
    }

    #[test]
    fn forked_from_is_null_for_ordinary_sessions() {
        let json = serde_json::to_value(session_list(
            &[record("a", AgentId::Claude, false)],
            0,
            1,
            NO_YOLO,
            false,
        ))
        .unwrap();
        assert!(json["sessions"][0]["forked_from"].is_null());
        assert_eq!(json["meta"]["state"], "complete");
        assert!(json["meta"]["next_offset"].is_null());
    }

    #[test]
    fn resume_command_carries_yolo_flags_when_hopback_would_use_them() {
        let codex = record("z9", AgentId::Codex, true);
        let flagged = ResumeContext {
            flag_yolo: true,
            policy: None,
        };
        assert_eq!(
            SessionOutput::new(&codex, flagged).resume_command,
            vec![
                "codex",
                "--dangerously-bypass-approvals-and-sandbox",
                "resume",
                "z9"
            ]
        );

        let always = ResumeContext {
            flag_yolo: false,
            policy: Some(PermissionPolicy::AlwaysYolo),
        };
        assert_eq!(
            SessionOutput::new(&record("c", AgentId::Claude, false), always).resume_command,
            vec!["claude", "--dangerously-skip-permissions", "--resume", "c"]
        );

        let inherit = ResumeContext {
            flag_yolo: false,
            policy: Some(PermissionPolicy::InheritKnown),
        };
        assert!(SessionOutput::new(&codex, inherit)
            .resume_command
            .contains(&"--dangerously-bypass-approvals-and-sandbox".to_string()));
        assert_eq!(
            SessionOutput::new(&record("n", AgentId::Codex, false), inherit).resume_command,
            vec!["codex", "resume", "n"]
        );

        // No policy stored: the session keeps the mode it was started with.
        assert!(SessionOutput::new(&codex, NO_YOLO)
            .resume_command
            .contains(&"--dangerously-bypass-approvals-and-sandbox".to_string()));
        let normal = ResumeContext {
            flag_yolo: false,
            policy: Some(PermissionPolicy::AlwaysNormal),
        };
        assert_eq!(
            SessionOutput::new(&codex, normal).resume_command,
            vec!["codex", "resume", "z9"]
        );
    }
}

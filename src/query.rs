use crate::config::AgentId;
use crate::search::Filters;

const DAY_SECS: i64 = 86_400;

#[derive(Debug, Clone, Default)]
pub struct ParsedQuery {
    pub text: String,
    pub filters: Filters,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keyword {
    Agent,
    Dir,
    Date,
}

impl Keyword {
    fn from_str(s: &str) -> Option<Keyword> {
        match s {
            "agent" => Some(Keyword::Agent),
            "dir" => Some(Keyword::Dir),
            "date" => Some(Keyword::Date),
            _ => None,
        }
    }
}

enum Token<'a> {
    Keyword(bool, Keyword, &'a str),

    Text(&'a str),
}

pub fn parse_query(raw: &str, now_secs: i64) -> ParsedQuery {
    let mut filters = Filters::default();
    let mut text_words: Vec<&str> = Vec::new();

    for tok in tokenize(raw) {
        match tok {
            Token::Text(w) => text_words.push(w),
            Token::Keyword(negated, kw, value) => match kw {
                Keyword::Agent => {
                    let (inc, exc) = split_values(value, negated);

                    filters.agents_include =
                        inc.iter().filter_map(|v| AgentId::resolve(v)).collect();
                    filters.agents_exclude =
                        exc.iter().filter_map(|v| AgentId::resolve(v)).collect();

                    filters.agents_include_unmatched =
                        !inc.is_empty() && filters.agents_include.is_empty();
                }
                Keyword::Dir => {
                    let (inc, exc) = split_values(value, negated);
                    filters.dirs_include = inc;
                    filters.dirs_exclude = exc;
                }
                Keyword::Date => {
                    filters.date_after = None;
                    filters.date_before = None;
                    filters.date_negated = false;
                    apply_date(&mut filters, value, negated, now_secs);
                }
            },
        }
    }

    ParsedQuery {
        text: text_words.join(" "),
        filters,
    }
}

fn tokenize(raw: &str) -> Vec<Token<'_>> {
    let bytes = raw.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    let n = bytes.len();

    while i < n {
        if bytes[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let start = i;

        let neg = bytes[i] == b'-';
        let kw_start = if neg { i + 1 } else { i };
        let mut j = kw_start;
        while j < n && bytes[j].is_ascii_alphabetic() {
            j += 1;
        }
        if j < n && bytes[j] == b':' && j > kw_start {
            if let Some(kw) = Keyword::from_str(&raw[kw_start..j]) {
                let vstart = j + 1;
                if vstart < n && bytes[vstart] == b'"' {
                    let inner = vstart + 1;
                    let mut k = inner;
                    while k < n && bytes[k] != b'"' {
                        k += 1;
                    }
                    let value = &raw[inner..k];
                    out.push(Token::Keyword(neg, kw, value));
                    i = if k < n { k + 1 } else { k };
                    continue;
                } else {
                    let mut k = vstart;
                    while k < n && !bytes[k].is_ascii_whitespace() {
                        k += 1;
                    }
                    let value = &raw[vstart..k];
                    out.push(Token::Keyword(neg, kw, value));
                    i = k;
                    continue;
                }
            }
        }

        let mut k = start;
        while k < n && !bytes[k].is_ascii_whitespace() {
            k += 1;
        }
        out.push(Token::Text(&raw[start..k]));
        i = k;
    }

    out
}

fn split_values(value: &str, keyword_negated: bool) -> (Vec<String>, Vec<String>) {
    let mut include = Vec::new();
    let mut exclude = Vec::new();
    for raw in value.split(',') {
        let v = raw.trim();
        if v.is_empty() {
            continue;
        }
        if let Some(stripped) = v.strip_prefix('!') {
            if !stripped.is_empty() {
                exclude.push(stripped.to_string());
            }
        } else if keyword_negated {
            exclude.push(v.to_string());
        } else {
            include.push(v.to_string());
        }
    }
    (include, exclude)
}

fn apply_date(filters: &mut Filters, value: &str, keyword_negated: bool, now_secs: i64) {
    let mut negated = keyword_negated;
    let mut v = value;
    if let Some(stripped) = v.strip_prefix('!') {
        v = stripped;
        negated = true;
    }
    let lower = v.to_ascii_lowercase();

    let today_midnight = now_secs - now_secs.rem_euclid(DAY_SECS);

    let (after, before) = match lower.as_str() {
        "today" => (Some(today_midnight), None),
        "yesterday" => (Some(today_midnight - DAY_SECS), Some(today_midnight)),
        "week" => (Some(now_secs - 7 * DAY_SECS), None),
        "month" => (Some(now_secs - 30 * DAY_SECS), None),
        other => match parse_relative(other) {
            Some((Op::GreaterThan, secs)) => (None, Some(now_secs - secs)),

            Some((Op::LessThan, secs)) => (Some(now_secs - secs), None),
            None => return,
        },
    };

    filters.date_after = after;
    filters.date_before = before;
    filters.date_negated = negated;
}

enum Op {
    LessThan,
    GreaterThan,
}

fn parse_relative(s: &str) -> Option<(Op, i64)> {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let mut i = 0;
    let op = match bytes[0] {
        b'<' => {
            i = 1;
            Op::LessThan
        }
        b'>' => {
            i = 1;
            Op::GreaterThan
        }
        _ => Op::LessThan,
    };

    let dstart = i;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i == dstart {
        return None;
    }
    let num: i64 = s[dstart..i].parse().ok()?;

    let unit_secs = match &s[i..] {
        "m" => 60,
        "h" => 3_600,
        "d" => 86_400,
        "w" => 604_800,
        "mo" => 2_592_000,
        "y" => 31_536_000,
        _ => return None,
    };
    Some((op, num.saturating_mul(unit_secs)))
}

pub fn keyword_token_at_end(raw: &str) -> Option<(Keyword, &str)> {
    let last = raw.rsplit(|c: char| c.is_whitespace()).next()?;
    let neg = last.strip_prefix('-').unwrap_or(last);
    let (kw, val) = neg.split_once(':')?;
    let keyword = Keyword::from_str(kw)?;

    let current = val.rsplit(',').next().unwrap_or(val);
    let current = current.strip_prefix('!').unwrap_or(current);
    Some((keyword, current))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed_now() -> i64 {
        crate::adapters::parse_iso8601_secs("2026-06-12T12:00:00Z").unwrap()
    }

    fn parse(raw: &str) -> ParsedQuery {
        parse_query(raw, fixed_now())
    }

    #[test]
    fn no_keywords_is_all_free_text() {
        let p = parse("api auth bug");
        assert_eq!(p.text, "api auth bug");
        assert!(p.filters.agents_include.is_empty());
        assert!(p.filters.dirs_include.is_empty());
        assert!(p.filters.date_after.is_none() && p.filters.date_before.is_none());
    }

    #[test]
    fn agent_keyword_extracted() {
        let p = parse("agent:claude api auth");
        assert_eq!(p.text, "api auth");
        assert_eq!(p.filters.agents_include, vec![AgentId::Claude]);
    }

    #[test]
    fn dir_keyword_extracted() {
        let p = parse("dir:my-project bug fix");
        assert_eq!(p.text, "bug fix");
        assert_eq!(p.filters.dirs_include, vec!["my-project".to_string()]);
    }

    #[test]
    fn both_keywords() {
        let p = parse("agent:claude dir:my-project auth");
        assert_eq!(p.text, "auth");
        assert_eq!(p.filters.agents_include, vec![AgentId::Claude]);
        assert_eq!(p.filters.dirs_include, vec!["my-project".to_string()]);
    }

    #[test]
    fn keywords_at_end_and_middle() {
        assert_eq!(parse("auth bug agent:codex").text, "auth bug");
        assert_eq!(
            parse("auth bug agent:codex").filters.agents_include,
            vec![AgentId::Codex]
        );
        let mid = parse("api agent:claude auth");
        assert_eq!(mid.text, "api auth");
        assert_eq!(mid.filters.agents_include, vec![AgentId::Claude]);
    }

    #[test]
    fn empty_and_only_keywords() {
        assert_eq!(parse("").text, "");
        let p = parse("agent:claude dir:project");
        assert_eq!(p.text, "");
        assert_eq!(p.filters.agents_include, vec![AgentId::Claude]);
        assert_eq!(p.filters.dirs_include, vec!["project".to_string()]);
    }

    #[test]
    fn quoted_value_keeps_spaces() {
        let p = parse("dir:\"my project\" bug");
        assert_eq!(p.text, "bug");
        assert_eq!(p.filters.dirs_include, vec!["my project".to_string()]);
    }

    #[test]
    fn duplicate_keyword_last_wins() {
        let p = parse("agent:claude agent:codex api");
        assert_eq!(p.text, "api");
        assert_eq!(p.filters.agents_include, vec![AgentId::Codex]);
    }

    #[test]
    fn whitespace_is_collapsed() {
        let p = parse("  agent:claude   api   auth  ");
        assert_eq!(p.text, "api auth");
        assert_eq!(p.filters.agents_include, vec![AgentId::Claude]);
    }

    #[test]
    fn dir_case_is_preserved() {
        assert_eq!(
            parse("dir:MyProject").filters.dirs_include,
            vec!["MyProject".to_string()]
        );
    }

    #[test]
    fn hyphenated_agent_value() {
        let p = parse("agent:copilot-cli api");
        assert_eq!(p.text, "api");
        assert_eq!(p.filters.agents_include, vec![AgentId::CopilotCli]);
    }

    #[test]
    fn dir_path_with_slashes() {
        let p = parse("dir:/home/user/project api");
        assert_eq!(p.text, "api");
        assert_eq!(
            p.filters.dirs_include,
            vec!["/home/user/project".to_string()]
        );
    }

    #[test]
    fn unknown_colon_word_stays_free_text() {
        let p = parse("error: something failed agent:claude");
        assert_eq!(p.text, "error: something failed");
        assert_eq!(p.filters.agents_include, vec![AgentId::Claude]);
    }

    #[test]
    fn unknown_agent_value_is_dropped() {
        let p = parse("agent:notanagent api");
        assert_eq!(p.text, "api");
        assert!(p.filters.agents_include.is_empty());
        assert!(p.filters.agents_include_unmatched);
    }

    #[test]
    fn agent_resolves_by_badge_or_id() {
        assert_eq!(
            parse("agent:copilot").filters.agents_include,
            vec![AgentId::CopilotCli]
        );
        assert_eq!(
            parse("agent:copilot-cli").filters.agents_include,
            vec![AgentId::CopilotCli]
        );

        assert_eq!(
            parse("agent:vscode").filters.agents_include,
            vec![AgentId::CopilotVscode]
        );
    }

    #[test]
    fn agent_multiple_values_or() {
        let p = parse("agent:claude,codex api");
        assert_eq!(
            p.filters.agents_include,
            vec![AgentId::Claude, AgentId::Codex]
        );
    }

    #[test]
    fn three_values() {
        let p = parse("agent:claude,codex,vibe");
        assert_eq!(
            p.filters.agents_include,
            vec![AgentId::Claude, AgentId::Codex, AgentId::Vibe]
        );
    }

    #[test]
    fn negation_dash_prefix() {
        let p = parse("-agent:claude api");
        assert_eq!(p.text, "api");
        assert_eq!(p.filters.agents_exclude, vec![AgentId::Claude]);
        assert!(p.filters.agents_include.is_empty());
    }

    #[test]
    fn negation_bang_prefix() {
        let p = parse("agent:!claude api");
        assert_eq!(p.filters.agents_exclude, vec![AgentId::Claude]);
        assert!(p.filters.agents_include.is_empty());
    }

    #[test]
    fn negation_dir() {
        let p = parse("-dir:project api");
        assert_eq!(p.filters.dirs_exclude, vec!["project".to_string()]);
        assert!(p.filters.dirs_include.is_empty());
    }

    #[test]
    fn dash_prefix_excludes_all_values() {
        let p = parse("-agent:claude,codex");
        assert_eq!(
            p.filters.agents_exclude,
            vec![AgentId::Claude, AgentId::Codex]
        );
        assert!(p.filters.agents_include.is_empty());
    }

    #[test]
    fn mixed_include_exclude() {
        let p = parse("agent:claude,!codex api");
        assert_eq!(p.text, "api");
        assert_eq!(p.filters.agents_include, vec![AgentId::Claude]);
        assert_eq!(p.filters.agents_exclude, vec![AgentId::Codex]);
    }

    #[test]
    fn mixed_two_include_one_exclude() {
        let p = parse("agent:claude,codex,!vibe");
        assert_eq!(
            p.filters.agents_include,
            vec![AgentId::Claude, AgentId::Codex]
        );
        assert_eq!(p.filters.agents_exclude, vec![AgentId::Vibe]);
    }

    #[test]
    fn date_today_sets_after_to_utc_midnight() {
        let p = parse("date:today api");
        assert_eq!(p.text, "api");
        let now = fixed_now();
        let midnight = now - now.rem_euclid(DAY_SECS);
        assert_eq!(p.filters.date_after, Some(midnight));
        assert_eq!(p.filters.date_before, None);
        assert!(!p.filters.date_negated);
    }

    #[test]
    fn date_yesterday_is_a_one_day_window() {
        let p = parse("date:yesterday");
        let now = fixed_now();
        let midnight = now - now.rem_euclid(DAY_SECS);
        assert_eq!(p.filters.date_after, Some(midnight - DAY_SECS));
        assert_eq!(p.filters.date_before, Some(midnight));
    }

    #[test]
    fn date_less_than_keeps_recent() {
        let p = parse("date:<1h api");
        assert_eq!(p.text, "api");
        assert_eq!(p.filters.date_after, Some(fixed_now() - 3_600));
        assert_eq!(p.filters.date_before, None);
    }

    #[test]
    fn date_no_operator_defaults_to_less_than() {
        let p = parse("date:1h");
        assert_eq!(p.filters.date_after, Some(fixed_now() - 3_600));
        assert_eq!(p.filters.date_before, None);
    }

    #[test]
    fn date_greater_than_keeps_old() {
        let p = parse("date:>1d");
        assert_eq!(p.filters.date_after, None);
        assert_eq!(p.filters.date_before, Some(fixed_now() - DAY_SECS));
    }

    #[test]
    fn date_units() {
        let now = fixed_now();
        assert_eq!(parse("date:<30m").filters.date_after, Some(now - 1_800));
        assert_eq!(
            parse("date:<2w").filters.date_after,
            Some(now - 14 * DAY_SECS)
        );
        assert_eq!(parse("date:<1mo").filters.date_after, Some(now - 2_592_000));
        assert_eq!(parse("date:<1y").filters.date_after, Some(now - 31_536_000));
    }

    #[test]
    fn date_week_and_month_shortcuts() {
        let now = fixed_now();
        assert_eq!(
            parse("date:week").filters.date_after,
            Some(now - 7 * DAY_SECS)
        );
        assert_eq!(
            parse("date:month").filters.date_after,
            Some(now - 30 * DAY_SECS)
        );
    }

    #[test]
    fn date_invalid_sets_no_filter() {
        let p = parse("date:invalid");
        assert!(p.filters.date_after.is_none() && p.filters.date_before.is_none());

        assert_eq!(p.text, "");
    }

    #[test]
    fn date_negation_both_forms() {
        assert!(parse("-date:today").filters.date_negated);
        assert!(parse("date:!today").filters.date_negated);
    }

    #[test]
    fn date_combined_with_other_filters() {
        let p = parse("agent:claude date:<1d dir:project api");
        assert_eq!(p.text, "api");
        assert_eq!(p.filters.agents_include, vec![AgentId::Claude]);
        assert_eq!(p.filters.dirs_include, vec!["project".to_string()]);
        assert_eq!(p.filters.date_after, Some(fixed_now() - DAY_SECS));
    }

    #[test]
    fn partial_input_never_panics() {
        for raw in [
            "agent:cl", "date:<", "dir:", "agent:", "-", "date:!", "agent:,", ":",
        ] {
            let _ = parse(raw);
        }

        assert!(parse("agent:cl").filters.agents_include.is_empty());
        assert_eq!(parse("agent:cl").text, "");

        assert!(parse("date:<").filters.date_after.is_none());
    }

    #[test]
    fn keyword_token_at_end_detects_partial() {
        assert_eq!(
            keyword_token_at_end("agent:cl"),
            Some((Keyword::Agent, "cl"))
        );
        assert_eq!(
            keyword_token_at_end("auth dir:proj"),
            Some((Keyword::Dir, "proj"))
        );
        assert_eq!(
            keyword_token_at_end("agent:claude,co"),
            Some((Keyword::Agent, "co"))
        );
        assert_eq!(
            keyword_token_at_end("agent:!co"),
            Some((Keyword::Agent, "co"))
        );

        assert_eq!(keyword_token_at_end("just text"), None);
        assert_eq!(keyword_token_at_end("error:foo"), None);
    }
}

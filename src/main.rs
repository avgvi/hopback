use std::sync::mpsc;

use aiz::cli::{Cli, Command, ConfigAction, HiddenAction, PermissionAction};
use aiz::model::{PermissionPolicy, Session, SessionKey};
use aiz::search::Filters;
use aiz::store::Store;
use aiz::tui::{BgEvent, Outcome};
use aiz::{config, index, launcher, search};
use anyhow::Result;
use clap::Parser;

fn main() -> Result<()> {
    if let Ok(dir) = std::env::var("AIZ_EXEC_TEST") {
        launcher::exec_in_dir(&["pwd".to_string()], &dir)?;
    }

    let args = Cli::parse();

    if args.stats {
        return run_stats();
    }
    if let Some(cmd) = &args.command {
        return run_subcommand(cmd, args.yolo);
    }

    let flag_filters = flag_seed_filters(&args);

    let (free_text, filters) = parse_query_with_flags(&args);

    if args.rebuild {
        let mut store = open_store()?;
        let stats = index::rebuild(&mut store, None)?;
        println!(
            "Index rebuilt: {} sessions ({} indexed).",
            store.session_count()?,
            stats.upserted
        );
        if !args.no_tui && !args.list {
            return run_tui(store, args.query, flag_filters, args.yolo);
        }
        return list_to_stdout(&store, &free_text, &filters);
    }

    if args.no_tui || args.list {
        let mut store = open_store()?;
        index::index(&mut store, None)?;
        return list_to_stdout(&store, &free_text, &filters);
    }

    let store = open_store()?;
    run_tui(store, args.query, flag_filters, args.yolo)
}

fn flag_seed_filters(args: &Cli) -> Filters {
    let mut f = Filters::default();
    if let Some(a) = args.agent.as_deref().and_then(config::AgentId::from_id) {
        f.merge_agent_include(a);
    }
    if let Some(d) = &args.directory {
        f.merge_dir_include(d.clone());
    }
    f
}

fn parse_query_with_flags(args: &Cli) -> (String, Filters) {
    let now = aiz::tui::format::now_unix_secs();
    let mut pq = search_query(&args.query, now);
    if let Some(a) = args.agent.as_deref().and_then(config::AgentId::from_id) {
        pq.filters.merge_agent_include(a);
    }
    if let Some(d) = &args.directory {
        pq.filters.merge_dir_include(d.clone());
    }
    (pq.text, pq.filters)
}

fn search_query(raw: &str, now_secs: i64) -> aiz::query::ParsedQuery {
    aiz::query::parse_query(raw, now_secs)
}

fn open_store() -> Result<Store> {
    Store::open(&config::db_path())
}

fn run_stats() -> Result<()> {
    let mut store = open_store()?;
    index::index(&mut store, None)?;
    let stats = store.compute_stats(15)?;
    let now = aiz::tui::format::now_unix_secs();
    let home = std::env::var("HOME").unwrap_or_default();
    print!("{}", aiz::stats::render(&stats, now, &home));
    Ok(())
}

fn run_subcommand(cmd: &Command, yolo: bool) -> Result<()> {
    match cmd {
        Command::Fork {
            to,
            target_cwd,
            dry_run,
            no_launch,
            no_register_codex_index,
            session_id,
        } => fork_by_id(
            session_id,
            to.as_deref(),
            target_cwd.clone(),
            *dry_run,
            *no_launch,
            !*no_register_codex_index,
            yolo,
        ),
        Command::Config { action } => run_config(action),
    }
}

fn run_config(action: &ConfigAction) -> Result<()> {
    match action {
        ConfigAction::Permission { action } => run_permission_config(action),
        ConfigAction::Hidden { action } => run_hidden_config(action),
    }
}

fn run_permission_config(action: &PermissionAction) -> Result<()> {
    let store = open_store()?;
    match action {
        PermissionAction::Get => {
            match store.get_permission_policy()? {
                Some(policy) => println!("{}", policy.as_str()),
                None => println!("ask"),
            }
            Ok(())
        }
        PermissionAction::Set { value } => {
            let policy = PermissionPolicy::from_cli_value(value).ok_or_else(|| {
                anyhow::anyhow!("unknown permission policy '{value}'; use normal, yolo, or inherit")
            })?;
            store.set_permission_policy(policy)?;
            println!("permission_policy={}", policy.as_str());
            Ok(())
        }
        PermissionAction::Reset => {
            store.reset_permission_policy()?;
            println!("permission_policy=ask");
            Ok(())
        }
    }
}

fn run_hidden_config(action: &HiddenAction) -> Result<()> {
    let store = open_store()?;
    match action {
        HiddenAction::List => {
            for s in store.get_hidden_sessions()? {
                println!(
                    "{:<24} {:<40} {}",
                    s.key(),
                    truncate(&s.title, 40),
                    s.directory
                );
            }
            Ok(())
        }
        HiddenAction::Unhide { session_id } => {
            let key = resolve_hidden_selector(&store, session_id)?;
            store.set_hidden(&key, false)?;
            println!("hidden=false {key}");
            Ok(())
        }
        HiddenAction::Reset => {
            store.clear_hidden()?;
            println!("hidden=reset");
            Ok(())
        }
    }
}

fn fork_by_id(
    session_id: &str,
    target_agent: Option<&str>,
    target_cwd: Option<std::path::PathBuf>,
    dry_run: bool,
    no_launch: bool,
    register_codex_index: bool,
    yolo: bool,
) -> Result<()> {
    let mut store = open_store()?;
    index::index(&mut store, None)?;

    let session = resolve_session_selector(&store, session_id)?;

    if let Some(target_agent) = target_agent {
        let target = config::AgentId::resolve(target_agent)
            .ok_or_else(|| anyhow::anyhow!("unknown target agent '{target_agent}'"))?;
        let policy = store.get_permission_policy()?;
        let launch_yolo = yolo_or_safe_default(launcher::launch_permission_decision_for_agent(
            target, yolo, policy,
        ));
        return translated_fork_by_session(
            &mut store,
            &session,
            target,
            TranslatedForkCliOptions {
                target_cwd,
                dry_run,
                no_launch,
                register_codex_index,
                yolo: launch_yolo,
            },
        );
    }

    if target_cwd.is_some() {
        anyhow::bail!("--target-cwd is only valid with fork --to <agent>");
    }

    if !launcher::can_fork(&session) {
        anyhow::bail!(
            "{} sessions have no native fork; only resume is available",
            session.agent.as_str()
        );
    }

    let policy = store.get_permission_policy()?;
    let launch_yolo =
        yolo_or_safe_default(launcher::launch_permission_decision(&session, yolo, policy));
    let argv = launcher::fork_argv(&session, None, None, launch_yolo)?;
    if dry_run || no_launch {
        println!(
            "{} native fork command: {}",
            if dry_run { "Would run" } else { "Run" },
            shell_join(&argv)
        );
        return Ok(());
    }

    record_native_fork_link(&session);
    launcher::fork(&session, None, launch_yolo)?;
    Ok(())
}

struct TranslatedForkCliOptions {
    target_cwd: Option<std::path::PathBuf>,
    dry_run: bool,
    no_launch: bool,
    register_codex_index: bool,
    yolo: bool,
}

fn translated_fork_by_session(
    store: &mut Store,
    session: &aiz::model::Session,
    target: config::AgentId,
    options: TranslatedForkCliOptions,
) -> Result<()> {
    let bridge_options = aiz::bridge::BridgeOptions {
        target_cwd: options.target_cwd,
        dry_run: options.dry_run,
        register_codex_index: options.register_codex_index,
        ..Default::default()
    };
    let result = aiz::bridge::translated_fork(session, target, &bridge_options)?;
    print_bridge_result(&result, options.dry_run);

    if !options.dry_run {
        store.upsert_sessions(std::slice::from_ref(&result.target_session))?;
        record_translated_fork_link(session, &result.target_session);
    }

    let codex_registration_failed = result
        .codex_registration
        .as_ref()
        .is_some_and(|r| r.attempted && !r.ok);
    if options.dry_run || options.no_launch || codex_registration_failed {
        if codex_registration_failed {
            print_codex_bootstrap_hint(&result);
        }
        return Ok(());
    }

    launcher::resume(&result.target_session, options.yolo)?;
    Ok(())
}

fn resolve_session_selector(store: &Store, selector: &str) -> Result<Session> {
    if let Some(key) = SessionKey::parse(selector) {
        return store
            .get_session(&key)?
            .ok_or_else(|| anyhow::anyhow!("no session with key '{key}'"));
    }

    let matches = store.find_sessions_by_raw_id(selector)?;
    match matches.as_slice() {
        [] => anyhow::bail!("no session with id '{selector}'"),
        [session] => Ok(session.clone()),
        many => {
            let candidates = many
                .iter()
                .map(|s| s.key().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            anyhow::bail!("ambiguous session id '{selector}'; use one of: {candidates}")
        }
    }
}

fn resolve_hidden_selector(store: &Store, selector: &str) -> Result<SessionKey> {
    if let Some(key) = SessionKey::parse(selector) {
        return if store.is_hidden(&key)? {
            Ok(key)
        } else {
            anyhow::bail!("no hidden session with key '{key}'")
        };
    }

    let matches: Vec<Session> = store
        .get_hidden_sessions()?
        .into_iter()
        .filter(|s| s.id == selector)
        .collect();
    match matches.as_slice() {
        [] => anyhow::bail!("no hidden session with id '{selector}'"),
        [session] => Ok(session.key()),
        many => {
            let candidates = many
                .iter()
                .map(|s| s.key().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            anyhow::bail!("ambiguous hidden session id '{selector}'; use one of: {candidates}")
        }
    }
}

fn list_to_stdout(store: &Store, query: &str, filters: &Filters) -> Result<()> {
    let results = search::search_filtered(store, query, filters, search::DEFAULT_LIMIT)?;
    for r in &results {
        let s = &r.session;
        println!(
            "{:<14} {:<40} {}",
            s.agent.as_str(),
            truncate(&s.title, 40),
            s.directory
        );
    }
    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max.saturating_sub(1)).collect::<String>() + "…"
    }
}

fn run_tui(store: Store, query: String, filters: Filters, yolo: bool) -> Result<()> {
    let initial = store.get_all_sessions()?;

    let tui_store = open_store().ok();

    let index_spawn = move |tx: mpsc::Sender<BgEvent>| {
        let mut store = store;
        let tx_del = tx.clone();
        let mut on_session = |s: &aiz::model::Session| {
            let _ = tx.send(BgEvent::Session(Box::new(s.clone())));
        };
        let mut on_deleted = |agent: config::AgentId, ids: &[String]| {
            let _ = tx_del.send(BgEvent::Deleted {
                agent,
                ids: ids.to_vec(),
            });
        };
        let _ = index::index_streaming(&mut store, Some(&mut on_session), Some(&mut on_deleted));
        let _ = tx.send(BgEvent::IndexingDone);
    };

    match aiz::tui::run(initial, query, filters, yolo, tui_store, index_spawn)? {
        Outcome::Resume { session, yolo } => {
            launcher::resume(&session, yolo)?;
            Ok(())
        }
        Outcome::Fork { session, yolo } => {
            record_native_fork_link(&session);

            launcher::fork(&session, None, yolo)?;
            Ok(())
        }
        Outcome::TranslatedFork {
            session,
            target,
            yolo,
        } => {
            let mut store = open_store()?;
            let result = aiz::bridge::translated_fork(
                &session,
                target,
                &aiz::bridge::BridgeOptions::default(),
            )?;
            print_bridge_result(&result, false);
            store.upsert_sessions(std::slice::from_ref(&result.target_session))?;
            record_translated_fork_link(&session, &result.target_session);
            if result
                .codex_registration
                .as_ref()
                .is_some_and(|r| r.attempted && !r.ok)
            {
                print_codex_bootstrap_hint(&result);
                return Ok(());
            }
            launcher::resume(&result.target_session, yolo)?;
            Ok(())
        }
        Outcome::Quit => Ok(()),
    }
}

fn yolo_or_safe_default(decision: launcher::YoloDecision) -> bool {
    matches!(decision, launcher::YoloDecision::On)
}

fn record_native_fork_link(parent: &aiz::model::Session) {
    use aiz::model::{ForkLink, ForkMode};
    let now = aiz::tui::format::now_unix_secs();
    if let Ok(store) = open_store() {
        let link = ForkLink {
            fork_session_id: None,
            parent_session_id: parent.id.clone(),
            parent_agent: parent.agent,
            fork_agent: parent.agent,
            mode: ForkMode::Native,
            directory: parent.directory.clone(),
            created: now,
        };
        let _ = store.insert_fork_link(&link);
    }
}

fn record_translated_fork_link(parent: &aiz::model::Session, child: &aiz::model::Session) {
    use aiz::model::{ForkLink, ForkMode};
    let now = aiz::tui::format::now_unix_secs();
    if let Ok(store) = open_store() {
        let link = ForkLink {
            fork_session_id: Some(child.id.clone()),
            parent_session_id: parent.id.clone(),
            parent_agent: parent.agent,
            fork_agent: child.agent,
            mode: ForkMode::Translated,
            directory: child.directory.clone(),
            created: now,
        };
        let _ = store.insert_fork_link(&link);
    }
}

fn print_bridge_result(result: &aiz::bridge::BridgeResult, dry_run: bool) {
    println!(
        "{} translated fork: {} -> {}",
        if dry_run { "Would create" } else { "Created" },
        result.source,
        result.target
    );
    println!("Target file: {}", result.target_path.display());
    println!("Resume: {}", shell_join(&result.resume_argv));
    if let Some(reg) = &result.codex_registration {
        if reg.ok {
            println!("Codex SQLite registration: ok");
        } else if let Some(err) = &reg.error {
            println!("Codex SQLite registration: failed ({err})");
        }
    }
    for warning in &result.warnings {
        println!("Warning: {warning}");
    }
}

fn print_codex_bootstrap_hint(result: &aiz::bridge::BridgeResult) {
    if result.target.agent != config::AgentId::Codex {
        return;
    }
    println!("Codex TUI resume may not see the new session yet. Bootstrap once with:");
    println!(
        "  codex exec --skip-git-repo-check resume {} \"ping\"",
        result.target.id
    );
}

fn shell_join(argv: &[String]) -> String {
    argv.iter()
        .map(|a| {
            if a.is_empty()
                || a.bytes().any(|b| {
                    !matches!(
                        b,
                        b'a'..=b'z'
                            | b'A'..=b'Z'
                            | b'0'..=b'9'
                            | b'-'
                            | b'_'
                            | b'/'
                            | b'.'
                            | b':'
                            | b'='
                            | b'@'
                            | b'+'
                    )
                })
            {
                format!("'{}'", a.replace('\'', "'\\''"))
            } else {
                a.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use aiz::config::AgentId;
    use aiz::model::{Session, SessionKey};

    fn sess(id: &str, agent: AgentId) -> Session {
        Session {
            id: id.into(),
            agent,
            title: id.into(),
            directory: "/work".into(),
            timestamp: 1,
            content: String::new(),
            message_count: 1,
            mtime: 1,
            yolo: false,
            source_path: None,
        }
    }

    #[test]
    fn selector_agent_id_resolves_exactly() {
        let mut store = Store::open_in_memory().unwrap();
        store
            .upsert_sessions(&[sess("same", AgentId::Claude), sess("same", AgentId::Codex)])
            .unwrap();

        let selected = resolve_session_selector(&store, "codex:same").unwrap();
        assert_eq!(selected.agent, AgentId::Codex);
        assert_eq!(selected.id, "same");
    }

    #[test]
    fn selector_bare_id_resolves_when_unique() {
        let mut store = Store::open_in_memory().unwrap();
        store
            .upsert_sessions(&[sess("unique", AgentId::Claude)])
            .unwrap();

        let selected = resolve_session_selector(&store, "unique").unwrap();
        assert_eq!(selected.agent, AgentId::Claude);
    }

    #[test]
    fn selector_bare_id_errors_when_ambiguous() {
        let mut store = Store::open_in_memory().unwrap();
        store
            .upsert_sessions(&[sess("same", AgentId::Claude), sess("same", AgentId::Codex)])
            .unwrap();

        let err = resolve_session_selector(&store, "same")
            .unwrap_err()
            .to_string();
        assert!(err.contains("ambiguous session id 'same'"));
        assert!(err.contains("claude:same"));
        assert!(err.contains("codex:same"));
    }

    #[test]
    fn selector_ignores_hidden_sessions() {
        let mut store = Store::open_in_memory().unwrap();
        store
            .upsert_sessions(&[sess("same", AgentId::Claude), sess("same", AgentId::Codex)])
            .unwrap();
        store
            .set_hidden(&SessionKey::new(AgentId::Claude, "same"), true)
            .unwrap();

        let selected = resolve_session_selector(&store, "same").unwrap();
        assert_eq!(selected.agent, AgentId::Codex);
    }

    #[test]
    fn hidden_selector_resolves_exact_and_ambiguous() {
        let mut store = Store::open_in_memory().unwrap();
        store
            .upsert_sessions(&[sess("same", AgentId::Claude), sess("same", AgentId::Codex)])
            .unwrap();
        store
            .set_hidden(&SessionKey::new(AgentId::Claude, "same"), true)
            .unwrap();
        store
            .set_hidden(&SessionKey::new(AgentId::Codex, "same"), true)
            .unwrap();

        let selected = resolve_hidden_selector(&store, "claude:same").unwrap();
        assert_eq!(selected.agent, AgentId::Claude);

        let err = resolve_hidden_selector(&store, "same")
            .unwrap_err()
            .to_string();
        assert!(err.contains("ambiguous hidden session id 'same'"));
        assert!(err.contains("claude:same"));
        assert!(err.contains("codex:same"));
    }
}

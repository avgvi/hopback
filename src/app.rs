use std::sync::mpsc;

use crate::cli::{Cli, Command, ConfigAction, HiddenAction, PermissionAction};
use crate::model::{PermissionPolicy, Session, SessionKey};
use crate::search::Filters;
use crate::store::Store;
use crate::tui::{BgEvent, Outcome};
use crate::{config, index, launcher, search};
use anyhow::Result;
use clap::Parser;

pub fn run() -> Result<()> {
    if let Ok(dir) = std::env::var("HOPBACK_EXEC_TEST") {
        launcher::exec_in_dir(&["pwd".to_string()], &dir)?;
    }

    let args = Cli::parse();

    if args.agent_context {
        print!("{}", crate::agent::SKILL);
        return Ok(());
    }
    if args.stats {
        return run_stats(args.no_refresh);
    }
    if let Some(cmd) = &args.command {
        return run_subcommand(cmd, &args);
    }

    let agent_flag = match args.agent.as_deref() {
        Some(name) => Some(config::AgentId::resolve(name).ok_or_else(|| {
            anyhow::anyhow!(
                "unknown agent '{name}'; known agents: {}",
                config::AgentId::ALL.map(|a| a.as_str()).join(", ")
            )
        })?),
        None => None,
    };

    let listing = args.json || args.no_tui || args.list;
    if args.query.trim() == crate::quick::CONTINUE_QUERY && !listing {
        return continue_here(&args, agent_flag);
    }
    if !listing
        && (args.limit.is_some() || args.offset.is_some() || args.all || args.include_hidden)
    {
        anyhow::bail!(
            "--limit, --offset, --all and --include-hidden need --json, --list or --no-tui"
        );
    }

    if args.include_messages && !args.json {
        anyhow::bail!("--include-messages needs --json");
    }

    let flag_filters = flag_seed_filters(&args);

    if listing {
        let (free_text, filters) = parse_query_with_flags(&args);
        let mut store = open_store()?;
        if args.rebuild {
            let stats = index::rebuild(&mut store, None)?;
            let banner = format!(
                "Index rebuilt: {} sessions ({} indexed).",
                store.session_count()?,
                stats.upserted
            );
            // In JSON mode stdout carries the JSON object and nothing else.
            if args.json {
                eprintln!("{banner}");
            } else {
                println!("{banner}");
            }
        } else if !args.no_refresh {
            index::index(&mut store, None)?;
        }
        return if args.json {
            json_to_stdout(&store, &free_text, &filters, &args)
        } else {
            list_to_stdout(&store, &free_text, &filters, &args)
        };
    }

    let mut store = open_store()?;
    if args.rebuild {
        let stats = index::rebuild(&mut store, None)?;
        println!(
            "Index rebuilt: {} sessions ({} indexed).",
            store.session_count()?,
            stats.upserted
        );
    }
    run_tui(
        store,
        args.query,
        flag_filters,
        args.yolo,
        !args.no_refresh,
        args.print,
    )
}

/// `hb .`: resume the newest session of the current folder, or of the closest
/// parent folder that has one.
fn continue_here(args: &Cli, agent: Option<config::AgentId>) -> Result<()> {
    let cwd = std::env::current_dir()
        .map_err(|e| anyhow::anyhow!("cannot read the current folder: {e}"))?;
    let mut store = open_store()?;
    if args.rebuild {
        index::rebuild(&mut store, None)?;
    } else if !args.no_refresh {
        index::index(&mut store, None)?;
    }

    let sessions = store.get_all_sessions()?;
    let Some(pick) = crate::quick::pick_for_directory(&sessions, &cwd, agent) else {
        let scope = match agent {
            Some(a) => format!("{} session", a.as_str()),
            None => "session".to_string(),
        };
        anyhow::bail!(
            "no {scope} found for {} or any parent folder. Run `hb` to browse every session.",
            cwd.display()
        );
    };

    let policy = store.get_permission_policy()?;
    let yolo = yolo_decided(launcher::launch_permission_decision(
        &pick.session,
        args.yolo,
        policy,
    ));
    if !pick.exact {
        eprintln!(
            "hopback: no session in {}; using the newest one from {}",
            cwd.display(),
            pick.directory.display()
        );
    }
    drop(store);
    resume_or_print(&pick.session, yolo, args.print)
}

/// Hands the terminal to the agent, or with `--print` writes two lines to
/// stdout: the folder to run in, then the shell-quoted command.
fn resume_or_print(session: &Session, yolo: bool, print: bool) -> Result<()> {
    if print {
        let argv = launcher::resume_argv(session, yolo)?;
        println!("{}", session.directory);
        println!("{}", shell_join(&argv));
        return Ok(());
    }
    match launcher::resume(session, yolo)? {}
}

fn flag_seed_filters(args: &Cli) -> Filters {
    let mut f = Filters::default();
    if let Some(a) = args.agent.as_deref().and_then(config::AgentId::resolve) {
        f.merge_agent_include(a);
    }
    if let Some(d) = &args.directory {
        f.merge_dir_include(d.clone());
    }
    f
}

fn parse_query_with_flags(args: &Cli) -> (String, Filters) {
    let now = crate::tui::format::now_unix_secs();
    let mut pq = search_query(&args.query, now);
    if let Some(a) = args.agent.as_deref().and_then(config::AgentId::resolve) {
        pq.filters.merge_agent_include(a);
    }
    if let Some(d) = &args.directory {
        pq.filters.merge_dir_include(d.clone());
    }
    (pq.text, pq.filters)
}

fn search_query(raw: &str, now_secs: i64) -> crate::query::ParsedQuery {
    crate::query::parse_query(raw, now_secs)
}

fn open_store() -> Result<Store> {
    let (store, imported) = crate::store::legacy::open_with_legacy_import(
        &config::db_path(),
        &config::legacy_db_path(),
    )?;
    if let Some(report) = imported {
        eprintln!(
            "hopback: copied {} pins, renames and hidden flags, {} settings and {} fork links from aiz ({} was left untouched)",
            report.overrides,
            report.settings,
            report.fork_links,
            config::legacy_db_path().display()
        );
    }
    Ok(store)
}

fn run_stats(no_refresh: bool) -> Result<()> {
    let mut store = open_store()?;
    if !no_refresh {
        index::index(&mut store, None)?;
    }
    let stats = store.compute_stats(15)?;
    let now = crate::tui::format::now_unix_secs();
    let home = std::env::var("HOME").unwrap_or_default();
    print!("{}", crate::stats::render(&stats, now, &home));
    Ok(())
}

fn run_subcommand(cmd: &Command, args: &Cli) -> Result<()> {
    use crate::cli::SessionArg;
    let change = |target: &SessionArg, change: SessionChange| {
        change_session(&target.session, change, target.json, args)
    };
    match cmd {
        Command::Fork {
            to,
            target_cwd,
            dry_run,
            no_launch,
            no_register_codex_index,
            json,
            session_id,
        } => fork_by_id(
            session_id,
            to.as_deref(),
            ForkCliOptions {
                target_cwd: target_cwd.clone(),
                dry_run: *dry_run,
                no_launch: *no_launch,
                register_codex_index: !*no_register_codex_index,
                json: *json,
                yolo: args.yolo,
                refresh: !args.no_refresh,
            },
        ),
        Command::Pin { target } => change(target, SessionChange::Pin(true)),
        Command::Unpin { target } => change(target, SessionChange::Pin(false)),
        Command::Rename {
            target,
            title,
            reset,
        } => {
            let title = if *reset {
                None
            } else {
                let title = title.as_deref().unwrap_or("").trim();
                if title.is_empty() {
                    anyhow::bail!("the title is empty; use --reset to remove a local title");
                }
                Some(title.to_string())
            };
            change(target, SessionChange::Rename(title))
        }
        Command::Hide { target } => change(target, SessionChange::Hide(true)),
        Command::Unhide { target } => change(target, SessionChange::Hide(false)),
        Command::Completions { shell, command } => {
            let name = command.clone().unwrap_or_else(crate::agent::invoked_name);
            crate::agent::write_completions(*shell, &name, &mut std::io::stdout());
            Ok(())
        }
        Command::Man => {
            crate::agent::write_man_page(&mut std::io::stdout())?;
            Ok(())
        }
        Command::Config { action } => run_config(action),
    }
}

/// A change to hopback's own metadata for one session.
enum SessionChange {
    Pin(bool),
    /// `None` removes the local title.
    Rename(Option<String>),
    Hide(bool),
}

/// Finds a session by `agent:id` or bare id, hidden or not.
fn resolve_any_selector(store: &Store, selector: &str) -> Result<SessionKey> {
    if let Some(key) = SessionKey::parse(selector) {
        return match store.get_record(&key)? {
            Some(_) => Ok(key),
            None => anyhow::bail!("no session with key '{key}'"),
        };
    }
    let matches: Vec<SessionKey> = store
        .list_records(true)?
        .into_iter()
        .filter(|r| r.session.id == selector)
        .map(|r| r.session.key())
        .collect();
    match matches.as_slice() {
        [] => anyhow::bail!("no session with id '{selector}'"),
        [key] => Ok(key.clone()),
        many => {
            let candidates = many
                .iter()
                .map(|k| k.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            anyhow::bail!("ambiguous session id '{selector}'; use one of: {candidates}")
        }
    }
}

fn change_session(selector: &str, change: SessionChange, json: bool, args: &Cli) -> Result<()> {
    let mut store = open_store()?;
    if !args.no_refresh {
        index::index(&mut store, None)?;
    }
    let key = resolve_any_selector(&store, selector)?;

    let done = match &change {
        SessionChange::Pin(on) => {
            store.set_pinned(&key, *on)?;
            if *on { "pinned" } else { "unpinned" }.to_string()
        }
        SessionChange::Rename(title) => {
            store.set_title_override(&key, title.as_deref())?;
            match title {
                Some(t) => format!("renamed to {t:?}:"),
                None => "rename removed:".to_string(),
            }
        }
        SessionChange::Hide(on) => {
            store.set_hidden(&key, *on)?;
            if *on { "hidden" } else { "unhidden" }.to_string()
        }
    };

    if json {
        let record = store
            .get_record(&key)?
            .ok_or_else(|| anyhow::anyhow!("session '{key}' disappeared"))?;
        let ctx = crate::output::ResumeContext {
            flag_yolo: args.yolo,
            policy: store.get_permission_policy()?,
        };
        crate::output::print_json(&crate::output::SessionChangeOutput::new(&record, ctx))
    } else {
        println!("{done} {key}");
        Ok(())
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

struct ForkCliOptions {
    target_cwd: Option<std::path::PathBuf>,
    dry_run: bool,
    no_launch: bool,
    register_codex_index: bool,
    json: bool,
    yolo: bool,
    refresh: bool,
}

fn fork_by_id(session_id: &str, target_agent: Option<&str>, options: ForkCliOptions) -> Result<()> {
    let mut store = open_store()?;
    if options.refresh {
        index::index(&mut store, None)?;
    }

    let session = resolve_session_selector(&store, session_id)?;
    let policy = store.get_permission_policy()?;

    if let Some(target_agent) = target_agent {
        let target = config::AgentId::resolve(target_agent)
            .ok_or_else(|| anyhow::anyhow!("unknown target agent '{target_agent}'"))?;
        let launch_yolo = yolo_decided(launcher::launch_permission_decision_for_agent(
            target,
            options.yolo,
            policy,
            launcher::inherited_yolo(&session),
        ));
        return translated_fork_by_session(&mut store, &session, target, &options, launch_yolo);
    }

    if options.target_cwd.is_some() {
        anyhow::bail!("--target-cwd is only valid with fork --to <agent>");
    }

    if !launcher::can_fork(&session) {
        anyhow::bail!(
            "{} sessions have no native fork; only resume is available",
            session.agent.as_str()
        );
    }

    let launch_yolo = yolo_decided(launcher::launch_permission_decision(
        &session,
        options.yolo,
        policy,
    ));

    if options.json {
        return native_fork_json(&session, &options, launch_yolo, policy);
    }

    let argv = launcher::fork_argv(&session, None, None, launch_yolo)?;
    if options.dry_run || options.no_launch {
        println!(
            "{} native fork command: {}",
            if options.dry_run { "Would run" } else { "Run" },
            shell_join(&argv)
        );
        return Ok(());
    }

    record_native_fork_link(&session, None);
    match launcher::fork(&session, None, launch_yolo)? {}
}

/// `fork --json` for a native fork. The agent creates the new session when
/// the command runs, so nothing is launched here. For Claude Code the new id
/// is chosen up front (`--session-id`), which lets the session be described
/// before it exists; other agents pick their own id, and `session` is null.
fn native_fork_json(
    parent: &Session,
    options: &ForkCliOptions,
    yolo: bool,
    policy: Option<PermissionPolicy>,
) -> Result<()> {
    let new_id = (parent.agent == config::AgentId::Claude).then(crate::bridge::new_session_id);
    let argv = launcher::fork_argv(parent, new_id.as_deref(), None, yolo)?;

    let session = new_id.as_ref().map(|id| {
        let mut child = parent.clone();
        child.id = id.clone();
        child.timestamp = crate::tui::format::now_unix_secs();
        child.source_path = None;
        let record = crate::store::sessions::SessionRecord {
            original_title: child.title.clone(),
            session: child,
            pinned: false,
            hidden: false,
            forked_from: Some(parent.key()),
        };
        crate::output::SessionOutput::new(
            &record,
            crate::output::ResumeContext {
                flag_yolo: options.yolo,
                policy,
            },
        )
    });

    if !options.dry_run {
        if let Some(id) = &new_id {
            // Remember the parent now, so the fork is marked once the agent
            // has created it and it is indexed.
            record_native_fork_link(parent, Some(id.clone()));
        }
    }

    crate::output::print_json(&crate::output::ForkOutput {
        schema_version: crate::output::JSON_SCHEMA_VERSION,
        mode: "native",
        dry_run: options.dry_run,
        created: false,
        session,
        launch_command: argv,
        warnings: Vec::new(),
    })
}

fn translated_fork_by_session(
    store: &mut Store,
    session: &crate::model::Session,
    target: config::AgentId,
    options: &ForkCliOptions,
    launch_yolo: bool,
) -> Result<()> {
    let bridge_options = crate::bridge::BridgeOptions {
        target_cwd: options.target_cwd.clone(),
        dry_run: options.dry_run,
        register_codex_index: options.register_codex_index,
        ..Default::default()
    };
    let mut result = crate::bridge::translated_fork(session, target, &bridge_options)?;
    // The bridge copies the parent's first prompt as the title; give it the
    // same cleaning and length limit as an indexed session.
    crate::title::finalize(&mut result.target_session);
    if !options.json {
        print_bridge_result(&result, options.dry_run);
    }

    if !options.dry_run {
        store.upsert_sessions(std::slice::from_ref(&result.target_session))?;
        record_translated_fork_link(session, &result.target_session);
    }

    let codex_registration_failed = result
        .codex_registration
        .as_ref()
        .is_some_and(|r| r.attempted && !r.ok);

    if options.json {
        let record = crate::store::sessions::SessionRecord {
            original_title: result.target_session.title.clone(),
            session: result.target_session.clone(),
            pinned: false,
            hidden: false,
            forked_from: Some(session.key()),
        };
        let ctx = crate::output::ResumeContext {
            flag_yolo: options.yolo,
            policy: store.get_permission_policy()?,
        };
        let session_out = crate::output::SessionOutput::new(&record, ctx);
        let mut warnings = result.warnings.clone();
        if codex_registration_failed {
            warnings.push(format!(
                "Codex may not list the new session until it is bootstrapped once with: codex exec --skip-git-repo-check resume {} \"ping\"",
                result.target.id
            ));
        }
        return crate::output::print_json(&crate::output::ForkOutput {
            schema_version: crate::output::JSON_SCHEMA_VERSION,
            mode: "translated",
            dry_run: options.dry_run,
            created: !options.dry_run,
            launch_command: session_out.resume_command.clone(),
            session: Some(session_out),
            warnings,
        });
    }

    if options.dry_run || options.no_launch || codex_registration_failed {
        if codex_registration_failed {
            print_codex_bootstrap_hint(&result);
        }
        return Ok(());
    }

    match launcher::resume(&result.target_session, launch_yolo)? {}
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

fn list_to_stdout(store: &Store, query: &str, filters: &Filters, args: &Cli) -> Result<()> {
    let matches = search::search_records(store, query, filters, args.include_hidden)?;
    let offset = args.offset.unwrap_or(0);
    let limit = page_limit(args, matches.len(), search::DEFAULT_LIMIT);
    for r in &matches[crate::output::page_bounds(matches.len(), offset, limit)] {
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

fn json_to_stdout(store: &Store, query: &str, filters: &Filters, args: &Cli) -> Result<()> {
    let matches = search::search_records(store, query, filters, args.include_hidden)?;
    let offset = args.offset.unwrap_or(0);
    let limit = page_limit(args, matches.len(), crate::output::DEFAULT_JSON_LIMIT);
    let ctx = crate::output::ResumeContext {
        flag_yolo: args.yolo,
        policy: store.get_permission_policy()?,
    };
    crate::output::print_json(&crate::output::session_list(
        &matches,
        offset,
        limit,
        ctx,
        args.include_messages,
    ))
}

/// `--all` means everything from the offset on; otherwise `--limit` or the
/// listing's default.
fn page_limit(args: &Cli, total: usize, default: usize) -> usize {
    if args.all {
        return total.saturating_sub(args.offset.unwrap_or(0));
    }
    args.limit
        .map(|n| usize::try_from(n).unwrap_or(usize::MAX))
        .unwrap_or(default)
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max.saturating_sub(1)).collect::<String>() + "…"
    }
}

fn run_tui(
    store: Store,
    query: String,
    filters: Filters,
    yolo: bool,
    refresh: bool,
    print: bool,
) -> Result<()> {
    let initial = store.get_all_sessions()?;

    let tui_store = open_store().ok();

    let index_spawn = move |tx: mpsc::Sender<BgEvent>| {
        let mut store = store;
        let tx_del = tx.clone();
        let mut on_session = |s: &crate::model::Session| {
            let _ = tx.send(BgEvent::Session(Box::new(s.clone())));
        };
        let mut on_deleted = |agent: config::AgentId, ids: &[String]| {
            let _ = tx_del.send(BgEvent::Deleted {
                agent,
                ids: ids.to_vec(),
            });
        };
        if refresh {
            let _ =
                index::index_streaming(&mut store, Some(&mut on_session), Some(&mut on_deleted));
            let _ = tx.send(BgEvent::IndexingDone);
        } else {
            let _ = tx.send(BgEvent::IndexingSkipped);
        }
    };

    match crate::tui::run(initial, query, filters, yolo, tui_store, index_spawn)? {
        Outcome::Resume { session, yolo } => resume_or_print(&session, yolo, print),
        Outcome::Fork { session, yolo } => {
            record_native_fork_link(&session, None);

            match launcher::fork(&session, None, yolo)? {}
        }
        Outcome::TranslatedFork {
            session,
            target,
            yolo,
        } => {
            let mut store = open_store()?;
            let mut result = crate::bridge::translated_fork(
                &session,
                target,
                &crate::bridge::BridgeOptions::default(),
            )?;
            crate::title::finalize(&mut result.target_session);
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
            match launcher::resume(&result.target_session, yolo)? {}
        }
        Outcome::Quit => Ok(()),
    }
}

fn yolo_decided(decision: launcher::YoloDecision) -> bool {
    matches!(decision, launcher::YoloDecision::On)
}

fn record_native_fork_link(parent: &crate::model::Session, fork_session_id: Option<String>) {
    use crate::model::{ForkLink, ForkMode};
    let now = crate::tui::format::now_unix_secs();
    if let Ok(store) = open_store() {
        let link = ForkLink {
            fork_session_id,
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

fn record_translated_fork_link(parent: &crate::model::Session, child: &crate::model::Session) {
    use crate::model::{ForkLink, ForkMode};
    let now = crate::tui::format::now_unix_secs();
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

fn print_bridge_result(result: &crate::bridge::BridgeResult, dry_run: bool) {
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

fn print_codex_bootstrap_hint(result: &crate::bridge::BridgeResult) {
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
    use crate::config::AgentId;
    use crate::model::{Session, SessionKey};

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

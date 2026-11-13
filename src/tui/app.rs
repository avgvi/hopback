use std::collections::HashMap;
use std::io::{self, Stdout};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::prelude::*;

use super::keys::{self, Command, MenuAction};
use super::layout::{self, AppLayout};
use crate::config::AgentId;
use crate::model::{PermissionPolicy, Session, SessionKey};
use crate::search::{self, Filters, SearchResult};

pub enum Outcome {
    Resume {
        session: Session,
        yolo: bool,
    },

    Fork {
        session: Session,
        yolo: bool,
    },

    TranslatedFork {
        session: Session,
        target: AgentId,
        yolo: bool,
    },

    Quit,
}

pub enum BgEvent {
    Session(Box<Session>),
    Deleted {
        agent: AgentId,
        ids: Vec<String>,
    },
    IndexingDone,
    /// The index was not refreshed (`--no-refresh`); what is shown is what
    /// the store already had.
    IndexingSkipped,
}

/// State of the background refresh, shown in the status line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Refresh {
    Running,
    Done { changed: usize, millis: u128 },
    Skipped,
}

const DEBOUNCE: Duration = Duration::from_millis(50);

const TICK: Duration = Duration::from_millis(16);

/// Two clicks on the same row within this time are a double-click.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

pub(super) struct App {
    all: Vec<Session>,

    seen: HashMap<SessionKey, usize>,

    results: Vec<SearchResult>,

    query: String,

    /// Cursor position in `query`, as a character index.
    cursor: usize,

    dir_filter: Option<String>,

    agents_present: Vec<AgentId>,

    yolo: bool,

    permission_policy: Option<PermissionPolicy>,

    selected: usize,

    show_preview: bool,

    /// Lines the preview is scrolled down by; reset when the selection moves.
    preview_scroll: u16,

    indexing: bool,

    refresh: Refresh,

    rerank_at: Option<Instant>,

    last_query_ms: f64,

    toast: Option<String>,

    rename_modal: Option<RenameModal>,

    fork_modal: Option<ForkModal>,

    pinned: std::collections::HashSet<SessionKey>,

    hidden: std::collections::HashSet<SessionKey>,

    /// Sessions that were created by forking another one through hopback.
    forked: std::collections::HashSet<SessionKey>,

    hide_modal: Option<HideModal>,

    /// The Ctrl+O actions menu for the selected session.
    actions_modal: Option<ActionsModal>,

    /// The F1 overlay that lists every key.
    help_open: bool,

    /// Time and row of the last click, to tell a double-click.
    last_click: Option<(Instant, usize)>,

    /// First visible row of the table, kept between frames so the list does
    /// not jump and so a click can be mapped back to a row.
    table_offset: std::cell::Cell<usize>,

    /// Where the last frame put things, for mouse hit testing.
    last_layout: std::cell::Cell<Option<AppLayout>>,

    store: Option<crate::store::Store>,
}

struct ActionsModal {
    selected: usize,
}

struct RenameModal {
    session_key: SessionKey,
    input: String,
    /// Cursor position in `input`, as a character index.
    cursor: usize,
}

struct HideModal {
    session_key: SessionKey,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ForkChoice {
    Native,
    Translated(AgentId),
}

struct ForkModal {
    session: Session,
    choices: Vec<ForkChoice>,
    selected: usize,
}

#[derive(Clone)]
enum PendingLaunch {
    Resume(Session),
    Fork(Session),
    TranslatedFork { session: Session, target: AgentId },
}

impl PendingLaunch {
    fn into_outcome(self, yolo: bool) -> Outcome {
        match self {
            PendingLaunch::Resume(session) => Outcome::Resume { session, yolo },
            PendingLaunch::Fork(session) => Outcome::Fork { session, yolo },
            PendingLaunch::TranslatedFork { session, target } => Outcome::TranslatedFork {
                session,
                target,
                yolo,
            },
        }
    }
}

impl App {
    fn new(initial: Vec<Session>, query: String, filters: Filters, yolo: bool) -> App {
        // An agent asked for on the command line becomes search text, so
        // the search line always says what is being filtered.
        let query = match filters.agents_include.first() {
            Some(agent) if !query_names_an_agent(&query) => {
                crate::query::set_agent(&query, Some(agent.badge()))
            }
            _ => query,
        };
        let dir_filter = filters.dirs_include.into_iter().next();
        let cursor = query.chars().count();
        let mut app = App {
            all: Vec::new(),
            seen: HashMap::new(),
            results: Vec::new(),
            query,
            cursor,
            dir_filter,
            agents_present: Vec::new(),
            yolo,
            permission_policy: None,
            selected: 0,
            show_preview: true,
            preview_scroll: 0,
            indexing: true,
            refresh: Refresh::Running,
            rerank_at: Some(Instant::now()),
            last_query_ms: 0.0,
            toast: None,
            rename_modal: None,
            fork_modal: None,
            pinned: std::collections::HashSet::new(),
            hidden: std::collections::HashSet::new(),
            forked: std::collections::HashSet::new(),
            hide_modal: None,
            actions_modal: None,
            help_open: false,
            last_click: None,
            table_offset: std::cell::Cell::new(0),
            last_layout: std::cell::Cell::new(None),
            store: None,
        };
        for s in initial {
            app.ingest(s);
        }
        app.recompute_agents_present();
        app
    }

    #[cfg(test)]
    pub(super) fn for_test(initial: Vec<Session>, query: &str, filters: Filters) -> App {
        let mut app = App::new(initial, query.to_string(), filters, false);
        app.rerank();
        app
    }

    #[cfg(test)]
    pub(super) fn finish_refresh_for_test(&mut self, changed: usize, millis: u128) {
        self.indexing = false;
        self.refresh = Refresh::Done { changed, millis };
    }

    #[cfg(test)]
    pub(super) fn set_query_for_test(&mut self, query: &str) {
        self.query = query.to_string();
        self.cursor = self.query.chars().count();
        self.rerank();
    }

    #[cfg(test)]
    pub(super) fn set_cursor_for_test(&mut self, cursor: usize) {
        self.cursor = cursor;
    }

    #[cfg(test)]
    pub(super) fn select_for_test(&mut self, row: usize) {
        self.select_row(row);
    }

    #[cfg(test)]
    pub(super) fn open_actions_for_test(&mut self) {
        self.open_actions();
    }

    #[cfg(test)]
    pub(super) fn open_help_for_test(&mut self) {
        self.help_open = true;
    }

    /// One key press, carried out the way the event loop does it.
    #[cfg(test)]
    pub(super) fn press_for_test(&mut self, key: KeyEvent) {
        let action = handle_key(self, key);
        let _ = apply_action(self, action);
    }

    /// Called by the renderer with the frame's layout and the table's scroll
    /// offset after drawing.
    pub(super) fn record_frame(&self, layout: AppLayout, table_offset: usize) {
        self.last_layout.set(Some(layout));
        self.table_offset.set(table_offset);
    }

    fn any_modal_open(&self) -> bool {
        self.rename_modal.is_some()
            || self.fork_modal.is_some()
            || self.hide_modal.is_some()
            || self.actions_modal.is_some()
            || self.help_open
    }

    fn open_actions(&mut self) {
        if self.selected_session().is_some() {
            self.actions_modal = Some(ActionsModal { selected: 0 });
        }
    }

    fn clear_query(&mut self) {
        self.query.clear();
        self.cursor = 0;
        self.request_rerank();
    }

    fn select_row(&mut self, row: usize) {
        if row < self.results.len() && row != self.selected {
            self.selected = row;
            self.preview_scroll = 0;
        }
    }

    /// Scrolls the preview by `delta` lines, staying inside the conversation.
    fn scroll_preview(&mut self, delta: isize) {
        let lines = self
            .selected_session()
            .map(|s| s.content.chars().take(6_000).filter(|c| *c == '\n').count() + 4)
            .unwrap_or(0) as isize;
        let next = (self.preview_scroll as isize + delta).clamp(0, lines.max(0));
        self.preview_scroll = next as u16;
    }

    #[cfg(test)]
    pub(super) fn set_preview_for_test(&mut self, on: bool) {
        self.show_preview = on;
    }

    #[cfg(test)]
    pub(super) fn mark_for_test(&mut self, id: &str, agent: AgentId, pinned: bool, forked: bool) {
        let key = SessionKey::new(agent, id);
        if pinned {
            self.pinned.insert(key.clone());
        }
        if forked {
            self.forked.insert(key);
        }
        self.rerank();
    }

    fn with_store(mut self, store: crate::store::Store) -> App {
        self.pinned = store.get_pinned_keys().unwrap_or_default();
        self.hidden = store.get_hidden_keys().unwrap_or_default();
        self.forked = store
            .list_fork_links()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|link| {
                link.fork_session_id
                    .map(|id| SessionKey::new(link.fork_agent, id))
            })
            .collect();
        self.permission_policy = store.get_permission_policy().unwrap_or(None);
        self.store = Some(store);
        self
    }

    fn toggle_pin(&mut self) {
        let Some(key) = self.selected_session().map(|s| s.key()) else {
            return;
        };
        let now_pinned = !self.pinned.contains(&key);
        if now_pinned {
            self.pinned.insert(key.clone());
        } else {
            self.pinned.remove(&key);
        }
        if let Some(store) = &self.store {
            let _ = store.set_pinned(&key, now_pinned);
        }
        self.toast = Some(if now_pinned {
            "pinned".into()
        } else {
            "unpinned".into()
        });
        self.request_rerank();
    }

    fn open_rename(&mut self) {
        if let Some(s) = self.selected_session() {
            self.rename_modal = Some(RenameModal {
                session_key: s.key(),
                cursor: s.title.chars().count(),
                input: s.title.clone(),
            });
        }
    }

    fn commit_rename(&mut self) {
        let Some(modal) = self.rename_modal.take() else {
            return;
        };
        let trimmed = modal.input.trim();
        let new_title = if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        };
        // An empty name brings back the title from the agent's own files.
        let mut restored = None;
        if let Some(store) = &self.store {
            let _ = store.set_title_override(&modal.session_key, new_title);
            if new_title.is_none() {
                restored = store
                    .get_record(&modal.session_key)
                    .ok()
                    .flatten()
                    .map(|record| record.original_title);
            }
        }
        if let Some(&idx) = self.seen.get(&modal.session_key) {
            if let Some(title) = new_title.map(str::to_string).or(restored) {
                self.all[idx].title = title;
            }
        }
        self.toast = Some(if new_title.is_some() {
            "renamed".into()
        } else {
            "title restored".into()
        });
        self.request_rerank();
    }

    fn open_hide(&mut self) {
        if let Some(s) = self.selected_session() {
            self.hide_modal = Some(HideModal {
                session_key: s.key(),
            });
        }
    }

    fn confirm_hide(&mut self) {
        let Some(modal) = self.hide_modal.take() else {
            return;
        };
        let Some(&idx) = self.seen.get(&modal.session_key) else {
            return;
        };
        if let Some(store) = &self.store {
            if store.set_hidden(&modal.session_key, true).is_err() {
                self.toast = Some("hide failed".into());
                return;
            }
        } else {
            self.toast = Some("hide failed".into());
            return;
        }
        let agent = self.all[idx].agent;
        let id = self.all[idx].id.clone();
        self.hidden.insert(modal.session_key);
        self.remove_ids(agent, &[id]);
        self.recompute_agents_present();
        self.toast = Some("hidden".into());
        self.request_rerank();
    }

    /// The search as it is applied. A value still being typed counts as the
    /// suggestion shown after the cursor, so the list already shows what Tab
    /// would give instead of going empty on a half-typed agent name.
    fn parse_live(&self) -> crate::query::ParsedQuery {
        let effective = match self.completion() {
            Some(rest) => format!("{}{rest}", self.query),
            None => self.query.clone(),
        };
        let mut pq = crate::query::parse_query(&effective, super::format::now_unix_secs());
        if let Some(d) = &self.dir_filter {
            pq.filters.merge_dir_include(d.clone());
        }
        pq
    }

    fn ingest(&mut self, s: Session) -> bool {
        let key = s.key();
        if self.hidden.contains(&key) {
            return false;
        }
        match self.seen.get(&key) {
            Some(&idx) => {
                self.all[idx] = s;
                false
            }
            None => {
                self.seen.insert(key, self.all.len());
                self.all.push(s);
                true
            }
        }
    }

    fn remove_ids(&mut self, agent: AgentId, ids: &[String]) -> bool {
        if ids.is_empty() {
            return false;
        }
        let to_drop: std::collections::HashSet<&str> = ids.iter().map(|s| s.as_str()).collect();
        let before = self.all.len();
        self.all
            .retain(|s| s.agent != agent || !to_drop.contains(s.id.as_str()));
        if self.all.len() == before {
            return false;
        }
        self.seen = self
            .all
            .iter()
            .enumerate()
            .map(|(i, s)| (s.key(), i))
            .collect();
        true
    }

    fn recompute_agents_present(&mut self) {
        self.agents_present = AgentId::ALL
            .into_iter()
            .filter(|a| self.all.iter().any(|s| s.agent == *a))
            .collect();
    }

    fn filter_cycle(&self) -> Vec<Option<AgentId>> {
        let mut v = vec![None];
        v.extend(self.agents_present.iter().map(|a| Some(*a)));
        v
    }

    /// The agent tab the search text stands for: the one agent it asks
    /// for. `None` is the All tab, also for a filter no single tab matches.
    fn active_tab(&self) -> Option<AgentId> {
        let filters = self.parse_live().filters;
        match filters.agents_include.as_slice() {
            [agent] if filters.agents_exclude.is_empty() => Some(*agent),
            _ => None,
        }
    }

    /// Tab and Shift+Tab: step through All and the agents that have
    /// sessions, by rewriting the `agent:` filter in the search text.
    fn cycle_filter(&mut self, delta: isize) {
        let cycle = self.filter_cycle();
        if cycle.is_empty() {
            return;
        }
        let active = self.active_tab();
        let cur = cycle.iter().position(|f| *f == active).unwrap_or(0);
        let len = cycle.len() as isize;
        let next = (((cur as isize + delta) % len) + len) % len;
        let agent = cycle[next as usize];
        self.query = crate::query::set_agent(&self.query, agent.map(|a| a.badge()));
        self.cursor = self.query.chars().count();
        self.request_rerank();
    }

    /// The rest of the agent or date value being typed at the end of the
    /// search line, when the cursor is there: "aude" after `agent:cl`.
    fn completion(&self) -> Option<String> {
        if self.cursor < self.query.chars().count() {
            return None;
        }
        // Agents that have sessions first, then every other name hopback knows.
        let mut names: Vec<&str> = self.agents_present.iter().map(|a| a.badge()).collect();
        for agent in AgentId::ALL {
            for name in [agent.badge(), agent.as_str()] {
                if !names.contains(&name) {
                    names.push(name);
                }
            }
        }
        crate::query::completion(&self.query, &names)
    }

    /// Finishes the suggested value. False when there was nothing to finish.
    fn accept_completion(&mut self) -> bool {
        let Some(rest) = self.completion() else {
            return false;
        };
        self.query.push_str(&rest);
        self.cursor = self.query.chars().count();
        self.request_rerank();
        true
    }

    fn request_rerank(&mut self) {
        self.rerank_at = Some(Instant::now() + DEBOUNCE);
    }

    fn rerank(&mut self) {
        let started = Instant::now();
        let prev_key = self.results.get(self.selected).map(|r| r.session.key());

        let empty_fts = HashMap::new();

        let pq = self.parse_live();
        self.results = search::rank(
            &self.all,
            pq.text.trim(),
            &pq.filters,
            &empty_fts,
            search::DEFAULT_LIMIT,
        );

        if !self.pinned.is_empty() {
            let pinned = &self.pinned;
            self.results
                .sort_by_key(|r| !pinned.contains(&r.session.key()));
        }

        self.selected = prev_key
            .as_ref()
            .and_then(|key| self.results.iter().position(|r| r.session.key() == *key))
            .unwrap_or(0);
        self.clamp_selection();
        if self.results.get(self.selected).map(|r| r.session.key()) != prev_key {
            self.preview_scroll = 0;
        }
        self.last_query_ms = started.elapsed().as_secs_f64() * 1000.0;
    }

    fn clamp_selection(&mut self) {
        if self.results.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.results.len() {
            self.selected = self.results.len() - 1;
        }
    }

    /// Indexed sessions per agent, in tab order.
    fn agent_counts(&self) -> Vec<(AgentId, usize)> {
        self.agents_present
            .iter()
            .map(|a| (*a, self.all.iter().filter(|s| s.agent == *a).count()))
            .collect()
    }

    /// Indexed sessions of the agents the search text asks for: all of them
    /// when it names none.
    fn indexed_in_tab(&self) -> usize {
        let filters = self.parse_live().filters;
        self.all
            .iter()
            .filter(|s| agent_in_scope(&filters, s.agent))
            .count()
    }

    fn move_selection(&mut self, delta: isize) {
        if self.results.is_empty() {
            return;
        }
        let max = self.results.len() as isize - 1;
        let next = (self.selected as isize + delta).clamp(0, max) as usize;
        if next != self.selected {
            self.preview_scroll = 0;
        }
        self.selected = next;
    }

    fn selected_session(&self) -> Option<&Session> {
        self.results.get(self.selected).map(|r| &r.session)
    }

    fn begin_resume(&mut self) -> Option<Outcome> {
        let session = self.selected_session()?.clone();
        self.resolve_pending_launch(PendingLaunch::Resume(session))
    }

    fn fork_choices_for(session: &Session) -> Vec<ForkChoice> {
        let mut choices = Vec::new();
        if crate::launcher::can_fork(session) {
            choices.push(ForkChoice::Native);
        }
        choices.extend(
            crate::bridge::translated_targets(session.agent)
                .into_iter()
                .map(ForkChoice::Translated),
        );
        choices
    }

    fn begin_fork(&mut self) -> Option<Outcome> {
        let session = self.selected_session()?.clone();
        let choices = Self::fork_choices_for(&session);
        match choices.as_slice() {
            [] => {
                self.toast = Some(format!(
                    "{} sessions cannot be forked",
                    session.agent.badge()
                ));
                None
            }
            [ForkChoice::Native] => self.resolve_pending_launch(PendingLaunch::Fork(session)),
            [ForkChoice::Translated(target)] => {
                self.resolve_pending_launch(PendingLaunch::TranslatedFork {
                    session,
                    target: *target,
                })
            }
            _ => {
                self.fork_modal = Some(ForkModal {
                    session,
                    choices,
                    selected: 0,
                });
                None
            }
        }
    }

    /// Launches with the permission mode the session had, or the stored
    /// policy when there is one.
    fn resolve_pending_launch(&mut self, pending: PendingLaunch) -> Option<Outcome> {
        let decision = match &pending {
            PendingLaunch::Resume(session) | PendingLaunch::Fork(session) => {
                crate::launcher::launch_permission_decision(
                    session,
                    self.yolo,
                    self.permission_policy,
                )
            }
            PendingLaunch::TranslatedFork { session, target } => {
                crate::launcher::launch_permission_decision_for_agent(
                    *target,
                    self.yolo,
                    self.permission_policy,
                    crate::launcher::inherited_yolo(session),
                )
            }
        };
        let yolo = matches!(decision, crate::launcher::YoloDecision::On);
        Some(pending.into_outcome(yolo))
    }

    fn yolo_for_copy(&self, session: &Session) -> bool {
        matches!(
            crate::launcher::launch_permission_decision(session, self.yolo, self.permission_policy),
            crate::launcher::YoloDecision::On
        )
    }
}

/// True when the search text carries an `agent:` filter, valid or not.
fn query_names_an_agent(query: &str) -> bool {
    crate::query::keyword_spans(query)
        .iter()
        .any(|span| span.keyword == crate::query::Keyword::Agent)
}

/// Whether the agent filters of the search text let this agent through.
fn agent_in_scope(filters: &Filters, agent: AgentId) -> bool {
    !filters.agents_exclude.contains(&agent)
        && !filters.agents_include_unmatched
        && (filters.agents_include.is_empty() || filters.agents_include.contains(&agent))
}

/// The name is edited like the search line: same cursor and delete keys.
fn handle_rename_modal_key(app: &mut App, key: KeyEvent) -> KeyAction {
    let Some(modal) = app.rename_modal.as_mut() else {
        return KeyAction::None;
    };
    match keys::map_key(key) {
        Command::Escape => KeyAction::CancelModal,
        Command::Quit => KeyAction::Quit,
        Command::Resume => KeyAction::CommitRename,
        Command::Edit(op) => {
            keys::apply_edit(&mut modal.input, &mut modal.cursor, op);
            KeyAction::None
        }
        _ => KeyAction::None,
    }
}

fn handle_fork_modal_key(app: &mut App, key: KeyEvent) -> KeyAction {
    let Some(modal) = app.fork_modal.as_mut() else {
        return KeyAction::None;
    };
    match key.code {
        KeyCode::Esc => KeyAction::CancelModal,
        KeyCode::Up | KeyCode::BackTab => {
            modal.selected = modal.selected.saturating_sub(1);
            KeyAction::None
        }
        KeyCode::Down | KeyCode::Tab => {
            if !modal.choices.is_empty() {
                modal.selected = (modal.selected + 1).min(modal.choices.len() - 1);
            }
            KeyAction::None
        }
        KeyCode::Enter => KeyAction::ConfirmForkChoice,
        _ => KeyAction::None,
    }
}

pub fn run(
    initial: Vec<Session>,
    initial_query: String,
    filters: Filters,
    yolo: bool,
    store: Option<crate::store::Store>,
    index_spawn: impl FnOnce(mpsc::Sender<BgEvent>) + Send + 'static,
) -> Result<Outcome> {
    let (tx, rx) = mpsc::channel::<BgEvent>();
    let bg = std::thread::spawn(move || index_spawn(tx));

    let mut terminal = setup_terminal()?;
    let mut app = App::new(initial, initial_query, filters, yolo);
    if let Some(store) = store {
        app = app.with_store(store);
    }
    let outcome = event_loop(&mut terminal, &mut app, &rx);
    restore_terminal(&mut terminal)?;
    let _ = bg.join();
    outcome
}

fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: &mut App,
    rx: &mpsc::Receiver<BgEvent>,
) -> Result<Outcome> {
    let mut updates = 0usize;
    let refresh_started = Instant::now();
    loop {
        let mut got_new = false;
        let mut agents_changed = false;
        while let Ok(ev) = rx.try_recv() {
            match ev {
                BgEvent::Session(s) => {
                    let agent = s.agent;
                    let is_new = app.ingest(*s);
                    if is_new && !app.agents_present.contains(&agent) {
                        agents_changed = true;
                    }
                    updates += 1;
                    got_new = true;
                }
                BgEvent::Deleted { agent, ids } => {
                    if app.remove_ids(agent, &ids) {
                        agents_changed = true;
                        got_new = true;
                    }
                }
                BgEvent::IndexingDone => {
                    app.indexing = false;
                    app.refresh = Refresh::Done {
                        changed: updates,
                        millis: refresh_started.elapsed().as_millis(),
                    };
                    got_new = true;
                }
                BgEvent::IndexingSkipped => {
                    app.indexing = false;
                    app.refresh = Refresh::Skipped;
                    got_new = true;
                }
            }
        }
        if agents_changed {
            app.recompute_agents_present();
        }
        if got_new && app.rerank_at.is_none() {
            app.rerank_at = Some(Instant::now() + DEBOUNCE);
        }

        if let Some(at) = app.rerank_at {
            if Instant::now() >= at {
                app.rerank();
                app.rerank_at = None;
            }
        }

        terminal.draw(|f| super::render::draw(f, app))?;

        if event::poll(TICK)? {
            let action = match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    // A footer message lasts until the next key.
                    app.toast = None;
                    handle_key(app, key)
                }
                Event::Mouse(mouse) => handle_mouse(app, mouse, Instant::now()),
                _ => KeyAction::None,
            };
            if let Some(outcome) = apply_action(app, action) {
                return Ok(outcome);
            }
        }
    }
}

/// Carries out what a key or the mouse asked for. Returns the outcome when
/// the TUI should close.
fn apply_action(app: &mut App, action: KeyAction) -> Option<Outcome> {
    match action {
        KeyAction::Quit => return Some(Outcome::Quit),
        KeyAction::Resume => return app.begin_resume(),
        KeyAction::CancelModal => {
            app.rename_modal = None;
            app.fork_modal = None;
            app.hide_modal = None;
            app.actions_modal = None;
            app.help_open = false;
        }
        KeyAction::TogglePin => app.toggle_pin(),
        KeyAction::OpenRename => app.open_rename(),
        KeyAction::CommitRename => app.commit_rename(),
        KeyAction::OpenHide => app.open_hide(),
        KeyAction::ConfirmHide => app.confirm_hide(),
        KeyAction::Fork => return app.begin_fork(),
        KeyAction::ConfirmForkChoice => {
            if let Some(modal) = app.fork_modal.take() {
                let pending = match modal.choices[modal.selected] {
                    ForkChoice::Native => PendingLaunch::Fork(modal.session),
                    ForkChoice::Translated(target) => PendingLaunch::TranslatedFork {
                        session: modal.session,
                        target,
                    },
                };
                return app.resolve_pending_launch(pending);
            }
        }
        KeyAction::CopyCommand => {
            if let Some(s) = app.selected_session() {
                if let Ok(argv) = crate::launcher::resume_argv(s, app.yolo_for_copy(s)) {
                    copy_to_clipboard(&shell_join(&argv));
                    app.toast = Some("command copied".to_string());
                }
            }
        }
        KeyAction::None => {}
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyAction {
    None,
    Quit,
    Resume,
    CancelModal,
    Fork,
    CopyCommand,

    TogglePin,

    OpenRename,

    CommitRename,

    ConfirmForkChoice,

    OpenHide,

    ConfirmHide,
}

fn handle_key(app: &mut App, key: KeyEvent) -> KeyAction {
    if app.rename_modal.is_some() {
        return handle_rename_modal_key(app, key);
    }
    if app.fork_modal.is_some() {
        return handle_fork_modal_key(app, key);
    }
    if app.hide_modal.is_some() {
        return handle_hide_modal_key(key);
    }

    if app.help_open {
        // Any key closes the help; Ctrl+C still quits.
        app.help_open = false;
        return match keys::map_key(key) {
            Command::Quit => KeyAction::Quit,
            _ => KeyAction::None,
        };
    }
    if app.actions_modal.is_some() {
        return handle_actions_modal_key(app, key);
    }

    match keys::map_key(key) {
        Command::Resume => return KeyAction::Resume,
        Command::Quit => return KeyAction::Quit,
        Command::Escape => {
            if app.query.is_empty() {
                return KeyAction::Quit;
            }
            app.clear_query();
        }
        Command::Move(delta) => app.move_selection(delta),
        // Tab finishes a suggested value first; otherwise it moves a tab on.
        Command::NextTab => {
            if !app.accept_completion() {
                app.cycle_filter(1);
            }
        }
        Command::PrevTab => app.cycle_filter(-1),
        Command::Fork => return KeyAction::Fork,
        Command::Pin => return KeyAction::TogglePin,
        Command::Rename => return KeyAction::OpenRename,
        Command::Hide => return KeyAction::OpenHide,
        Command::OpenActions => app.open_actions(),
        Command::TogglePreview => app.show_preview = !app.show_preview,
        Command::Copy => return KeyAction::CopyCommand,
        Command::Help => app.help_open = true,
        // At the end of the line, → takes the suggestion.
        Command::Edit(keys::EditOp::Right) if app.accept_completion() => {}
        Command::Edit(op) => {
            if keys::apply_edit(&mut app.query, &mut app.cursor, op) {
                app.request_rerank();
            }
        }
        Command::None => {}
    }
    KeyAction::None
}

fn handle_actions_modal_key(app: &mut App, key: KeyEvent) -> KeyAction {
    let Some(modal) = app.actions_modal.as_mut() else {
        return KeyAction::None;
    };
    let command = keys::map_key(key);
    let chosen = match command {
        Command::Escape => return KeyAction::CancelModal,
        Command::Quit => return KeyAction::Quit,
        // Ctrl+O again closes the menu it opened.
        Command::OpenActions => return KeyAction::CancelModal,
        Command::Move(delta) => {
            let last = MenuAction::ALL.len() as isize - 1;
            modal.selected = (modal.selected as isize + delta).clamp(0, last) as usize;
            return KeyAction::None;
        }
        Command::NextTab => {
            modal.selected = (modal.selected + 1).min(MenuAction::ALL.len() - 1);
            return KeyAction::None;
        }
        Command::PrevTab => {
            modal.selected = modal.selected.saturating_sub(1);
            return KeyAction::None;
        }
        Command::Resume => MenuAction::ALL[modal.selected],
        // An action's own shortcut also works from inside the menu.
        other => match MenuAction::from_command(other) {
            Some(action) => action,
            None => return KeyAction::None,
        },
    };
    app.actions_modal = None;
    match chosen {
        MenuAction::Fork => KeyAction::Fork,
        MenuAction::Pin => KeyAction::TogglePin,
        MenuAction::Rename => KeyAction::OpenRename,
        MenuAction::Hide => KeyAction::OpenHide,
        MenuAction::Copy => KeyAction::CopyCommand,
    }
}

/// Click selects a row, double-click resumes it, right-click opens the
/// actions menu for it, and the wheel scrolls the list or, over the preview,
/// the preview.
fn handle_mouse(app: &mut App, mouse: MouseEvent, now: Instant) -> KeyAction {
    if app.any_modal_open() {
        return KeyAction::None;
    }
    let Some(frame) = app.last_layout.get() else {
        return KeyAction::None;
    };
    let wheel = match mouse.kind {
        MouseEventKind::ScrollUp => Some(-keys::WHEEL_LINES),
        MouseEventKind::ScrollDown => Some(keys::WHEEL_LINES),
        _ => None,
    };
    if let Some(delta) = wheel {
        match layout::pane_at(&frame, mouse.column, mouse.row) {
            Some(layout::Pane::Results) => app.move_selection(delta),
            Some(layout::Pane::Preview) => app.scroll_preview(delta),
            None => {}
        }
        return KeyAction::None;
    }

    if mouse.kind == MouseEventKind::Down(MouseButton::Right) {
        let row = layout::row_at(
            frame.results,
            app.table_offset.get(),
            app.results.len(),
            mouse.column,
            mouse.row,
        );
        if let Some(row) = row {
            app.select_row(row);
            app.open_actions();
        }
        app.last_click = None;
        return KeyAction::None;
    }

    if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
        let row = layout::row_at(
            frame.results,
            app.table_offset.get(),
            app.results.len(),
            mouse.column,
            mouse.row,
        );
        let Some(row) = row else {
            app.last_click = None;
            return KeyAction::None;
        };
        let double = app.last_click.is_some_and(|(at, last_row)| {
            last_row == row && now.duration_since(at) <= DOUBLE_CLICK
        });
        app.select_row(row);
        if double {
            app.last_click = None;
            return KeyAction::Resume;
        }
        app.last_click = Some((now, row));
    }
    KeyAction::None
}

fn handle_hide_modal_key(key: KeyEvent) -> KeyAction {
    match key.code {
        KeyCode::Esc => KeyAction::CancelModal,
        KeyCode::Enter => KeyAction::ConfirmHide,
        _ => KeyAction::None,
    }
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    Ok(Terminal::new(backend)?)
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;
    Ok(())
}

fn shell_join(argv: &[String]) -> String {
    argv.iter()
        .map(|a| {
            if a.is_empty() || a.bytes().any(|b| !matches!(b, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'/' | b'.' | b':' | b'=' | b'@' | b'+')) {
                format!("'{}'", a.replace('\'', "'\\''"))
            } else {
                a.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn copy_to_clipboard(text: &str) {
    use base64_osc52 as b64;
    let payload = b64::encode(text.as_bytes());

    let seq = format!("\x1b]52;c;{payload}\x07");
    let mut out = io::stdout();
    let _ = io::Write::write_all(&mut out, seq.as_bytes());
    let _ = io::Write::flush(&mut out);
}

mod base64_osc52 {
    const CHARS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    pub fn encode(data: &[u8]) -> String {
        let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
        for chunk in data.chunks(3) {
            let b = [
                chunk[0],
                *chunk.get(1).unwrap_or(&0),
                *chunk.get(2).unwrap_or(&0),
            ];
            let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | (b[2] as u32);
            out.push(CHARS[((n >> 18) & 63) as usize] as char);
            out.push(CHARS[((n >> 12) & 63) as usize] as char);
            out.push(if chunk.len() > 1 {
                CHARS[((n >> 6) & 63) as usize] as char
            } else {
                '='
            });
            out.push(if chunk.len() > 2 {
                CHARS[(n & 63) as usize] as char
            } else {
                '='
            });
        }
        out
    }
}

pub(super) struct AppView<'a> {
    pub query: &'a str,
    pub cursor: usize,
    pub table_offset: usize,
    /// Selected entry of the actions menu, when it is open.
    pub actions_modal: Option<usize>,
    pub help_open: bool,
    pub selected_is_pinned: bool,
    pub results: &'a [SearchResult],
    pub selected: usize,
    pub show_preview: bool,
    /// Agents the search text asks for; their tabs are filled in.
    pub active_agents: Vec<AgentId>,
    /// True while the search text has no `agent:` filter: the All tab.
    pub all_tab_active: bool,
    /// The rest of the value being typed, offered after the cursor.
    pub completion: Option<String>,
    pub shown: usize,
    /// Indexed sessions behind the active tab.
    pub indexed: usize,
    /// Indexed sessions across all agents.
    pub indexed_all: usize,
    pub agent_counts: Vec<(AgentId, usize)>,
    pub refresh: Refresh,
    pub indexing: bool,
    pub query_ms: f64,
    pub toast: Option<&'a str>,
    pub can_fork_selected: bool,

    pub preview_scroll: u16,

    /// The name being edited and the cursor in it, as a character index.
    pub rename_input: Option<(&'a str, usize)>,
    pub fork_modal: Option<(&'a str, &'a [ForkChoice], usize)>,
    pub hide_modal: bool,
    pub pinned: &'a std::collections::HashSet<SessionKey>,
    pub forked: &'a std::collections::HashSet<SessionKey>,
}

impl App {
    pub(super) fn view(&self) -> AppView<'_> {
        let agent_filters = self.parse_live().filters;
        AppView {
            query: &self.query,
            cursor: self.cursor.min(self.query.chars().count()),
            table_offset: self.table_offset.get(),
            actions_modal: self.actions_modal.as_ref().map(|m| m.selected),
            help_open: self.help_open,
            selected_is_pinned: self
                .selected_session()
                .is_some_and(|s| self.pinned.contains(&s.key())),
            results: &self.results,
            selected: self.selected,
            show_preview: self.show_preview,
            active_agents: agent_filters.agents_include,
            all_tab_active: !query_names_an_agent(&self.query),
            completion: self.completion(),
            shown: self.results.len(),
            indexed: self.indexed_in_tab(),
            indexed_all: self.all.len(),
            agent_counts: self.agent_counts(),
            refresh: self.refresh,
            indexing: self.indexing,
            query_ms: self.last_query_ms,
            toast: self.toast.as_deref(),
            can_fork_selected: self
                .selected_session()
                .is_some_and(|s| !Self::fork_choices_for(s).is_empty()),
            preview_scroll: self.preview_scroll,
            rename_input: self
                .rename_modal
                .as_ref()
                .map(|m| (m.input.as_str(), m.cursor)),
            fork_modal: self
                .fork_modal
                .as_ref()
                .map(|m| (m.session.agent.badge(), m.choices.as_slice(), m.selected)),
            hide_modal: self.hide_modal.is_some(),
            pinned: &self.pinned,
            forked: &self.forked,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AgentId;
    use ratatui::crossterm::event::KeyModifiers;

    fn sess(id: &str, title: &str, ts: i64) -> Session {
        sess_agent(id, title, ts, AgentId::Claude)
    }

    fn sess_agent(id: &str, title: &str, ts: i64, agent: AgentId) -> Session {
        Session {
            id: id.into(),
            agent,
            title: title.into(),
            directory: "/w".into(),
            timestamp: ts,
            content: format!("{title} body"),
            message_count: 1,
            mtime: ts,
            yolo: false,
            source_path: None,
        }
    }

    fn app_with(sessions: Vec<Session>) -> App {
        App::new(sessions, String::new(), Filters::default(), false)
    }

    fn key(agent: AgentId, id: &str) -> SessionKey {
        SessionKey::new(agent, id)
    }

    #[test]
    fn seeded_from_initial_sessions() {
        let app = app_with(vec![
            sess("a", "alpha", 10),
            sess("b", "beta", 20),
            sess("c", "gamma", 30),
        ]);
        assert_eq!(app.all.len(), 3);
    }

    #[test]
    fn ingest_dedupes_by_key() {
        let mut app = app_with(vec![]);
        assert!(app.ingest(sess("a", "first", 1)));
        assert!(!app.ingest(sess("a", "updated", 2)));
        assert!(app.ingest(sess("b", "other", 3)));
        assert_eq!(app.all.len(), 2);
        assert_eq!(
            app.all[app.seen[&key(AgentId::Claude, "a")]].title,
            "updated"
        );
    }

    #[test]
    fn ingest_keeps_same_raw_id_for_different_agents() {
        let mut app = app_with(vec![]);
        assert!(app.ingest(sess_agent("same", "claude", 1, AgentId::Claude)));
        assert!(app.ingest(sess_agent("same", "codex", 2, AgentId::Codex)));
        assert_eq!(app.all.len(), 2);
        assert_eq!(app.seen.len(), 2);
    }

    #[test]
    fn ingest_skips_hidden_sessions() {
        let mut app = app_with(vec![]);
        app.hidden.insert(key(AgentId::Claude, "a"));
        assert!(!app.ingest(sess("a", "hidden", 1)));
        assert!(app.all.is_empty());
    }

    #[test]
    fn remove_ids_drops_rows_and_rebuilds_seen_index() {
        let mut app = app_with(vec![
            sess("a", "alpha", 10),
            sess("b", "beta", 20),
            sess("c", "gamma", 30),
        ]);
        assert!(app.remove_ids(AgentId::Claude, &["b".to_string()]));
        assert_eq!(app.all.len(), 2);
        assert_eq!(app.all[app.seen[&key(AgentId::Claude, "a")]].id, "a");
        assert_eq!(app.all[app.seen[&key(AgentId::Claude, "c")]].id, "c");
        assert!(!app.seen.contains_key(&key(AgentId::Claude, "b")));
        assert!(!app.remove_ids(AgentId::Claude, &["zzz".to_string()]));
        app.rerank();
        assert!(app.results.iter().all(|r| r.session.id != "b"));
    }

    #[test]
    fn rerank_empty_query_lists_newest_first() {
        let mut app = app_with(vec![
            sess("a", "alpha", 10),
            sess("b", "beta", 30),
            sess("c", "gamma", 20),
        ]);
        app.rerank();
        let ids: Vec<_> = app.results.iter().map(|r| r.session.id.clone()).collect();
        assert_eq!(ids, vec!["b", "c", "a"]);
    }

    #[test]
    fn rerank_filters_by_query_and_preserves_selection() {
        let mut app = app_with(vec![
            sess("a", "authentication middleware", 10),
            sess("b", "database schema", 20),
        ]);
        app.rerank();
        app.query = "auth".into();
        app.rerank();
        assert_eq!(app.results.len(), 1);
        assert_eq!(app.results[0].session.id, "a");
    }

    #[test]
    fn navigation_clamps() {
        let mut app = app_with(vec![sess("a", "a", 1), sess("b", "b", 2)]);
        app.rerank();
        app.move_selection(-5);
        assert_eq!(app.selected, 0);
        app.move_selection(100);
        assert_eq!(app.selected, 1);
    }

    #[test]
    fn agents_present_tracks_loaded_agents() {
        let app = app_with(vec![
            sess_agent("a", "a", 1, AgentId::Claude),
            sess_agent("b", "b", 2, AgentId::Codex),
            sess_agent("c", "c", 3, AgentId::Claude),
        ]);
        assert_eq!(app.agents_present, vec![AgentId::Claude, AgentId::Codex]);
    }

    #[test]
    fn tab_writes_the_agent_into_the_search_line() {
        let mut app = app_with(vec![
            sess_agent("a", "a", 1, AgentId::Claude),
            sess_agent("b", "b", 2, AgentId::Codex),
        ]);
        assert_eq!(app.active_tab(), None);
        app.cycle_filter(1);
        assert_eq!(app.query, "agent:claude");
        assert_eq!(app.cursor, 12, "the cursor follows to the end");
        assert_eq!(app.active_tab(), Some(AgentId::Claude));
        app.cycle_filter(1);
        assert_eq!(app.query, "agent:codex");
        assert_eq!(app.active_tab(), Some(AgentId::Codex));
        app.cycle_filter(1);
        assert_eq!(app.query, "", "All removes the filter");
        assert_eq!(app.active_tab(), None);
        app.cycle_filter(-1);
        assert_eq!(app.query, "agent:codex", "Shift+Tab goes the other way");
    }

    #[test]
    fn tab_keeps_the_rest_of_the_search_text() {
        let mut app = app_with(vec![
            sess_agent("a", "auth fix", 1, AgentId::Claude),
            sess_agent("b", "auth plan", 2, AgentId::Codex),
        ]);
        type_text(&mut app, "auth date:week");
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.query, "auth date:week agent:claude");
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.query, "auth date:week agent:codex");
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.query, "auth date:week");
    }

    #[test]
    fn the_tab_follows_what_is_typed() {
        let mut app = app_with(vec![
            sess_agent("a", "a", 1, AgentId::Claude),
            sess_agent("b", "b", 2, AgentId::Codex),
            sess_agent("c", "c", 3, AgentId::Vibe),
        ]);
        assert!(app.view().all_tab_active);

        type_text(&mut app, "agent:codex");
        app.rerank();
        let view = app.view();
        assert!(!view.all_tab_active);
        assert_eq!(view.active_agents, vec![AgentId::Codex]);
        assert_eq!(view.indexed, 1, "the count is the codex tab's");
        assert_eq!(view.indexed_all, 3);

        // Tab moves on from the typed agent instead of adding a second one.
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.query, "agent:vibe");

        // Several agents: their tabs are both filled in, and Tab starts over.
        app.set_query_for_test("agent:claude,codex");
        let view = app.view();
        assert_eq!(view.active_agents, vec![AgentId::Claude, AgentId::Codex]);
        assert_eq!(view.indexed, 2);
        assert_eq!(app.results.len(), 2);
        assert_eq!(app.active_tab(), None);

        // Everything but one agent.
        app.set_query_for_test("agent:!codex");
        let view = app.view();
        assert!(!view.all_tab_active && view.active_agents.is_empty());
        assert_eq!(view.indexed, 2);
    }

    #[test]
    fn every_tab_name_is_a_filter_hopback_understands() {
        for agent in AgentId::ALL {
            assert_eq!(AgentId::resolve(agent.badge()), Some(agent), "{agent:?}");
            let query = crate::query::set_agent("", Some(agent.badge()));
            let parsed = crate::query::parse_query(&query, 0);
            assert_eq!(parsed.filters.agents_include, vec![agent]);
        }
    }

    #[test]
    fn agent_filter_scopes_results() {
        let mut app = app_with(vec![
            sess_agent("a", "a", 1, AgentId::Claude),
            sess_agent("b", "b", 2, AgentId::Codex),
        ]);
        app.cycle_filter(-1);
        app.rerank();
        assert_eq!(app.results.len(), 1);
        assert_eq!(app.results[0].session.id, "b");
        assert_eq!(app.indexed_in_tab(), 1);
        assert_eq!(app.view().indexed_all, 2);
        assert_eq!(
            app.agent_counts(),
            vec![(AgentId::Claude, 1), (AgentId::Codex, 1)]
        );
    }

    #[test]
    fn typed_agent_keyword_filters_live() {
        let mut app = app_with(vec![
            sess_agent("a", "alpha", 1, AgentId::Claude),
            sess_agent("b", "beta", 2, AgentId::Codex),
        ]);
        app.query = "agent:codex".into();
        app.rerank();
        assert_eq!(app.results.len(), 1);
        assert_eq!(app.results[0].session.id, "b");
    }

    #[test]
    fn typed_keyword_does_not_leak_into_free_text() {
        let mut app = app_with(vec![sess_agent("a", "agent", 1, AgentId::Claude)]);
        app.query = "agent:claude".into();
        app.rerank();
        assert_eq!(app.results.len(), 1);
        assert_eq!(app.results[0].session.id, "a");

        app.query = "zzzznomatch".into();
        app.rerank();
        assert!(app.results.is_empty());
    }

    #[test]
    fn agent_from_the_command_line_shows_in_the_search_line() {
        let sessions = || {
            vec![
                sess_agent("a", "a", 1, AgentId::Claude),
                sess_agent("b", "b", 2, AgentId::Codex),
            ]
        };
        let codex = Filters {
            agents_include: vec![AgentId::Codex],
            ..Default::default()
        };
        let mut app = App::new(sessions(), String::new(), codex.clone(), false);
        app.rerank();
        assert_eq!(app.query, "agent:codex");
        assert_eq!(app.cursor, 11);
        assert_eq!(app.active_tab(), Some(AgentId::Codex));
        assert_eq!(app.results.len(), 1);

        // Search words given with it stay in front.
        let app = App::new(sessions(), "auth".to_string(), codex.clone(), false);
        assert_eq!(app.query, "auth agent:codex");

        // An agent already typed in the query wins over the flag.
        let app = App::new(sessions(), "agent:claude".to_string(), codex, false);
        assert_eq!(app.query, "agent:claude");
    }

    #[test]
    fn an_agent_without_sessions_still_shows_and_can_be_cleared() {
        let mut app = App::new(
            vec![sess_agent("a", "a", 1, AgentId::Claude)],
            String::new(),
            Filters {
                agents_include: vec![AgentId::Vibe],
                ..Default::default()
            },
            false,
        );
        app.rerank();
        assert_eq!(
            app.query, "agent:vibe",
            "the reason for an empty list is visible"
        );
        assert!(app.results.is_empty());
        press(&mut app, KeyCode::Esc);
        app.rerank();
        assert_eq!(app.query, "");
        assert_eq!(app.results.len(), 1);
    }

    #[test]
    fn tab_finishes_a_suggested_value_before_it_switches_tabs() {
        let mut app = app_with(vec![
            sess_agent("a", "a", 1, AgentId::Claude),
            sess_agent("b", "b", 2, AgentId::Codex),
        ]);
        type_text(&mut app, "agent:co");
        assert_eq!(app.view().completion.as_deref(), Some("dex"));
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.query, "agent:codex");
        assert_eq!(app.view().completion, None);
        // Nothing left to finish: the next Tab is a tab switch again.
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.query, "");

        // The right arrow at the end of the line takes it too.
        type_text(&mut app, "date:ye");
        assert_eq!(app.view().completion.as_deref(), Some("sterday"));
        press(&mut app, KeyCode::Right);
        assert_eq!(app.query, "date:yesterday");
        assert_eq!(app.cursor, 14);
    }

    #[test]
    fn a_half_typed_value_already_filters_as_its_suggestion() {
        let mut app = app_with(vec![
            sess_agent("a", "a", 1, AgentId::Claude),
            sess_agent("b", "b", 2, AgentId::Codex),
        ]);
        type_text(&mut app, "agent:co");
        app.rerank();
        let ids: Vec<_> = app.results.iter().map(|r| r.session.id.as_str()).collect();
        assert_eq!(ids, vec!["b"], "the list does not go empty while typing");
        assert_eq!(app.view().active_agents, vec![AgentId::Codex]);
        assert_eq!(app.query, "agent:co", "the text itself is not changed");

        // A name nothing completes is still an unknown agent: nothing matches.
        app.set_query_for_test("agent:zz");
        assert!(app.results.is_empty());
    }

    #[test]
    fn suggestions_only_show_at_the_end_of_the_line() {
        let mut app = app_with(vec![sess_agent("a", "a", 1, AgentId::Claude)]);
        type_text(&mut app, "agent:cl");
        assert_eq!(app.view().completion.as_deref(), Some("aude"));
        press(&mut app, KeyCode::Left);
        assert_eq!(app.view().completion, None);
        // In the middle of the line the right arrow only moves the cursor.
        press(&mut app, KeyCode::Right);
        assert_eq!(app.query, "agent:cl");
        assert_eq!(app.cursor, 8);
    }

    #[test]
    fn suggestions_prefer_agents_that_have_sessions() {
        let mut app = app_with(vec![sess_agent("a", "a", 1, AgentId::Codex)]);
        type_text(&mut app, "agent:c");
        assert_eq!(
            app.view().completion.as_deref(),
            Some("odex"),
            "codex has sessions here, claude does not"
        );
        // A full name works as well as the short tab name.
        app.set_query_for_test("agent:anti");
        assert_eq!(app.view().completion.as_deref(), Some("gravity"));
    }

    #[test]
    fn handle_key_typing_and_actions() {
        let mut app = app_with(vec![sess("a", "a", 1)]);
        app.rerank();
        let press = |c: char| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        assert!(matches!(handle_key(&mut app, press('h')), KeyAction::None));
        assert!(matches!(handle_key(&mut app, press('i')), KeyAction::None));
        assert_eq!(app.query, "hi");
        assert!(matches!(
            handle_key(
                &mut app,
                KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)
            ),
            KeyAction::None
        ));
        assert_eq!(app.query, "h");
        assert!(matches!(
            handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            KeyAction::Resume
        ));
        // Esc clears a search first, and only quits once it is empty.
        assert!(matches!(
            handle_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            KeyAction::None
        ));
        assert_eq!(app.query, "");
        assert_eq!(app.cursor, 0);
        assert!(matches!(
            handle_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            KeyAction::Quit
        ));
        assert!(matches!(
            handle_key(
                &mut app,
                KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)
            ),
            KeyAction::Quit
        ));
    }

    fn press(app: &mut App, code: KeyCode) -> KeyAction {
        handle_key(app, KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn press_ctrl(app: &mut App, c: char) -> KeyAction {
        handle_key(app, KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
    }

    fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            press(app, KeyCode::Char(c));
        }
    }

    #[test]
    fn ctrl_c_quits_even_with_a_search_typed() {
        let mut app = app_with(vec![sess("a", "a", 1)]);
        type_text(&mut app, "auth");
        assert_eq!(press_ctrl(&mut app, 'c'), KeyAction::Quit);
    }

    #[test]
    fn search_line_is_edited_at_the_cursor() {
        let mut app = app_with(vec![sess("a", "a", 1)]);
        type_text(&mut app, "fix auth");
        assert_eq!((app.query.as_str(), app.cursor), ("fix auth", 8));

        press(&mut app, KeyCode::Home);
        type_text(&mut app, "re");
        assert_eq!((app.query.as_str(), app.cursor), ("refix auth", 2));

        press(&mut app, KeyCode::End);
        press(&mut app, KeyCode::Left);
        press(&mut app, KeyCode::Left);
        press(&mut app, KeyCode::Delete);
        assert_eq!(app.query, "refix auh");

        press_ctrl(&mut app, 'e');
        press_ctrl(&mut app, 'w');
        assert_eq!(app.query, "refix ");
        press_ctrl(&mut app, 'a');
        press(&mut app, KeyCode::Right);
        press(&mut app, KeyCode::Right);
        press_ctrl(&mut app, 'u');
        assert_eq!((app.query.as_str(), app.cursor), ("fix ", 0));
        press(&mut app, KeyCode::Backspace);
        assert_eq!(app.query, "fix ", "nothing before the cursor to delete");
    }

    #[test]
    fn editing_the_search_reranks_but_moving_the_cursor_does_not() {
        let mut app = app_with(vec![sess("a", "alpha", 1)]);
        app.rerank();
        app.rerank_at = None;
        press(&mut app, KeyCode::Left);
        assert!(app.rerank_at.is_none());
        press(&mut app, KeyCode::Char('x'));
        assert!(app.rerank_at.is_some());
    }

    #[test]
    fn ctrl_p_toggles_the_preview_and_no_longer_pins() {
        let mut app = app_with(vec![sess("a", "a", 1)]);
        app.rerank();
        assert!(app.show_preview);
        assert_eq!(press_ctrl(&mut app, 'p'), KeyAction::None);
        assert!(!app.show_preview);
        assert!(app.pinned.is_empty());
        press_ctrl(&mut app, 'p');
        assert!(app.show_preview);
    }

    #[test]
    fn pin_rename_and_hide_work_straight_from_the_list() {
        let mut app = app_with(vec![sess("a", "old title", 1)]);
        app.rerank();

        let action = press_ctrl(&mut app, 't');
        assert_eq!(action, KeyAction::TogglePin);
        apply_action(&mut app, action);
        assert!(app.pinned.contains(&key(AgentId::Claude, "a")));
        assert_eq!(app.toast.as_deref(), Some("pinned"));
        let action = press_ctrl(&mut app, 't');
        apply_action(&mut app, action);
        assert!(app.pinned.is_empty(), "the same key unpins");

        for open in [press_ctrl(&mut app, 'r'), press(&mut app, KeyCode::F(2))] {
            assert_eq!(open, KeyAction::OpenRename);
            apply_action(&mut app, open);
            assert_eq!(app.rename_modal.as_ref().unwrap().input, "old title");
            let esc = press(&mut app, KeyCode::Esc);
            apply_action(&mut app, esc);
            assert!(app.rename_modal.is_none());
        }

        let action = press_ctrl(&mut app, 'x');
        assert_eq!(action, KeyAction::OpenHide);
        apply_action(&mut app, action);
        assert!(app.hide_modal.is_some(), "hiding still asks first");
        assert_eq!(app.query, "", "none of the keys typed into the search");
    }

    #[test]
    fn unbound_ctrl_keys_are_inert() {
        let mut app = app_with(vec![sess("a", "a", 1)]);
        app.rerank();
        for c in ['`', 'g', 'k'] {
            assert_eq!(press_ctrl(&mut app, c), KeyAction::None, "Ctrl+{c}");
        }
        assert!(app.rename_modal.is_none() && app.hide_modal.is_none());
        assert!(app.show_preview);
        assert_eq!(app.query, "");
    }

    #[test]
    fn actions_menu_runs_an_action_by_its_own_shortcut() {
        let cases = [
            ('f', KeyAction::Fork),
            ('t', KeyAction::TogglePin),
            ('r', KeyAction::OpenRename),
            ('x', KeyAction::OpenHide),
            ('y', KeyAction::CopyCommand),
        ];
        for (letter, expected) in cases {
            let mut app = app_with(vec![sess("a", "a", 1)]);
            app.rerank();
            assert_eq!(press_ctrl(&mut app, 'o'), KeyAction::None);
            assert!(app.actions_modal.is_some(), "Ctrl+O opens the menu");
            assert_eq!(press_ctrl(&mut app, letter), expected, "Ctrl+{letter}");
            assert!(
                app.actions_modal.is_none(),
                "the menu closes after Ctrl+{letter}"
            );
            assert_eq!(app.query, "");
        }
    }

    #[test]
    fn actions_menu_pin_and_rename_take_effect() {
        let mut app = app_with(vec![sess("a", "old title", 1)]);
        app.rerank();
        // Pin is the first entry, Rename the second.
        press_ctrl(&mut app, 'o');
        let action = press(&mut app, KeyCode::Enter);
        assert!(apply_action(&mut app, action).is_none());
        assert!(app.pinned.contains(&key(AgentId::Claude, "a")));

        press_ctrl(&mut app, 'o');
        press(&mut app, KeyCode::Down);
        let action = press(&mut app, KeyCode::Enter);
        apply_action(&mut app, action);
        assert_eq!(app.rename_modal.as_ref().unwrap().input, "old title");
    }

    #[test]
    fn actions_menu_navigation_enter_and_escape() {
        let mut app = app_with(vec![sess("a", "a", 1)]);
        app.rerank();
        press_ctrl(&mut app, 'o');
        // Letters do nothing in the menu and are not typed.
        for c in ['z', 'p', 'r'] {
            assert_eq!(press(&mut app, KeyCode::Char(c)), KeyAction::None);
        }
        assert!(app.actions_modal.is_some());
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Up);
        assert_eq!(app.actions_modal.as_ref().unwrap().selected, 1);
        assert_eq!(press(&mut app, KeyCode::Enter), KeyAction::OpenRename);

        // The selection stops at both ends.
        press_ctrl(&mut app, 'o');
        press(&mut app, KeyCode::Up);
        assert_eq!(app.actions_modal.as_ref().unwrap().selected, 0);
        press(&mut app, KeyCode::PageDown);
        assert_eq!(
            app.actions_modal.as_ref().unwrap().selected,
            MenuAction::ALL.len() - 1
        );

        let esc = press(&mut app, KeyCode::Esc);
        assert_eq!(esc, KeyAction::CancelModal);
        apply_action(&mut app, esc);
        assert!(app.actions_modal.is_none());
        assert_eq!(app.query, "");

        // With nothing selected there is no menu to open.
        let mut empty = app_with(vec![]);
        press_ctrl(&mut empty, 'o');
        assert!(empty.actions_modal.is_none());
    }

    #[test]
    fn f1_opens_help_and_any_key_closes_it() {
        let mut app = app_with(vec![sess("a", "a", 1)]);
        app.rerank();
        assert_eq!(press(&mut app, KeyCode::F(1)), KeyAction::None);
        assert!(app.help_open);
        assert_eq!(press(&mut app, KeyCode::Char('q')), KeyAction::None);
        assert!(!app.help_open);
        assert_eq!(app.query, "", "the closing key is not typed");

        press(&mut app, KeyCode::F(1));
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            KeyAction::None,
            "no resume"
        );
        assert!(!app.help_open);
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn click(app: &mut App, column: u16, row: u16, at: Instant) -> KeyAction {
        handle_mouse(
            app,
            mouse(MouseEventKind::Down(MouseButton::Left), column, row),
            at,
        )
    }

    /// Five sessions on a 160x30 screen: the table's first session is on
    /// screen row 7 and the preview starts at column 96.
    fn mouse_app() -> App {
        let mut app = app_with(
            (0..5)
                .map(|i| sess(&format!("s{i}"), &format!("title {i}"), 100 - i))
                .collect(),
        );
        app.rerank();
        app.record_frame(layout::app(Rect::new(0, 0, 160, 30), true), 0);
        app
    }

    #[test]
    fn click_selects_and_double_click_resumes() {
        let mut app = mouse_app();
        let t0 = Instant::now();
        assert_eq!(click(&mut app, 10, 9, t0), KeyAction::None);
        assert_eq!(app.selected, 2);

        let second = click(&mut app, 12, 9, t0 + Duration::from_millis(200));
        assert_eq!(second, KeyAction::Resume, "second click on the same row");
        assert_eq!(app.selected, 2);
    }

    #[test]
    fn slow_or_different_row_clicks_are_not_a_double_click() {
        let mut app = mouse_app();
        let t0 = Instant::now();
        click(&mut app, 10, 8, t0);
        assert_eq!(
            click(&mut app, 10, 8, t0 + Duration::from_millis(900)),
            KeyAction::None,
            "too slow"
        );
        assert_eq!(
            click(&mut app, 10, 10, t0 + Duration::from_millis(1_000)),
            KeyAction::None,
            "another row"
        );
        assert_eq!(app.selected, 3);
    }

    #[test]
    fn clicks_outside_the_rows_do_nothing() {
        let mut app = mouse_app();
        let t0 = Instant::now();
        for (column, row) in [(10, 0), (10, 2), (10, 6), (10, 20), (120, 9), (10, 29)] {
            assert_eq!(click(&mut app, column, row, t0), KeyAction::None);
            assert_eq!(app.selected, 0, "({column},{row}) must not select");
        }
    }

    #[test]
    fn wheel_scrolls_the_list_or_the_preview_under_the_pointer() {
        let mut app = mouse_app();
        let now = Instant::now();
        handle_mouse(&mut app, mouse(MouseEventKind::ScrollDown, 10, 10), now);
        assert_eq!(app.selected, 3, "three rows per notch over the list");
        assert_eq!(app.preview_scroll, 0);
        handle_mouse(&mut app, mouse(MouseEventKind::ScrollUp, 10, 10), now);
        assert_eq!(app.selected, 0);

        handle_mouse(&mut app, mouse(MouseEventKind::ScrollDown, 120, 10), now);
        assert_eq!(app.selected, 0, "the list stays put over the preview");
        assert_eq!(app.preview_scroll, 3);
        handle_mouse(&mut app, mouse(MouseEventKind::ScrollUp, 120, 10), now);
        handle_mouse(&mut app, mouse(MouseEventKind::ScrollUp, 120, 10), now);
        assert_eq!(app.preview_scroll, 0, "never above the top");

        // Over the status line, search box or footer the wheel does nothing.
        handle_mouse(&mut app, mouse(MouseEventKind::ScrollDown, 10, 2), now);
        assert_eq!((app.selected, app.preview_scroll), (0, 0));
    }

    #[test]
    fn moving_the_selection_resets_the_preview_scroll() {
        let mut app = mouse_app();
        let now = Instant::now();
        handle_mouse(&mut app, mouse(MouseEventKind::ScrollDown, 120, 10), now);
        assert_eq!(app.preview_scroll, 3);
        press(&mut app, KeyCode::Down);
        assert_eq!(app.preview_scroll, 0);
    }

    #[test]
    fn mouse_is_ignored_while_a_modal_is_open() {
        let mut app = mouse_app();
        press_ctrl(&mut app, 'o');
        let now = Instant::now();
        assert_eq!(click(&mut app, 10, 9, now), KeyAction::None);
        handle_mouse(&mut app, mouse(MouseEventKind::ScrollDown, 10, 10), now);
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn clicks_follow_the_table_scroll_offset() {
        let mut app = app_with(
            (0..60)
                .map(|i| sess(&format!("s{i}"), &format!("title {i}"), 1_000 - i))
                .collect(),
        );
        app.rerank();
        app.record_frame(layout::app(Rect::new(0, 0, 160, 30), true), 40);
        click(&mut app, 10, 7, Instant::now());
        assert_eq!(app.selected, 40);
    }

    #[test]
    fn tab_key_cycles_filter() {
        let mut app = app_with(vec![
            sess_agent("a", "a", 1, AgentId::Claude),
            sess_agent("b", "b", 2, AgentId::Codex),
        ]);
        let tab = KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE);
        assert!(matches!(handle_key(&mut app, tab), KeyAction::None));
        assert_eq!(app.active_tab(), Some(AgentId::Claude));
        assert_eq!(app.query, "agent:claude");
    }

    #[test]
    fn right_click_opens_the_actions_menu_for_that_row() {
        let mut app = mouse_app();
        let now = Instant::now();
        let right = mouse(MouseEventKind::Down(MouseButton::Right), 10, 9);
        assert_eq!(handle_mouse(&mut app, right, now), KeyAction::None);
        assert_eq!(app.selected, 2, "the row under the pointer is selected");
        assert!(app.actions_modal.is_some());

        // Outside the rows nothing opens.
        let mut app = mouse_app();
        let outside = mouse(MouseEventKind::Down(MouseButton::Right), 10, 1);
        handle_mouse(&mut app, outside, now);
        assert!(app.actions_modal.is_none());
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn rename_is_edited_with_the_search_line_keys() {
        let mut app = app_with(vec![sess("a", "old title", 1)]);
        app.rerank();
        let open = press_ctrl(&mut app, 'r');
        apply_action(&mut app, open);
        assert_eq!(app.rename_modal.as_ref().unwrap().cursor, 9);

        // Home, then type at the start; Ctrl+W at the end removes a word.
        press(&mut app, KeyCode::Home);
        type_text(&mut app, "my ");
        press(&mut app, KeyCode::End);
        press_ctrl(&mut app, 'w');
        type_text(&mut app, "name");
        assert_eq!(app.rename_modal.as_ref().unwrap().input, "my old name");
        assert_eq!(app.query, "", "the search line is not touched");

        let commit = press(&mut app, KeyCode::Enter);
        assert_eq!(commit, KeyAction::CommitRename);
        apply_action(&mut app, commit);
        assert_eq!(app.all[0].title, "my old name");
        assert_eq!(app.toast.as_deref(), Some("renamed"));
    }

    fn sess_yolo(id: &str, agent: AgentId) -> Session {
        let mut s = sess_agent(id, id, 1, agent);
        s.yolo = true;
        s
    }

    #[test]
    fn resume_starts_at_once_with_the_mode_the_session_had() {
        let mut app = app_with(vec![sess_agent("a", "a", 1, AgentId::Claude)]);
        app.rerank();
        assert!(matches!(
            app.begin_resume(),
            Some(Outcome::Resume { yolo: false, .. })
        ));

        let mut app = app_with(vec![sess_yolo("a", AgentId::Claude)]);
        app.rerank();
        assert!(matches!(
            app.begin_resume(),
            Some(Outcome::Resume { yolo: true, .. })
        ));
    }

    #[test]
    fn begin_resume_direct_for_opencode() {
        let mut app = app_with(vec![sess_agent("a", "a", 1, AgentId::OpenCode)]);
        app.rerank();
        let outcome = app.begin_resume();
        assert!(matches!(outcome, Some(Outcome::Resume { yolo: false, .. })));
    }

    #[test]
    fn the_yolo_flag_and_a_stored_policy_override_the_session() {
        let mut app = App::new(
            vec![sess_agent("a", "a", 1, AgentId::Claude)],
            String::new(),
            Filters::default(),
            true,
        );
        app.rerank();
        assert!(matches!(
            app.begin_resume(),
            Some(Outcome::Resume { yolo: true, .. })
        ));

        let mut app = app_with(vec![sess_agent("a", "a", 1, AgentId::Claude)]);
        app.permission_policy = Some(PermissionPolicy::AlwaysYolo);
        app.rerank();
        assert!(matches!(
            app.begin_resume(),
            Some(Outcome::Resume { yolo: true, .. })
        ));

        let mut app = app_with(vec![sess_yolo("a", AgentId::Claude)]);
        app.permission_policy = Some(PermissionPolicy::AlwaysNormal);
        app.rerank();
        assert!(matches!(
            app.begin_resume(),
            Some(Outcome::Resume { yolo: false, .. })
        ));
    }

    #[test]
    fn a_fork_keeps_the_mode_of_the_session_it_is_made_from() {
        let mut app = app_with(vec![sess_yolo("a", AgentId::Claude)]);
        app.rerank();
        assert!(app.begin_fork().is_none());
        let modal = app.fork_modal.take().unwrap();
        assert_eq!(modal.choices[modal.selected], ForkChoice::Native);
        assert!(matches!(
            app.resolve_pending_launch(PendingLaunch::Fork(modal.session.clone())),
            Some(Outcome::Fork { yolo: true, .. })
        ));
        assert!(matches!(
            app.resolve_pending_launch(PendingLaunch::TranslatedFork {
                session: modal.session,
                target: AgentId::Codex,
            }),
            Some(Outcome::TranslatedFork { yolo: true, .. })
        ));
    }

    #[test]
    fn shell_join_quotes_only_when_needed() {
        assert_eq!(
            shell_join(&["claude".into(), "--resume".into(), "abc-123".into()]),
            "claude --resume abc-123"
        );

        assert_eq!(
            shell_join(&["code".into(), "/my path".into()]),
            "code '/my path'"
        );

        assert_eq!(shell_join(&["x".into(), "a'b".into()]), "x 'a'\\''b'");
    }

    #[test]
    fn base64_encodes_with_padding() {
        assert_eq!(base64_osc52::encode(b""), "");
        assert_eq!(base64_osc52::encode(b"f"), "Zg==");
        assert_eq!(base64_osc52::encode(b"fo"), "Zm8=");
        assert_eq!(base64_osc52::encode(b"foo"), "Zm9v");
        assert_eq!(base64_osc52::encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn toggle_pin_adds_marks_and_floats_to_top() {
        let mut app = app_with(vec![
            sess("a", "alpha", 10),
            sess("b", "beta", 30),
            sess("c", "gamma", 20),
        ]);
        app.rerank();

        app.selected = app
            .results
            .iter()
            .position(|r| r.session.id == "a")
            .unwrap();
        app.toggle_pin();
        assert!(app.pinned.contains(&key(AgentId::Claude, "a")));
        app.rerank();

        assert_eq!(app.results[0].session.id, "a");

        app.selected = app
            .results
            .iter()
            .position(|r| r.session.id == "a")
            .unwrap();
        app.toggle_pin();
        assert!(!app.pinned.contains(&key(AgentId::Claude, "a")));
        app.rerank();
        assert_eq!(app.results[0].session.id, "b");
    }

    #[test]
    fn hide_modal_confirms_and_removes_row() {
        let store = crate::store::Store::open_in_memory().unwrap();
        let mut app = App::new(
            vec![sess("a", "alpha", 10), sess("b", "beta", 20)],
            String::new(),
            Filters::default(),
            false,
        )
        .with_store(store);
        app.rerank();
        app.selected = app
            .results
            .iter()
            .position(|r| r.session.id == "a")
            .unwrap();
        app.open_hide();
        assert!(app.view().hide_modal);

        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert!(matches!(
            handle_key(&mut app, enter),
            KeyAction::ConfirmHide
        ));
        app.confirm_hide();

        assert!(app.hidden.contains(&key(AgentId::Claude, "a")));
        assert!(app
            .store
            .as_ref()
            .unwrap()
            .is_hidden(&key(AgentId::Claude, "a"))
            .unwrap());
        assert!(!app.seen.contains_key(&key(AgentId::Claude, "a")));
        app.rerank();
        assert_eq!(app.results.len(), 1);
        assert_eq!(app.results[0].session.id, "b");
        assert_eq!(app.toast.as_deref(), Some("hidden"));
    }

    #[test]
    fn hide_modal_esc_cancels_without_change() {
        let mut app = app_with(vec![sess("a", "alpha", 10)]);
        app.rerank();
        app.open_hide();
        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        assert!(matches!(handle_key(&mut app, esc), KeyAction::CancelModal));
        app.hide_modal = None;
        assert_eq!(app.all.len(), 1);
        assert!(app.hidden.is_empty());
    }

    #[test]
    fn rename_modal_edits_and_commits() {
        let mut app = app_with(vec![sess("a", "old title", 1)]);
        app.rerank();

        app.open_rename();
        assert_eq!(app.rename_modal.as_ref().unwrap().input, "old title");

        let ctrl_u = KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL);
        handle_key(&mut app, ctrl_u);
        for c in "new name".chars() {
            handle_key(
                &mut app,
                KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
            );
        }
        assert_eq!(app.rename_modal.as_ref().unwrap().input, "new name");

        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert!(matches!(
            handle_key(&mut app, enter),
            KeyAction::CommitRename
        ));
        app.commit_rename();
        assert!(app.rename_modal.is_none());
        assert_eq!(
            app.all[app.seen[&key(AgentId::Claude, "a")]].title,
            "new name"
        );
    }

    #[test]
    fn rename_modal_esc_cancels_without_change() {
        let mut app = app_with(vec![sess("a", "keep me", 1)]);
        app.rerank();
        app.open_rename();
        for c in "scratch".chars() {
            handle_key(
                &mut app,
                KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
            );
        }
        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        assert!(matches!(handle_key(&mut app, esc), KeyAction::CancelModal));

        app.rename_modal = None;
        assert_eq!(
            app.all[app.seen[&key(AgentId::Claude, "a")]].title,
            "keep me"
        );
    }

    #[test]
    fn ctrl_y_requests_copy() {
        let mut app = app_with(vec![sess("a", "a", 1)]);
        app.rerank();
        let ctrl_y = KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL);
        assert!(matches!(
            handle_key(&mut app, ctrl_y),
            KeyAction::CopyCommand
        ));

        let plain_y = KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE);
        assert!(matches!(handle_key(&mut app, plain_y), KeyAction::None));
        assert_eq!(app.query, "y");
    }

    #[test]
    fn ctrl_f_requests_fork_but_plain_f_types() {
        let mut app = app_with(vec![sess("a", "a", 1)]);
        app.rerank();

        let ctrl_f = KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL);
        assert!(matches!(handle_key(&mut app, ctrl_f), KeyAction::Fork));

        let plain_f = KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE);
        assert!(matches!(handle_key(&mut app, plain_f), KeyAction::None));
        assert_eq!(app.query, "f");
    }

    #[test]
    fn initial_query_is_typed_in_and_already_filters() {
        let mut app = App::new(
            vec![
                sess("a", "authentication middleware", 10),
                sess("b", "database schema", 20),
            ],
            "auth".to_string(),
            Filters::default(),
            false,
        );
        app.rerank();
        assert_eq!(
            app.view().query,
            "auth",
            "the query shows in the search line"
        );
        let ids: Vec<_> = app.results.iter().map(|r| r.session.id.as_str()).collect();
        assert_eq!(ids, vec!["a"]);

        // It is ordinary search text: typing and Backspace keep editing it.
        app.query.push('x');
        app.rerank();
        assert!(app.results.is_empty());
        app.query.pop();
        app.rerank();
        assert_eq!(app.results.len(), 1);
    }

    #[test]
    fn view_reports_fork_capability_of_selection() {
        let mut app = app_with(vec![sess_agent("a", "a", 1, AgentId::Claude)]);
        app.rerank();
        assert!(app.view().can_fork_selected);
    }
}

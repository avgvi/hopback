use std::collections::HashMap;
use std::io::{self, Stdout};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::prelude::*;

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
    Deleted { agent: AgentId, ids: Vec<String> },
    IndexingDone,
}

const DEBOUNCE: Duration = Duration::from_millis(50);

const TICK: Duration = Duration::from_millis(16);

const PAGE: isize = 10;

pub(super) struct App {
    all: Vec<Session>,

    seen: HashMap<SessionKey, usize>,

    results: Vec<SearchResult>,

    query: String,

    dir_filter: Option<String>,

    active_filter: Option<AgentId>,

    agents_present: Vec<AgentId>,

    yolo: bool,

    permission_policy: Option<PermissionPolicy>,

    selected: usize,

    show_preview: bool,

    preview_height: u16,

    indexing: bool,

    rerank_at: Option<Instant>,

    last_query_ms: f64,

    toast: Option<String>,

    permission_modal: Option<PermissionModal>,

    rename_modal: Option<RenameModal>,

    fork_modal: Option<ForkModal>,

    pinned: std::collections::HashSet<SessionKey>,

    hidden: std::collections::HashSet<SessionKey>,

    hide_modal: Option<HideModal>,

    store: Option<crate::store::Store>,
}

struct RenameModal {
    session_key: SessionKey,
    input: String,
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
    fn permission_agent(&self) -> AgentId {
        match self {
            PendingLaunch::Resume(session) | PendingLaunch::Fork(session) => session.agent,
            PendingLaunch::TranslatedFork { target, .. } => *target,
        }
    }

    fn action_label(&self) -> &'static str {
        match self {
            PendingLaunch::Resume(_) => "resume",
            PendingLaunch::Fork(_) => "native fork",
            PendingLaunch::TranslatedFork { .. } => "translated fork",
        }
    }

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PermissionChoice {
    NormalOnce,
    YoloOnce,
    AlwaysNormal,
    AlwaysYolo,
    InheritKnown,
}

impl PermissionChoice {
    pub(super) const ALL: [PermissionChoice; 5] = [
        PermissionChoice::NormalOnce,
        PermissionChoice::YoloOnce,
        PermissionChoice::AlwaysNormal,
        PermissionChoice::AlwaysYolo,
        PermissionChoice::InheritKnown,
    ];

    pub(super) const fn label(self) -> &'static str {
        match self {
            PermissionChoice::NormalOnce => "Normal once",
            PermissionChoice::YoloOnce => "Yolo once",
            PermissionChoice::AlwaysNormal => "Always normal",
            PermissionChoice::AlwaysYolo => "Always yolo",
            PermissionChoice::InheritKnown => "Inherit known",
        }
    }

    pub(super) const fn description(self) -> &'static str {
        match self {
            PermissionChoice::NormalOnce => "Launch safely now; ask again next time",
            PermissionChoice::YoloOnce => "Skip prompts now; ask again next time",
            PermissionChoice::AlwaysNormal => "Save normal mode; do not ask again",
            PermissionChoice::AlwaysYolo => "Save yolo mode; do not ask again",
            PermissionChoice::InheritKnown => "Save inherit mode; ask only when unknown",
        }
    }

    fn saved_policy(self) -> Option<PermissionPolicy> {
        match self {
            PermissionChoice::AlwaysNormal => Some(PermissionPolicy::AlwaysNormal),
            PermissionChoice::AlwaysYolo => Some(PermissionPolicy::AlwaysYolo),
            PermissionChoice::InheritKnown => Some(PermissionPolicy::InheritKnown),
            PermissionChoice::NormalOnce | PermissionChoice::YoloOnce => None,
        }
    }
}

struct PermissionModal {
    pending: PendingLaunch,
    selected: usize,
}

impl App {
    fn new(initial: Vec<Session>, query: String, filters: Filters, yolo: bool) -> App {
        let active_filter = filters.agents_include.first().copied();
        let dir_filter = filters.dirs_include.into_iter().next();
        let mut app = App {
            all: Vec::new(),
            seen: HashMap::new(),
            results: Vec::new(),
            query,
            dir_filter,
            active_filter,
            agents_present: Vec::new(),
            yolo,
            permission_policy: None,
            selected: 0,
            show_preview: true,
            preview_height: 12,
            indexing: true,
            rerank_at: Some(Instant::now()),
            last_query_ms: 0.0,
            toast: None,
            permission_modal: None,
            rename_modal: None,
            fork_modal: None,
            pinned: std::collections::HashSet::new(),
            hidden: std::collections::HashSet::new(),
            hide_modal: None,
            store: None,
        };
        for s in initial {
            app.ingest(s);
        }
        app.recompute_agents_present();
        if let Some(a) = app.active_filter {
            if !app.agents_present.contains(&a) {
                app.active_filter = None;
            }
        }
        app
    }

    fn with_store(mut self, store: crate::store::Store) -> App {
        self.pinned = store.get_pinned_keys().unwrap_or_default();
        self.hidden = store.get_hidden_keys().unwrap_or_default();
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
        if let Some(store) = &self.store {
            let _ = store.set_title_override(&modal.session_key, new_title);
        }
        if let Some(&idx) = self.seen.get(&modal.session_key) {
            if let Some(t) = new_title {
                self.all[idx].title = t.to_string();
            }
        }
        self.toast = Some("renamed".into());
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

    fn parse_live(&self) -> crate::query::ParsedQuery {
        let mut pq = crate::query::parse_query(&self.query, super::format::now_unix_secs());
        if let Some(a) = self.active_filter {
            pq.filters.merge_agent_include(a);
        }
        if let Some(d) = &self.dir_filter {
            pq.filters.merge_dir_include(d.clone());
        }
        pq
    }

    fn effective_filters(&self) -> Filters {
        self.parse_live().filters
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

    fn cycle_filter(&mut self, delta: isize) {
        let cycle = self.filter_cycle();
        if cycle.is_empty() {
            return;
        }
        let cur = cycle
            .iter()
            .position(|f| *f == self.active_filter)
            .unwrap_or(0);
        let len = cycle.len() as isize;
        let next = (((cur as isize + delta) % len) + len) % len;
        self.active_filter = cycle[next as usize];
        self.request_rerank();
    }

    fn filtered_total(&self) -> usize {
        let f = self.effective_filters();
        self.all.iter().filter(|s| f.matches_pub(s)).count()
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
            .and_then(|key| self.results.iter().position(|r| r.session.key() == key))
            .unwrap_or(0);
        self.clamp_selection();
        self.last_query_ms = started.elapsed().as_secs_f64() * 1000.0;
    }

    fn clamp_selection(&mut self) {
        if self.results.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.results.len() {
            self.selected = self.results.len() - 1;
        }
    }

    fn resize_preview(&mut self, delta: i16) {
        let next = self.preview_height as i16 + delta;
        self.preview_height = next.clamp(4, 40) as u16;
    }

    fn move_selection(&mut self, delta: isize) {
        if self.results.is_empty() {
            return;
        }
        let max = self.results.len() as isize - 1;
        let next = (self.selected as isize + delta).clamp(0, max);
        self.selected = next as usize;
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
            [] => None,
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

    fn resolve_pending_launch(&mut self, pending: PendingLaunch) -> Option<Outcome> {
        let decision = match &pending {
            PendingLaunch::Resume(session) | PendingLaunch::Fork(session) => {
                crate::launcher::launch_permission_decision(
                    session,
                    self.yolo,
                    self.permission_policy,
                )
            }
            PendingLaunch::TranslatedFork { target, .. } => {
                crate::launcher::launch_permission_decision_for_agent(
                    *target,
                    self.yolo,
                    self.permission_policy,
                )
            }
        };
        match decision {
            crate::launcher::YoloDecision::On => Some(pending.into_outcome(true)),
            crate::launcher::YoloDecision::Off => Some(pending.into_outcome(false)),
            crate::launcher::YoloDecision::Prompt => {
                self.permission_modal = Some(PermissionModal {
                    pending,
                    selected: 0,
                });
                None
            }
        }
    }

    fn confirm_permission_choice(&mut self, modal: PermissionModal) -> Outcome {
        let choice = PermissionChoice::ALL[modal.selected];
        if let Some(policy) = choice.saved_policy() {
            if let Some(store) = &self.store {
                let _ = store.set_permission_policy(policy);
            }
            self.permission_policy = Some(policy);
        }
        let yolo = match choice {
            PermissionChoice::NormalOnce | PermissionChoice::AlwaysNormal => false,
            PermissionChoice::YoloOnce | PermissionChoice::AlwaysYolo => true,
            PermissionChoice::InheritKnown => self.inherited_yolo_for_pending(&modal.pending),
        };
        modal.pending.into_outcome(yolo)
    }

    fn inherited_yolo_for_pending(&self, pending: &PendingLaunch) -> bool {
        let decision = match pending {
            PendingLaunch::Resume(session) | PendingLaunch::Fork(session) => {
                crate::launcher::inherited_yolo_decision(session)
            }
            PendingLaunch::TranslatedFork { target, .. } => {
                crate::launcher::launch_permission_decision_for_agent(
                    *target,
                    false,
                    Some(PermissionPolicy::InheritKnown),
                )
            }
        };
        matches!(decision, crate::launcher::YoloDecision::On)
    }

    fn yolo_for_copy(&self, session: &Session) -> bool {
        matches!(
            crate::launcher::launch_permission_decision(session, self.yolo, self.permission_policy),
            crate::launcher::YoloDecision::On
        )
    }
}

fn handle_permission_modal_key(app: &mut App, key: KeyEvent) -> KeyAction {
    let Some(modal) = app.permission_modal.as_mut() else {
        return KeyAction::None;
    };
    match key.code {
        KeyCode::Esc => KeyAction::CancelModal,
        KeyCode::Up | KeyCode::Left | KeyCode::BackTab => {
            modal.selected = modal.selected.saturating_sub(1);
            KeyAction::None
        }
        KeyCode::Down | KeyCode::Right | KeyCode::Tab => {
            modal.selected = (modal.selected + 1).min(PermissionChoice::ALL.len() - 1);
            KeyAction::None
        }
        KeyCode::Char(c @ '1'..='5') => {
            modal.selected = (c as usize - '1' as usize).min(PermissionChoice::ALL.len() - 1);
            KeyAction::ConfirmPermission
        }
        KeyCode::Enter => KeyAction::ConfirmPermission,
        _ => KeyAction::None,
    }
}

fn handle_rename_modal_key(app: &mut App, key: KeyEvent) -> KeyAction {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let Some(modal) = app.rename_modal.as_mut() else {
        return KeyAction::None;
    };
    match key.code {
        KeyCode::Esc => KeyAction::CancelModal,
        KeyCode::Enter => KeyAction::CommitRename,
        KeyCode::Backspace => {
            modal.input.pop();
            KeyAction::None
        }
        KeyCode::Char('u') if ctrl => {
            modal.input.clear();
            KeyAction::None
        }
        KeyCode::Char(c) if !ctrl => {
            modal.input.push(c);
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
                    if updates > 0 {
                        app.toast = Some(format!(
                            "{updates} session{} updated",
                            if updates == 1 { "" } else { "s" }
                        ));
                    }
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
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match handle_key(app, key) {
                        KeyAction::Quit => return Ok(Outcome::Quit),
                        KeyAction::Resume => {
                            if let Some(outcome) = app.begin_resume() {
                                return Ok(outcome);
                            }
                        }
                        KeyAction::ConfirmPermission => {
                            if let Some(modal) = app.permission_modal.take() {
                                return Ok(app.confirm_permission_choice(modal));
                            }
                        }
                        KeyAction::CancelModal => {
                            app.permission_modal = None;
                            app.rename_modal = None;
                            app.fork_modal = None;
                            app.hide_modal = None;
                        }
                        KeyAction::TogglePin => app.toggle_pin(),
                        KeyAction::OpenRename => app.open_rename(),
                        KeyAction::CommitRename => app.commit_rename(),
                        KeyAction::OpenHide => app.open_hide(),
                        KeyAction::ConfirmHide => app.confirm_hide(),
                        KeyAction::Fork => {
                            if let Some(outcome) = app.begin_fork() {
                                return Ok(outcome);
                            }
                        }
                        KeyAction::ConfirmForkChoice => {
                            if let Some(modal) = app.fork_modal.take() {
                                match modal.choices[modal.selected] {
                                    ForkChoice::Native => {
                                        if let Some(outcome) = app.resolve_pending_launch(
                                            PendingLaunch::Fork(modal.session),
                                        ) {
                                            return Ok(outcome);
                                        }
                                    }
                                    ForkChoice::Translated(target) => {
                                        if let Some(outcome) = app.resolve_pending_launch(
                                            PendingLaunch::TranslatedFork {
                                                session: modal.session,
                                                target,
                                            },
                                        ) {
                                            return Ok(outcome);
                                        }
                                    }
                                }
                            }
                        }
                        KeyAction::CopyCommand => {
                            if let Some(s) = app.selected_session() {
                                if let Ok(argv) =
                                    crate::launcher::resume_argv(s, app.yolo_for_copy(s))
                                {
                                    copy_to_clipboard(&shell_join(&argv));
                                    app.toast = Some("command copied".to_string());
                                }
                            }
                        }
                        KeyAction::None => {}
                    }
                }
            }
        }
    }
}

enum KeyAction {
    None,
    Quit,
    Resume,
    ConfirmPermission,
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
    if app.permission_modal.is_some() {
        return handle_permission_modal_key(app, key);
    }
    if app.rename_modal.is_some() {
        return handle_rename_modal_key(app, key);
    }
    if app.fork_modal.is_some() {
        return handle_fork_modal_key(app, key);
    }
    if app.hide_modal.is_some() {
        return handle_hide_modal_key(key);
    }

    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    match key.code {
        KeyCode::Esc => return KeyAction::Quit,
        KeyCode::Char('c') if ctrl => return KeyAction::Quit,
        KeyCode::Enter => return KeyAction::Resume,

        KeyCode::Char('f') if ctrl => return KeyAction::Fork,

        KeyCode::Char('y') if ctrl => return KeyAction::CopyCommand,

        KeyCode::Char('p') if ctrl => return KeyAction::TogglePin,
        KeyCode::Char('r') if ctrl => return KeyAction::OpenRename,
        KeyCode::Char('x') if ctrl => return KeyAction::OpenHide,

        KeyCode::Up if ctrl => app.resize_preview(2),
        KeyCode::Down if ctrl => app.resize_preview(-2),
        KeyCode::Up => app.move_selection(-1),
        KeyCode::Down => app.move_selection(1),
        KeyCode::PageUp => app.move_selection(-PAGE),
        KeyCode::PageDown => app.move_selection(PAGE),

        KeyCode::Tab if shift => app.cycle_filter(-1),
        KeyCode::Tab => app.cycle_filter(1),
        KeyCode::BackTab => app.cycle_filter(-1),

        KeyCode::Backspace => {
            app.query.pop();
            app.request_rerank();
        }
        KeyCode::Char('u') if ctrl => {
            app.query.clear();
            app.request_rerank();
        }

        KeyCode::Char('`') if ctrl => app.show_preview = !app.show_preview,
        KeyCode::Char(c) if !ctrl => {
            app.query.push(c);
            app.request_rerank();
        }
        _ => {}
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
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    Ok(Terminal::new(backend)?)
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
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
    pub results: &'a [SearchResult],
    pub selected: usize,
    pub show_preview: bool,
    pub active_filter: Option<AgentId>,
    pub agents_present: &'a [AgentId],
    pub shown: usize,
    pub total: usize,
    pub indexing: bool,
    pub query_ms: f64,
    pub toast: Option<&'a str>,
    pub can_fork_selected: bool,

    pub preview_height: u16,

    pub permission_modal: Option<(&'a str, &'static str, usize)>,

    pub rename_input: Option<&'a str>,
    pub fork_modal: Option<(&'a str, &'a [ForkChoice], usize)>,
    pub hide_modal: bool,
    pub pinned: &'a std::collections::HashSet<SessionKey>,
}

impl App {
    pub(super) fn view(&self) -> AppView<'_> {
        AppView {
            query: &self.query,
            results: &self.results,
            selected: self.selected,
            show_preview: self.show_preview,
            active_filter: self.active_filter,
            agents_present: &self.agents_present,
            shown: self.results.len(),
            total: self.filtered_total(),
            indexing: self.indexing,
            query_ms: self.last_query_ms,
            toast: self.toast.as_deref(),
            can_fork_selected: self
                .selected_session()
                .is_some_and(|s| !Self::fork_choices_for(s).is_empty()),
            preview_height: self.preview_height,
            permission_modal: self.permission_modal.as_ref().map(|m| {
                (
                    m.pending.permission_agent().badge(),
                    m.pending.action_label(),
                    m.selected,
                )
            }),
            rename_input: self.rename_modal.as_ref().map(|m| m.input.as_str()),
            fork_modal: self
                .fork_modal
                .as_ref()
                .map(|m| (m.session.agent.badge(), m.choices.as_slice(), m.selected)),
            hide_modal: self.hide_modal.is_some(),
            pinned: &self.pinned,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AgentId;

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
    fn cycle_filter_walks_all_then_present_agents() {
        let mut app = app_with(vec![
            sess_agent("a", "a", 1, AgentId::Claude),
            sess_agent("b", "b", 2, AgentId::Codex),
        ]);
        assert_eq!(app.active_filter, None);
        app.cycle_filter(1);
        assert_eq!(app.active_filter, Some(AgentId::Claude));
        app.cycle_filter(1);
        assert_eq!(app.active_filter, Some(AgentId::Codex));
        app.cycle_filter(1);
        assert_eq!(app.active_filter, None);
        app.cycle_filter(-1);
        assert_eq!(app.active_filter, Some(AgentId::Codex));
    }

    #[test]
    fn active_filter_scopes_results() {
        let mut app = app_with(vec![
            sess_agent("a", "a", 1, AgentId::Claude),
            sess_agent("b", "b", 2, AgentId::Codex),
        ]);
        app.active_filter = Some(AgentId::Codex);
        app.rerank();
        assert_eq!(app.results.len(), 1);
        assert_eq!(app.results[0].session.id, "b");
        assert_eq!(app.filtered_total(), 1);
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
    fn chip_and_typed_agent_union() {
        let mut app = app_with(vec![
            sess_agent("a", "alpha", 1, AgentId::Claude),
            sess_agent("b", "beta", 2, AgentId::Codex),
            sess_agent("c", "gamma", 3, AgentId::Vibe),
        ]);
        app.active_filter = Some(AgentId::Claude);
        app.query = "agent:codex".into();
        app.rerank();
        let ids: std::collections::HashSet<_> =
            app.results.iter().map(|r| r.session.id.as_str()).collect();
        assert_eq!(ids.len(), 2);
        assert!(
            ids.contains("a") && ids.contains("b"),
            "chip+typed agents both shown"
        );
        assert!(!ids.contains("c"), "vibe (neither chip nor typed) excluded");
    }

    #[test]
    fn initial_agent_filter_from_cli() {
        let app = App::new(
            vec![
                sess_agent("a", "a", 1, AgentId::Claude),
                sess_agent("b", "b", 2, AgentId::Codex),
            ],
            String::new(),
            Filters {
                agents_include: vec![AgentId::Codex],
                ..Default::default()
            },
            false,
        );
        assert_eq!(app.active_filter, Some(AgentId::Codex));
    }

    #[test]
    fn cli_filter_for_absent_agent_falls_back_to_all() {
        let app = App::new(
            vec![
                sess_agent("a", "a", 1, AgentId::Claude),
                sess_agent("b", "b", 2, AgentId::Codex),
            ],
            String::new(),
            Filters {
                agents_include: vec![AgentId::Vibe],
                ..Default::default()
            },
            false,
        );
        assert_eq!(app.active_filter, None);
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

    #[test]
    fn tab_key_cycles_filter() {
        let mut app = app_with(vec![
            sess_agent("a", "a", 1, AgentId::Claude),
            sess_agent("b", "b", 2, AgentId::Codex),
        ]);
        let tab = KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE);
        assert!(matches!(handle_key(&mut app, tab), KeyAction::None));
        assert_eq!(app.active_filter, Some(AgentId::Claude));
    }

    #[test]
    fn begin_resume_opens_permission_modal_for_yolo_capable_agent_without_policy() {
        let mut app = app_with(vec![sess_agent("a", "a", 1, AgentId::Claude)]);
        app.rerank();
        assert!(app.begin_resume().is_none());
        assert!(app.permission_modal.is_some());
        assert_eq!(app.permission_modal.as_ref().unwrap().selected, 0);
    }

    #[test]
    fn begin_resume_direct_for_opencode() {
        let mut app = app_with(vec![sess_agent("a", "a", 1, AgentId::OpenCode)]);
        app.rerank();
        let outcome = app.begin_resume();
        assert!(matches!(outcome, Some(Outcome::Resume { yolo: false, .. })));
        assert!(app.permission_modal.is_none());
    }

    #[test]
    fn begin_resume_skips_modal_under_yolo_flag() {
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
        assert!(app.permission_modal.is_none());
    }

    #[test]
    fn saved_always_yolo_policy_skips_permission_modal() {
        let mut app = app_with(vec![sess_agent("a", "a", 1, AgentId::Claude)]);
        app.permission_policy = Some(PermissionPolicy::AlwaysYolo);
        app.rerank();
        assert!(matches!(
            app.begin_resume(),
            Some(Outcome::Resume { yolo: true, .. })
        ));
        assert!(app.permission_modal.is_none());
    }

    #[test]
    fn permission_modal_keys_move_and_confirm() {
        let mut app = app_with(vec![sess_agent("a", "a", 1, AgentId::Claude)]);
        app.rerank();
        app.begin_resume();

        let down = KeyEvent::new(KeyCode::Down, KeyModifiers::NONE);
        assert!(matches!(handle_key(&mut app, down), KeyAction::None));
        assert_eq!(app.permission_modal.as_ref().unwrap().selected, 1);

        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert!(matches!(
            handle_key(&mut app, enter),
            KeyAction::ConfirmPermission
        ));
    }

    #[test]
    fn permission_modal_digits_confirm_and_esc_cancels() {
        let mut app = app_with(vec![sess_agent("a", "a", 1, AgentId::Claude)]);
        app.rerank();
        app.begin_resume();
        let four = KeyEvent::new(KeyCode::Char('4'), KeyModifiers::NONE);
        assert!(matches!(
            handle_key(&mut app, four),
            KeyAction::ConfirmPermission
        ));
        app.begin_resume();
        let esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        assert!(matches!(handle_key(&mut app, esc), KeyAction::CancelModal));
    }

    #[test]
    fn confirming_permission_choice_launches_and_saves_policy_when_requested() {
        let mut app = app_with(vec![sess_agent("a", "a", 1, AgentId::Claude)]);
        app.rerank();
        app.begin_resume();
        let mut modal = app.permission_modal.take().unwrap();
        modal.selected = 3;
        let outcome = app.confirm_permission_choice(modal);
        assert!(matches!(outcome, Outcome::Resume { yolo: true, .. }));
        assert_eq!(app.permission_policy, Some(PermissionPolicy::AlwaysYolo));
    }

    #[test]
    fn begin_fork_opens_permission_modal_for_yolo_capable_native_fork() {
        let mut app = app_with(vec![sess_agent("a", "a", 1, AgentId::Claude)]);
        app.rerank();
        assert!(app.begin_fork().is_none());
        let modal = app.fork_modal.take().unwrap();
        assert_eq!(modal.choices[modal.selected], ForkChoice::Native);
        assert!(app
            .resolve_pending_launch(PendingLaunch::Fork(modal.session))
            .is_none());
        assert!(matches!(
            app.permission_modal.as_ref().map(|m| &m.pending),
            Some(PendingLaunch::Fork(_))
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
    fn resize_preview_clamps() {
        let mut app = app_with(vec![sess("a", "a", 1)]);
        assert_eq!(app.preview_height, 12);
        app.resize_preview(2);
        assert_eq!(app.preview_height, 14);

        for _ in 0..20 {
            app.resize_preview(-2);
        }
        assert_eq!(app.preview_height, 4);

        for _ in 0..40 {
            app.resize_preview(2);
        }
        assert_eq!(app.preview_height, 40);
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
    fn ctrl_p_toggles_pin_but_plain_p_types() {
        let mut app = app_with(vec![sess("a", "a", 1)]);
        app.rerank();
        let ctrl_p = KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL);
        assert!(matches!(handle_key(&mut app, ctrl_p), KeyAction::TogglePin));
        let plain_p = KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE);
        assert!(matches!(handle_key(&mut app, plain_p), KeyAction::None));
        assert_eq!(app.query, "p");
    }

    #[test]
    fn ctrl_x_opens_hide_but_plain_x_types() {
        let mut app = app_with(vec![sess("a", "a", 1)]);
        app.rerank();
        let ctrl_x = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL);
        assert!(matches!(handle_key(&mut app, ctrl_x), KeyAction::OpenHide));
        let plain_x = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE);
        assert!(matches!(handle_key(&mut app, plain_x), KeyAction::None));
        assert_eq!(app.query, "x");
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

        let ctrl_r = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL);
        assert!(matches!(
            handle_key(&mut app, ctrl_r),
            KeyAction::OpenRename
        ));
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
    fn view_reports_fork_capability_of_selection() {
        let mut app = app_with(vec![sess_agent("a", "a", 1, AgentId::Claude)]);
        app.rerank();
        assert!(app.view().can_fork_selected);
    }
}

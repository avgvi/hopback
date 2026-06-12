use std::collections::HashMap;
use std::sync::mpsc;
use std::thread;

use anyhow::Result;

use crate::adapters::{registry, Adapter, KnownSessions};
use crate::config::AgentId;
use crate::model::{Session, SessionKey};
use crate::store::Store;

type OnSession<'a> = &'a mut dyn FnMut(&Session);
type OnDeleted<'a> = &'a mut dyn FnMut(AgentId, &[String]);

struct AdapterPass {
    agent: AgentId,
    upserts: Vec<Session>,
    deleted: Vec<String>,

    uses_fastpath: bool,
    dir_mtimes: Vec<(String, i64)>,
    removed_dirs: Vec<String>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct IndexStats {
    pub upserted: usize,
    pub deleted: usize,

    pub skipped_dirs: usize,
}

pub fn index(store: &mut Store, on_session: Option<OnSession<'_>>) -> Result<IndexStats> {
    run(store, registry::available(), false, on_session, None)
}

pub fn index_streaming(
    store: &mut Store,
    on_session: Option<OnSession<'_>>,
    on_deleted: Option<OnDeleted<'_>>,
) -> Result<IndexStats> {
    run(store, registry::available(), false, on_session, on_deleted)
}

pub fn rebuild(store: &mut Store, on_session: Option<OnSession<'_>>) -> Result<IndexStats> {
    run(store, registry::available(), true, on_session, None)
}

pub fn index_with(
    store: &mut Store,
    adapters: Vec<Box<dyn Adapter>>,
    force_rebuild: bool,
    on_session: Option<OnSession<'_>>,
) -> Result<IndexStats> {
    run(store, adapters, force_rebuild, on_session, None)
}

pub fn index_with_streaming(
    store: &mut Store,
    adapters: Vec<Box<dyn Adapter>>,
    force_rebuild: bool,
    on_session: Option<OnSession<'_>>,
    on_deleted: Option<OnDeleted<'_>>,
) -> Result<IndexStats> {
    run(store, adapters, force_rebuild, on_session, on_deleted)
}

fn run(
    store: &mut Store,
    adapters: Vec<Box<dyn Adapter>>,
    force_rebuild: bool,
    mut on_session: Option<OnSession<'_>>,
    mut on_deleted: Option<OnDeleted<'_>>,
) -> Result<IndexStats> {
    if force_rebuild {
        store.clear_sessions()?;
    }

    let known: KnownSessions = if force_rebuild {
        HashMap::new()
    } else {
        store.get_known_sessions()?
    };

    let mut dir_mtimes_by_agent: HashMap<AgentId, HashMap<String, i64>> = HashMap::new();
    for a in &adapters {
        if a.uses_dir_mtime_fastpath() {
            dir_mtimes_by_agent.insert(a.id(), store.get_dir_mtimes(a.id())?);
        }
    }

    let (tx, rx) = mpsc::channel::<Session>();

    let passes: Vec<Result<AdapterPass>> = thread::scope(|scope| {
        let mut handles = Vec::new();
        for adapter in &adapters {
            let known = &known;
            let known_dirs = dir_mtimes_by_agent.get(&adapter.id()).cloned();
            let tx = tx.clone();
            let adapter = adapter.as_ref();
            handles.push(scope.spawn(move || scan_one(adapter, known, known_dirs, tx)));
        }

        drop(tx);

        for session in rx.iter() {
            if let Some(cb) = on_session.as_deref_mut() {
                cb(&session);
            }
        }

        handles
            .into_iter()
            .map(|h| {
                h.join()
                    .unwrap_or_else(|_| Err(anyhow::anyhow!("adapter worker panicked")))
            })
            .collect()
    });

    let mut stats = IndexStats::default();
    for pass in passes {
        let pass = pass?;
        stats.upserted += pass.upserts.len();
        stats.deleted += pass.deleted.len();

        if !pass.deleted.is_empty() {
            if let Some(cb) = on_deleted.as_deref_mut() {
                cb(pass.agent, &pass.deleted);
            }
        }
        let (dir_agent, dirs, removed) = if pass.uses_fastpath {
            (
                Some(pass.agent),
                pass.dir_mtimes.as_slice(),
                pass.removed_dirs.as_slice(),
            )
        } else {
            (None, [].as_slice(), [].as_slice())
        };
        store.apply_pass(
            pass.agent,
            &pass.upserts,
            &pass.deleted,
            dir_agent,
            dirs,
            removed,
        )?;
    }

    let _ = store.reconcile_fork_links();

    Ok(stats)
}

fn scan_one(
    adapter: &dyn Adapter,
    known: &KnownSessions,
    known_dirs: Option<HashMap<String, i64>>,
    tx: mpsc::Sender<Session>,
) -> Result<AdapterPass> {
    let agent = adapter.id();

    if !adapter.uses_dir_mtime_fastpath() {
        let mut emit = |s: &Session| {
            let _ = tx.send(s.clone());
        };
        let res = adapter.find_incremental(known, &mut emit)?;
        return Ok(AdapterPass {
            agent,
            upserts: res.upserts,
            deleted: res.deleted,
            uses_fastpath: false,
            dir_mtimes: Vec::new(),
            removed_dirs: Vec::new(),
        });
    }

    let known_dirs = known_dirs.unwrap_or_default();
    let current_dirs = adapter.scan_dirs()?;
    if !adapter.is_available() {
        let deleted = known
            .iter()
            .filter(|(key, _)| key.agent == agent)
            .map(|(key, _)| key.id.clone())
            .collect();
        let removed_dirs = known_dirs.keys().cloned().collect();
        return Ok(AdapterPass {
            agent,
            upserts: Vec::new(),
            deleted,
            uses_fastpath: true,
            dir_mtimes: Vec::new(),
            removed_dirs,
        });
    }

    let current = adapter.scan()?;
    let mut upserts = Vec::new();

    for (id, (path, mtime)) in &current {
        let key = SessionKey::new(agent, id.clone());

        let dir = path
            .parent()
            .and_then(|p| p.to_str())
            .unwrap_or("")
            .to_string();

        if let (Some(known_dir_mtime), Some(known_session_mtime)) =
            (known_dirs.get(&dir), known.get(&key))
        {
            if current_dirs.get(&dir) == Some(known_dir_mtime) && *known_session_mtime >= *mtime {
                continue;
            }
        }

        let changed = match known.get(&key) {
            None => true,
            Some(known_mtime) => *mtime > *known_mtime,
        };
        if !changed {
            continue;
        }
        match adapter.parse(path) {
            Ok(Some(mut session)) => {
                session.mtime = *mtime;
                let _ = tx.send(session.clone());
                upserts.push(session);
            }
            Ok(None) => {}
            Err(_) => {}
        }
    }

    let deleted = known
        .iter()
        .filter(|(key, _)| key.agent == agent && !current.contains_key(&key.id))
        .map(|(key, _)| key.id.clone())
        .collect();

    let dir_mtimes = current_dirs.iter().map(|(d, m)| (d.clone(), *m)).collect();
    let removed_dirs = known_dirs
        .keys()
        .filter(|d| !current_dirs.contains_key(*d))
        .cloned()
        .collect();

    Ok(AdapterPass {
        agent,
        upserts,
        deleted,
        uses_fastpath: true,
        dir_mtimes,
        removed_dirs,
    })
}

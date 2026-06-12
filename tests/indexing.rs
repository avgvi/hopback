use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use aiz::adapters::{mtime_nanos, Adapter};
use aiz::config::AgentId;
use aiz::index;
use aiz::model::{Session, SessionKey};
use aiz::store::Store;

fn write(path: &Path, contents: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

struct CountingAdapter {
    root: PathBuf,
    parse_calls: Arc<AtomicUsize>,
    fastpath: bool,
}

impl CountingAdapter {
    fn new(root: PathBuf, fastpath: bool) -> CountingAdapter {
        CountingAdapter {
            root,
            parse_calls: Arc::new(AtomicUsize::new(0)),
            fastpath,
        }
    }
}

impl Adapter for CountingAdapter {
    fn id(&self) -> AgentId {
        AgentId::Claude
    }
    fn is_available(&self) -> bool {
        self.root.is_dir()
    }
    fn uses_dir_mtime_fastpath(&self) -> bool {
        self.fastpath
    }
    fn scan_dirs(&self) -> anyhow::Result<HashMap<String, i64>> {
        let mut out = HashMap::new();
        if let Ok(entries) = std::fs::read_dir(&self.root) {
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    if let Ok(m) = mtime_nanos(&p) {
                        out.insert(p.to_string_lossy().to_string(), m);
                    }
                }
            }
        }
        Ok(out)
    }
    fn scan(&self) -> anyhow::Result<HashMap<String, (PathBuf, i64)>> {
        let mut out = HashMap::new();
        let projects = match std::fs::read_dir(&self.root) {
            Ok(p) => p,
            Err(_) => return Ok(out),
        };
        for project in projects.flatten() {
            let pdir = project.path();
            if !pdir.is_dir() {
                continue;
            }
            if let Ok(files) = std::fs::read_dir(&pdir) {
                for f in files.flatten() {
                    let path = f.path();
                    if path.extension().and_then(|x| x.to_str()) != Some("jsonl") {
                        continue;
                    }
                    let id = path.file_stem().unwrap().to_string_lossy().to_string();
                    let m = mtime_nanos(&path).unwrap();
                    out.insert(id, (path, m));
                }
            }
        }
        Ok(out)
    }
    fn parse(&self, path: &Path) -> anyhow::Result<Option<Session>> {
        self.parse_calls.fetch_add(1, Ordering::SeqCst);
        let id = path.file_stem().unwrap().to_string_lossy().to_string();
        let bytes = std::fs::read(path)?;
        Ok(aiz::adapters::claude::parse_jsonl(
            &bytes,
            &id,
            mtime_nanos(path).unwrap_or(0),
            "",
        ))
    }
    fn resume_command(&self, session: &Session, _yolo: bool) -> Vec<String> {
        vec!["claude".into(), "--resume".into(), session.id.clone()]
    }
}

fn session_line(prompt: &str) -> String {
    format!(
        "{{\"type\":\"user\",\"cwd\":\"/w\",\"message\":{{\"content\":\"{prompt}\"}}}}\n{{\"type\":\"assistant\",\"message\":{{\"content\":\"ok\"}}}}\n"
    )
}

fn key(id: &str) -> SessionKey {
    SessionKey::new(AgentId::Claude, id)
}

#[test]
fn first_run_indexes_then_warm_run_does_no_parse_work() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("projects");
    write(
        &root.join("proj/aaa.jsonl"),
        &session_line("first prompt here"),
    );
    write(
        &root.join("proj/bbb.jsonl"),
        &session_line("second prompt here"),
    );

    let mut store = Store::open_in_memory().unwrap();

    let adapter = CountingAdapter::new(root.clone(), false);
    let calls = adapter.parse_calls.clone();
    let mut streamed = 0usize;
    let stats = index::index_with(
        &mut store,
        vec![Box::new(adapter)],
        false,
        Some(&mut |_s| streamed += 1),
    )
    .unwrap();
    assert_eq!(stats.upserted, 2);
    assert_eq!(
        streamed, 2,
        "all parsed sessions should stream to the callback"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(store.session_count().unwrap(), 2);

    let adapter2 = CountingAdapter::new(root.clone(), false);
    let calls2 = adapter2.parse_calls.clone();
    let stats2 = index::index_with(&mut store, vec![Box::new(adapter2)], false, None).unwrap();
    assert_eq!(
        calls2.load(Ordering::SeqCst),
        0,
        "warm run must not re-parse"
    );
    assert_eq!(stats2.upserted, 0);
    assert_eq!(store.session_count().unwrap(), 2);
}

#[test]
fn deleting_a_file_removes_it_next_run() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("projects");
    write(
        &root.join("proj/aaa.jsonl"),
        &session_line("keep me around"),
    );
    write(
        &root.join("proj/bbb.jsonl"),
        &session_line("delete me later"),
    );

    let mut store = Store::open_in_memory().unwrap();
    index::index_with(
        &mut store,
        vec![Box::new(CountingAdapter::new(root.clone(), false))],
        false,
        None,
    )
    .unwrap();
    assert_eq!(store.session_count().unwrap(), 2);

    std::fs::remove_file(root.join("proj/bbb.jsonl")).unwrap();
    let stats = index::index_with(
        &mut store,
        vec![Box::new(CountingAdapter::new(root.clone(), false))],
        false,
        None,
    )
    .unwrap();
    assert_eq!(stats.deleted, 1);
    assert_eq!(store.session_count().unwrap(), 1);
    assert!(store.get_session(&key("aaa")).unwrap().is_some());
    assert!(store.get_session(&key("bbb")).unwrap().is_none());
}

#[test]
fn index_streaming_reports_deleted_ids() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("projects");
    write(
        &root.join("proj/aaa.jsonl"),
        &session_line("keep me around"),
    );
    write(
        &root.join("proj/bbb.jsonl"),
        &session_line("delete me later"),
    );

    let mut store = Store::open_in_memory().unwrap();
    index::index_with(
        &mut store,
        vec![Box::new(CountingAdapter::new(root.clone(), false))],
        false,
        None,
    )
    .unwrap();
    assert_eq!(store.session_count().unwrap(), 2);

    std::fs::remove_file(root.join("proj/bbb.jsonl")).unwrap();
    let mut deleted: Vec<(AgentId, String)> = Vec::new();
    {
        let mut on_deleted = |agent: AgentId, ids: &[String]| {
            deleted.extend(ids.iter().map(|id| (agent, id.clone())))
        };
        let stats = index::index_with_streaming(
            &mut store,
            vec![Box::new(CountingAdapter::new(root.clone(), false))],
            false,
            None,
            Some(&mut on_deleted),
        )
        .unwrap();
        assert_eq!(stats.deleted, 1);
    }
    assert_eq!(
        deleted,
        vec![(AgentId::Claude, "bbb".to_string())],
        "deleted agent+id must reach the callback"
    );
    assert!(store.get_session(&key("bbb")).unwrap().is_none());
}

#[test]
fn rebuild_reconstructs_from_files() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("projects");
    write(
        &root.join("proj/aaa.jsonl"),
        &session_line("a real conversation"),
    );

    let mut store = Store::open_in_memory().unwrap();
    index::index_with(
        &mut store,
        vec![Box::new(CountingAdapter::new(root.clone(), false))],
        false,
        None,
    )
    .unwrap();

    store
        .upsert_sessions(&[Session {
            id: "ghost".into(),
            agent: AgentId::Claude,
            title: "ghost".into(),
            directory: "/w".into(),
            timestamp: 0,
            content: "x".into(),
            message_count: 1,
            mtime: 0,
            yolo: false,
            source_path: None,
        }])
        .unwrap();
    assert_eq!(store.session_count().unwrap(), 2);

    let stats = index::index_with(
        &mut store,
        vec![Box::new(CountingAdapter::new(root.clone(), false))],
        true,
        None,
    )
    .unwrap();
    assert_eq!(stats.upserted, 1);
    assert_eq!(store.session_count().unwrap(), 1);
    assert!(store.get_session(&key("ghost")).unwrap().is_none());
}

#[test]
fn dir_mtime_fastpath_skips_unchanged_directories() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("projects");
    write(
        &root.join("proj/aaa.jsonl"),
        &session_line("fastpath one here"),
    );
    write(
        &root.join("proj/bbb.jsonl"),
        &session_line("fastpath two here"),
    );

    let mut store = Store::open_in_memory().unwrap();

    let a1 = CountingAdapter::new(root.clone(), true);
    let c1 = a1.parse_calls.clone();
    index::index_with(&mut store, vec![Box::new(a1)], false, None).unwrap();
    assert_eq!(c1.load(Ordering::SeqCst), 2);
    assert_eq!(store.session_count().unwrap(), 2);
    assert!(!store.get_dir_mtimes(AgentId::Claude).unwrap().is_empty());

    let a2 = CountingAdapter::new(root.clone(), true);
    let c2 = a2.parse_calls.clone();
    index::index_with(&mut store, vec![Box::new(a2)], false, None).unwrap();
    assert_eq!(
        c2.load(Ordering::SeqCst),
        0,
        "unchanged dir must be skipped"
    );
    assert_eq!(store.session_count().unwrap(), 2);
}

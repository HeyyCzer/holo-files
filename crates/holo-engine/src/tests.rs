//! Scenario tests with an in-memory filesystem and a fake provider.

use std::{
    collections::{BTreeMap, HashMap},
    io,
    ops::Range,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
    },
};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use holo_cfapi::{LocalState, PinState, PlaceholderSpec, PlaceholderState};
use holo_core::{
    AccountInfo, ByteStream, Capabilities, ChangeBatch, ChangeCursor, CloudProvider, ContentHash,
    HashAlgo, ItemKind, ProviderError, ProviderKind, ProviderResult, RelPath, RemoteItem,
    RemoteItemId, UploadRequest, UploadTarget, hash::QuickXorHash, ignore_rules::IgnoreRules,
};
use holo_state::{AccountRecord, ItemRecord, RootRecord, StateStore};
use tokio::sync::{Notify, mpsc};

use crate::{
    EngineConfig, LocalTree,
    context::{RootContext, Suppressor},
    local::{Classified, classify},
    ops::{self, LocalOp},
    plan::{RemoteAction, plan},
    remote,
};

const ROOT: &str = r"C:\Sync";

// ------------------------------------------------------------------ fake tree

#[derive(Debug, Clone)]
struct Node {
    path: PathBuf,
    is_dir: bool,
    size: u64,
    content: Vec<u8>,
    modified: DateTime<Utc>,
    ph: Option<PlaceholderState>,
}

#[derive(Default)]
struct FakeTree {
    nodes: Mutex<BTreeMap<String, Node>>,
    trashed: Mutex<Vec<PathBuf>>,
}

fn key(p: &Path) -> String {
    p.to_string_lossy().to_lowercase()
}

fn old() -> DateTime<Utc> {
    DateTime::from_timestamp(1_700_000_000, 0).unwrap()
}

fn qx(data: &[u8]) -> ContentHash {
    let mut h = QuickXorHash::new();
    h.update(data);
    ContentHash {
        algo: HashAlgo::QuickXor,
        value: h.finalize_base64(),
    }
}

impl FakeTree {
    fn new() -> Arc<Self> {
        let t = Arc::new(Self::default());
        t.put(Path::new(ROOT), true, b"", None);
        t
    }
    fn put(&self, p: &Path, is_dir: bool, content: &[u8], ph: Option<PlaceholderState>) {
        self.nodes.lock().unwrap().insert(
            key(p),
            Node {
                path: p.into(),
                is_dir,
                size: content.len() as u64,
                content: content.to_vec(),
                modified: old(),
                ph,
            },
        );
    }
    fn local_file(&self, rel: &str, content: &[u8]) {
        self.put(
            &RelPath::parse(rel).unwrap().to_local(Path::new(ROOT)),
            false,
            content,
            None,
        );
    }
    fn synced_file(&self, rel: &str, id: &str, content: &[u8], partial: bool) {
        let ph = PlaceholderState {
            in_sync: true,
            pin: PinState::Unspecified,
            partial,
            identity: id.as_bytes().to_vec(),
        };
        self.put(
            &RelPath::parse(rel).unwrap().to_local(Path::new(ROOT)),
            false,
            content,
            Some(ph),
        );
    }
    fn get(&self, rel: &str) -> Option<Node> {
        self.nodes
            .lock()
            .unwrap()
            .get(&key(&RelPath::parse(rel)
                .unwrap()
                .to_local(Path::new(ROOT))))
            .cloned()
    }
    fn with<T>(&self, p: &Path, f: impl FnOnce(&mut Node) -> T) -> io::Result<T> {
        let mut m = self.nodes.lock().unwrap();
        m.get_mut(&key(p))
            .map(f)
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
    }
    fn names(&self) -> Vec<String> {
        self.nodes
            .lock()
            .unwrap()
            .values()
            .map(|n| n.path.display().to_string())
            .collect()
    }
}

impl LocalTree for FakeTree {
    fn state(&self, path: &Path) -> io::Result<Option<LocalState>> {
        Ok(self
            .nodes
            .lock()
            .unwrap()
            .get(&key(path))
            .map(|n| LocalState {
                is_dir: n.is_dir,
                size: n.size,
                modified: n.modified,
                placeholder: n.ph.clone(),
            }))
    }
    fn create_placeholder(
        &self,
        parent: &Path,
        name: &str,
        spec: &PlaceholderSpec,
    ) -> io::Result<()> {
        let p = parent.join(name);
        if self.state(parent)?.is_none() {
            return Err(io::ErrorKind::NotFound.into());
        }
        if self.state(&p)?.is_some() {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        let ph = PlaceholderState {
            in_sync: true,
            pin: PinState::Unspecified,
            partial: !spec.is_dir,
            identity: spec.identity.clone(),
        };
        self.nodes.lock().unwrap().insert(
            key(&p),
            Node {
                path: p,
                is_dir: spec.is_dir,
                size: spec.size,
                content: vec![],
                modified: spec.modified,
                ph: Some(ph),
            },
        );
        Ok(())
    }
    fn update_placeholder(
        &self,
        path: &Path,
        spec: &PlaceholderSpec,
        dehydrate: bool,
    ) -> io::Result<bool> {
        self.with(path, |n| {
            n.size = spec.size;
            n.modified = spec.modified;
            let ph = n.ph.as_mut().expect("placeholder");
            ph.identity = spec.identity.clone();
            ph.in_sync = true;
            if dehydrate {
                ph.partial = true;
                n.content.clear();
            }
            false
        })
    }
    fn mark_synced(&self, path: &Path, identity: &[u8], _usn: Option<i64>) -> io::Result<()> {
        self.with(path, |n| {
            let (pin, partial) =
                n.ph.as_ref()
                    .map(|p| (p.pin, p.partial))
                    .unwrap_or((PinState::Unspecified, false));
            n.ph = Some(PlaceholderState {
                in_sync: true,
                pin,
                partial,
                identity: identity.to_vec(),
            });
        })
    }
    fn detach(&self, path: &Path) -> io::Result<()> {
        self.with(path, |n| {
            if let Some(p) = n.ph.as_mut() {
                p.identity.clear();
                p.in_sync = false;
            }
        })
    }
    fn usn(&self, _path: &Path) -> io::Result<Option<i64>> {
        Ok(Some(1))
    }
    fn set_pin(&self, path: &Path, pin: PinState) -> io::Result<()> {
        self.with(path, |n| n.ph.as_mut().map(|p| p.pin = pin).unwrap_or(()))
    }
    fn hydrate(&self, path: &Path) -> io::Result<()> {
        self.with(path, |n| {
            n.content = vec![0; n.size as usize];
            n.ph.as_mut().map(|p| p.partial = false).unwrap_or(())
        })
    }
    fn dehydrate(&self, path: &Path) -> io::Result<()> {
        self.with(path, |n| {
            n.ph.as_mut().map(|p| p.partial = true).unwrap_or(())
        })
    }
    fn hash(&self, path: &Path, _algo: HashAlgo) -> io::Result<Option<ContentHash>> {
        Ok(self
            .nodes
            .lock()
            .unwrap()
            .get(&key(path))
            .and_then(|n| (!n.ph.as_ref().is_some_and(|p| p.partial)).then(|| qx(&n.content))))
    }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        let mut m = self.nodes.lock().unwrap();
        let (fk, tk) = (key(from), key(to));
        if !m.contains_key(&fk) {
            return Err(io::ErrorKind::NotFound.into());
        }
        let moving: Vec<String> = m
            .keys()
            .filter(|k| **k == fk || k.starts_with(&format!("{fk}\\")))
            .cloned()
            .collect();
        for k in moving {
            let mut n = m.remove(&k).unwrap();
            let rest = &n.path.to_string_lossy()[from.to_string_lossy().len()..].to_owned();
            n.path = PathBuf::from(format!("{}{}", to.display(), rest));
            m.insert(format!("{tk}{}", &k[fk.len()..]), n);
        }
        Ok(())
    }
    fn remove_file(&self, path: &Path) -> io::Result<()> {
        self.nodes
            .lock()
            .unwrap()
            .remove(&key(path))
            .map(|_| ())
            .ok_or(io::ErrorKind::NotFound.into())
    }
    fn remove_dir(&self, path: &Path) -> io::Result<()> {
        if !self.read_dir(path)?.is_empty() {
            return Err(io::Error::other("directory not empty"));
        }
        self.remove_file(path)
    }
    fn read_dir(&self, path: &Path) -> io::Result<Vec<String>> {
        let prefix = format!("{}\\", key(path));
        Ok(self
            .nodes
            .lock()
            .unwrap()
            .iter()
            .filter(|(k, _)| k.starts_with(&prefix) && !k[prefix.len()..].contains('\\'))
            .map(|(_, n)| n.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect())
    }
    fn trash(&self, path: &Path, _trash_dir: &Path) -> io::Result<()> {
        self.remove_file(path)?;
        self.trashed.lock().unwrap().push(path.into());
        Ok(())
    }
}

// -------------------------------------------------------------- fake provider

struct FakeProvider {
    tree: Arc<FakeTree>,
    items: Mutex<HashMap<RemoteItemId, RemoteItem>>,
    pending: Mutex<Vec<RemoteItem>>,
    next: AtomicU32,
    deleted: Mutex<Vec<RemoteItemId>>,
}

impl FakeProvider {
    fn new(tree: Arc<FakeTree>) -> Arc<Self> {
        Arc::new(Self {
            tree,
            items: Mutex::default(),
            pending: Mutex::default(),
            next: AtomicU32::new(1),
            deleted: Mutex::default(),
        })
    }
    fn add(&self, item: RemoteItem) {
        self.items.lock().unwrap().insert(item.id.clone(), item);
    }
    fn get(&self, id: &str) -> Option<RemoteItem> {
        self.items
            .lock()
            .unwrap()
            .get(&RemoteItemId::new(id))
            .cloned()
    }
}

#[async_trait]
impl CloudProvider for FakeProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::OneDrive
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            ranged_download: true,
            hash_algo: Some(HashAlgo::QuickXor),
            scoped_changes: true,
            allows_duplicate_names: false,
        }
    }
    async fn account(&self) -> ProviderResult<AccountInfo> {
        Err(ProviderError::NotImplemented("fake"))
    }
    async fn item_by_path(&self, _path: &str) -> ProviderResult<RemoteItem> {
        Err(ProviderError::NotFound)
    }
    async fn item(&self, id: &RemoteItemId) -> ProviderResult<RemoteItem> {
        self.items
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or(ProviderError::NotFound)
    }
    async fn changes(
        &self,
        _s: &RemoteItemId,
        _c: Option<&ChangeCursor>,
    ) -> ProviderResult<ChangeBatch> {
        Ok(ChangeBatch {
            items: std::mem::take(&mut *self.pending.lock().unwrap()),
            cursor: ChangeCursor("c".into()),
            has_more: false,
        })
    }
    async fn download(
        &self,
        _id: &RemoteItemId,
        _r: Option<Range<u64>>,
    ) -> ProviderResult<ByteStream> {
        Err(ProviderError::NotImplemented("fake"))
    }
    async fn create_folder(&self, parent: &RemoteItemId, name: &str) -> ProviderResult<RemoteItem> {
        let id = format!("d{}", self.next.fetch_add(1, Ordering::Relaxed));
        let item = folder(&id, parent.as_str(), name);
        self.add(item.clone());
        Ok(item)
    }
    async fn upload(&self, req: UploadRequest<'_>) -> ProviderResult<RemoteItem> {
        let content = self
            .tree
            .nodes
            .lock()
            .unwrap()
            .get(&key(req.source))
            .map(|n| n.content.clone())
            .unwrap_or_default();
        let mut items = self.items.lock().unwrap();
        let item = match req.target {
            UploadTarget::New { parent, name } => {
                if items.values().any(|i| {
                    i.parent_id.as_ref() == Some(parent) && i.name.eq_ignore_ascii_case(name)
                }) {
                    return Err(ProviderError::NameConflict);
                }
                let id = format!("u{}", self.next.fetch_add(1, Ordering::Relaxed));
                file(&id, parent.as_str(), name, &content, "e1")
            }
            UploadTarget::Replace { id, if_match } => {
                let cur = items.get(id).ok_or(ProviderError::NotFound)?;
                if if_match.is_some() && cur.etag.as_deref() != if_match {
                    return Err(ProviderError::PreconditionFailed);
                }
                let mut next = file(
                    id.as_str(),
                    cur.parent_id.as_ref().unwrap().as_str(),
                    &cur.name,
                    &content,
                    "",
                );
                next.etag = Some(format!("{}+", cur.etag.clone().unwrap_or_default()));
                next
            }
        };
        items.insert(item.id.clone(), item.clone());
        Ok(item)
    }
    async fn move_item(
        &self,
        id: &RemoteItemId,
        parent: &RemoteItemId,
        name: &str,
    ) -> ProviderResult<RemoteItem> {
        let mut items = self.items.lock().unwrap();
        let it = items.get_mut(id).ok_or(ProviderError::NotFound)?;
        it.parent_id = Some(parent.clone());
        it.name = name.into();
        Ok(it.clone())
    }
    async fn delete(&self, id: &RemoteItemId) -> ProviderResult<()> {
        self.items.lock().unwrap().remove(id);
        self.deleted.lock().unwrap().push(id.clone());
        Ok(())
    }
}

// -------------------------------------------------------------------- helpers

fn file(id: &str, parent: &str, name: &str, content: &[u8], etag: &str) -> RemoteItem {
    RemoteItem {
        id: RemoteItemId::new(id),
        parent_id: Some(RemoteItemId::new(parent)),
        name: name.into(),
        kind: ItemKind::File,
        size: content.len() as u64,
        modified: old(),
        etag: Some(etag.into()),
        ctag: None,
        hash: Some(qx(content)),
        deleted: false,
    }
}

fn folder(id: &str, parent: &str, name: &str) -> RemoteItem {
    RemoteItem {
        kind: ItemKind::Folder,
        hash: None,
        ..file(id, parent, name, b"", "e")
    }
}

fn deleted(id: &str) -> RemoteItem {
    RemoteItem {
        deleted: true,
        ..file(id, "?", "", b"", "")
    }
}

fn rp(s: &str) -> RelPath {
    RelPath::parse(s).unwrap()
}

struct Harness {
    ctx: Arc<RootContext>,
    tree: Arc<FakeTree>,
    provider: Arc<FakeProvider>,
}

fn harness() -> Harness {
    let tree = FakeTree::new();
    let provider = FakeProvider::new(tree.clone());
    let state = Arc::new(StateStore::open_in_memory().unwrap());
    state
        .upsert_account(&AccountRecord {
            id: "acc".into(),
            provider: ProviderKind::OneDrive,
            remote_id: "x".into(),
            display_name: "x".into(),
            email: None,
            created_at: Utc::now(),
        })
        .unwrap();
    let root = RootRecord {
        id: "r".into(),
        account_id: "acc".into(),
        local_path: ROOT.into(),
        remote_root_id: RemoteItemId::new("R"),
        remote_path: "/".into(),
        enabled: true,
        cursor: None,
        created_at: Utc::now(),
    };
    state.insert_root(&root).unwrap();
    let (tx, _rx) = mpsc::unbounded_channel();
    let cfg = EngineConfig {
        settle_time: std::time::Duration::ZERO,
        machine_name: "PC".into(),
        concurrency: crate::adaptive::Concurrency::Fixed(4),
        ..Default::default()
    };
    let ctx = Arc::new(RootContext {
        root,
        local_root: ROOT.into(),
        provider: provider.clone(),
        state,
        tree: tree.clone(),
        ignore: IgnoreRules::from_patterns(holo_core::ignore_rules::DEFAULT_PATTERNS),
        cfg,
        suppress: Suppressor::default(),
        wake_remote: Notify::new(),
        wake_ops: Notify::new(),
        events: tx,
        pull_failures: Default::default(),
        dir_lock: Default::default(),
        load: None,
    });
    Harness {
        ctx,
        tree,
        provider,
    }
}

impl Harness {
    fn apply(&self, changes: &[RemoteItem]) {
        remote::apply(&self.ctx, changes, false).unwrap();
    }
    fn pending_ops(&self) -> Vec<LocalOp> {
        self.ctx
            .state
            .ops(None)
            .unwrap()
            .iter()
            .map(|o| serde_json::from_str(&o.payload).unwrap())
            .collect()
    }
    async fn run_ops(&self) {
        while let Some(op) = self.ctx.state.next_op("r").unwrap() {
            let parsed: LocalOp = serde_json::from_str(&op.payload).unwrap();
            ops::execute(&self.ctx, &parsed).await.unwrap();
            self.ctx.state.complete_op(op.id).unwrap();
        }
    }
    fn known(&self) -> HashMap<RemoteItemId, ItemRecord> {
        self.ctx
            .state
            .all_items("r")
            .unwrap()
            .into_iter()
            .map(|r| (r.remote_id.clone(), r))
            .collect()
    }
}

// ---------------------------------------------------------------- plan (pure)

fn root_id() -> RemoteItemId {
    RemoteItemId::new("R")
}

fn known(items: &[(&str, &str, &str, bool)]) -> HashMap<RemoteItemId, ItemRecord> {
    items
        .iter()
        .map(|(id, parent, path, dir)| {
            let it = if *dir {
                folder(id, parent, rp(path).name())
            } else {
                file(id, parent, rp(path).name(), b"x", "e")
            };
            (it.id.clone(), ItemRecord::from_remote("r", &it, rp(path)))
        })
        .collect()
}

#[test]
fn plan_orders_parents_before_children() {
    let changes = [
        file("F", "A", "b.txt", b"1", "e"),
        folder("A", "R", "a"),
        folder("R", "", "root"),
    ];
    let actions = plan(
        &root_id(),
        &HashMap::new(),
        &changes,
        false,
        &IgnoreRules::none(),
    );
    let paths: Vec<_> = actions
        .iter()
        .map(|a| match a {
            RemoteAction::Create { path, .. } => path.as_str().to_owned(),
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    assert_eq!(paths, ["a", "a/b.txt"]);
}

#[test]
fn plan_folder_rename_is_one_change() {
    let k = known(&[("A", "R", "a", true), ("F", "A", "a/f.txt", false)]);
    let actions = plan(
        &root_id(),
        &k,
        &[folder("A", "R", "z")],
        false,
        &IgnoreRules::none(),
    );
    assert_eq!(actions.len(), 1);
    assert!(
        matches!(&actions[0], RemoteAction::Change { from, to, content: false, .. } if from.as_str() == "a" && to.as_str() == "z")
    );
}

#[test]
fn plan_folder_delete_subsumes_children() {
    let k = known(&[("A", "R", "a", true), ("F", "A", "a/f.txt", false)]);
    let actions = plan(
        &root_id(),
        &k,
        &[deleted("F"), deleted("A")],
        false,
        &IgnoreRules::none(),
    );
    assert_eq!(
        actions,
        vec![RemoteAction::Delete {
            id: RemoteItemId::new("A"),
            path: rp("a"),
            is_dir: true
        }]
    );
}

#[test]
fn plan_skips_ignored_and_handles_scope_and_full_resync() {
    let ignore = IgnoreRules::from_patterns(&["node_modules/"]);
    let k = known(&[("F", "R", "f.txt", false), ("G", "R", "g.txt", false)]);
    let changes = [
        folder("N", "R", "node_modules"),
        file("M", "N", "x.js", b"", "e"),
        file("F", "ELSEWHERE", "f.txt", b"x", "e"), // moved out of the root
        file("S", "R", "a:b.txt", b"", "e"),        // needs sanitizing
    ];
    let actions = plan(&root_id(), &k, &changes, true, &ignore);
    assert!(actions.contains(&RemoteAction::Delete {
        id: RemoteItemId::new("F"),
        path: rp("f.txt"),
        is_dir: false
    }));
    // G missing from a full enumeration → gone.
    assert!(actions.contains(&RemoteAction::Delete {
        id: RemoteItemId::new("G"),
        path: rp("g.txt"),
        is_dir: false
    }));
    assert!(
        actions
            .iter()
            .any(|a| matches!(a, RemoteAction::Create { path, .. } if path.as_str() == "a：b.txt"))
    );
    assert!(!actions.iter().any(
        |a| matches!(a, RemoteAction::Create { path, .. } if path.as_str().contains("node_modules"))
    ));
}

#[test]
fn plan_detects_content_change() {
    let k = known(&[("F", "R", "f.txt", false)]);
    let actions = plan(
        &root_id(),
        &k,
        &[file("F", "R", "f.txt", b"new", "e2")],
        false,
        &IgnoreRules::none(),
    );
    assert!(matches!(
        &actions[0],
        RemoteAction::Change { content: true, .. }
    ));
}

// --------------------------------------------------------- remote → local

#[test]
fn remote_create_makes_dehydrated_placeholders() {
    let h = harness();
    h.apply(&[
        folder("A", "R", "docs"),
        file("F", "A", "a.txt", b"hello", "e"),
    ]);
    let n = h.tree.get("docs/a.txt").unwrap();
    let ph = n.ph.unwrap();
    assert!(ph.in_sync && ph.partial);
    assert_eq!(ph.identity, b"F");
    assert_eq!(n.size, 5);
    assert_eq!(h.known().len(), 2);
}

#[test]
fn remote_edit_dehydrates_in_sync_file() {
    let h = harness();
    h.apply(&[file("F", "R", "a.txt", b"v1", "e1")]);
    h.tree.hydrate(&h.ctx.local(&rp("a.txt"))).unwrap();
    h.apply(&[file("F", "R", "a.txt", b"version2", "e2")]);
    let n = h.tree.get("a.txt").unwrap();
    assert!(
        n.ph.unwrap().partial,
        "new content must be fetched on next read"
    );
    assert_eq!(n.size, 8);
}

#[test]
fn remote_edit_with_local_edit_keeps_both() {
    let h = harness();
    h.apply(&[file("F", "R", "a.txt", b"v1", "e1")]);
    // Local edit: content changed, no longer in sync.
    h.tree
        .with(&h.ctx.local(&rp("a.txt")), |n| {
            n.content = b"local".to_vec();
            n.ph.as_mut().unwrap().in_sync = false;
            n.ph.as_mut().unwrap().partial = false;
        })
        .unwrap();
    h.apply(&[file("F", "R", "a.txt", b"remote", "e2")]);

    let names = h.tree.names();
    let copy = names
        .iter()
        .find(|n| n.contains("conflict PC"))
        .expect("conflict copy")
        .clone();
    assert!(
        h.tree.get("a.txt").unwrap().ph.unwrap().in_sync,
        "remote version at original name"
    );
    let copy_node = h
        .tree
        .nodes
        .lock()
        .unwrap()
        .get(&copy.to_lowercase())
        .cloned()
        .unwrap();
    assert_eq!(copy_node.content, b"local");
    assert!(
        copy_node.ph.unwrap().identity.is_empty(),
        "copy detached from remote item"
    );
    assert!(
        matches!(&h.pending_ops()[..], [LocalOp::Upload { path }] if path.as_str().contains("conflict PC"))
    );
}

#[test]
fn remote_delete_removes_synced_and_preserves_edited() {
    let h = harness();
    h.apply(&[
        file("F", "R", "keep.txt", b"1", "e"),
        file("G", "R", "gone.txt", b"2", "e"),
        file("H", "R", "full.txt", b"3", "e"),
    ]);
    h.tree
        .with(&h.ctx.local(&rp("keep.txt")), |n| {
            n.ph.as_mut().unwrap().in_sync = false
        })
        .unwrap();
    h.tree.hydrate(&h.ctx.local(&rp("full.txt"))).unwrap();
    h.apply(&[deleted("F"), deleted("G"), deleted("H")]);

    assert!(h.tree.get("gone.txt").is_none());
    assert!(h.tree.get("full.txt").is_none());
    assert_eq!(
        h.tree.trashed.lock().unwrap().len(),
        1,
        "hydrated copy goes to holo trash"
    );
    assert!(
        h.tree.get("keep.txt").is_some(),
        "locally edited file survives"
    );
    assert_eq!(
        h.pending_ops(),
        vec![LocalOp::Upload {
            path: rp("keep.txt")
        }]
    );
    assert!(h.known().is_empty());
}

#[test]
fn remote_folder_move_moves_subtree() {
    let h = harness();
    h.apply(&[folder("A", "R", "a"), file("F", "A", "f.txt", b"1", "e")]);
    h.apply(&[folder("A", "R", "b")]);
    assert!(h.tree.get("b/f.txt").is_some());
    assert_eq!(
        h.ctx
            .state
            .item("r", &RemoteItemId::new("F"))
            .unwrap()
            .unwrap()
            .rel_path,
        rp("b/f.txt")
    );
}

#[test]
fn first_sync_merges_identical_local_file() {
    let h = harness();
    h.tree.local_file("same.txt", b"abc");
    h.tree.local_file("diff.txt", b"local");
    h.apply(&[
        file("S", "R", "same.txt", b"abc", "e"),
        file("D", "R", "diff.txt", b"remote", "e"),
    ]);
    assert_eq!(h.tree.get("same.txt").unwrap().ph.unwrap().identity, b"S");
    assert!(
        h.tree
            .names()
            .iter()
            .any(|n| n.contains("diff (conflict PC"))
    );
    assert_eq!(h.tree.get("diff.txt").unwrap().ph.unwrap().identity, b"D");
}

// ---------------------------------------------------------- local → remote

#[tokio::test]
async fn new_local_file_is_uploaded_and_marked_synced() {
    let h = harness();
    h.apply(&[folder("A", "R", "docs")]);
    h.provider.add(folder("A", "R", "docs"));
    h.tree.local_file("docs/new.txt", b"data");

    let actions = classify(&h.ctx, &h.ctx.local(&rp("docs/new.txt"))).unwrap();
    assert_eq!(actions, vec![Classified::Upload(rp("docs/new.txt"))]);
    ops::enqueue(
        &h.ctx,
        &LocalOp::Upload {
            path: rp("docs/new.txt"),
        },
    )
    .unwrap();
    h.run_ops().await;

    let ph = h.tree.get("docs/new.txt").unwrap().ph.unwrap();
    assert!(ph.in_sync);
    let id = String::from_utf8(ph.identity).unwrap();
    assert_eq!(
        h.provider.get(&id).unwrap().parent_id.unwrap().as_str(),
        "A"
    );
    assert!(
        h.ctx
            .state
            .item_by_path("r", &rp("docs/new.txt"))
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn new_local_folder_tree_creates_remote_parents() {
    let h = harness();
    h.tree.put(&h.ctx.local(&rp("x")), true, b"", None);
    h.tree.put(&h.ctx.local(&rp("x/y")), true, b"", None);
    h.tree.local_file("x/y/z.txt", b"1");
    ops::enqueue(
        &h.ctx,
        &LocalOp::Upload {
            path: rp("x/y/z.txt"),
        },
    )
    .unwrap();
    h.run_ops().await;
    let y = h.ctx.state.item_by_path("r", &rp("x/y")).unwrap().unwrap();
    let z = h
        .ctx
        .state
        .item_by_path("r", &rp("x/y/z.txt"))
        .unwrap()
        .unwrap();
    assert_eq!(z.parent_id.unwrap(), y.remote_id);
    assert!(
        h.tree.get("x").unwrap().ph.unwrap().in_sync,
        "local folder converted to placeholder"
    );
}

#[tokio::test]
async fn local_edit_over_stale_base_becomes_conflict_copy() {
    let h = harness();
    h.apply(&[file("F", "R", "a.txt", b"v1", "e1")]);
    h.provider.add(file("F", "R", "a.txt", b"v2-remote", "e2")); // changed remotely, not pulled yet
    h.tree
        .with(&h.ctx.local(&rp("a.txt")), |n| {
            n.content = b"local".to_vec();
            n.size = 5;
            let ph = n.ph.as_mut().unwrap();
            ph.in_sync = false;
            ph.partial = false;
        })
        .unwrap();
    ops::execute(&h.ctx, &LocalOp::Upload { path: rp("a.txt") })
        .await
        .unwrap();
    assert_eq!(
        h.provider.get("F").unwrap().etag.as_deref(),
        Some("e2"),
        "remote not overwritten"
    );
    assert!(h.tree.names().iter().any(|n| n.contains("a (conflict PC")));
}

#[tokio::test]
async fn unchanged_content_is_not_reuploaded() {
    let h = harness();
    h.apply(&[file("F", "R", "a.txt", b"same", "e1")]);
    h.provider.add(file("F", "R", "a.txt", b"same", "e1"));
    h.tree
        .with(&h.ctx.local(&rp("a.txt")), |n| {
            n.content = b"same".to_vec();
            let ph = n.ph.as_mut().unwrap();
            ph.in_sync = false; // e.g. touched / saved without edits
            ph.partial = false;
        })
        .unwrap();
    ops::execute(&h.ctx, &LocalOp::Upload { path: rp("a.txt") })
        .await
        .unwrap();
    assert_eq!(h.provider.get("F").unwrap().etag.as_deref(), Some("e1"));
    assert!(h.tree.get("a.txt").unwrap().ph.unwrap().in_sync);
}

#[tokio::test]
async fn local_rename_and_delete_reach_remote() {
    let h = harness();
    h.apply(&[file("F", "R", "a.txt", b"1", "e")]);
    h.provider.add(file("F", "R", "a.txt", b"1", "e"));
    h.tree
        .rename(&h.ctx.local(&rp("a.txt")), &h.ctx.local(&rp("b.txt")))
        .unwrap();
    ops::execute(
        &h.ctx,
        &LocalOp::Move {
            id: RemoteItemId::new("F"),
            to: rp("b.txt"),
        },
    )
    .await
    .unwrap();
    assert_eq!(h.provider.get("F").unwrap().name, "b.txt");

    // Delete is skipped while the file still exists locally (undelete safety)...
    ops::execute(
        &h.ctx,
        &LocalOp::Delete {
            id: RemoteItemId::new("F"),
            path: rp("b.txt"),
        },
    )
    .await
    .unwrap();
    assert!(h.provider.get("F").is_some());
    // ...and performed once it's really gone.
    h.tree.remove_file(&h.ctx.local(&rp("b.txt"))).unwrap();
    ops::execute(
        &h.ctx,
        &LocalOp::Delete {
            id: RemoteItemId::new("F"),
            path: rp("b.txt"),
        },
    )
    .await
    .unwrap();
    assert!(h.provider.get("F").is_none());
    assert!(h.known().is_empty());
}

#[test]
fn classify_handles_pins_and_ignores() {
    let h = harness();
    h.tree.synced_file("pinned.bin", "P", b"", true);
    h.tree
        .set_pin(&h.ctx.local(&rp("pinned.bin")), PinState::Pinned)
        .unwrap();
    h.tree.synced_file("free.bin", "U", b"x", false);
    h.tree
        .set_pin(&h.ctx.local(&rp("free.bin")), PinState::Unpinned)
        .unwrap();
    h.tree
        .put(&h.ctx.local(&rp("node_modules")), true, b"", None);
    h.tree.local_file("node_modules/x.js", b"1");

    assert!(matches!(
        &classify(&h.ctx, &h.ctx.local(&rp("pinned.bin"))).unwrap()[..],
        [Classified::Hydrate(_)]
    ));
    assert!(matches!(
        &classify(&h.ctx, &h.ctx.local(&rp("free.bin"))).unwrap()[..],
        [Classified::Dehydrate(_)]
    ));
    assert!(
        classify(&h.ctx, &h.ctx.local(&rp("node_modules/x.js")))
            .unwrap()
            .is_empty()
    );
}

// ------------------------------------------------------------------- preview

fn run_preview(
    h: &Harness,
    changes: &[RemoteItem],
    full: bool,
    hash: bool,
    threshold: usize,
) -> crate::preview::Report {
    let known = h.known();
    crate::preview::preview(&crate::preview::PreviewInput {
        tree: h.tree.as_ref(),
        ignore: &h.ctx.ignore,
        local_root: Path::new(ROOT),
        remote_root: &RemoteItemId::new("R"),
        known: &known,
        changes,
        full,
        partial: false,
        hash,
        mass_delete_threshold: threshold,
    })
    .unwrap()
}

fn kinds(r: &crate::preview::Report) -> Vec<(crate::preview::Kind, String)> {
    r.entries
        .iter()
        .map(|e| (e.kind, e.path.as_str().to_owned()))
        .collect()
}

#[test]
fn preview_cold_start_into_existing_folder_changes_nothing() {
    use crate::preview::Kind::*;
    let h = harness();
    h.tree.local_file("same.txt", b"abc");
    h.tree.local_file("diff.txt", b"local");
    h.tree.local_file("only-local.txt", b"12345");
    h.tree.put(&h.ctx.local(&rp("docs")), true, b"", None);
    h.tree.local_file("docs/new.md", b"#");
    h.tree
        .put(&h.ctx.local(&rp("node_modules")), true, b"", None);
    h.tree.local_file("node_modules/x.js", b"1");
    let remote = [
        folder("R", "", "root"),
        file("S", "R", "same.txt", b"abc", "e"),
        file("D", "R", "diff.txt", b"remote!", "e"),
        folder("DOCS", "R", "docs"),
        file("P", "DOCS", "remote.pdf", &[0; 100], "e"),
        file("B", "R", "big.iso", &[0; 1000], "e"),
    ];
    let before = h.tree.names();

    let r = run_preview(&h, &remote, true, true, 200);
    assert_eq!(
        kinds(&r),
        vec![
            (Placeholder, "big.iso".into()),
            (Placeholder, "docs/remote.pdf".into()),
            (Link, "same.txt".into()),
            (MergeFolder, "docs".into()),
            (Upload, "docs/new.md".into()),
            (Upload, "only-local.txt".into()),
            (Conflict, "diff.txt".into()),
        ]
    );
    assert_eq!(r.network(), (0, 1 + 5 + 5), "only uploads move bytes now");
    assert_eq!(r.on_demand(), 1100);
    assert_eq!(h.tree.names(), before, "dry run must not touch the tree");
    assert!(h.known().is_empty(), "dry run must not touch the database");
    assert!(h.ctx.state.ops(None).unwrap().is_empty());

    let unverified = run_preview(&h, &remote, true, false, 200);
    assert!(
        unverified
            .entries
            .iter()
            .any(|e| e.kind == Unverified && e.path.as_str() == "same.txt")
    );
}

#[test]
fn preview_incremental_reports_both_directions() {
    use crate::preview::Kind::*;
    let h = harness();
    h.apply(&[
        file("E", "R", "edited.txt", b"v1", "e"),
        file("G", "R", "gone.txt", b"x", "e"),
        file("K", "R", "keep.txt", b"k", "e"),
        file("P", "R", "pinned.bin", b"p1", "e"),
        file("M", "R", "old-name.txt", b"m", "e"),
    ]);
    // Local: edit, delete, edit-before-remote-delete, pinned+hydrated.
    h.tree
        .with(&h.ctx.local(&rp("edited.txt")), |n| {
            n.content = b"v2".to_vec();
            n.ph.as_mut().unwrap().in_sync = false;
            n.ph.as_mut().unwrap().partial = false;
        })
        .unwrap();
    h.tree.remove_file(&h.ctx.local(&rp("gone.txt"))).unwrap();
    h.tree
        .with(&h.ctx.local(&rp("keep.txt")), |n| {
            n.ph.as_mut().unwrap().in_sync = false
        })
        .unwrap();
    h.tree.hydrate(&h.ctx.local(&rp("pinned.bin"))).unwrap();
    h.tree
        .set_pin(&h.ctx.local(&rp("pinned.bin")), PinState::Pinned)
        .unwrap();

    let remote = [
        deleted("K"),
        file("P", "R", "pinned.bin", b"p2-new", "e2"),
        file("M", "R", "new-name.txt", b"m", "e"),
    ];
    let r = run_preview(&h, &remote, false, true, 200);
    assert_eq!(
        kinds(&r),
        vec![
            (Redownload, "pinned.bin".into()),
            (MoveLocal, "new-name.txt".into()),
            (UploadReplace, "edited.txt".into()),
            (KeepAndReupload, "keep.txt".into()),
            (DeleteRemote, "gone.txt".into()),
        ]
    );
    assert_eq!(
        r.entries
            .iter()
            .find(|e| e.kind == MoveLocal)
            .unwrap()
            .from
            .as_ref()
            .unwrap()
            .as_str(),
        "old-name.txt"
    );

    // Over the threshold, remote deletes are reported as held.
    let r = run_preview(&h, &[], false, true, 0);
    assert!(
        r.entries
            .iter()
            .any(|e| e.kind == DeleteRemoteHeld && e.path.as_str() == "gone.txt")
    );
}

#[test]
fn preview_partial_sample_never_infers_deletes() {
    let h = harness();
    h.apply(&[
        file("A", "R", "a.txt", b"1", "e"),
        file("B", "R", "b.txt", b"2", "e"),
    ]);
    let known = h.known();
    let sample = [file("A", "R", "a.txt", b"1", "e")]; // B simply wasn't sampled
    let r = crate::preview::preview(&crate::preview::PreviewInput {
        tree: h.tree.as_ref(),
        ignore: &h.ctx.ignore,
        local_root: Path::new(ROOT),
        remote_root: &RemoteItemId::new("R"),
        known: &known,
        changes: &sample,
        full: true,
        partial: true,
        hash: true,
        mass_delete_threshold: 200,
    })
    .unwrap();
    assert!(r.partial && !r.full);
    assert!(
        r.is_empty(),
        "nothing to do, and b.txt must not be reported as deleted: {:?}",
        r.entries
    );
}

#[tokio::test]
async fn parallel_executor_uploads_everything_and_creates_each_folder_once() {
    let h = harness();
    let mut files = Vec::new();
    for d in ["a", "a/b", "c"] {
        for i in 0..10 {
            let rel = format!("{d}/f{i}.txt");
            h.tree.local_file(&rel, rel.as_bytes());
            files.push(rel);
        }
    }
    for f in &files {
        ops::enqueue(&h.ctx, &LocalOp::Upload { path: rp(f) }).unwrap();
    }

    let (stop, rx) = tokio::sync::watch::channel(false);
    let exec = tokio::spawn(ops::run(h.ctx.clone(), rx));
    for _ in 0..200 {
        if h.ctx.state.ops(None).unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    stop.send(true).unwrap();
    exec.await.unwrap();

    assert!(h.ctx.state.ops(None).unwrap().is_empty(), "journal drained");
    for f in &files {
        let ph = h.tree.get(f).unwrap().ph.unwrap();
        assert!(ph.in_sync, "{f} uploaded");
    }
    let folders: Vec<String> = h
        .provider
        .items
        .lock()
        .unwrap()
        .values()
        .filter(|i| i.kind == ItemKind::Folder)
        .map(|i| i.name.clone())
        .collect();
    let mut sorted = folders.clone();
    sorted.sort();
    assert_eq!(sorted, ["a", "b", "c"], "no duplicate folders: {folders:?}");

    let progress: ops::ExecProgress = serde_json::from_str(
        &h.ctx
            .state
            .kv_get(&ops::progress_key("r"))
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(progress.done, files.len() as u64);
    assert!(progress.running.is_empty());
}

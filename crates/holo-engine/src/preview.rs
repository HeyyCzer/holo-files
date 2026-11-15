//! Dry run: compare remote × local × last-synced state and list what the engine
//! *would* do, without touching the filesystem, the database or the cloud.
//!
//! Uses the same [`plan`] and [`decide`](crate::decide) functions as the real sync.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    io,
    path::Path,
};

use holo_cfapi::PinState;
use holo_core::{RelPath, RemoteItem, RemoteItemId, ignore_rules::IgnoreRules};
use holo_state::ItemRecord;
use serde::Serialize;

use crate::{
    LocalTree,
    decide::{
        ContentOutcome, CreateOutcome, DeleteOutcome, decide_content, decide_create,
        decide_delete_file, identity,
    },
    plan::{RemoteAction, plan},
};

/// Where bytes or changes flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// Cloud → this PC.
    Down,
    /// This PC → cloud.
    Up,
    /// Both (conflict: local copy goes up, remote version comes down).
    Both,
    /// Only bookkeeping / local metadata, no content transfer.
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// New remote file/folder → dehydrated placeholder (downloads only when opened).
    Placeholder,
    /// Pinned file changed remotely → downloaded again now.
    Redownload,
    /// Unchanged local copy of a remotely changed file → local bytes dropped.
    Refresh,
    /// Pinned but not local → downloaded now.
    Hydrate,
    /// Same name on both sides, identical content → linked, no transfer.
    Link,
    /// Same folder on both sides → merged.
    MergeFolder,
    /// Same name on both sides, content not compared (`--no-hash`).
    Unverified,
    /// Remote moved/renamed → local move.
    MoveLocal,
    /// New local file → uploaded.
    Upload,
    /// Local edit of a synced file → uploaded over the remote version.
    UploadReplace,
    /// New local folder → created remotely.
    CreateRemoteFolder,
    /// Edited on both sides / different content → both kept.
    Conflict,
    /// Remote deleted, local is only a placeholder → removed.
    DeleteLocal,
    /// Remote deleted, local fully downloaded → moved to the holo trash.
    TrashLocal,
    /// Remote deleted but changed locally → kept and re-uploaded.
    KeepAndReupload,
    /// Missing locally (deleted while holo was off) → deleted remotely (recycle bin).
    DeleteRemote,
    /// Like `DeleteRemote`, but over the mass-delete threshold → held until confirmed.
    DeleteRemoteHeld,
    /// "Free up space" set → local bytes dropped.
    Dehydrate,
}

impl Kind {
    pub fn direction(self) -> Direction {
        use Kind::*;
        match self {
            Redownload | Hydrate => Direction::Down,
            Upload | UploadReplace | CreateRemoteFolder | KeepAndReupload | DeleteRemote
            | DeleteRemoteHeld => Direction::Up,
            Conflict => Direction::Both,
            Placeholder | Refresh | Link | MergeFolder | Unverified | MoveLocal | DeleteLocal
            | TrashLocal | Dehydrate => Direction::None,
        }
    }

    pub fn describe(self) -> &'static str {
        use Kind::*;
        match self {
            Placeholder => "new in cloud → placeholder (downloads on open)",
            Redownload => "changed in cloud, pinned → download now",
            Refresh => "changed in cloud → local copy dropped, fetched on open",
            Hydrate => "pinned → download now",
            Link => "identical on both sides → link, no transfer",
            MergeFolder => "folder on both sides → merge",
            Unverified => "same name on both sides, not compared (link or conflict)",
            MoveLocal => "moved/renamed in cloud → move locally",
            Upload => "new local file → upload",
            UploadReplace => "edited locally → upload new version",
            CreateRemoteFolder => "new local folder → create in cloud",
            Conflict => "differs on both sides → keep both (local copy renamed + uploaded)",
            DeleteLocal => "deleted in cloud → remove placeholder",
            TrashLocal => "deleted in cloud → local copy to holo trash",
            KeepAndReupload => "deleted in cloud but edited locally → keep + re-upload",
            DeleteRemote => "missing locally → delete in cloud (recycle bin)",
            DeleteRemoteHeld => "missing locally → delete in cloud, HELD (mass-delete guard)",
            Dehydrate => "free up space → drop local copy",
        }
    }

    /// Whether `bytes` actually cross the network now (vs. on demand / not at all).
    pub fn transfers(self) -> bool {
        matches!(
            self,
            Kind::Redownload
                | Kind::Hydrate
                | Kind::Upload
                | Kind::UploadReplace
                | Kind::KeepAndReupload
                | Kind::Conflict
        )
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    pub kind: Kind,
    pub direction: Direction,
    pub path: RelPath,
    /// Previous path for moves.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<RelPath>,
    pub is_dir: bool,
    pub bytes: u64,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct Totals {
    pub count: u64,
    pub bytes: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Report {
    /// Remote side was a complete listing (cold start / `--full`).
    pub full: bool,
    pub remote_changes: usize,
    pub entries: Vec<Entry>,
}

impl Report {
    fn push(&mut self, kind: Kind, path: RelPath, from: Option<RelPath>, is_dir: bool, bytes: u64) {
        self.entries.push(Entry {
            kind,
            direction: kind.direction(),
            path,
            from,
            is_dir,
            bytes,
        });
    }

    pub fn summary(&self) -> BTreeMap<Kind, Totals> {
        let mut m: BTreeMap<Kind, Totals> = BTreeMap::new();
        for e in &self.entries {
            let t = m.entry(e.kind).or_default();
            t.count += 1;
            t.bytes += e.bytes;
        }
        m
    }

    /// Bytes that would transfer immediately in each direction.
    pub fn network(&self) -> (u64, u64) {
        let (mut down, mut up) = (0, 0);
        for e in self.entries.iter().filter(|e| e.kind.transfers()) {
            match e.kind {
                Kind::Redownload | Kind::Hydrate => down += e.bytes,
                _ => up += e.bytes,
            }
        }
        (down, up)
    }

    /// Bytes available on demand (placeholders).
    pub fn on_demand(&self) -> u64 {
        self.entries
            .iter()
            .filter(|e| matches!(e.kind, Kind::Placeholder | Kind::Refresh))
            .map(|e| e.bytes)
            .sum()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

pub struct PreviewInput<'a> {
    pub tree: &'a dyn LocalTree,
    pub ignore: &'a IgnoreRules,
    pub local_root: &'a Path,
    pub remote_root: &'a RemoteItemId,
    /// Last-synced items (empty on cold start).
    pub known: &'a HashMap<RemoteItemId, ItemRecord>,
    pub changes: &'a [RemoteItem],
    pub full: bool,
    /// Compare content of same-name files (reads local files; no download).
    pub hash: bool,
    pub mass_delete_threshold: usize,
}

/// Build the report (blocking: reads local metadata, and local file content when hashing).
pub fn preview(inp: &PreviewInput<'_>) -> io::Result<Report> {
    let local = |rel: &RelPath| rel.to_local(inp.local_root);
    let actions = plan(
        inp.remote_root,
        inp.known,
        inp.changes,
        inp.full,
        inp.ignore,
    );
    let deleting: HashSet<RemoteItemId> = actions
        .iter()
        .filter_map(|a| match a {
            RemoteAction::Delete { id, .. } => Some(id.clone()),
            _ => None,
        })
        .collect();

    let mut report = Report {
        full: inp.full,
        remote_changes: inp.changes.len(),
        ..Default::default()
    };
    // Paths decided by the remote side; the local scan must not count them twice.
    let mut covered: HashSet<String> = HashSet::new();
    let mut covered_trees: Vec<RelPath> = Vec::new();
    let mut touched_ids: HashSet<RemoteItemId> = HashSet::new();

    for action in &actions {
        match action {
            RemoteAction::Create { item, path } => {
                covered.insert(path.key());
                touched_ids.insert(item.id.clone());
                let st = inp.tree.state(&local(path))?;
                let is_file_pair = st.as_ref().is_some_and(|s| !s.is_dir) && !item.is_dir();
                if !inp.hash && is_file_pair && st.as_ref().and_then(identity).is_none() {
                    report.push(Kind::Unverified, path.clone(), None, false, item.size);
                    continue;
                }
                let outcome = decide_create(st.as_ref(), item, &deleting, |algo| {
                    inp.tree.hash(&local(path), algo)
                })?;
                let (kind, bytes) = match outcome {
                    CreateOutcome::Placeholder | CreateOutcome::ReplaceStale => {
                        (Kind::Placeholder, item.size)
                    }
                    CreateOutcome::AlreadyLinked => continue,
                    CreateOutcome::MergeFolder => (Kind::MergeFolder, 0),
                    CreateOutcome::LinkIdentical => (Kind::Link, item.size),
                    CreateOutcome::Conflict => (Kind::Conflict, st.map(|s| s.size).unwrap_or(0)),
                };
                report.push(kind, path.clone(), None, item.is_dir(), bytes);
            }
            RemoteAction::Change {
                item,
                from,
                to,
                content,
            } => {
                covered.insert(to.key());
                covered.insert(from.key());
                touched_ids.insert(item.id.clone());
                if from != to {
                    report.push(
                        Kind::MoveLocal,
                        to.clone(),
                        Some(from.clone()),
                        item.is_dir(),
                        0,
                    );
                }
                if *content && !item.is_dir() {
                    // The move hasn't happened yet: the file is still at `from`.
                    let st = inp.tree.state(&local(from))?;
                    let (kind, bytes) = match decide_content(st.as_ref()) {
                        ContentOutcome::Recreate => (Kind::Placeholder, item.size),
                        ContentOutcome::Refresh { pinned: true } => (Kind::Redownload, item.size),
                        ContentOutcome::Refresh { pinned: false } => (Kind::Refresh, item.size),
                        ContentOutcome::Conflict => {
                            (Kind::Conflict, st.map(|s| s.size).unwrap_or(0))
                        }
                    };
                    report.push(kind, to.clone(), None, false, bytes);
                }
            }
            RemoteAction::Delete { id, path, .. } => {
                touched_ids.insert(id.clone());
                covered_trees.push(path.clone());
                preview_delete(inp, &mut report, path)?;
            }
            RemoteAction::Forget { id } => {
                touched_ids.insert(id.clone());
            }
        }
    }

    scan_local(inp, &mut report, &covered, &covered_trees)?;
    missing_locally(inp, &mut report, &touched_ids, &covered_trees)?;

    report
        .entries
        .sort_by(|a, b| a.kind.cmp(&b.kind).then_with(|| a.path.cmp(&b.path)));
    Ok(report)
}

fn preview_delete(inp: &PreviewInput<'_>, report: &mut Report, rel: &RelPath) -> io::Result<()> {
    let path = rel.to_local(inp.local_root);
    let Some(st) = inp.tree.state(&path)? else {
        return Ok(());
    };
    if st.is_dir {
        for name in inp.tree.read_dir(&path)? {
            preview_delete(inp, report, &rel.join(&name))?;
        }
        return Ok(());
    }
    let kind = match decide_delete_file(&st, inp.ignore.is_ignored(rel, false)) {
        DeleteOutcome::Remove => Kind::DeleteLocal,
        DeleteOutcome::Trash => Kind::TrashLocal,
        DeleteOutcome::Skip => return Ok(()),
        DeleteOutcome::KeepAndReupload => Kind::KeepAndReupload,
    };
    report.push(kind, rel.clone(), None, false, st.size);
    Ok(())
}

/// Local changes the remote side doesn't already account for.
fn scan_local(
    inp: &PreviewInput<'_>,
    report: &mut Report,
    covered: &HashSet<String>,
    covered_trees: &[RelPath],
) -> io::Result<()> {
    let by_path: HashMap<String, &ItemRecord> =
        inp.known.values().map(|r| (r.rel_path.key(), r)).collect();
    let mut stack = vec![RelPath::root()];
    while let Some(dir) = stack.pop() {
        let dir_path = dir.to_local(inp.local_root);
        if inp.tree.state(&dir_path)?.is_none() {
            continue;
        }
        for name in inp.tree.read_dir(&dir_path)? {
            let rel = dir.join(&name);
            if covered_trees.iter().any(|t| rel.starts_with(t)) {
                continue;
            }
            let Some(st) = inp.tree.state(&rel.to_local(inp.local_root))? else {
                continue;
            };
            if inp.ignore.is_ignored(&rel, st.is_dir) {
                continue;
            }
            if st.is_dir {
                if st.needs_upload()
                    && !covered.contains(&rel.key())
                    && !by_path.get(&rel.key()).is_some_and(|r| r.is_dir)
                {
                    report.push(Kind::CreateRemoteFolder, rel.clone(), None, true, 0);
                }
                stack.push(rel);
                continue;
            }
            if covered.contains(&rel.key()) {
                continue;
            }
            match &st.placeholder {
                Some(p) if p.in_sync => match p.pin {
                    PinState::Pinned if p.partial => {
                        report.push(Kind::Hydrate, rel, None, false, st.size)
                    }
                    PinState::Unpinned if !p.partial => {
                        report.push(Kind::Dehydrate, rel, None, false, st.size)
                    }
                    _ => {}
                },
                _ => {
                    let base = identity(&st)
                        .and_then(|id| inp.known.get(&id))
                        .or_else(|| by_path.get(&rel.key()).copied().filter(|r| !r.is_dir));
                    if let Some(b) = base
                        && inp.hash
                        && let Some(h) = &b.hash
                        && inp
                            .tree
                            .hash(&rel.to_local(inp.local_root), h.algo)?
                            .as_ref()
                            == Some(h)
                    {
                        continue; // touched but unchanged
                    }
                    let kind = if base.is_some() {
                        Kind::UploadReplace
                    } else {
                        Kind::Upload
                    };
                    report.push(kind, rel, None, false, st.size);
                }
            }
        }
    }
    Ok(())
}

/// Known items that vanished locally while holo wasn't running.
fn missing_locally(
    inp: &PreviewInput<'_>,
    report: &mut Report,
    touched: &HashSet<RemoteItemId>,
    covered_trees: &[RelPath],
) -> io::Result<()> {
    let mut items: Vec<&ItemRecord> = inp
        .known
        .values()
        .filter(|r| !touched.contains(&r.remote_id))
        .collect();
    items.sort_by_key(|r| r.rel_path.depth());
    let mut gone: Vec<&ItemRecord> = Vec::new();
    for r in items {
        if covered_trees.iter().any(|t| r.rel_path.starts_with(t))
            || gone
                .iter()
                .any(|g| g.is_dir && r.rel_path.starts_with(&g.rel_path))
        {
            continue;
        }
        if inp
            .tree
            .state(&r.rel_path.to_local(inp.local_root))?
            .is_none()
        {
            gone.push(r);
        }
    }
    let kind = if gone.len() > inp.mass_delete_threshold {
        Kind::DeleteRemoteHeld
    } else {
        Kind::DeleteRemote
    };
    for r in gone {
        report.push(kind, r.rel_path.clone(), None, r.is_dir, r.size);
    }
    Ok(())
}

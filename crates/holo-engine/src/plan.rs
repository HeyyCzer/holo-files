//! Pure planning of remote → local changes. No I/O: given the last-synced state and a
//! batch of remote changes, decide what has to happen locally and in which order.

use std::collections::{HashMap, HashSet};

use holo_core::{RelPath, RemoteItem, RemoteItemId, ignore_rules::IgnoreRules, naming};
use holo_state::ItemRecord;

#[derive(Debug, Clone, PartialEq)]
pub enum RemoteAction {
    /// New remote item → create a placeholder.
    Create { item: RemoteItem, path: RelPath },
    /// Known item changed: moved/renamed (`from != to`) and/or new content.
    Change {
        item: RemoteItem,
        from: RelPath,
        to: RelPath,
        content: bool,
    },
    /// Gone remotely (deleted, or moved out of this root).
    Delete {
        id: RemoteItemId,
        path: RelPath,
        is_dir: bool,
    },
    /// Became ignored: stop tracking, leave local files alone.
    Forget { id: RemoteItemId },
}

impl RemoteAction {
    fn sort_key(&self) -> (u8, isize) {
        match self {
            // Creates/changes shallow-first so parents exist before children.
            Self::Create { path, .. } => (0, path.depth() as isize),
            Self::Change { to, .. } => (0, to.depth() as isize),
            // Deletes deepest-first.
            Self::Delete { path, .. } => (1, -(path.depth() as isize)),
            Self::Forget { .. } => (2, 0),
        }
    }
}

struct Node<'a> {
    parent: Option<&'a RemoteItemId>,
    name: &'a str,
}

/// `known`: last-synced items of the root. `changes`: remote items from the change
/// feed (may contain duplicates, items outside the root and the root itself).
/// `full`: `changes` is a complete enumeration, so known items missing from it are gone.
pub fn plan(
    root_id: &RemoteItemId,
    known: &HashMap<RemoteItemId, ItemRecord>,
    changes: &[RemoteItem],
    full: bool,
    ignore: &IgnoreRules,
) -> Vec<RemoteAction> {
    // Last occurrence of an id wins.
    let mut latest: HashMap<&RemoteItemId, &RemoteItem> = HashMap::new();
    for c in changes {
        latest.insert(&c.id, c);
    }

    // Tree view after applying the batch.
    let mut view: HashMap<&RemoteItemId, Node<'_>> = known
        .values()
        .map(|r| {
            (
                &r.remote_id,
                Node {
                    parent: r.parent_id.as_ref(),
                    name: &r.name,
                },
            )
        })
        .collect();
    for (id, item) in &latest {
        if item.deleted {
            view.remove(id);
        } else {
            view.insert(
                id,
                Node {
                    parent: item.parent_id.as_ref(),
                    name: &item.name,
                },
            );
        }
    }

    let mut cache: HashMap<RemoteItemId, Option<RelPath>> = HashMap::new();
    let mut actions = Vec::new();

    for (id, item) in &latest {
        if *id == root_id {
            continue;
        }
        let old = known.get(*id);
        let new_path = if item.deleted {
            None
        } else {
            path_of(id, root_id, &view, &mut cache)
        };

        match (old, new_path) {
            (Some(old), None) => actions.push(RemoteAction::Delete {
                id: old.remote_id.clone(),
                path: old.rel_path.clone(),
                is_dir: old.is_dir,
            }),
            (None, None) => {}
            (old, Some(path)) if ignore.is_ignored(&path, item.is_dir()) => {
                if old.is_some() {
                    actions.push(RemoteAction::Forget {
                        id: item.id.clone(),
                    });
                }
            }
            (None, Some(path)) => actions.push(RemoteAction::Create {
                item: (*item).clone(),
                path,
            }),
            (Some(old), Some(to)) => {
                let content = old.to_remote().content_differs(item);
                let moved = old.rel_path != to;
                if moved || content || old.etag != item.etag {
                    actions.push(RemoteAction::Change {
                        item: (*item).clone(),
                        from: old.rel_path.clone(),
                        to,
                        content,
                    });
                }
            }
        }
    }

    if full {
        let seen: HashSet<&RemoteItemId> = latest.keys().copied().collect();
        for r in known.values().filter(|r| !seen.contains(&r.remote_id)) {
            actions.push(RemoteAction::Delete {
                id: r.remote_id.clone(),
                path: r.rel_path.clone(),
                is_dir: r.is_dir,
            });
        }
    }

    // A deleted folder implies its descendants; drop redundant child deletes.
    let deleted_dirs: Vec<RelPath> = actions
        .iter()
        .filter_map(|a| match a {
            RemoteAction::Delete {
                path, is_dir: true, ..
            } => Some(path.clone()),
            _ => None,
        })
        .collect();
    actions.retain(|a| match a {
        RemoteAction::Delete { path, .. } => !deleted_dirs
            .iter()
            .any(|d| d != path && path.starts_with(d)),
        _ => true,
    });

    actions.sort_by_key(RemoteAction::sort_key);
    actions
}

fn path_of(
    id: &RemoteItemId,
    root_id: &RemoteItemId,
    view: &HashMap<&RemoteItemId, Node<'_>>,
    cache: &mut HashMap<RemoteItemId, Option<RelPath>>,
) -> Option<RelPath> {
    if id == root_id {
        return Some(RelPath::root());
    }
    if let Some(p) = cache.get(id) {
        return p.clone();
    }
    // Iterative walk up (guards against cycles in inconsistent batches).
    let mut chain = Vec::new();
    let mut cur = id;
    let base = loop {
        if cur == root_id {
            break Some(RelPath::root());
        }
        if let Some(p) = cache.get(cur) {
            break p.clone();
        }
        if chain.len() > 256 {
            break None;
        }
        match view.get(cur) {
            Some(node) => {
                chain.push((cur, node.name));
                match node.parent {
                    Some(p) => cur = p,
                    None => break None,
                }
            }
            None => break None,
        }
    };
    let mut path = base;
    for (cid, name) in chain.into_iter().rev() {
        path = path.map(|p| p.join(&naming::sanitize_for_windows(name)));
        cache.insert(cid.clone(), path.clone());
    }
    path
}

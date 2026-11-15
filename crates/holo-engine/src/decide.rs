//! Pure per-item decisions shared by the real sync ([`crate::remote`]) and the
//! dry-run preview ([`crate::preview`]), so the preview can never drift from what
//! the engine actually does.

use std::{collections::HashSet, io};

use holo_cfapi::{LocalState, PinState};
use holo_core::{ContentHash, HashAlgo, RemoteItem, RemoteItemId};

pub fn identity(st: &LocalState) -> Option<RemoteItemId> {
    st.identity()
        .and_then(|b| std::str::from_utf8(b).ok())
        .map(RemoteItemId::new)
}

/// What happens when a new remote item has to appear at a local path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreateOutcome {
    /// Nothing there: create a dehydrated placeholder (content downloads on demand).
    Placeholder,
    /// Already linked to this remote item (re-applied after a crash).
    AlreadyLinked,
    /// A local folder with the same name: adopt it.
    MergeFolder,
    /// A local file with identical content: adopt it, no transfer.
    LinkIdentical,
    /// A stale placeholder of an item deleted in the same batch: replace it.
    ReplaceStale,
    /// Different local content: keep it as a conflict copy (uploaded) and place the remote one.
    Conflict,
}

/// `local_hash` is only called when a content comparison is needed.
pub fn decide_create(
    local: Option<&LocalState>,
    item: &RemoteItem,
    deleting: &HashSet<RemoteItemId>,
    local_hash: impl FnOnce(HashAlgo) -> io::Result<Option<ContentHash>>,
) -> io::Result<CreateOutcome> {
    let Some(st) = local else {
        return Ok(CreateOutcome::Placeholder);
    };
    let id = identity(st);
    if id.as_ref() == Some(&item.id) {
        return Ok(CreateOutcome::AlreadyLinked);
    }
    if id.is_none() && st.is_dir && item.is_dir() {
        return Ok(CreateOutcome::MergeFolder);
    }
    if id.is_none()
        && !st.is_dir
        && !item.is_dir()
        && let Some(remote) = &item.hash
        && local_hash(remote.algo)?.as_ref() == Some(remote)
    {
        return Ok(CreateOutcome::LinkIdentical);
    }
    if id.is_some_and(|id| deleting.contains(&id)) {
        return Ok(CreateOutcome::ReplaceStale);
    }
    Ok(CreateOutcome::Conflict)
}

/// What happens when a known file's remote content changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentOutcome {
    /// Gone locally: recreate the placeholder.
    Recreate,
    /// Unchanged locally: drop local bytes; pinned files are downloaded again right away.
    Refresh { pinned: bool },
    /// Changed on both sides: keep both.
    Conflict,
}

pub fn decide_content(local: Option<&LocalState>) -> ContentOutcome {
    match local {
        None => ContentOutcome::Recreate,
        Some(st) if !st.needs_upload() => ContentOutcome::Refresh {
            pinned: st
                .placeholder
                .as_ref()
                .is_some_and(|p| p.pin == PinState::Pinned),
        },
        Some(_) => ContentOutcome::Conflict,
    }
}

/// What happens to a local file whose remote item was deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteOutcome {
    /// Only a dehydrated placeholder: just remove it.
    Remove,
    /// Fully local and unchanged: move to the holo trash.
    Trash,
    /// Ignored local file: leave it alone.
    Skip,
    /// Changed locally: keep it and upload it again as a new item.
    KeepAndReupload,
}

pub fn decide_delete_file(st: &LocalState, ignored: bool) -> DeleteOutcome {
    match &st.placeholder {
        Some(p) if p.in_sync && p.partial => DeleteOutcome::Remove,
        Some(p) if p.in_sync => DeleteOutcome::Trash,
        _ if ignored => DeleteOutcome::Skip,
        _ => DeleteOutcome::KeepAndReupload,
    }
}

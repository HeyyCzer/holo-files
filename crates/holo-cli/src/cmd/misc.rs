use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

use anyhow::bail;
use holo_state::{OpRecord, OpState, RootRecord};

use super::Ctx;
use crate::{OpsAction, StreamArgs};

pub fn config(ctx: &Ctx) -> anyhow::Result<()> {
    println!("config : {}", ctx.paths.config_file.display());
    println!("state  : {}", ctx.paths.db().display());
    println!(
        "tokens : {} (DPAPI-encrypted)",
        ctx.paths.tokens().display()
    );
    println!("logs   : {}", ctx.paths.logs().display());
    println!("trash  : {}", ctx.paths.trash().display());
    Ok(())
}

pub async fn status(ctx: &Ctx, stream: StreamArgs) -> anyhow::Result<()> {
    print_status(ctx)?;
    if !stream.stream {
        return Ok(());
    }
    println!("-- watching (Ctrl+C to stop) --");
    let mut last: HashMap<String, RootSnapshot> = HashMap::new();
    for r in ctx.state.roots()? {
        last.insert(r.id.clone(), RootSnapshot::read(ctx, &r)?);
    }
    watch(stream, || {
        let roots = ctx.state.roots()?;
        let live: HashSet<&str> = roots.iter().map(|r| r.id.as_str()).collect();
        last.retain(|id, snap| {
            let keep = live.contains(id.as_str());
            if !keep {
                println!("{}  {}  root removed", now(), snap.local_path);
            }
            keep
        });
        for r in &roots {
            let cur = RootSnapshot::read(ctx, r)?;
            match last.get(&r.id) {
                Some(prev) if *prev == cur => {}
                prev => println!("{}  {}", now(), cur.line(prev)),
            }
            last.insert(r.id.clone(), cur);
        }
        Ok(())
    })
    .await
}

#[derive(PartialEq, Eq)]
struct RootSnapshot {
    local_path: String,
    items: u64,
    synced: bool,
    pending: u64,
    held: u64,
    failed: u64,
}

impl RootSnapshot {
    fn read(ctx: &Ctx, r: &RootRecord) -> anyhow::Result<Self> {
        let c = ctx.state.op_counts(&r.id)?;
        Ok(Self {
            local_path: r.local_path.clone(),
            items: ctx.state.item_count(&r.id)?,
            synced: r.cursor.is_some(),
            pending: c.pending,
            held: c.held,
            failed: c.failed,
        })
    }

    /// One compact line; numbers that moved since `prev` carry their delta.
    fn line(&self, prev: Option<&Self>) -> String {
        let field = |name: &str, cur: u64, old: Option<u64>| match old {
            Some(o) if o != cur => format!("{name}={cur} ({:+})", cur as i64 - o as i64),
            _ => format!("{name}={cur}"),
        };
        let mut s = format!(
            "{}  {}  {}  {}  {}",
            self.local_path,
            field("items", self.items, prev.map(|p| p.items)),
            field("pending", self.pending, prev.map(|p| p.pending)),
            field("held", self.held, prev.map(|p| p.held)),
            field("failed", self.failed, prev.map(|p| p.failed)),
        );
        if !self.synced {
            s.push_str("  (initial sync pending)");
        } else if prev.is_some_and(|p| !p.synced) {
            s.push_str("  (initial sync done)");
        }
        s
    }
}

fn print_status(ctx: &Ctx) -> anyhow::Result<()> {
    let roots = ctx.state.roots()?;
    if roots.is_empty() {
        println!("No sync roots.");
    }
    for r in roots {
        let c = ctx.state.op_counts(&r.id)?;
        let items = ctx.state.item_count(&r.id)?;
        let synced = if r.cursor.is_some() {
            "initial sync done"
        } else {
            "initial sync pending"
        };
        println!("{}  ↔  {}:{}", r.local_path, r.account_id, r.remote_path);
        println!("   {items} items tracked, {synced}");
        println!(
            "   ops: {} pending, {} held, {} failed",
            c.pending, c.held, c.failed
        );
        if c.held > 0 {
            println!(
                "   ! held operations need confirmation: `holo ops` then `holo ops release` or `holo ops discard`"
            );
        }
    }
    Ok(())
}

pub async fn ops(ctx: &Ctx, action: OpsAction, stream: StreamArgs) -> anyhow::Result<()> {
    if stream.stream && !matches!(action, OpsAction::List) {
        bail!("--stream only works with `holo ops` / `holo ops list`");
    }
    match action {
        OpsAction::List => {
            let ops = ctx.state.ops(None)?;
            if ops.is_empty() {
                println!("Journal is empty.");
            }
            for o in &ops {
                println!("{}", op_line(o));
            }
            if stream.stream {
                println!("-- watching (Ctrl+C to stop) --");
                let mut last: HashMap<i64, OpRecord> = ops.into_iter().map(|o| (o.id, o)).collect();
                watch(stream, || {
                    let cur: HashMap<i64, OpRecord> = ctx
                        .state
                        .ops(None)?
                        .into_iter()
                        .map(|o| (o.id, o))
                        .collect();
                    print_op_changes(&last, &cur);
                    last = cur;
                    Ok(())
                })
                .await?;
            }
        }
        OpsAction::Release => println!(
            "Released {} held operation(s).",
            ctx.state.release_held_ops(None)?
        ),
        OpsAction::Discard => println!(
            "Discarded {} held operation(s).",
            ctx.state.discard_held_ops(None)?
        ),
        OpsAction::Retry => {
            let mut n = 0;
            for o in ctx
                .state
                .ops(None)?
                .into_iter()
                .filter(|o| o.state == OpState::Failed)
            {
                ctx.state.set_op_state(o.id, OpState::Pending, None)?;
                n += 1;
            }
            println!("Re-queued {n} failed operation(s).");
        }
    }
    Ok(())
}

/// `+` new, `~` state/attempts/error changed, `-` left the journal.
fn print_op_changes(last: &HashMap<i64, OpRecord>, cur: &HashMap<i64, OpRecord>) {
    let mut gone: Vec<&OpRecord> = last.values().filter(|o| !cur.contains_key(&o.id)).collect();
    gone.sort_by_key(|o| o.id);
    for o in gone {
        // Finished ops are deleted from the journal; held ops only vanish on discard.
        let what = if o.state == OpState::Held {
            "dropped"
        } else {
            "done"
        };
        println!(
            "{}  - #{:<6} {:<8} {:<14} {}",
            now(),
            o.id,
            what,
            o.kind,
            o.payload
        );
    }
    let mut ids: Vec<i64> = cur.keys().copied().collect();
    ids.sort_unstable();
    for id in ids {
        let o = &cur[&id];
        match last.get(&id) {
            None => println!("{}  + {}", now(), op_line(o)),
            Some(p)
                if p.state != o.state
                    || p.attempts != o.attempts
                    || p.last_error != o.last_error =>
            {
                println!("{}  ~ {}", now(), op_line(o))
            }
            Some(_) => {}
        }
    }
}

fn op_line(o: &OpRecord) -> String {
    format!(
        "#{:<6} {:<8} {:<14} attempts={} {}{}",
        o.id,
        o.state.as_str(),
        o.kind,
        o.attempts,
        o.payload,
        o.last_error
            .as_ref()
            .map(|e| format!("  — {e}"))
            .unwrap_or_default()
    )
}

fn now() -> String {
    chrono::Local::now().format("%H:%M:%S").to_string()
}

/// Run `tick` every `--interval` ms until Ctrl+C.
async fn watch(
    stream: StreamArgs,
    mut tick: impl FnMut() -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let period = Duration::from_millis(stream.interval.max(100));
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);
    loop {
        tokio::select! {
            _ = &mut ctrl_c => return Ok(()),
            _ = tokio::time::sleep(period) => tick()?,
        }
    }
}

/// Pin = "always keep on this device"; unpin = "free up space". The running daemon
/// sees the attribute change and hydrates / dehydrates.
pub fn pin(path: &Path, keep: bool) -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        use holo_cfapi::{PinState, placeholder};
        let path = std::path::absolute(path)?;
        placeholder::set_pin(
            &path,
            if keep {
                PinState::Pinned
            } else {
                PinState::Unpinned
            },
        )?;
        if !keep && path.is_file() {
            // Freeing space needs no network: do it right away.
            let _ = placeholder::dehydrate(&path);
        }
        println!(
            "{} {}",
            if keep { "Pinned" } else { "Freed" },
            path.display()
        );
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = (path, keep);
        anyhow::bail!("pinning requires Windows")
    }
}

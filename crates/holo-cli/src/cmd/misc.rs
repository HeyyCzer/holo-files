use std::path::Path;

use holo_state::OpState;

use super::Ctx;
use crate::OpsAction;

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

pub fn status(ctx: &Ctx) -> anyhow::Result<()> {
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

pub fn ops(ctx: &Ctx, action: OpsAction) -> anyhow::Result<()> {
    match action {
        OpsAction::List => {
            let ops = ctx.state.ops(None)?;
            if ops.is_empty() {
                println!("Journal is empty.");
            }
            for o in ops {
                println!(
                    "#{:<6} {:<8} {:<14} attempts={} {}{}",
                    o.id,
                    o.state.as_str(),
                    o.kind,
                    o.attempts,
                    o.payload,
                    o.last_error.map(|e| format!("  — {e}")).unwrap_or_default()
                );
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

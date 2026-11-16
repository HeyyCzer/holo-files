//! `holo plan`: dry run. Lists what a sync would do, changes nothing.

use std::{collections::HashMap, path::Path, sync::Arc};

use anyhow::{Context as _, bail};
use holo_core::{ChangeCursor, ignore_rules::IgnoreRules};
use holo_engine::{
    preview::{Direction, PreviewInput, Report, preview},
    remote::fetch_changes_with,
};

use super::{Ctx, human_bytes};
use crate::providers;

pub struct PlanArgs<'a> {
    pub target: &'a str,
    pub remote: Option<&'a str>,
    pub account: Option<&'a str>,
    pub full: bool,
    pub no_hash: bool,
    pub json: Option<&'a Path>,
    pub show: usize,
    pub sample: Option<usize>,
}

fn arrow(d: Direction) -> &'static str {
    match d {
        Direction::Down => "↓",
        Direction::Up => "↑",
        Direction::Both => "⇄",
        Direction::None => "·",
    }
}

fn code<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_string(v)
        .unwrap_or_default()
        .trim_matches('"')
        .to_owned()
}

pub async fn plan(ctx: &Ctx, a: PlanArgs<'_>) -> anyhow::Result<()> {
    let engine_cfg = ctx.cfg.engine(&ctx.paths);

    // Either an existing root, or a prospective one (`--remote`) before `add-root`.
    let (local, account, remote_root, known, cursor, title) = match a.remote {
        Some(remote) => {
            if ctx.root(a.target).is_ok() {
                bail!(
                    "'{}' is already a sync root; run `holo plan` without --remote",
                    a.target
                );
            }
            let local = super::roots::validate_local(ctx, Path::new(a.target))?;
            let account = ctx.account(a.account)?;
            let title = format!(
                "{}  ↔  {}:{remote}  (not added yet)",
                local.display(),
                account.id
            );
            (local, account, None, HashMap::new(), None, title)
        }
        None => {
            let root = ctx.root(a.target)?;
            let account = ctx.account(Some(&root.account_id))?;
            let known = ctx
                .state
                .all_items(&root.id)?
                .into_iter()
                .map(|r| (r.remote_id.clone(), r))
                .collect();
            let cursor = if a.full {
                None
            } else {
                root.cursor.clone().map(ChangeCursor)
            };
            let title = format!(
                "{}  ↔  {}:{}",
                root.local_path, account.id, root.remote_path
            );
            (
                root.local_path.clone().into(),
                account,
                Some(root.remote_root_id),
                known,
                cursor,
                title,
            )
        }
    };

    let provider = providers::for_account(&ctx.cfg, &ctx.paths, &account).await?;
    let remote_root = match remote_root {
        Some(id) => id,
        None => {
            let remote = a.remote.expect("prospective root has --remote");
            let item = provider
                .item_by_path(remote)
                .await
                .with_context(|| format!("remote folder '{remote}'"))?;
            if !item.is_dir() {
                bail!("'{remote}' is not a folder");
            }
            item.id
        }
    };

    let started = std::time::Instant::now();
    let fetched = fetch_changes_with(
        provider.as_ref(),
        &remote_root,
        cursor,
        a.sample,
        |l| {
            let secs = started.elapsed().as_secs_f64().max(0.1);
            eprint!(
                "\rListing cloud… {} items, {} pages, {:.0} items/s   ",
                l.items,
                l.pages,
                l.items as f64 / secs
            );
        },
    )
    .await?;
    eprintln!();
    if fetched.truncated {
        eprintln!(
            "Stopped after {} items (--sample): this plan is PARTIAL.",
            fetched.changes.len()
        );
    }
    let ignore = IgnoreRules::load(&local, engine_cfg.use_default_ignores, &engine_cfg.ignore)
        .map_err(|e| anyhow::anyhow!("invalid ignore rules: {e}"))?;
    if !a.no_hash {
        eprintln!("Comparing local files (same-name files are hashed; nothing is downloaded)…");
    }

    let tree = local_tree();
    let (hash, threshold) = (!a.no_hash, engine_cfg.mass_delete_threshold);
    let local2 = local.clone();
    let report: Report = tokio::task::spawn_blocking(move || {
        preview(&PreviewInput {
            tree: tree.as_ref(),
            ignore: &ignore,
            local_root: &local2,
            remote_root: &remote_root,
            known: &known,
            changes: &fetched.changes,
            full: fetched.full,
            partial: fetched.truncated,
            hash,
            mass_delete_threshold: threshold,
        })
    })
    .await??;

    print_report(&title, &report, a.show, a.no_hash);
    if let Some(path) = a.json {
        std::fs::write(path, serde_json::to_vec_pretty(&report)?)?;
        println!("\nFull plan written to {}", path.display());
    }
    Ok(())
}

fn print_report(title: &str, r: &Report, limit: usize, no_hash: bool) {
    println!("\nPlan for {title}");
    if r.partial {
        println!(
            "  !! PARTIAL: based on a sample of {} cloud items (--sample). Totals cover the sample only;",
            r.remote_changes
        );
        println!(
            "  !! items whose parent folder wasn't sampled are omitted and no cloud deletions are inferred."
        );
    }
    println!(
        "  {} ({} remote item(s){})",
        if r.partial {
            "sample"
        } else if r.full {
            "full comparison"
        } else {
            "changes since last sync"
        },
        r.remote_changes,
        if no_hash {
            ", content NOT compared"
        } else {
            ""
        }
    );
    if r.is_empty() {
        println!("\n  Nothing to do: everything is in sync.");
        return;
    }

    println!("\n  ↓ cloud→PC   ↑ PC→cloud   ⇄ both   · no transfer now\n");
    for (kind, t) in r.summary() {
        let size = if t.bytes > 0 {
            human_bytes(t.bytes)
        } else {
            String::new()
        };
        println!(
            "  {} {:>7}  {:<64} {:>10}",
            arrow(kind.direction()),
            t.count,
            kind.describe(),
            size
        );
    }
    let (down, up) = r.network();
    println!(
        "\n  Transfer now: ↓ {}   ↑ {}      Available on demand: {}",
        human_bytes(down),
        human_bytes(up),
        human_bytes(r.on_demand())
    );

    let shown = if limit == 0 {
        r.entries.len()
    } else {
        limit.min(r.entries.len())
    };
    println!(
        "\n  Details ({shown} of {}{}):",
        r.entries.len(),
        if shown < r.entries.len() {
            "; --show 0 for all, --json FILE to export"
        } else {
            ""
        }
    );
    for e in r.entries.iter().take(shown) {
        let from = e
            .from
            .as_ref()
            .map(|f| format!("{f} → "))
            .unwrap_or_default();
        let size = if e.bytes > 0 && !e.is_dir {
            human_bytes(e.bytes)
        } else {
            String::new()
        };
        println!(
            "  {} {:<20} {from}{}{}  {size}",
            arrow(e.direction),
            code(&e.kind),
            e.path,
            if e.is_dir { "/" } else { "" }
        );
    }
    println!("\n  Dry run: nothing was changed.");
}

#[cfg(windows)]
fn local_tree() -> Arc<dyn holo_engine::LocalTree> {
    Arc::new(holo_engine::local_tree::CfLocalTree)
}

#[cfg(not(windows))]
fn local_tree() -> Arc<dyn holo_engine::LocalTree> {
    unimplemented!("holo only runs on Windows")
}

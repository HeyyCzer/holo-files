//! `--stream` views. On a terminal the panel is redrawn in place (and in `holo ops`
//! the journal log scrolls above it); when piped, plain lines are appended instead.

use std::collections::{HashMap, VecDeque};
use std::io::{IsTerminal, Write};
use std::time::{Duration, Instant};

use crossterm::{cursor, queue, terminal};
use holo_engine::ops::{ExecProgress, progress_key};
use holo_state::{OpRecord, OpState, RootRecord};

use super::{Ctx, daemon_running};
use crate::StreamArgs;

/// Throughput is averaged over this window.
const RATE_WINDOW: Duration = Duration::from_secs(30);

pub async fn status(ctx: &Ctx, stream: StreamArgs) -> anyhow::Result<()> {
    let mut live = Live::new();
    let mut rates = Rates::default();
    every(stream, &mut live, |live| {
        let footer = panel(ctx, &mut rates)?;
        live.draw(&[], &footer)
    })
    .await
}

pub async fn ops(ctx: &Ctx, stream: StreamArgs) -> anyhow::Result<()> {
    let mut live = Live::new();
    let mut rates = Rates::default();
    let mut last: HashMap<i64, OpRecord> = ctx
        .state
        .ops(None)?
        .into_iter()
        .map(|o| (o.id, o))
        .collect();
    println!(
        "{} ops in the journal (`holo ops` lists them). Watching changes, Ctrl+C to stop.",
        last.len()
    );
    every(stream, &mut live, |live| {
        let cur: HashMap<i64, OpRecord> = ctx
            .state
            .ops(None)?
            .into_iter()
            .map(|o| (o.id, o))
            .collect();
        let log = op_changes(&last, &cur);
        last = cur;
        let footer = panel(ctx, &mut rates)?;
        live.draw(&log, &footer)
    })
    .await
}

/// Draw now, then every `--interval` ms until Ctrl+C.
async fn every(
    stream: StreamArgs,
    live: &mut Live,
    mut tick: impl FnMut(&mut Live) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let period = Duration::from_millis(stream.interval.max(100));
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);
    live.start();
    let r = async {
        loop {
            tick(live)?;
            tokio::select! {
                _ = &mut ctrl_c => return Ok(()),
                _ = tokio::time::sleep(period) => {}
            }
        }
    }
    .await;
    live.finish();
    r
}

// ------------------------------------------------------------------ panel

#[derive(Default)]
struct Rates {
    /// Per root: (when, ops done) samples.
    samples: HashMap<String, VecDeque<(Instant, u64)>>,
}

impl Rates {
    /// Ops per second over the last `RATE_WINDOW`, once there is enough data.
    fn add(&mut self, root: &str, done: u64) -> Option<f64> {
        let now = Instant::now();
        let s = self.samples.entry(root.to_string()).or_default();
        if s.back().is_some_and(|(_, d)| *d > done) {
            s.clear(); // daemon restarted
        }
        s.push_back((now, done));
        while s.len() > 2 && now.duration_since(s[0].0) > RATE_WINDOW {
            s.pop_front();
        }
        let (t0, d0) = *s.front()?;
        let secs = now.duration_since(t0).as_secs_f64();
        (secs >= 2.0).then(|| (done - d0) as f64 / secs)
    }
}

fn panel(ctx: &Ctx, rates: &mut Rates) -> anyhow::Result<Vec<String>> {
    let running = daemon_running(ctx);
    let mut out = vec![format!(
        "holo · {} · daemon {} · Ctrl+C to stop",
        chrono::Local::now().format("%H:%M:%S"),
        if running { "running" } else { "stopped" }
    )];
    let roots = ctx.state.roots()?;
    if roots.is_empty() {
        out.push("No sync roots.".into());
    }
    for r in &roots {
        out.push(String::new());
        root_panel(ctx, r, running, rates, &mut out)?;
    }
    Ok(out)
}

fn root_panel(
    ctx: &Ctx,
    r: &RootRecord,
    daemon: bool,
    rates: &mut Rates,
    out: &mut Vec<String>,
) -> anyhow::Result<()> {
    let c = ctx.state.op_counts(&r.id)?;
    let items = ctx.state.item_count(&r.id)?;
    out.push(format!(
        "{}  ↔  {}{}",
        r.local_path,
        r.remote_path,
        if r.cursor.is_some() {
            ""
        } else {
            "   (initial sync pending)"
        }
    ));
    out.push(format!(
        "  items {items}   pending {}   held {}   failed {}",
        c.pending, c.held, c.failed
    ));
    if c.held > 0 {
        out.push("  ! held ops need `holo ops release` or `holo ops discard`".into());
    }
    if !daemon {
        return Ok(());
    }
    let progress: ExecProgress = ctx
        .state
        .kv_get(&progress_key(&r.id))?
        .and_then(|j| serde_json::from_str(&j).ok())
        .unwrap_or_default();
    let rate = rates.add(&r.id, progress.done);
    let speed = match rate {
        Some(r) if r > 0.0 => {
            let eta = if c.pending > 0 {
                format!(
                    " · ETA {}",
                    duration(Duration::from_secs_f64(c.pending as f64 / r))
                )
            } else {
                String::new()
            };
            format!("{r:.1} ops/s{eta}")
        }
        Some(_) if c.pending > 0 => "0 ops/s (waiting: retries/deferred?)".into(),
        Some(_) => "idle".into(),
        None => "measuring…".into(),
    };
    out.push(format!(
        "  {speed} · {} done since start{}",
        progress.done,
        if progress.failed > 0 {
            format!(", {} failed", progress.failed)
        } else {
            String::new()
        }
    ));
    let workers = if progress.auto {
        format!(
            "{} of max {} (auto: {})",
            progress.workers, progress.max_workers, progress.workers_reason
        )
    } else {
        progress.workers.to_string()
    };
    out.push(format!(
        "  running {} · workers {workers}:",
        progress.running.len()
    ));
    let now = chrono::Utc::now().timestamp_millis();
    for op in &progress.running {
        let age = Duration::from_millis((now - op.started_ms).max(0) as u64);
        out.push(format!(
            "    {:<13} {:>6}  {}",
            op.kind,
            duration(age),
            Path(op.path.clone())
        ));
    }
    Ok(())
}

fn duration(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..60 => format!("{:.1}s", d.as_secs_f64()),
        60..3600 => format!("{}m{:02}s", s / 60, s % 60),
        _ => format!("{}h{:02}m", s / 3600, (s % 3600) / 60),
    }
}

// -------------------------------------------------------------------- log

/// `+` queued, `~` retry/failed/held, `✓` done, `-` dropped (held op discarded).
fn op_changes(last: &HashMap<i64, OpRecord>, cur: &HashMap<i64, OpRecord>) -> Vec<String> {
    let t = chrono::Local::now().format("%H:%M:%S");
    let mut out = Vec::new();
    let mut gone: Vec<&OpRecord> = last.values().filter(|o| !cur.contains_key(&o.id)).collect();
    gone.sort_by_key(|o| o.id);
    for o in gone {
        // Finished ops are deleted from the journal; held ops only vanish on discard.
        let mark = if o.state == OpState::Held { "-" } else { "✓" };
        out.push(format!("{t}  {mark} {:<13} {}", o.kind, Path(target(o))));
    }
    let mut ids: Vec<i64> = cur.keys().copied().collect();
    ids.sort_unstable();
    for id in ids {
        let o = &cur[&id];
        let line = match last.get(&id) {
            None => format!("{t}  + {:<13} {}", o.kind, Path(target(o))),
            Some(p)
                if p.state != o.state
                    || p.attempts != o.attempts
                    || p.last_error != o.last_error =>
            {
                let what = match o.state {
                    OpState::Pending => format!("retry #{}", o.attempts),
                    s => s.as_str().to_string(),
                };
                format!(
                    "{t}  ~ {:<13} {} [{what}]{}",
                    o.kind,
                    Path(target(o)),
                    o.last_error
                        .as_ref()
                        .map(|e| format!(" {e}"))
                        .unwrap_or_default()
                )
            }
            Some(_) => continue,
        };
        out.push(line);
    }
    out
}

/// The path an op touches, from its JSON payload.
fn target(o: &OpRecord) -> String {
    serde_json::from_str::<serde_json::Value>(&o.payload)
        .ok()
        .and_then(|v| {
            v.get("path")
                .or_else(|| v.get("to"))
                .and_then(|p| p.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| o.payload.clone())
}

/// A path that is shortened from the left (keeping the file name) when the line is
/// too wide. Marked with control chars so `fit` knows what it may cut.
struct Path(String);

const PATH_START: char = '\u{1}';
const PATH_END: char = '\u{2}';

impl std::fmt::Display for Path {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{PATH_START}{}{PATH_END}", self.0)
    }
}

/// Make `line` at most `width` columns: cut the marked path from the left first,
/// then the end of the line.
fn fit(line: &str, width: usize) -> String {
    let plain_len = line
        .chars()
        .filter(|c| *c != PATH_START && *c != PATH_END)
        .count();
    let mut s: String = if plain_len > width
        && let (Some(a), Some(b)) = (line.find(PATH_START), line.find(PATH_END))
    {
        let path = &line[a + 1..b];
        let excess = plain_len - width;
        let path_len = path.chars().count();
        // Room for "…" + tail; at least one char of the tail.
        let keep = path_len.saturating_sub(excess + 1).max(1);
        let tail: String = path.chars().skip(path_len - keep).collect();
        let short = if keep < path_len {
            format!("…{tail}")
        } else {
            tail
        };
        format!("{}{short}{}", &line[..a], &line[b + 1..])
    } else {
        line.replace([PATH_START, PATH_END], "")
    };
    if s.chars().count() > width {
        s = s.chars().take(width.saturating_sub(1)).collect();
        s.push('…');
    }
    s
}

// ------------------------------------------------------------------- live

struct Live {
    tty: bool,
    /// Footer lines currently on screen (redrawn next time).
    lines: u16,
    last_footer: Vec<String>,
}

impl Live {
    fn new() -> Self {
        Self {
            tty: std::io::stdout().is_terminal(),
            lines: 0,
            last_footer: Vec::new(),
        }
    }

    fn start(&mut self) {
        if self.tty {
            let _ = queue!(std::io::stdout(), cursor::Hide);
        }
    }

    fn finish(&mut self) {
        if self.tty {
            let mut out = std::io::stdout();
            let _ = queue!(out, cursor::Show);
            let _ = out.flush();
        }
    }

    /// Append `log` above the footer and redraw the footer in place.
    fn draw(&mut self, log: &[String], footer: &[String]) -> anyhow::Result<()> {
        let mut out = std::io::stdout().lock();
        if !self.tty {
            // Piped: no cursor tricks; print the panel only when it changed.
            for l in log {
                writeln!(out, "{}", fit(l, usize::MAX))?;
            }
            // The first line carries the clock; it alone doesn't count as a change.
            if footer.get(1..) != self.last_footer.get(1..) {
                for l in footer {
                    writeln!(out, "{}", fit(l, usize::MAX))?;
                }
                writeln!(out)?;
                self.last_footer = footer.to_vec();
            }
            return Ok(out.flush()?);
        }
        let (w, h) = terminal::size().unwrap_or((120, 40));
        let width = (w as usize).saturating_sub(1).max(20);
        if self.lines > 0 {
            queue!(out, cursor::MoveToPreviousLine(self.lines))?;
        }
        queue!(out, terminal::Clear(terminal::ClearType::FromCursorDown))?;
        for l in log {
            writeln!(out, "{}", fit(l, width))?;
        }
        let max = (h as usize).saturating_sub(1).max(1);
        let shown = &footer[..footer.len().min(max)];
        for l in shown {
            writeln!(out, "{}", fit(l, width))?;
        }
        self.lines = shown.len() as u16;
        Ok(out.flush()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_shortens_path_from_the_left() {
        let line = format!(
            "12:00:00  ✓ upload        {}",
            Path("a/b/c/d/file.ts".into())
        );
        assert_eq!(fit(&line, 100), "12:00:00  ✓ upload        a/b/c/d/file.ts");
        let short = fit(&line, 34);
        assert_eq!(short.chars().count(), 34);
        assert!(short.ends_with("…file.ts"), "{short}");
        let wider = fit(&line, 40);
        assert_eq!(wider, "12:00:00  ✓ upload        …b/c/d/file.ts");
    }

    #[test]
    fn fit_truncates_plain_lines() {
        assert_eq!(fit("abcdefgh", 5), "abcd…");
        assert_eq!(fit("abc", 5), "abc");
    }
}

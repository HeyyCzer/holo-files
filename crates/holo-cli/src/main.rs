mod cmd;
mod config;
mod providers;

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use holo_core::ProviderKind;

use crate::config::{Config, Paths};

#[derive(Parser)]
#[command(
    name = "holo",
    version,
    about = "Fast OneDrive client with Files On-Demand"
)]
struct Cli {
    /// Verbose logging (`-v` debug, `-vv` trace).
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Show config/data locations.
    Config,
    /// Sign in to a cloud account (opens the browser).
    Login {
        #[arg(long, default_value = "onedrive")]
        provider: ProviderKind,
    },
    /// Sign out and forget an account's tokens.
    Logout { account: String },
    /// List accounts with quota.
    Accounts,
    /// Register a folder as a Files On-Demand sync root.
    AddRoot {
        /// Local folder (created if missing; must be on NTFS, not under %TEMP%).
        local: PathBuf,
        /// Remote folder path, e.g. `/Documents`. Default: drive root.
        #[arg(long, default_value = "/")]
        remote: String,
        /// Account id (see `holo accounts`). Optional if there is only one.
        #[arg(long)]
        account: Option<String>,
        /// Create the remote folder (and missing parents) if it doesn't exist.
        #[arg(long)]
        create: bool,
    },
    /// Unregister a sync root. Files that are only in the cloud disappear locally.
    RemoveRoot {
        /// Local path or root id.
        root: String,
        /// Confirm.
        #[arg(long)]
        yes: bool,
    },
    /// List sync roots.
    Roots,
    /// Dry run: compare cloud, local folder and last sync; list what would change.
    ///
    /// For an existing root pass its path or id. Before `add-root`, pass the local
    /// folder plus `--remote` to preview the first sync (cold start).
    Plan {
        /// Sync root (path or id), or a local folder together with --remote.
        target: String,
        /// Remote folder for a root not added yet (cold-start preview).
        #[arg(long)]
        remote: Option<String>,
        #[arg(long)]
        account: Option<String>,
        /// Compare against a full remote listing instead of changes since last sync.
        #[arg(long)]
        full: bool,
        /// Don't hash same-name local files (faster; those show as "unverified").
        #[arg(long)]
        no_hash: bool,
        /// Write the full plan as JSON.
        #[arg(long)]
        json: Option<PathBuf>,
        /// Detail lines to print (0 = all). Only affects output, not the work done.
        #[arg(long, alias = "limit", default_value_t = 100)]
        show: usize,
        /// Stop listing the cloud after N items and show a PARTIAL plan (quick look;
        /// totals cover the sample only, no remote deletions are inferred).
        #[arg(long)]
        sample: Option<usize>,
    },
    /// Run the sync engine in the foreground (Ctrl+C to stop).
    Run,
    /// Show per-root sync status.
    Status {
        #[command(flatten)]
        stream: StreamArgs,
    },
    /// Inspect or act on the local operation journal.
    Ops {
        #[command(subcommand)]
        action: Option<OpsAction>,
        #[command(flatten)]
        stream: StreamArgs,
    },
    /// Run `holo run` in the background at logon (per-user scheduled task, no window).
    Service {
        #[command(subcommand)]
        action: ServiceAction,
    },
    /// "Always keep on this device" for a file or folder.
    Pin { path: PathBuf },
    /// "Free up space": keep only in the cloud.
    Free { path: PathBuf },
}

#[derive(Subcommand)]
pub enum ServiceAction {
    /// Register the logon task (and start it now unless --no-start).
    Install {
        #[arg(long)]
        no_start: bool,
    },
    /// Stop the daemon and remove the logon task.
    Uninstall,
    /// Start the daemon now through the task.
    Start,
    /// Ask the running daemon to shut down cleanly.
    Stop,
    /// Show whether the task is installed and the daemon is running.
    Status,
}

#[derive(clap::Args, Clone, Copy)]
pub struct StreamArgs {
    /// Keep watching and print changes as they happen (Ctrl+C to stop).
    #[arg(long)]
    pub stream: bool,
    /// Poll interval for --stream, in milliseconds.
    #[arg(long, default_value_t = 1000, requires = "stream")]
    pub interval: u64,
}

#[derive(Subcommand)]
pub enum OpsAction {
    /// List queued operations (default).
    List,
    /// Allow held operations (e.g. after the mass-delete guard) to run.
    Release,
    /// Drop held operations.
    Discard,
    /// Re-queue permanently failed operations.
    Retry,
}

fn init_logging(
    verbose: u8,
    paths: &Paths,
    to_file: bool,
) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    use tracing_subscriber::{EnvFilter, fmt, prelude::*};
    let level = match verbose {
        0 => "info",
        1 => "debug",
        _ => "trace",
    };
    let filter = EnvFilter::try_from_env("HOLO_LOG")
        .unwrap_or_else(|_| EnvFilter::new(format!("warn,holo_cli={level},holo_engine={level},holo_onedrive={level},holo_cfapi={level},holo_auth={level}")));
    let stderr = fmt::layer().with_target(false).with_writer(std::io::stderr);
    if to_file {
        let appender = tracing_appender::rolling::daily(paths.logs(), "holo.log");
        let (writer, guard) = tracing_appender::non_blocking(appender);
        tracing_subscriber::registry()
            .with(filter)
            .with(stderr)
            .with(fmt::layer().with_ansi(false).json().with_writer(writer))
            .init();
        Some(guard)
    } else {
        tracing_subscriber::registry()
            .with(filter)
            .with(stderr)
            .init();
        None
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let paths = Paths::resolve()?;
    let _guard = init_logging(cli.verbose, &paths, matches!(cli.command, Command::Run));
    let cfg = Config::load(&paths)?;
    let ctx = cmd::Ctx::new(cfg, paths)?;

    match cli.command {
        Command::Config => cmd::misc::config(&ctx),
        Command::Login { provider } => cmd::accounts::login(&ctx, provider).await,
        Command::Logout { account } => cmd::accounts::logout(&ctx, &account).await,
        Command::Accounts => cmd::accounts::list(&ctx).await,
        Command::AddRoot {
            local,
            remote,
            account,
            create,
        } => cmd::roots::add(&ctx, &local, &remote, account.as_deref(), create).await,
        Command::RemoveRoot { root, yes } => cmd::roots::remove(&ctx, &root, yes),
        Command::Roots => cmd::roots::list(&ctx),
        Command::Plan {
            target,
            remote,
            account,
            full,
            no_hash,
            json,
            show,
            sample,
        } => {
            cmd::plan::plan(
                &ctx,
                cmd::plan::PlanArgs {
                    target: &target,
                    remote: remote.as_deref(),
                    account: account.as_deref(),
                    full,
                    no_hash,
                    json: json.as_deref(),
                    show,
                    sample,
                },
            )
            .await
        }
        Command::Run => cmd::run::run(&ctx).await,
        Command::Status { stream } => cmd::misc::status(&ctx, stream).await,
        Command::Ops { action, stream } => {
            cmd::misc::ops(&ctx, action.unwrap_or(OpsAction::List), stream).await
        }
        Command::Service { action } => cmd::service::service(&ctx, action).await,
        Command::Pin { path } => cmd::misc::pin(&path, true),
        Command::Free { path } => cmd::misc::pin(&path, false),
    }
}

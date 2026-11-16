//! `holo` CLI internals, shared with the `holo-ui` tray app.

pub mod cmd;
pub mod config;
pub mod load;
pub mod providers;

use clap::Subcommand;

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
    Status {
        /// Print nothing; exit code 0 if the daemon is running, 1 if not (for scripts).
        #[arg(long)]
        check: bool,
    },
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

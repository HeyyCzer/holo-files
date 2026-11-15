//! Provider-agnostic sync engine.
//!
//! ```text
//!            ┌────────────── remote poller ──────────────┐
//! provider ──┤ changes() → plan (pure) → apply to LocalTree├── state (SQLite)
//!            └───────────────────────────────────────────┘
//!  Cloud Filter callbacks ─► LocalEvent ─► debounce/classify ─► op journal ─► executor ─► provider
//!  Cloud Filter fetch_data ─► hydrate (ranged download) ─► placeholder
//! ```
//!
//! The engine only talks to the filesystem through [`LocalTree`], so all logic can be
//! tested with an in-memory tree and a fake provider.

pub mod config;
pub mod context;
pub mod decide;
pub mod hydrate;
pub mod local;
pub mod local_tree;
pub mod ops;
pub mod plan;
pub mod preview;
pub mod remote;
pub mod runtime;

pub use config::EngineConfig;
pub use context::RootContext;
pub use local_tree::LocalTree;
pub use runtime::RootHandle;

#[cfg(test)]
mod tests;

//! Files On-Demand on top of the Windows Cloud Filter API (`cldflt.sys` / `cfapi.dll`).
//!
//! This crate knows nothing about clouds. It exposes:
//! - sync-root registration ([`root`]),
//! - placeholder operations used by the engine ([`placeholder`]),
//! - a Cloud Filter callback bridge ([`filter`]) that forwards to a [`CallbackHandler`].
//!
//! ## Design rule: never block Explorer
//! Rename/delete/dehydrate callbacks are approved immediately and forwarded as
//! [`LocalEvent`]s; any network work happens later on the engine's queue. Only
//! `fetch_data` (hydration) waits on the network, because the reading process
//! genuinely needs the bytes.

pub mod aligned;
mod types;

#[cfg(windows)]
pub mod filter;
#[cfg(windows)]
pub mod placeholder;
#[cfg(windows)]
pub mod root;

pub use types::*;

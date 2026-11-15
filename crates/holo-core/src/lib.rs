//! Provider-agnostic core of holo-files.
//!
//! Everything in this crate is pure (no I/O besides reading ignore files) so the sync
//! engine can be tested without a network or a Cloud Filter sync root.

pub mod error;
pub mod hash;
pub mod ignore_rules;
pub mod model;
pub mod naming;
pub mod path;
pub mod provider;

pub use error::{ProviderError, ProviderResult};
pub use model::*;
pub use path::RelPath;
pub use provider::{ByteStream, CloudProvider, UploadRequest, UploadTarget};

//! Generic Engine abstraction for Wasmer Engines.

#![deny(missing_docs, trivial_numeric_casts, unused_extern_crates)]
#![warn(unused_import_braces)]
#![cfg_attr(
    feature = "cargo-clippy",
    allow(clippy::new_without_default, clippy::new_without_default)
)]
#![cfg_attr(
    feature = "cargo-clippy",
    warn(
        clippy::float_arithmetic,
        clippy::mut_mut,
        clippy::nonminimal_bool,
        clippy::option_map_unwrap_or,
        clippy::option_map_unwrap_or_else,
        clippy::print_stdout,
        clippy::unicode_not_nfc,
        clippy::use_self
    )
)]

mod engine;
mod error;
mod executable;
mod resolver;
mod trap;

pub use crate::engine::{Engine, EngineId};
pub use crate::error::{DeserializeError, ImportError, InstantiationError, LinkError};
pub use crate::executable::Executable;
pub use crate::resolver::resolve_imports;
pub use crate::trap::*;

/// Version number of this crate.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

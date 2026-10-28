//! The Simeji 23.6.1 conversion engine, executed from its original ARM
//! libraries and dictionaries.
//!
//! [`elf`] links the bundled Android libraries into guest memory, [`arm`]
//! runs them in a bounded Unicorn machine, and the host module implements
//! their Android libc imports. [`Engine`] is the conversion session on top.

pub mod arm;
pub mod elf;
mod engine;
mod error;
mod host;
mod user_dictionary;

pub use engine::{Engine, EnginePaths};
pub use error::EngineError;
pub use simeji_protocol::{Candidate, Clause, Conversion};

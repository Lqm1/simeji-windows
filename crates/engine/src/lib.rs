//! The Simeji 23.6.1 conversion engine, executed from its original ARM
//! libraries and dictionaries.
//!
//! [`elf`] links the bundled Android libraries into guest memory, [`arm`]
//! runs them in a bounded Unicorn machine, and the private `host` module
//! answers their Android libc imports. [`Engine`] is the conversion session
//! built on top: romaji transliteration, sentence and clause conversion,
//! learning and the user dictionary.

pub mod arm;
pub mod elf;
mod engine;
mod host;
mod user_dictionary;

pub use arm::ArmError;
pub use engine::Engine;
pub use simeji_protocol::{Candidate, Clause, Conversion};

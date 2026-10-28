//! The conversion worker process.
//!
//! The TSF DLL runs inside every application that receives text, so it keeps
//! no engine state. This process owns the [`simeji_engine::Engine`] and the
//! optional cloud lookups, and answers [`simeji_protocol::Request`]s on the
//! user's [`simeji_protocol::WORKER_PIPE`].

pub mod cloud;
pub mod server;
pub mod user_data;

use std::path::PathBuf;

const DICTIONARY_DIRECTORY: &str = "Dictionary";

pub fn installed_dictionary() -> std::io::Result<PathBuf> {
    let executable = std::env::current_exe()?;
    let directory = executable
        .parent()
        .ok_or_else(|| std::io::Error::other("The executable has no directory"))?;
    Ok(directory.join(DICTIONARY_DIRECTORY))
}

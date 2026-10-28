//! The conversion worker process.
//!
//! The TSF DLL runs inside every application that receives text, so it keeps
//! no engine state. This process owns the [`simeji_engine::Engine`] and the
//! optional cloud lookups, and answers [`simeji_protocol::Request`]s over the
//! owner-only pipe from [`simeji_transport`].

pub mod cloud;
pub mod server;

use std::path::PathBuf;

/// `%LOCALAPPDATA%\SimejiWindows\UserData`: learning, user dictionary and cloud settings.
pub fn user_data_directory() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .map(|local| PathBuf::from(local).join("SimejiWindows").join("UserData"))
}

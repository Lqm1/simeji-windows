//! Opt-in cloud conversion. [`CloudClient`] performs the HTTP lookups and
//! caching; [`CloudQueue`] runs them in the background for the latest reading.

mod client;
mod queue;

pub use client::CloudClient;
pub use queue::CloudQueue;
pub use simeji_protocol::CloudWord;

use std::path::Path;

/// File in the user data directory that records the user's opt-in.
const ENABLED_FILE: &str = "cloud-enabled.json";

/// Whether the user enabled cloud conversion. Read on every use so that the
/// running worker follows `simeji-cloud enable` and `disable` immediately.
/// Anything other than an explicit `true` keeps readings on this machine.
fn enabled_in(directory: &Path) -> bool {
    std::fs::read(directory.join(ENABLED_FILE)).is_ok_and(|bytes| bytes == b"true")
}

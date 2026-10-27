//! Opt-in cloud conversion. [`CloudClient`] performs the HTTP lookups and
//! caching; [`CloudQueue`] runs them in the background for the latest reading.

mod client;
mod queue;

pub use client::{CloudClient, ENDPOINT};
pub use queue::CloudQueue;
pub use simeji_protocol::CloudWord;

use std::path::Path;

const ENABLED_FILE: &str = "cloud-enabled.json";
const MAX_READING_CHARS: usize = 256;

/// Read on every use, so a running worker follows `simeji-cloud` at once.
/// Anything but an explicit `true` keeps readings on this machine.
fn enabled_in(directory: &Path) -> bool {
    std::fs::read(directory.join(ENABLED_FILE)).is_ok_and(|bytes| bytes == b"true")
}

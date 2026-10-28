//! Opt-in cloud conversion. [`CloudClient`] performs the HTTP lookups and
//! caching; [`CloudQueue`] runs them in the background for the latest reading.

mod client;
mod queue;

pub use client::{CloudClient, ENDPOINT};
pub use queue::CloudQueue;

const MAX_READING_CHARS: usize = 256;

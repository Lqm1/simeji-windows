//! Debounced cloud lookups for the latest reading only, off the engine thread.

use super::{CloudClient, CloudWord, MAX_READING_CHARS, enabled_in};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex},
    thread::{self, JoinHandle},
    time::Duration,
};

/// Typing pauses this long before the reading is looked up.
const DEBOUNCE: Duration = Duration::from_millis(150);

#[derive(Default)]
struct State {
    generation: u64,
    pending: Option<String>,
    result: Option<(String, Vec<CloudWord>)>,
    enabled: bool,
    stopped: bool,
}
type Shared = Arc<(Mutex<State>, Condvar)>;

pub struct CloudQueue {
    directory: PathBuf,
    shared: Shared,
    thread: Option<JoinHandle<()>>,
}
impl CloudQueue {
    pub fn new(directory: &Path) -> Self {
        let shared = Shared::default();
        let background = shared.clone();
        let directory = directory.to_path_buf();
        let root = directory.clone();
        let thread = thread::spawn(move || run_lookups(&root, background));
        Self {
            directory,
            shared,
            thread: Some(thread),
        }
    }
    /// Replaces the pending lookup with `reading` and reports whether cloud
    /// conversion is enabled. An empty reading cancels pending work.
    pub fn queue(&self, reading: String) -> bool {
        let enabled = enabled_in(&self.directory);
        let (lock, changed) = &*self.shared;
        let mut state = lock.lock().unwrap_or_else(|e| e.into_inner());
        if state.enabled != enabled {
            state.generation = state.generation.wrapping_add(1);
            state.result = None;
            state.pending = None;
        }
        state.enabled = enabled;
        if !enabled || reading.is_empty() || reading.chars().count() > MAX_READING_CHARS {
            state.generation = state.generation.wrapping_add(1);
            state.pending = None;
            state.result = None;
        } else if state.pending.as_ref() != Some(&reading)
            && !state
                .result
                .as_ref()
                .is_some_and(|(query, _)| *query == reading)
        {
            state.generation = state.generation.wrapping_add(1);
            state.pending = Some(reading);
            state.result = None;
        }
        changed.notify_one();
        enabled
    }
    /// Returns the finished result for `reading`, or nothing while it is pending.
    pub fn poll(&self, reading: &str) -> Vec<CloudWord> {
        if !enabled_in(&self.directory) {
            return Vec::new();
        }
        let state = self.shared.0.lock().unwrap_or_else(|e| e.into_inner());
        if !state.enabled {
            return Vec::new();
        }
        state
            .result
            .as_ref()
            .filter(|(query, _)| query == reading)
            .map(|(_, words)| words.clone())
            .unwrap_or_default()
    }
}

impl Drop for CloudQueue {
    fn drop(&mut self) {
        {
            let mut state = self.shared.0.lock().unwrap_or_else(|e| e.into_inner());
            state.stopped = true;
            self.shared.1.notify_one();
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run_lookups(directory: &Path, shared: Shared) {
    let (lock, changed) = &*shared;
    loop {
        let mut state = lock.lock().unwrap_or_else(|e| e.into_inner());
        while state.pending.is_none() && !state.stopped {
            state = changed.wait(state).unwrap_or_else(|e| e.into_inner());
        }
        if state.stopped {
            return;
        }
        let generation = state.generation;
        let (mut state, delay) = changed
            .wait_timeout(state, DEBOUNCE)
            .unwrap_or_else(|e| e.into_inner());
        if state.stopped {
            return;
        }
        if !delay.timed_out() || state.generation != generation {
            continue;
        }
        let Some(query) = state.pending.take() else {
            continue;
        };
        drop(state);
        let words = CloudClient::open(directory)
            .and_then(|mut client| client.query(&query))
            .unwrap_or_default();
        let mut state = lock.lock().unwrap_or_else(|e| e.into_inner());
        if state.enabled && state.generation == generation {
            state.result = Some((query, words));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn debounces_to_the_latest_reading_without_blocking_queue() {
        let directory =
            std::env::temp_dir().join(format!("simeji-cloud-async-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("cloud-enabled.json"), b"true").unwrap();
        let stored = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let cache = serde_json::json!([{"query":"new","stored":stored,"ttl":60,"words":[{"text":"cached new","reading":"new","property":0,"kind":0,"scene":""}]}]);
        std::fs::write(
            directory.join("cloud-cache.json"),
            serde_json::to_vec(&cache).unwrap(),
        )
        .unwrap();
        let queue = CloudQueue::new(&directory);
        let start = std::time::Instant::now();
        queue.queue("old".into());
        queue.queue("new".into());
        assert!(start.elapsed() < Duration::from_millis(100));
        assert!(queue.poll("new").is_empty());
        while queue.poll("new").is_empty() && start.elapsed() < Duration::from_secs(3) {
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(queue.poll("new")[0].text, "cached new");
        assert!(queue.poll("old").is_empty());
        drop(queue);
        std::fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn new_reading_and_disable_discard_previous_results() {
        let directory =
            std::env::temp_dir().join(format!("simeji-cloud-queue-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let queue = CloudQueue {
            directory: directory.clone(),
            shared: Shared::default(),
            thread: None,
        };
        std::fs::write(directory.join("cloud-enabled.json"), b"true").unwrap();
        queue.queue("old".into());
        {
            let mut state = queue.shared.0.lock().unwrap();
            state.result = Some((
                "old".into(),
                vec![CloudWord {
                    text: "old candidate".into(),
                    reading: "old".into(),
                    property: 0,
                    kind: 0,
                    scene: String::new(),
                }],
            ));
        }
        assert_eq!(queue.poll("old").len(), 1);
        assert!(queue.poll("new").is_empty());
        queue.queue("new".into());
        assert!(queue.poll("old").is_empty());
        std::fs::write(directory.join("cloud-enabled.json"), b"false").unwrap();
        queue.queue("new".into());
        assert!(!queue.shared.0.lock().unwrap().enabled);
        assert!(queue.shared.0.lock().unwrap().pending.is_none());
        std::fs::remove_dir_all(directory).unwrap();
    }
}

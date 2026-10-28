//! Debounced cloud lookups for the latest reading only, off the engine thread.

use super::{CloudClient, MAX_READING_CHARS};
use crate::user_data::UserData;
use simeji_protocol::CloudCandidate;
use std::{
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
    result: Option<(String, Vec<CloudCandidate>)>,
    enabled: bool,
    stopped: bool,
}
type Shared = Arc<(Mutex<State>, Condvar)>;

pub struct CloudQueue {
    user_data: UserData,
    shared: Shared,
    thread: Option<JoinHandle<()>>,
}
impl CloudQueue {
    pub fn new(user_data: &UserData) -> Self {
        let shared = Shared::default();
        let background = shared.clone();
        let lookups = user_data.clone();
        let thread = thread::spawn(move || run_lookups(&lookups, background));
        Self {
            user_data: user_data.clone(),
            shared,
            thread: Some(thread),
        }
    }
    /// Replaces the pending lookup with `reading` and reports whether cloud
    /// conversion is enabled. An empty reading cancels pending work.
    pub fn queue(&self, reading: String) -> bool {
        let enabled = self.user_data.settings().cloud_conversion;
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
    /// Returns the finished result for `reading`, which is empty if the
    /// lookup failed, or `None` while it is pending.
    pub fn poll(&self, reading: &str) -> Option<Vec<CloudCandidate>> {
        if !self.user_data.settings().cloud_conversion {
            return None;
        }
        let state = self.shared.0.lock().unwrap_or_else(|e| e.into_inner());
        if !state.enabled {
            return None;
        }
        state
            .result
            .as_ref()
            .filter(|(query, _)| query == reading)
            .map(|(_, words)| words.clone())
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

fn run_lookups(user_data: &UserData, shared: Shared) {
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
        let words = CloudClient::open(user_data)
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
    use crate::user_data::Settings;

    fn enabled_user_data(name: &str) -> UserData {
        let user_data = UserData::at(
            std::env::temp_dir().join(format!("simeji-{name}-{}", std::process::id())),
        );
        user_data
            .save_settings(&Settings {
                cloud_conversion: true,
            })
            .unwrap();
        user_data
    }

    #[test]
    fn debounces_to_the_latest_reading_without_blocking_queue() {
        let user_data = enabled_user_data("cloud-async");
        let stored = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let cache = serde_json::json!([{"reading":"new","stored":stored,"ttl":60,"candidates":[{"text":"cached new","reading":"new","property":0,"kind":0,"scene":""}]}]);
        std::fs::create_dir_all(user_data.cache_directory()).unwrap();
        std::fs::write(
            user_data.cache_directory().join("cloud.json"),
            serde_json::to_vec(&cache).unwrap(),
        )
        .unwrap();
        let queue = CloudQueue::new(&user_data);
        let start = std::time::Instant::now();
        queue.queue("old".into());
        queue.queue("new".into());
        assert!(start.elapsed() < Duration::from_millis(100));
        assert!(queue.poll("new").is_none());
        while queue.poll("new").is_none() && start.elapsed() < Duration::from_secs(3) {
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(queue.poll("new").unwrap()[0].text, "cached new");
        assert!(queue.poll("old").is_none());
        drop(queue);
        std::fs::remove_dir_all(user_data.root()).unwrap();
    }

    #[test]
    fn new_reading_and_disable_discard_previous_results() {
        let user_data = enabled_user_data("cloud-queue");
        let queue = CloudQueue {
            user_data: user_data.clone(),
            shared: Shared::default(),
            thread: None,
        };
        queue.queue("old".into());
        {
            let mut state = queue.shared.0.lock().unwrap();
            state.result = Some((
                "old".into(),
                vec![CloudCandidate {
                    text: "old candidate".into(),
                    reading: "old".into(),
                    property: 0,
                    kind: 0,
                    scene: String::new(),
                }],
            ));
        }
        assert_eq!(queue.poll("old").unwrap().len(), 1);
        assert!(queue.poll("new").is_none());
        queue.queue("new".into());
        assert!(queue.poll("old").is_none());
        user_data.save_settings(&Settings::default()).unwrap();
        queue.queue("new".into());
        assert!(!queue.shared.0.lock().unwrap().enabled);
        assert!(queue.shared.0.lock().unwrap().pending.is_none());
        std::fs::remove_dir_all(user_data.root()).unwrap();
    }
}

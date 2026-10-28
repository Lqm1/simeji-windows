//! Conversions of the reading being typed, made in the background on a
//! second engine so that typing never waits for them.
//!
//! The engine starts from a copy of the user's learning and repeats every
//! later learning step, so its conversions match the main engine's. It
//! writes only to that copy. It closes after a while without typing to give
//! back its memory.

use crate::user_data::UserData;
use simeji_engine::{Engine, EnginePaths};
use simeji_protocol::{Candidate, Conversion};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, MutexGuard},
    thread::{self, JoinHandle},
    time::Duration,
};

/// Typing pauses this long before the reading is converted.
const DEBOUNCE: Duration = Duration::from_millis(40);
/// Typing pauses this long before the first clause's alternatives are made.
const CLAUSE_DELAY: Duration = Duration::from_millis(150);
/// The engine closes after this long without a reading to convert.
const IDLE_CLOSE: Duration = Duration::from_secs(300);
/// The engine's copy of the learning, under the cache directory.
const LEARNING_COPY: &str = "SuggestionLearning";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Suggestion {
    pub conversion: Conversion,
    /// Alternatives for the first clause, once they are ready.
    pub first_clause: Option<Vec<Candidate>>,
}

#[derive(Default)]
struct State {
    generation: u64,
    pending: Option<String>,
    result: Option<(String, Suggestion)>,
    /// Learning steps the main engine took since the copy was made.
    learned: Vec<(Candidate, Option<Candidate>)>,
    reopen: bool,
    open: bool,
    stopped: bool,
}

type Shared = Arc<(Mutex<State>, Condvar)>;

fn lock(shared: &Shared) -> MutexGuard<'_, State> {
    shared.0.lock().unwrap_or_else(|e| e.into_inner())
}

pub struct SuggestionQueue {
    shared: Shared,
    thread: Option<JoinHandle<()>>,
}

impl SuggestionQueue {
    pub fn new(dictionary: &Path, user_data: &UserData) -> Self {
        let shared = Shared::default();
        let background = shared.clone();
        let dictionary = dictionary.to_path_buf();
        let user_data = user_data.clone();
        let thread = thread::spawn(move || run(&dictionary, &user_data, &background));
        Self {
            shared,
            thread: Some(thread),
        }
    }

    /// Replaces the pending conversion with `reading`; an empty reading
    /// cancels it.
    pub fn queue(&self, reading: String) {
        let mut state = lock(&self.shared);
        let known = state.pending.as_ref() == Some(&reading)
            || state
                .result
                .as_ref()
                .is_some_and(|(query, _)| *query == reading);
        if known && !reading.is_empty() {
            return;
        }
        state.generation = state.generation.wrapping_add(1);
        state.result = None;
        state.pending = (!reading.is_empty()).then_some(reading);
        self.shared.1.notify_one();
    }

    /// The finished conversion of `reading`, or `None` while it is pending.
    pub fn poll(&self, reading: &str) -> Option<Suggestion> {
        lock(&self.shared)
            .result
            .as_ref()
            .filter(|(query, _)| query == reading)
            .map(|(_, suggestion)| suggestion.clone())
    }

    /// Repeats a learning step of the main engine. A closed engine reads it
    /// from the learning files when it opens.
    pub fn learned(&self, candidate: Candidate, previous: Option<Candidate>) {
        let mut state = lock(&self.shared);
        if state.open {
            state.learned.push((candidate, previous));
            self.shared.1.notify_one();
        }
    }

    /// The engine reads the user dictionary only when it opens.
    pub fn user_dictionary_changed(&self) {
        let mut state = lock(&self.shared);
        if state.open {
            state.reopen = true;
            self.shared.1.notify_one();
        }
    }
}

impl Drop for SuggestionQueue {
    fn drop(&mut self) {
        lock(&self.shared).stopped = true;
        self.shared.1.notify_one();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// What the background thread does next.
struct Work {
    generation: u64,
    reading: Option<String>,
    learned: Vec<(Candidate, Option<Candidate>)>,
    reopen: bool,
}

fn run(dictionary: &Path, user_data: &UserData, shared: &Shared) {
    let mut engine: Option<Engine> = None;
    while let Some(work) = next_work(shared, &mut engine) {
        if work.reopen {
            engine = None;
        }
        if engine.is_none() && work.reading.is_some() {
            lock(shared).open = true;
            engine = open(dictionary, user_data);
            lock(shared).open = engine.is_some();
        }
        if let Some(open) = engine.as_mut() {
            for (candidate, previous) in &work.learned {
                if open.learn(candidate, previous.as_ref()).is_err() {
                    break;
                }
            }
        }
        if let Some(reading) = work.reading {
            convert(&mut engine, shared, work.generation, reading);
        }
        if engine.as_ref().is_some_and(Engine::is_faulted) {
            engine = None;
            lock(shared).open = false;
        }
    }
}

/// Waits for work, closing an idle engine; `None` when the queue stops.
fn next_work(shared: &Shared, engine: &mut Option<Engine>) -> Option<Work> {
    let (_, changed) = &**shared;
    let mut state = lock(shared);
    loop {
        if state.stopped {
            return None;
        }
        let has_work = state.pending.is_some() || !state.learned.is_empty() || state.reopen;
        if !has_work {
            if engine.is_some() {
                let (next, idle) = changed
                    .wait_timeout(state, IDLE_CLOSE)
                    .unwrap_or_else(|e| e.into_inner());
                state = next;
                if idle.timed_out() && state.pending.is_none() && state.learned.is_empty() {
                    *engine = None;
                    state.open = false;
                }
            } else {
                state = changed.wait(state).unwrap_or_else(|e| e.into_inner());
            }
            continue;
        }
        if state.pending.is_some() {
            let generation = state.generation;
            let (next, delay) = changed
                .wait_timeout(state, DEBOUNCE)
                .unwrap_or_else(|e| e.into_inner());
            state = next;
            if !delay.timed_out() && state.generation != generation {
                continue;
            }
        }
        if state.stopped {
            return None;
        }
        return Some(Work {
            generation: state.generation,
            reading: state.pending.take(),
            learned: std::mem::take(&mut state.learned),
            reopen: std::mem::take(&mut state.reopen),
        });
    }
}

/// Publishes the conversion, then the first clause's alternatives unless
/// a newer reading comes first.
fn convert(engine: &mut Option<Engine>, shared: &Shared, generation: u64, reading: String) {
    let conversion = engine
        .as_mut()
        .and_then(|engine| engine.convert_sentence(&reading).ok())
        .unwrap_or_default();
    let first = conversion
        .clauses
        .first()
        .map(|clause| clause.reading.clone());
    let mut state = lock(shared);
    if state.generation != generation {
        return;
    }
    state.result = Some((
        reading,
        Suggestion {
            conversion,
            first_clause: None,
        },
    ));
    // The alternatives make the first Space immediate. They wait for a
    // pause in typing so that they do not hold up the next reading.
    let (state, _) = shared
        .1
        .wait_timeout_while(state, CLAUSE_DELAY, |state| {
            state.generation == generation && !state.stopped
        })
        .unwrap_or_else(|e| e.into_inner());
    if state.generation != generation || state.stopped {
        return;
    }
    drop(state);
    let alternatives = match (engine.as_mut(), first) {
        (Some(engine), Some(first)) => engine.clause_candidates(&first, "").unwrap_or_default(),
        _ => Vec::new(),
    };
    let mut state = lock(shared);
    if state.generation == generation
        && let Some((_, suggestion)) = state.result.as_mut()
    {
        suggestion.first_clause = Some(alternatives);
    }
}

/// Opens an engine on a fresh copy of the learning files.
fn open(dictionary: &Path, user_data: &UserData) -> Option<Engine> {
    let copy = user_data.cache_directory().join(LEARNING_COPY);
    copy_learning(&user_data.learning_directory(), &copy).ok()?;
    Engine::open(EnginePaths {
        dictionary,
        learning: &copy,
        user_dictionary: &user_data.user_dictionary_file(),
    })
    .ok()
}

fn copy_learning(learning: &Path, copy: &PathBuf) -> std::io::Result<()> {
    if copy.exists() {
        fs::remove_dir_all(copy)?;
    }
    fs::create_dir_all(copy)?;
    let Ok(entries) = fs::read_dir(learning) else {
        return Ok(());
    };
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            fs::copy(entry.path(), copy.join(entry.file_name()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn wait_for(queue: &SuggestionQueue, reading: &str, ready: impl Fn(&Suggestion) -> bool) {
        let start = Instant::now();
        while !queue
            .poll(reading)
            .is_some_and(|suggestion| ready(&suggestion))
        {
            assert!(start.elapsed() < Duration::from_secs(20), "no suggestion");
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn converts_the_latest_reading_and_follows_learning() {
        let user_data = UserData::at(
            std::env::temp_dir().join(format!("simeji-suggestions-{}", std::process::id())),
        );
        let dictionary = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/dictionary");
        let queue = SuggestionQueue::new(&dictionary, &user_data);
        let start = Instant::now();
        queue.queue("ね".into());
        queue.queue("ねこ".into());
        assert!(start.elapsed() < Duration::from_millis(50));
        assert!(queue.poll("ねこ").is_none());
        wait_for(&queue, "ねこ", |suggestion| {
            suggestion.first_clause.is_some()
        });
        assert!(queue.poll("ね").is_none());
        let suggestion = queue.poll("ねこ").unwrap();
        assert_eq!(suggestion.conversion.candidates[0].text, "猫");
        let learned = suggestion
            .first_clause
            .unwrap()
            .into_iter()
            .find(|candidate| candidate.text == "根子")
            .unwrap();

        // Learning moves the chosen word to the front, as on the main engine.
        queue.learned(learned, None);
        queue.queue("ねこは".into());
        wait_for(&queue, "ねこは", |_| true);
        queue.queue("ねこ".into());
        wait_for(&queue, "ねこ", |suggestion| {
            suggestion.first_clause.is_some()
        });
        assert_eq!(
            queue.poll("ねこ").unwrap().first_clause.unwrap()[0].text,
            "根子"
        );
        assert!(
            !user_data.learning_directory().exists(),
            "only the copy is written"
        );

        queue.queue(String::new());
        assert!(queue.poll("ねこ").is_none());
        drop(queue);
        fs::remove_dir_all(user_data.root()).unwrap();
    }
}

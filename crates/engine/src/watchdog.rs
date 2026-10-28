//! Stops a guest call that runs past its deadline.
//!
//! Unicorn's own timeout starts a thread per call that sleeps in 2 µs steps.
//! On Windows each sleep lasts a whole 15.6 ms timer tick, and every call
//! waits for that thread to end, which made each native call take ~15 ms.
//! This thread instead sleeps until the deadline and is only woken to arm
//! or disarm it.
#![allow(unsafe_code)]

use std::{
    sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError},
    thread::JoinHandle,
    time::{Duration, Instant},
};
use unicorn_engine::{uc_emu_stop, uc_engine};

#[derive(Default)]
struct Watch {
    deadline: Option<Instant>,
    expired: bool,
    shutdown: bool,
}

type Shared = Arc<(Mutex<Watch>, Condvar)>;

fn lock(shared: &Shared) -> MutexGuard<'_, Watch> {
    shared.0.lock().unwrap_or_else(PoisonError::into_inner)
}

struct EngineHandle(*mut uc_engine);

// SAFETY: the handle is only used for `uc_emu_stop`, which Unicorn's own
// timeout thread calls concurrently with emulation, and the thread ends
// before the engine is closed.
unsafe impl Send for EngineHandle {}

pub(crate) struct Watchdog {
    shared: Shared,
    thread: Option<JoinHandle<()>>,
}

impl Watchdog {
    /// The caller must drop the watchdog before closing the engine.
    pub fn new(engine: *mut uc_engine) -> Self {
        let shared = Shared::default();
        let watched = shared.clone();
        let engine = EngineHandle(engine);
        let thread = std::thread::spawn(move || watch(&watched, &engine));
        Self {
            shared,
            thread: Some(thread),
        }
    }

    pub fn arm(&self, timeout: Duration) {
        let mut watch = lock(&self.shared);
        watch.deadline = Some(Instant::now() + timeout);
        watch.expired = false;
        self.shared.1.notify_one();
    }

    /// Returns whether the deadline passed before the call ended.
    pub fn disarm(&self) -> bool {
        let mut watch = lock(&self.shared);
        watch.deadline = None;
        std::mem::take(&mut watch.expired)
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        lock(&self.shared).shutdown = true;
        self.shared.1.notify_one();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn watch(shared: &Shared, engine: &EngineHandle) {
    let mut watch = lock(shared);
    loop {
        if watch.shutdown {
            return;
        }
        watch = match watch.deadline {
            None => shared.1.wait(watch).unwrap_or_else(PoisonError::into_inner),
            Some(deadline) => {
                let now = Instant::now();
                if now >= deadline {
                    watch.deadline = None;
                    watch.expired = true;
                    // SAFETY: the engine outlives this thread, see `Watchdog::new`.
                    unsafe {
                        uc_emu_stop(engine.0);
                    }
                    watch
                } else {
                    shared
                        .1
                        .wait_timeout(watch, deadline - now)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0
                }
            }
        };
    }
}

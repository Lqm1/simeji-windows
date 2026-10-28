//! Collects the background lookups for the reading being typed.
//!
//! While a lookup is on its way, a thread timer on the application's UI
//! thread asks the worker for it. The worker answers from finished results
//! only and the pipe wait is short, so polling never stalls the application
//! behind a conversion. The timer stops when nothing is pending.

use crate::{text_service::Shared, worker::Worker};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::{Rc, Weak},
    time::{Duration, Instant},
};
use windows::Win32::{
    Foundation::HWND,
    UI::WindowsAndMessaging::{KillTimer, SetTimer},
};

const POLL_INTERVAL_MS: u32 = 40;
/// A reading whose lookups take longer than this is given up.
const GIVE_UP: Duration = Duration::from_secs(10);

thread_local! {
    static TIMERS: RefCell<BTreeMap<usize, Weak<TimerState>>> = const { RefCell::new(BTreeMap::new()) };
}

struct TimerState {
    id: Cell<usize>,
    shared: Weak<Shared>,
    refresh: Box<dyn Fn()>,
    /// The reading being polled for and when polling for it began.
    polling: RefCell<Option<(String, Instant)>>,
}

impl TimerState {
    fn stop(&self) {
        let id = self.id.replace(0);
        if id != 0 {
            unsafe {
                let _ = KillTimer(None, id);
            }
            TIMERS.with(|timers| timers.borrow_mut().remove(&id));
        }
        self.polling.take();
    }
}

pub(crate) struct LookupTimer(Rc<TimerState>);

impl LookupTimer {
    pub fn new(shared: &Rc<Shared>, refresh: Box<dyn Fn()>) -> Self {
        Self(Rc::new(TimerState {
            id: Cell::new(0),
            shared: Rc::downgrade(shared),
            refresh,
            polling: RefCell::new(None),
        }))
    }

    /// Polls until the pending lookups arrive.
    pub fn start(&self) {
        if self.0.id.get() != 0 {
            return;
        }
        let id = unsafe { SetTimer(None, 0, POLL_INTERVAL_MS, Some(poll)) };
        if id != 0 {
            self.0.id.set(id);
            TIMERS.with(|timers| timers.borrow_mut().insert(id, Rc::downgrade(&self.0)));
        }
    }
}

impl Drop for LookupTimer {
    fn drop(&mut self) {
        self.0.stop();
    }
}

unsafe extern "system" fn poll(_window: HWND, _message: u32, id: usize, _time: u32) {
    let Some(timer) = TIMERS.with(|timers| timers.borrow().get(&id).and_then(Weak::upgrade)) else {
        return;
    };
    let Some(shared) = timer.shared.upgrade() else {
        timer.stop();
        return;
    };
    // The timer may fire while a key is being handled; skip the tick then.
    let reading = match shared.composer.try_borrow() {
        Ok(composer) => composer.pending_lookup(),
        Err(_) => return,
    };
    let Some(reading) = reading else {
        timer.stop();
        return;
    };
    let started = {
        let mut polling = timer.polling.borrow_mut();
        match polling.as_ref() {
            Some((polled, started)) if *polled == reading => *started,
            _ => polling.insert((reading.clone(), Instant::now())).1,
        }
    };
    if started.elapsed() > GIVE_UP {
        timer.stop();
        return;
    }
    let Some(lookup) = Worker.poll_lookup(reading.clone()) else {
        return;
    };
    if let Ok(mut composer) = shared.composer.try_borrow_mut() {
        composer.receive_lookup(&reading, lookup);
        drop(composer);
        (timer.refresh)();
    }
}

//! Collects finished cloud lookups while the user types.
//!
//! A thread timer on the application's UI thread asks the worker for the
//! result. Polling never performs HTTP and waits only briefly for the pipe,
//! so it cannot stall the application behind an engine call.

use crate::{text_service::Shared, worker::Worker};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    rc::{Rc, Weak},
};
use windows::Win32::{
    Foundation::HWND,
    UI::WindowsAndMessaging::{KillTimer, SetTimer},
};

const POLL_INTERVAL_MS: u32 = 150;

thread_local! {
    static TIMERS: RefCell<BTreeMap<usize, Weak<Shared>>> = const { RefCell::new(BTreeMap::new()) };
}

pub(crate) struct CloudTimer(usize);

impl CloudTimer {
    pub fn start(shared: &Rc<Shared>) -> Option<Self> {
        let id = unsafe { SetTimer(None, 0, POLL_INTERVAL_MS, Some(poll_cloud)) };
        if id == 0 {
            return None;
        }
        TIMERS.with(|timers| timers.borrow_mut().insert(id, Rc::downgrade(shared)));
        Some(Self(id))
    }
}

impl Drop for CloudTimer {
    fn drop(&mut self) {
        unsafe {
            let _ = KillTimer(None, self.0);
        }
        TIMERS.with(|timers| timers.borrow_mut().remove(&self.0));
    }
}

unsafe extern "system" fn poll_cloud(_window: HWND, _message: u32, id: usize, _time: u32) {
    let Some(shared) = TIMERS.with(|timers| timers.borrow().get(&id).and_then(Weak::upgrade))
    else {
        return;
    };
    // The timer may fire while a key is being handled; skip the tick then.
    let public = || {
        shared
            .document
            .try_borrow()
            .is_ok_and(|document| document.public_field)
    };
    if !public() {
        return;
    }
    let reading = match shared.composer.try_borrow() {
        Ok(composer) => match composer.cloud_lookup_reading() {
            Some(reading) => reading,
            None => return,
        },
        Err(_) => return,
    };
    let Some(candidates) = Worker.poll_cloud(reading.clone()) else {
        return;
    };
    if public()
        && let Ok(mut composer) = shared.composer.try_borrow_mut()
    {
        composer.receive_cloud_candidates(&reading, candidates);
    }
}

//! Process-wide queue for glasses hardware events (keys, wear sensor, brightness…).
//!
//! The XREAL plugin delivers `GlassesEventData` on an SDK-owned thread via the callback
//! registered with `SetGlassesEventCallback` (see `session::try_start`). Godot objects
//! must not be touched there, so the callback only pushes into this queue;
//! `XrealHeadTracker::process()` drains it on the main thread and emits signals.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Mutex;

use crate::ffi::GlassesEventData;

/// Bounded so a stalled main loop (or events before any tracker exists) can't grow the
/// queue without limit; oldest events are dropped first.
const QUEUE_CAP: usize = 256;

/// `XREALActionType.ACTION_TYPE_TEMPERATURE_STATE` — `para` carries the temperature level.
const ACTION_TYPE_TEMPERATURE_STATE: i32 = 2028;

static QUEUE: Mutex<VecDeque<GlassesEventData>> = Mutex::new(VecDeque::new());

/// Latest glasses temperature level seen on the event funnel: `0` NORMAL / `1` WARM /
/// `2` HOT (mirrors the SDK's `XREALTemperatureLevel`), or `-1` until the glasses first
/// report one. Cached here on the SDK thread so a getter can poll it without draining the
/// queue — and so it survives queue overflow (the notification is the actionable bit).
static TEMPERATURE_LEVEL: AtomicI32 = AtomicI32::new(-1);

/// The `extern "C"` callback handed to `SetGlassesEventCallback`. Runs on an SDK thread:
/// no Godot calls, no logging — just cache the poll-able state and queue the 16-byte payload.
pub extern "C" fn on_glasses_event(data: GlassesEventData) {
    if data.action_type == ACTION_TYPE_TEMPERATURE_STATE {
        TEMPERATURE_LEVEL.store(data.para as i32, Ordering::Relaxed);
    }
    let mut queue = match QUEUE.lock() {
        Ok(queue) => queue,
        Err(poisoned) => poisoned.into_inner(),
    };
    if queue.len() >= QUEUE_CAP {
        queue.pop_front();
    }
    queue.push_back(data);
}

/// Latest cached glasses temperature level (see [`TEMPERATURE_LEVEL`]): `0`/`1`/`2`, or
/// `-1` if none has arrived yet. A plain poll — no signal, no queue drain.
pub fn temperature_level() -> i32 {
    TEMPERATURE_LEVEL.load(Ordering::Relaxed)
}

/// Take all pending events (main thread).
pub fn drain() -> Vec<GlassesEventData> {
    let mut queue = match QUEUE.lock() {
        Ok(queue) => queue,
        Err(poisoned) => poisoned.into_inner(),
    };
    queue.drain(..).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialize the tests: they share the process-global QUEUE.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn event(action_type: i32, para: u32) -> GlassesEventData {
        GlassesEventData {
            action_type,
            para,
            para2: 0,
            para3: 0.0,
        }
    }

    #[test]
    fn drain_returns_events_in_order_and_empties_the_queue() {
        let _guard = TEST_LOCK.lock().unwrap();
        drain();
        on_glasses_event(event(1, 4));
        on_glasses_event(event(2024, 1));
        let events = drain();
        assert_eq!(events.len(), 2);
        assert_eq!((events[0].action_type, events[0].para), (1, 4));
        assert_eq!((events[1].action_type, events[1].para), (2024, 1));
        assert!(drain().is_empty());
    }

    #[test]
    fn temperature_state_event_updates_the_pollable_level() {
        let _guard = TEST_LOCK.lock().unwrap();
        drain();
        // A TEMPERATURE_STATE event caches its `para` as the level...
        on_glasses_event(event(ACTION_TYPE_TEMPERATURE_STATE, 2));
        assert_eq!(temperature_level(), 2);
        on_glasses_event(event(ACTION_TYPE_TEMPERATURE_STATE, 0));
        assert_eq!(temperature_level(), 0);
        // ...while unrelated events leave the cached level untouched.
        on_glasses_event(event(2024, 1));
        assert_eq!(temperature_level(), 0);
        drain();
    }

    #[test]
    fn queue_drops_oldest_beyond_capacity() {
        let _guard = TEST_LOCK.lock().unwrap();
        drain();
        for i in 0..(QUEUE_CAP + 10) {
            on_glasses_event(event(1, i as u32));
        }
        let events = drain();
        assert_eq!(events.len(), QUEUE_CAP);
        // The 10 oldest were dropped; the first remaining is #10.
        assert_eq!(events[0].para, 10);
        assert_eq!(events.last().unwrap().para, (QUEUE_CAP + 9) as u32);
    }
}

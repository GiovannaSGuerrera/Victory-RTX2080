//! Opt-in wall-clock timings of buffer-pass functions, without resource data.
use std::time::{Duration, Instant};
use std::cell::{Cell, RefCell};

thread_local! {
    static MIRROR_ON: Cell<bool> = const { Cell::new(false) };
    static MIRROR_TOTALS: RefCell<MirrorTotals> = RefCell::new(MirrorTotals::default());
}

/// Parts measured inside `mirror()`:
/// 0 = lookup (view_at), 1 = made mutex lock, 2 = note_buffer, 3 = make_view, 4 = keep, 5 = null_for,
/// 6 = VIEWS RwLock read, 7 = VIEWS RwLock write
#[derive(Default)]
struct MirrorTotals {
    elapsed: [Duration; 8],
    calls: [u64; 8],
    paths: [u64; 4],
}

/// Timings are collected only on the thread of a sampled buffer pass.
#[inline]
pub(crate) fn mirror_measure<T>(part: usize, f: impl FnOnce() -> T) -> T {
    if !MIRROR_ON.with(Cell::get) { return f(); }
    let started = Instant::now();
    let value = f();
    let elapsed = started.elapsed();
    MIRROR_TOTALS.with(|totals| {
        let mut totals = totals.borrow_mut();
        totals.elapsed[part] += elapsed;
        totals.calls[part] += 1;
    });
    value
}

/// Fast hit, unchanged under lock, rebuilt view, invalid record.
pub(crate) fn mirror_path(path: usize) {
    if MIRROR_ON.with(Cell::get) {
        MIRROR_TOTALS.with(|totals| totals.borrow_mut().paths[path] += 1);
    }
}

pub(crate) struct Profile {
    frame: u64,
    enabled: bool,
    elapsed: [Duration; 7],
    calls: [u64; 7],
    previous_mirror: bool,
}

impl Profile {
    pub(crate) fn new(frame: u64) -> Self {
        // VICTORY_PROFILE_BUFFERS=1 samples one pass in 16. Timings include
        // waits and instrumentation overhead; they are not CPU utilization.
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        let on = *ON.get_or_init(|| std::env::var_os("VICTORY_PROFILE_BUFFERS").is_some_and(|v| v == "1"));
        let enabled = on && frame.is_multiple_of(16);
        Self::sampled(frame, enabled)
    }

    fn sampled(frame: u64, enabled: bool) -> Self {
        let previous_mirror = MIRROR_ON.with(|active| active.replace(enabled));
        if enabled { MIRROR_TOTALS.with(|totals| *totals.borrow_mut() = MirrorTotals::default()); }
        Self { frame, enabled, elapsed: [Duration::ZERO; 7], calls: [0; 7], previous_mirror }
    }

    #[inline]
    pub(crate) fn measure<T>(&mut self, part: usize, f: impl FnOnce() -> T) -> T {
        if !self.enabled {
            return f();
        }
        let at = Instant::now();
        let value = f();
        self.elapsed[part] += at.elapsed();
        self.calls[part] += 1;
        value
    }

    pub(crate) fn report(&self, slots: usize) {
        if self.enabled {
            let ms = self.elapsed.map(|d| d.as_secs_f64() * 1000.0);
            tracing::info!(frame = self.frame, slots,
                plan_ms = ms[0], forget_ms = ms[1], record_ms = ms[2],
                mirror_ms = ms[3], track_ms = ms[4], spans_ms = ms[5],
                invalid_flush_ms = ms[6],
                calls = ?self.calls,
                "sampled buffer function wall times (VICTORY_PROFILE_BUFFERS=1; one pass in 16)");
            MIRROR_TOTALS.with(|totals| {
                let totals = totals.borrow();
                let ms = totals.elapsed.map(|d| d.as_secs_f64() * 1000.0);
                tracing::info!(frame = self.frame, slots,
                    lookup_ms = ms[0], made_lock_wait_ms = ms[1], note_ms = ms[2],
                    make_ms = ms[3], keep_ms = ms[4], null_ms = ms[5],
                    views_read_ms = ms[6], views_write_ms = ms[7],
                    calls = ?totals.calls, paths = ?totals.paths,
                    "sampled mirror internals (paths: fast, locked unchanged, rebuilt, invalid)");
            });
        }
    }
}

impl Drop for Profile {
    fn drop(&mut self) { MIRROR_ON.with(|active| active.set(self.previous_mirror)); }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mirror_measurements_are_thread_local_and_end_with_the_pass() {
        let profile = Profile::sampled(16, true);
        assert_eq!(mirror_measure(0, || 42), 42);
        mirror_path(0);
        std::thread::spawn(|| {
            mirror_measure(0, || ());
            mirror_path(0);
            MIRROR_TOTALS.with(|t| assert_eq!(t.borrow().calls, [0; 6]));
        }).join().unwrap();
        MIRROR_TOTALS.with(|t| {
            assert_eq!(t.borrow().calls, [1, 0, 0, 0, 0, 0]);
            assert_eq!(t.borrow().paths, [1, 0, 0, 0]);
        });
        drop(profile);
        mirror_measure(0, || ());
        MIRROR_TOTALS.with(|t| assert_eq!(t.borrow().calls[0], 1));
        let _next = Profile::sampled(32, true);
        MIRROR_TOTALS.with(|t| assert_eq!(t.borrow().calls, [0; 6]));
    }
}

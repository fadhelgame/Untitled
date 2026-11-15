//! macOS frame pacing.
//!
//! OpenGL vsync is unreliable on ProMotion displays: `swap_buffers` lets two frames through and
//! then blocks for a 60 Hz period, so the app paints 120 times a second in uneven pairs and looks
//! like 60 fps. `main.rs` therefore turns vsync off on macOS, and this module holds each frame
//! until the next display refresh instead, using a `CVDisplayLink` (the system's own refresh
//! clock, which follows the real rate of the display: 120 Hz on ProMotion).
//!
//! On other systems vsync stays on and `wait` does nothing.

/// Blocks until the display has refreshed since the previous frame. Returns the time spent
/// waiting in milliseconds, so it can be left out of the frame time shown in the status bar.
#[cfg(not(target_os = "macos"))]
pub fn wait() -> f32 {
    0.0
}

#[cfg(target_os = "macos")]
pub use mac::wait;

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::Relaxed};
    use std::sync::{Condvar, Mutex};
    use std::time::{Duration, Instant};

    type Link = *mut c_void;
    type Callback = extern "C" fn(Link, *const c_void, *const c_void, u64, *mut u64, *mut c_void) -> i32;

    #[link(name = "CoreVideo", kind = "framework")]
    extern "C" {
        fn CVDisplayLinkCreateWithActiveCGDisplays(out: *mut Link) -> i32;
        fn CVDisplayLinkSetOutputCallback(link: Link, callback: Callback, user: *mut c_void) -> i32;
        fn CVDisplayLinkStart(link: Link) -> i32;
        fn CVDisplayLinkStop(link: Link) -> i32;
    }

    /// Display refreshes seen so far.
    static TICKS: AtomicU64 = AtomicU64::new(0);
    /// The value of `TICKS` when the last frame started.
    static FRAME_TICK: AtomicU64 = AtomicU64::new(0);
    static RUNNING: AtomicBool = AtomicBool::new(false);
    /// The display link, created on first use. 0 = not created yet, 1 = creation failed.
    static LINK: AtomicUsize = AtomicUsize::new(0);
    static LOCK: Mutex<()> = Mutex::new(());
    static REFRESHED: Condvar = Condvar::new();

    /// Refreshes with no frame after which the clock stops, so an idle app wakes nothing up.
    const IDLE_TICKS: u64 = 120;
    /// Never hold a frame longer than this, whatever happens to the display link (display asleep,
    /// screen unplugged): the app must stay responsive.
    const MAX_WAIT: Duration = Duration::from_millis(50);

    extern "C" fn on_refresh(link: Link, _: *const c_void, _: *const c_void, _: u64, _: *mut u64, _: *mut c_void) -> i32 {
        let _guard = LOCK.lock();
        let tick = TICKS.fetch_add(1, Relaxed) + 1;
        if tick.saturating_sub(FRAME_TICK.load(Relaxed)) > IDLE_TICKS {
            RUNNING.store(false, Relaxed);
            unsafe { CVDisplayLinkStop(link) };
        }
        REFRESHED.notify_all();
        0
    }

    fn link() -> Option<Link> {
        match LINK.load(Relaxed) {
            0 => {
                let mut link: Link = std::ptr::null_mut();
                let ok = unsafe {
                    CVDisplayLinkCreateWithActiveCGDisplays(&mut link) == 0
                        && !link.is_null()
                        && CVDisplayLinkSetOutputCallback(link, on_refresh, std::ptr::null_mut()) == 0
                };
                LINK.store(if ok { link as usize } else { 1 }, Relaxed);
                ok.then_some(link)
            }
            1 => None,
            link => Some(link as Link),
        }
    }

    pub fn wait() -> f32 {
        // Only ever called from the UI thread, so creating the link needs no extra locking.
        let Some(link) = link() else { return 0.0 };
        let start = Instant::now();
        let mut guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        if !RUNNING.load(Relaxed) {
            // First frame after a pause: paint it right away and start the clock for the next ones.
            FRAME_TICK.store(TICKS.load(Relaxed), Relaxed);
            RUNNING.store(true, Relaxed);
            drop(guard);
            unsafe { CVDisplayLinkStart(link) };
            return 0.0;
        }
        let last = FRAME_TICK.load(Relaxed);
        while TICKS.load(Relaxed) == last && RUNNING.load(Relaxed) {
            let left = MAX_WAIT.saturating_sub(start.elapsed());
            if left.is_zero() {
                break;
            }
            guard = REFRESHED.wait_timeout(guard, left).unwrap_or_else(|e| e.into_inner()).0;
        }
        FRAME_TICK.store(TICKS.load(Relaxed), Relaxed);
        drop(guard);
        start.elapsed().as_secs_f32() * 1000.0
    }
}

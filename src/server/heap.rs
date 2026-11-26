//! Give the C libraries' freed heap back to the OS.
//!
//! Lain's Rust code allocates through mimalloc, which returns freed pages
//! to the OS on its own. The C libraries it links (libgit2 reading blobs,
//! tree-sitter parsing sources, zlib, the C++ runtime) allocate through
//! glibc `malloc` instead, and glibc keeps what they free inside its
//! per-thread arenas. Measured on the Parallax demo federation
//! (`docs/plans/layered-snapshots.md`, phase 0): about 0.9 GB of a
//! 1.47 GB engine right after startup, and another ~0.7 GB after the
//! first snapshot build, was such freed-but-kept glibc memory. One
//! `malloc_trim(0)` returned all of it.
//!
//! [`trim`] runs at the points that churn the C heap the most: the end of
//! startup indexing, each snapshot build, each snapshot index job. A
//! rate-limited [`trim_if_idle_due`] catches the rest (live re-index,
//! reloads, tool calls) once things go quiet. `LAIN_HEAP_TRIM=0`
//! disables all of it. Off glibc it does nothing.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::OnceLock;

/// Minimum quiet time between idle trims.
pub const IDLE_TRIM_INTERVAL_SECS: i64 = 60;

static TRIMS: AtomicU64 = AtomicU64::new(0);
static ACTIVITY_SINCE_TRIM: AtomicBool = AtomicBool::new(false);
static LAST_TRIM_UNIX: AtomicI64 = AtomicI64::new(0);

#[cfg(all(target_os = "linux", target_env = "gnu"))]
extern "C" {
    fn malloc_trim(pad: usize) -> std::os::raw::c_int;
}

/// `LAIN_HEAP_TRIM=0` (or `false`/`off`) turns trimming off. Read once.
pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        !matches!(
            std::env::var("LAIN_HEAP_TRIM")
                .as_deref()
                .map(str::to_ascii_lowercase)
                .as_deref(),
            Ok("0") | Ok("false") | Ok("off")
        )
    })
}

/// Whether this build can trim at all (glibc on Linux).
pub const fn supported() -> bool {
    cfg!(all(target_os = "linux", target_env = "gnu"))
}

/// Number of trims this process has run. Lets tests check that a code
/// path trims without depending on the host's memory behaviour.
pub fn trim_count() -> u64 {
    TRIMS.load(Ordering::Relaxed)
}

/// Something that may have churned the C heap happened (a tool call, a
/// re-index). Arms the next [`trim_if_idle_due`].
pub fn note_activity() {
    ACTIVITY_SINCE_TRIM.store(true, Ordering::Relaxed);
}

/// Unconditional `malloc_trim(0)`, no logging, no counting. The memory
/// probe uses it; everything else calls [`trim`].
pub fn trim_raw() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    // SAFETY: `malloc_trim` only walks glibc's own arenas under their
    // locks; it is thread-safe and any `pad` is valid.
    unsafe {
        malloc_trim(0);
    }
}

/// Trim the C heap now and log what it gave back. `reason` names the
/// call site in the log line.
pub fn trim(reason: &str) {
    if !enabled() || !supported() {
        return;
    }
    let before = crate::server::mem_probe::rss_and_peak_kb().map(|(rss, _)| rss);
    let started = std::time::Instant::now();
    trim_raw();
    let elapsed_ms = started.elapsed().as_millis() as u64;
    let after = crate::server::mem_probe::rss_and_peak_kb().map(|(rss, _)| rss);
    TRIMS.fetch_add(1, Ordering::Relaxed);
    ACTIVITY_SINCE_TRIM.store(false, Ordering::Relaxed);
    // Only forward: the idle path may already have claimed a later stamp.
    LAST_TRIM_UNIX.fetch_max(crate::server::time::now_unix(), Ordering::Relaxed);
    let freed_mb = match (before, after) {
        (Some(b), Some(a)) => b.saturating_sub(a) / 1024,
        _ => 0,
    };
    tracing::info!(
        target: "lain::heap",
        reason,
        freed_mb,
        rss_mb = after.unwrap_or(0) / 1024,
        elapsed_ms,
        "heap trim"
    );
}

/// The idle fallback: trim if something ran since the last trim and the
/// last one is at least [`IDLE_TRIM_INTERVAL_SECS`] old. Safe to call
/// from several threads; at most one of them trims per interval.
/// Returns whether it trimmed.
pub fn trim_if_idle_due(now_unix: i64) -> bool {
    if !enabled() || !supported() || !ACTIVITY_SINCE_TRIM.load(Ordering::Relaxed) {
        return false;
    }
    let last = LAST_TRIM_UNIX.load(Ordering::Relaxed);
    if now_unix.saturating_sub(last) < IDLE_TRIM_INTERVAL_SECS {
        return false;
    }
    // Claim this interval; a concurrent caller that lost the race returns.
    if LAST_TRIM_UNIX
        .compare_exchange(last, now_unix, Ordering::AcqRel, Ordering::Relaxed)
        .is_err()
    {
        return false;
    }
    trim("idle");
    true
}

#[cfg(all(test, target_os = "linux", target_env = "gnu"))]
mod tests {
    use super::*;

    extern "C" {
        fn malloc(size: usize) -> *mut std::ffi::c_void;
        fn free(ptr: *mut std::ffi::c_void);
    }

    /// The failure mode this module exists for, reproduced in miniature:
    /// a C library on some thread allocates many small blocks through glibc,
    /// frees most of them but keeps a few scattered ones alive. glibc keeps
    /// the freed pages in that thread's arena; RSS stays up until a trim.
    #[test]
    fn trim_returns_a_fragmented_glibc_arena_to_the_os() {
        const BLOCKS: usize = 200_000;
        const SIZE: usize = 1024; // ~200 MiB, below the mmap threshold
        let (before_free, after_free, kept) = std::thread::spawn(|| {
            let ptrs: Vec<usize> = (0..BLOCKS)
                .map(|_| {
                    // SAFETY: plain malloc of a small block; freed below.
                    let p = unsafe { malloc(SIZE) };
                    assert!(!p.is_null());
                    // Touch it so it is resident.
                    unsafe { std::ptr::write_bytes(p as *mut u8, 1, SIZE) };
                    p as usize
                })
                .collect();
            let before = crate::server::mem_probe::rss_and_peak_kb().unwrap().0;
            let mut kept = Vec::new();
            for (i, p) in ptrs.into_iter().enumerate() {
                if i % 64 == 0 {
                    kept.push(p); // pins the arena's top: no automatic shrink
                } else {
                    // SAFETY: each pointer came from malloc and is freed once.
                    unsafe { free(p as *mut _) };
                }
            }
            let after = crate::server::mem_probe::rss_and_peak_kb().unwrap().0;
            (before, after, kept)
        })
        .join()
        .unwrap();
        // Preconditions: the blocks were resident, and glibc kept the
        // freed ones resident (the bug), so there is something to trim.
        assert!(
            before_free > 150 * 1024,
            "test setup: expected >150 MiB resident, got {before_free} KiB"
        );
        assert!(
            after_free > 120 * 1024,
            "test setup: glibc returned the freed blocks by itself ({after_free} KiB)"
        );
        let retained_kb = after_free;
        trim_raw();
        let after_trim = crate::server::mem_probe::rss_and_peak_kb().unwrap().0;
        // Other tests run concurrently and allocate; demand a large drop.
        assert!(
            after_trim + 100 * 1024 < retained_kb,
            "trim gave back too little: {retained_kb} KiB -> {after_trim} KiB"
        );
        for p in kept {
            // SAFETY: still-live blocks from malloc above, freed once.
            unsafe { free(p as *mut _) };
        }
    }

    #[test]
    fn idle_trim_needs_activity_and_is_rate_limited() {
        // Only this test calls trim_if_idle_due; other tests may call
        // trim(), which only ever moves the stamp forward to real time.
        // A clock far ahead of real time, so trims that other tests run
        // concurrently (which stamp the real time) cannot reset it.
        let now = crate::server::time::now_unix() + 100 * IDLE_TRIM_INTERVAL_SECS;
        LAST_TRIM_UNIX.store(now - IDLE_TRIM_INTERVAL_SECS - 1, Ordering::Relaxed);
        ACTIVITY_SINCE_TRIM.store(false, Ordering::Relaxed);
        assert!(!trim_if_idle_due(now), "trimmed with no activity");
        note_activity();
        let before = trim_count();
        assert!(trim_if_idle_due(now), "did not trim when due");
        assert!(trim_count() > before);
        note_activity();
        assert!(
            !trim_if_idle_due(now + 1),
            "trimmed again inside the interval"
        );
        assert!(trim_if_idle_due(now + IDLE_TRIM_INTERVAL_SECS + 1));
    }
}

//! Give the C libraries' freed heap back to the OS.
//!
//! Lain's Rust code allocates through mimalloc. The C libraries it links
//! (libgit2 reading blobs, tree-sitter parsing sources, zlib, the C++
//! runtime) allocate through glibc `malloc` instead, and glibc keeps what they free inside its
//! per-thread arenas. Measured on the Parallax demo federation
//! (`docs/plans/layered-snapshots.md`, phase 0): about 0.9 GB of a
//! 1.47 GB engine right after startup, and another ~0.7 GB after the
//! first snapshot build, was such freed-but-kept glibc memory. One
//! `malloc_trim(0)` returned all of it. mimalloc also keeps freed pages a
//! while before returning them: after a snapshot index job, a forced
//! `mi_collect` gave back another ~0.3 GB, so [`trim`] runs both
//! ([`crate::server::mem_probe::allocator_collect`]).
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
/// call site in the log line. Returns the KiB of RSS it gave back (0 when
/// trimming is off or unsupported).
pub fn trim(reason: &str) -> u64 {
    if !enabled() || !supported() {
        return 0;
    }
    let before = crate::server::mem_probe::rss_and_peak_kb().map(|(rss, _)| rss);
    let started = std::time::Instant::now();
    crate::server::mem_probe::allocator_collect();
    let elapsed_ms = started.elapsed().as_millis() as u64;
    let after = crate::server::mem_probe::rss_and_peak_kb().map(|(rss, _)| rss);
    TRIMS.fetch_add(1, Ordering::Relaxed);
    ACTIVITY_SINCE_TRIM.store(false, Ordering::Relaxed);
    // Only forward: the idle path may already have claimed a later stamp.
    LAST_TRIM_UNIX.fetch_max(crate::server::time::now_unix(), Ordering::Relaxed);
    let freed_kb = match (before, after) {
        (Some(b), Some(a)) => b.saturating_sub(a),
        _ => 0,
    };
    tracing::info!(
        target: "lain::heap",
        reason,
        freed_mb = freed_kb / 1024,
        rss_mb = after.unwrap_or(0) / 1024,
        elapsed_ms,
        "heap trim"
    );
    freed_kb
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

// Plain `#[cfg(test)]` on the module (platform gates inside) so tooling
// such as cargo-mutants recognises it as test code.
#[cfg(test)]
#[cfg(all(target_os = "linux", target_env = "gnu"))]
pub(crate) mod tests {
    use super::*;

    extern "C" {
        fn malloc(size: usize) -> *mut std::ffi::c_void;
        fn free(ptr: *mut std::ffi::c_void);
    }

    /// The failure mode this module exists for, in miniature: a C library
    /// on some thread allocates many small blocks through glibc, frees most
    /// of them and keeps a few scattered ones alive. glibc keeps the freed
    /// pages in that thread's arena. Returns the process RSS (KiB) after the
    /// frees and the still-live blocks (release them with [`free_kept`]).
    pub(crate) fn fragment_glibc_heap() -> (u64, Vec<usize>) {
        const BLOCKS: usize = 200_000;
        const SIZE: usize = 1024; // ~200 MiB, below the mmap threshold
        let (before_free, after_free, kept) = std::thread::spawn(|| {
            let ptrs: Vec<usize> = (0..BLOCKS)
                .map(|_| {
                    // SAFETY: plain malloc of a small block, freed below or
                    // by `free_kept`.
                    let p = unsafe { malloc(SIZE) };
                    assert!(!p.is_null());
                    // SAFETY: `p` points to SIZE writable bytes.
                    unsafe { std::ptr::write_bytes(p as *mut u8, 1, SIZE) };
                    p as usize
                })
                .collect();
            let before = crate::server::mem_probe::rss_and_peak_kb().unwrap().0;
            let mut kept = Vec::new();
            for (i, p) in ptrs.into_iter().enumerate() {
                if i % 64 == 0 {
                    kept.push(p); // pins the arena: no automatic shrink
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
        // Preconditions: the blocks were resident, and glibc kept the freed
        // ones resident (the bug), so there is something to give back.
        assert!(
            before_free > 150 * 1024,
            "test setup: expected >150 MiB resident, got {before_free} KiB"
        );
        assert!(
            after_free > 120 * 1024,
            "test setup: glibc returned the freed blocks by itself ({after_free} KiB)"
        );
        (after_free, kept)
    }

    pub(crate) fn free_kept(kept: Vec<usize>) {
        for p in kept {
            // SAFETY: still-live blocks from `fragment_glibc_heap`, freed once.
            unsafe { free(p as *mut _) };
        }
    }

    #[test]
    fn trim_returns_a_fragmented_glibc_arena_to_the_os() {
        if !enabled() {
            return; // LAIN_HEAP_TRIM=0 in the test environment
        }
        let (resident_kb, kept) = fragment_glibc_heap();
        let before = trim_count();
        let freed_kb = trim("test");
        let after_kb = crate::server::mem_probe::rss_and_peak_kb().unwrap().0;
        free_kept(kept);
        assert!(trim_count() > before, "the trim was not counted");
        // Other tests run concurrently and allocate; demand a large drop.
        assert!(
            freed_kb >= 100 * 1024,
            "trim reported {freed_kb} KiB freed of a fragmented ~200 MiB arena"
        );
        assert!(
            after_kb + 100 * 1024 < resident_kb,
            "trim gave back too little: {resident_kb} KiB -> {after_kb} KiB"
        );
    }

    #[test]
    fn idle_trim_needs_activity_is_rate_limited_and_fires_at_the_interval() {
        if !enabled() {
            return;
        }
        // Only this test calls trim_if_idle_due; other tests may call trim(),
        // which only ever moves the stamp forward to real time. A clock far
        // ahead of real time keeps those from resetting it.
        let now = crate::server::time::now_unix() + 100 * IDLE_TRIM_INTERVAL_SECS;
        LAST_TRIM_UNIX.store(now - IDLE_TRIM_INTERVAL_SECS, Ordering::Relaxed);
        ACTIVITY_SINCE_TRIM.store(false, Ordering::Relaxed);
        assert!(!trim_if_idle_due(now), "trimmed with no activity");
        note_activity();
        let before = trim_count();
        assert!(
            trim_if_idle_due(now),
            "did not trim exactly one interval after the last trim"
        );
        assert!(trim_count() > before);
        note_activity();
        assert!(
            !trim_if_idle_due(now + 1),
            "trimmed again inside the interval"
        );
        assert!(
            !trim_if_idle_due(now + IDLE_TRIM_INTERVAL_SECS - 1),
            "trimmed a second before the interval ended"
        );
        assert!(trim_if_idle_due(now + IDLE_TRIM_INTERVAL_SECS));
    }
}

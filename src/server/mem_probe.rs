//! Per-phase memory probe for the snapshot pipeline (phase 0 of
//! `docs/plans/layered-snapshots.md`).
//!
//! Off unless `LAIN_MEM_PROBE=1`. When on, [`phase`] logs one
//! `mem_probe` line with the process's current RSS and the peak RSS
//! *since the previous probe*, then resets the kernel's peak counter
//! (`/proc/self/clear_refs` ← `5`) so the next phase's peak is its own.
//! `scripts/measure_pr_memory.py` reads these lines from the server log
//! to split a PR analysis's peak into hydrate / project / join / trace.
//!
//! With `LAIN_MEM_PROBE_COLLECT=1` each probe first asks mimalloc to
//! return free pages (`mi_collect`), so the logged RSS is live data, not
//! allocator retention.
//!
//! Linux only; elsewhere (or when `/proc` is unreadable) the probe logs
//! nothing. It never fails the caller.

use std::sync::OnceLock;

/// Is the probe on? Read once per process.
pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var("LAIN_MEM_PROBE")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    })
}

fn status_field_kb(text: &str, name: &str) -> Option<u64> {
    text.lines()
        .find_map(|l| l.strip_prefix(name))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|kb| kb.parse().ok())
}

/// `(VmRSS, VmHWM)` in KiB from `/proc/self/status`.
pub fn rss_and_peak_kb() -> Option<(u64, u64)> {
    let text = std::fs::read_to_string("/proc/self/status").ok()?;
    Some((
        status_field_kb(&text, "VmRSS:")?,
        status_field_kb(&text, "VmHWM:")?,
    ))
}

/// `VmSwap` in KiB: what the process has in swap right now. `VmHWM`
/// counts resident pages only, so on a host that swaps the phase peak
/// undercounts; logging swap next to it makes that visible.
pub fn swap_kb() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|t| status_field_kb(&t, "VmSwap:"))
        .unwrap_or(0)
}

// `mi_collect` from the mimalloc C library that the `mimalloc` crate
// links (the binary's global allocator). Declared here rather than
// through `libmimalloc-sys`'s `extended` feature to keep the dependency
// set unchanged. `extern crate` makes sure the static library is linked
// even into binaries that do not install mimalloc as their allocator.
extern crate mimalloc as _;
extern "C" {
    fn mi_collect(force: bool);
}

/// Ask mimalloc to return every free page it can to the OS now.
/// The probe calls it before reading RSS when `LAIN_MEM_PROBE_COLLECT=1`,
/// which separates live data (stays resident) from pages the allocator
/// merely holds after a free (go away). Without the global allocator
/// being mimalloc this only collects mimalloc's own (empty) heaps.
pub fn allocator_collect() {
    // SAFETY: `mi_collect` takes a plain bool, has no preconditions and
    // is safe to call from any thread at any time.
    unsafe { mi_collect(true) }
}

fn collect_enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        std::env::var("LAIN_MEM_PROBE_COLLECT")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
    })
}

/// Reset the kernel's peak-RSS counter (`VmHWM`) to the current RSS.
/// Returns whether the reset was accepted.
pub fn reset_peak() -> bool {
    std::fs::write("/proc/self/clear_refs", "5").is_ok()
}

/// Record the end of a phase: log RSS now and the peak since the last
/// probe, then reset the peak. `label` names the phase that just ended;
/// `detail` is free text (repo id, snapshot id, counts).
pub fn phase(label: &str, detail: &str) {
    if !enabled() {
        return;
    }
    // Peak first: a collect only lowers RSS, and the peak must be the
    // phase's own, read before anything else touches memory.
    let peak_kb = rss_and_peak_kb().map(|(_, p)| p);
    let collected = collect_enabled();
    if collected {
        allocator_collect();
    }
    let (Some(peak_kb), Some((rss_kb, _))) = (peak_kb, rss_and_peak_kb()) else {
        return;
    };
    let swap_kb = swap_kb();
    let reset = reset_peak();
    tracing::info!(
        target: "lain::mem_probe",
        phase = label,
        detail,
        rss_kb,
        swap_kb,
        peak_kb,
        collected,
        peak_reset = reset,
        "mem_probe"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn reads_rss_and_peak_and_peak_is_at_least_rss() {
        let (rss, peak) = rss_and_peak_kb().expect("/proc/self/status readable on linux");
        assert!(rss > 0);
        assert!(peak >= rss, "VmHWM {peak} < VmRSS {rss}");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn reset_brings_peak_down_to_current_rss() {
        // Touch ~64 MiB, free it, then reset: the peak must drop back
        // near the current RSS. Proves the per-phase peak is per phase.
        let big = vec![1u8; 64 << 20];
        std::hint::black_box(&big);
        drop(big);
        let (_, before) = rss_and_peak_kb().unwrap();
        if !reset_peak() {
            // Some sandboxes mount /proc read-only; nothing to check.
            return;
        }
        let (rss, after) = rss_and_peak_kb().unwrap();
        assert!(
            after <= before,
            "peak grew across a reset: {before} -> {after}"
        );
        // Other test threads allocate concurrently; allow slack.
        assert!(
            after <= rss + (32 << 10),
            "peak {after} KiB far above rss {rss} KiB after reset"
        );
    }
}

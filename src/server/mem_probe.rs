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
//! With `LAIN_MEM_PROBE_COLLECT=1` each probe first asks both allocators
//! to return free pages (mimalloc's `mi_collect`, glibc's `malloc_trim`
//! for the C libraries), so the logged RSS is live data, not allocator
//! retention.
//!
//! Linux only; elsewhere (or when `/proc` is unreadable) the probe logs
//! nothing. It never fails the caller.

use std::sync::OnceLock;

/// `1` or `true` (any case) turns a flag on; anything else, or unset, is off.
fn flag_on(value: Option<&str>) -> bool {
    matches!(value, Some(v) if v == "1" || v.eq_ignore_ascii_case("true"))
}

/// Is the probe on (`LAIN_MEM_PROBE`)? Read once per process.
pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| flag_on(std::env::var("LAIN_MEM_PROBE").ok().as_deref()))
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

/// Ask mimalloc, then glibc ([`crate::server::heap::trim_raw`]), to return
/// every free page they can to the OS now.
/// The probe calls it before reading RSS when `LAIN_MEM_PROBE_COLLECT=1`,
/// which separates live data (stays resident) from pages the allocator
/// merely holds after a free (go away).
pub fn allocator_collect() {
    // SAFETY: `mi_collect` takes a plain bool, has no preconditions and
    // is safe to call from any thread at any time.
    unsafe { mi_collect(true) }
    crate::server::heap::trim_raw();
}

fn collect_enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| flag_on(std::env::var("LAIN_MEM_PROBE_COLLECT").ok().as_deref()))
}

/// Reset the kernel's peak-RSS counter (`VmHWM`) to the current RSS.
/// Returns whether the reset was accepted.
pub fn reset_peak() -> bool {
    std::fs::write("/proc/self/clear_refs", "5").is_ok()
}

/// One probe reading, in KiB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sample {
    /// Resident now (after the collect, when one ran).
    pub rss_kb: u64,
    /// In swap now.
    pub swap_kb: u64,
    /// Resident peak since the previous reset.
    pub peak_kb: u64,
}

/// Read peak, optionally collect, read RSS and swap, reset the peak.
/// The peak is read first: a collect only lowers RSS, and the peak must
/// be the phase's own. `None` when `/proc` is unreadable.
fn sample_and_reset(collect: bool) -> Option<(Sample, bool)> {
    let (_, peak_kb) = rss_and_peak_kb()?;
    if collect {
        allocator_collect();
    }
    let (rss_kb, _) = rss_and_peak_kb()?;
    let sample = Sample {
        rss_kb,
        swap_kb: swap_kb(),
        peak_kb,
    };
    Some((sample, reset_peak()))
}

/// Record the end of a phase: log RSS now and the peak since the last
/// probe, then reset the peak. `label` names the phase that just ended;
/// `detail` is free text (repo id, snapshot id, counts). Returns what it
/// logged; `None` when the probe is off.
pub fn phase(label: &str, detail: &str) -> Option<Sample> {
    if !enabled() {
        return None;
    }
    let collected = collect_enabled();
    let (sample, reset) = sample_and_reset(collected)?;
    tracing::info!(
        target: "lain::mem_probe",
        phase = label,
        detail,
        rss_kb = sample.rss_kb,
        swap_kb = sample.swap_kb,
        peak_kb = sample.peak_kb,
        collected,
        peak_reset = reset,
        "mem_probe"
    );
    Some(sample)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_turn_on_only_for_1_or_true() {
        assert!(flag_on(Some("1")));
        assert!(flag_on(Some("true")));
        assert!(flag_on(Some("TRUE")));
        assert!(!flag_on(Some("0")));
        assert!(!flag_on(Some("yes")));
        assert!(!flag_on(Some("")));
        assert!(!flag_on(None));
    }

    #[test]
    fn status_fields_parse_by_name() {
        let text = "Name:\tlain\nVmHWM:\t  2048 kB\nVmRSS:\t  1500 kB\nVmSwap:\t     7 kB\n";
        assert_eq!(status_field_kb(text, "VmRSS:"), Some(1500));
        assert_eq!(status_field_kb(text, "VmHWM:"), Some(2048));
        assert_eq!(status_field_kb(text, "VmSwap:"), Some(7));
        assert_eq!(status_field_kb(text, "VmData:"), None);
    }

    #[test]
    fn phase_is_silent_unless_the_probe_is_on() {
        if std::env::var_os("LAIN_MEM_PROBE").is_some() {
            return; // a developer ran the suite with the probe on
        }
        assert_eq!(phase("test", ""), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn reads_rss_and_peak_and_peak_is_at_least_rss() {
        let (rss, peak) = rss_and_peak_kb().expect("/proc/self/status readable on linux");
        assert!(
            rss > 1024,
            "a test process holds more than 1 MiB: {rss} KiB"
        );
        assert!(peak >= rss, "VmHWM {peak} < VmRSS {rss}");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_sample_reads_the_process_and_resets_the_peak() {
        let big = vec![1u8; 64 << 20];
        std::hint::black_box(&big);
        drop(big);
        let (sample, reset) = sample_and_reset(false).expect("readable /proc");
        assert!(reset, "/proc/self/clear_refs refused the peak reset");
        assert!(sample.rss_kb > 1024);
        assert!(
            sample.peak_kb >= 64 * 1024,
            "the 64 MiB touched before the sample is missing from its peak: {sample:?}"
        );
        let (_, after) = rss_and_peak_kb().unwrap();
        // Other test threads allocate concurrently; allow slack.
        assert!(
            after < sample.peak_kb,
            "peak not reset: {after} KiB vs {} KiB before",
            sample.peak_kb
        );
    }

    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    #[test]
    fn a_collect_returns_freed_c_heap() {
        let (resident_kb, kept) = crate::server::heap::tests::fragment_glibc_heap();
        allocator_collect();
        let (after, _) = rss_and_peak_kb().unwrap();
        crate::server::heap::tests::free_kept(kept);
        assert!(
            after + 100 * 1024 < resident_kb,
            "collect gave back too little: {resident_kb} KiB -> {after} KiB"
        );
    }
}

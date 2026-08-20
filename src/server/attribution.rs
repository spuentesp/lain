//! Defensive attribution: inotify watcher on the workspace + a
//! platform-appropriate PID lookup to attribute file changes to
//! registered agents.
//!
//! When the workspace filesystem reports a Modify/Create event for a file,
//! the watcher tries to figure out which registered agent actually wrote
//! the change. Two strategies are tried in order:
//!
//! 1. **PID lookup.** Walk `/proc/<pid>/fd` (Linux) or shell out to `lsof`
//!    (macOS) and look for any process that has the target file open with
//!    a write FD. If that PID matches a registered agent's `pid` field,
//!    the edit is attributed to that agent. The PID-lookup strategy is
//!    abstracted behind the [`AttributionBackend`] trait so the Linux
//!    implementation (procfs walk), the macOS implementation (lsof shell
//!    out), and a no-op fallback (Windows / `--no-process-attribution`)
//!    share a single interface.
//! 2. **Single-agent heuristic.** If no PID match and exactly one
//!    *interactive* agent is currently connected, attribute the edit to
//!    that agent. (Two-or-more agents with no PID match is treated as
//!    unattributed; the audit log gets a "unattributed edit" entry.)
//!
//! Successful attribution auto-claims the file for the agent (intent:
//! `Edit`) on the shared `OccupancyMap` and broadcasts a `ClaimGranted`
//! event so SSE subscribers see the update.

use crate::server::presence::{
    ClaimIntent, ClaimRequest, OccupancyMap, PresenceEvent, PresenceRegistry,
};
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
#[allow(unused_imports)]
use std::process::Command;
use std::sync::Arc;
use tokio::sync::broadcast;

/// Strategy for mapping a filesystem path to the PID that has it open
/// for writing. The watcher consults the backend on every Modify/Create
/// event so attribution can keep up with edits attributed to specific
/// agents.
///
/// Backends are constructed once at server start and shared via
/// `Arc<dyn AttributionBackend>` — the watcher thread may run on a
/// different thread from the constructor.
pub trait AttributionBackend: Send + Sync {
    /// Return the PID of a process that has `path` open for write, if
    /// one can be determined. `None` means "couldn't tell" (the watcher
    /// will fall through to the single-agent heuristic).
    fn lookup_writer_pid(&self, path: &Path) -> Option<u32>;

    /// Short stable identifier for the backend (used in logs / status).
    /// Examples: `"procfs"`, `"lsof"`, `"noop"`.
    fn name(&self) -> &'static str;
}

/// Linux backend: walk `/proc/<pid>/fd` and look for any process that
/// has `path` open with a write FD. This is the conservative approach
/// — we don't have a fast way to know which PID "owns" a file, so we
/// look for any writer.
///
/// On non-Linux platforms the type is still defined (so the trait can
/// stay object-safe everywhere) but its implementation always returns
/// `None`. The CLI is expected to pick `LsofBackend` on macOS and
/// `NoopBackend` elsewhere — see `cli::server::run_server`.
pub struct ProcFsBackend;

#[cfg(target_os = "linux")]
impl AttributionBackend for ProcFsBackend {
    fn lookup_writer_pid(&self, path: &Path) -> Option<u32> {
        use std::fs;

        let entries = fs::read_dir("/proc").ok()?;
        for entry in entries.flatten() {
            let name = entry.file_name();
            let pid: u32 = match name.to_str().and_then(|s| s.parse().ok()) {
                Some(p) => p,
                None => continue,
            };
            let fd_dir = entry.path().join("fd");
            let fds = match fs::read_dir(fd_dir) {
                Ok(it) => it,
                Err(_) => continue,
            };
            for fd in fds.flatten() {
                // `read_link` can race with a process closing the FD
                // between `read_dir` and `read_link` (the kernel returns
                // ENOENT). Skip that FD instead of bailing out — there
                // may be many other writers.
                let Ok(target) = fs::read_link(fd.path()) else {
                    continue;
                };
                if target == path && is_fd_writable(pid, &fd.file_name()) {
                    return Some(pid);
                }
            }
        }
        None
    }

    fn name(&self) -> &'static str {
        "procfs"
    }
}

/// Return `true` if `/proc/<pid>/fdinfo/<fd_name>` reports an open
/// mode of `O_WRONLY` (0x1) or `O_RDWR` (0x2). The previous
/// implementation read the file's *permission* mode from
/// `/proc/<pid>/fd/<fd>` (which doesn't carry open flags), and
/// silently never matched a regular file (0o644 & 0o3 == 0).
///
/// `fd_name` is the file name (a string of digits) of the FD entry
/// inside `/proc/<pid>/fd/`. We splice it into the fdinfo path and
/// read the `flags:` line.
#[cfg(target_os = "linux")]
fn is_fd_writable(pid: u32, fd_name: &std::ffi::OsStr) -> bool {
    use std::fs;
    let fdinfo_path = std::path::PathBuf::from(format!("/proc/{pid}/fdinfo/"))
        .join(fd_name);
    let Ok(contents) = fs::read_to_string(&fdinfo_path) else {
        return false;
    };
    for line in contents.lines() {
        if let Some(rest) = line.strip_prefix("flags:") {
            let flags = rest.trim();
            // `flags` is reported in octal (e.g. `0100001` = O_WRONLY).
            // Parse it as octal and check the low two bits, which are
            // `O_WRONLY` (0o1) / `O_RDWR` (0o2). We treat any other
            // permission mode as "not for writing".
            if let Ok(flags_int) = u32::from_str_radix(flags, 8) {
                return flags_int & 0o3 != 0;
            }
            return false;
        }
    }
    false
}

#[cfg(not(target_os = "linux"))]
impl AttributionBackend for ProcFsBackend {
    fn lookup_writer_pid(&self, _path: &Path) -> Option<u32> {
        None
    }

    fn name(&self) -> &'static str {
        "procfs"
    }
}

/// macOS backend: shell out to `lsof` and parse the first writer PID.
///
/// `lsof -F p <path>` emits one `p<PID>` line per process that has
/// `path` open in any mode. We return the first such PID as the
/// "writer" — agents that edit a file will hold a write FD on it, and
/// any other read-only open is rare enough to not materially skew
/// attribution. If we need write-mode filtering later we can switch
/// to `lsof -F paw` and parse the `a` field, but the heuristic is
/// good enough for first-pass attribution.
///
/// Returns `None` if `lsof` is missing from PATH, the process fails to
/// spawn, or no record matches. This is best-effort — the watcher will
/// silently degrade to the single-agent heuristic.
pub struct LsofBackend;

impl AttributionBackend for LsofBackend {
    fn lookup_writer_pid(&self, path: &Path) -> Option<u32> {
        let output = Command::new("lsof")
            .arg("-F")
            .arg("p")
            .arg(path)
            .output()
            .ok()?;

        // `lsof` returns exit code 1 when no matches are found; that is
        // not an error for our purposes. Any other non-zero exit code
        // (e.g. 127 from a missing binary — though `Command::output`
        // already returns `Err` for that case and never gets here) we
        // also treat as "no writer pid".
        if !output.status.success() && output.stdout.is_empty() {
            return None;
        }

        parse_lsof_pid_output(&output.stdout)
    }

    fn name(&self) -> &'static str {
        "lsof"
    }
}

/// Parse `lsof -F p` output and return the first numeric PID. Each
/// line is `p<PID>`; we ignore anything that doesn't parse as a PID.
/// This is intentionally permissive: a malformed line just gets
/// skipped, and the watcher treats ambiguity as "no writer pid".
fn parse_lsof_pid_output(stdout: &[u8]) -> Option<u32> {
    let text = std::str::from_utf8(stdout).ok()?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix('p') {
            if let Ok(pid) = rest.trim().parse::<u32>() {
                return Some(pid);
            }
        }
    }
    None
}

/// No-op backend: always returns `None`. Used on Windows (no `/proc` or
/// `lsof`) and when the operator passes `--no-process-attribution` on
/// any platform. The watcher still fires events, but PID-based
/// attribution silently degrades to the single-agent fallback in
/// `attribute_edit`.
pub struct NoopBackend;

impl AttributionBackend for NoopBackend {
    fn lookup_writer_pid(&self, _path: &Path) -> Option<u32> {
        None
    }

    fn name(&self) -> &'static str {
        "noop"
    }
}

/// Background watcher that attributes workspace file changes to the
/// registered agents that wrote them.
///
/// Construct via `AttributionWatcher::new` (picks a platform-default
/// backend — procfs on Linux, noop elsewhere) or `new_with_backend` for
/// explicit injection. Call `start` to spawn the watcher thread. The
/// returned `JoinHandle` can be dropped — the thread exits when its
/// `notify::RecommendedWatcher` is dropped (which happens when the
/// closure channel closes).
pub struct AttributionWatcher {
    presence: Arc<PresenceRegistry>,
    occupancy: Arc<OccupancyMap>,
    event_tx: broadcast::Sender<(u64, PresenceEvent)>,
    events_log: Arc<crate::server::events_log::EventsLog>,
    config_dir: PathBuf,
    backend: Arc<dyn AttributionBackend>,
}

impl AttributionWatcher {
    /// Construct a watcher with the platform-default backend. On Linux
    /// this is [`ProcFsBackend`] (real PID attribution); on every other
    /// platform it's [`NoopBackend`] (single-agent fallback only).
    ///
    /// Call `new_with_backend` instead if you need to override the
    /// backend explicitly (e.g. from `lain server --no-process-attribution`,
    /// or to inject `LsofBackend` on macOS).
    pub fn new(
        presence: Arc<PresenceRegistry>,
        occupancy: Arc<OccupancyMap>,
        event_tx: broadcast::Sender<(u64, PresenceEvent)>,
        events_log: Arc<crate::server::events_log::EventsLog>,
        config_dir: PathBuf,
    ) -> Self {
        let backend: Arc<dyn AttributionBackend> = if cfg!(target_os = "linux") {
            Arc::new(ProcFsBackend)
        } else {
            Arc::new(NoopBackend)
        };
        Self::new_with_backend(backend, presence, occupancy, event_tx, events_log, config_dir)
    }

    /// Construct a watcher with an explicit [`AttributionBackend`]. This
    /// is the constructor the `lain server` CLI uses so it can honor
    /// `--no-process-attribution` and pick `LsofBackend` on macOS.
    pub fn new_with_backend(
        backend: Arc<dyn AttributionBackend>,
        presence: Arc<PresenceRegistry>,
        occupancy: Arc<OccupancyMap>,
        event_tx: broadcast::Sender<(u64, PresenceEvent)>,
        events_log: Arc<crate::server::events_log::EventsLog>,
        config_dir: PathBuf,
    ) -> Self {
        Self {
            presence,
            occupancy,
            event_tx,
            events_log,
            config_dir,
            backend,
        }
    }

    /// Watch the config directory's parent (the project root) for file
    /// changes. Spawn a watcher thread; auto-attribute each change to the
    /// agent whose PID has the file open for write.
    pub fn start(self) -> std::thread::JoinHandle<()> {
        let presence = self.presence;
        let occupancy = self.occupancy;
        let event_tx = self.event_tx;
        let events_log = self.events_log;
        let root = self.config_dir;
        let backend = self.backend;

        std::thread::spawn(move || {
            let (tx, rx) = std::sync::mpsc::channel::<notify::Result<Event>>();
            let mut watcher = match RecommendedWatcher::new(
                move |res| {
                    let _ = tx.send(res);
                },
                Config::default(),
            ) {
                Ok(w) => w,
                Err(_) => return,
            };
            if watcher.watch(&root, RecursiveMode::Recursive).is_err() {
                return;
            }

            for res in rx {
                if let Ok(event) = res {
                    if matches!(
                        event.kind,
                        EventKind::Modify(_) | EventKind::Create(_)
                    ) {
                        for path in event.paths {
                            if path.is_file() {
                                attribute_edit(
                                    &path,
                                    &presence,
                                    &occupancy,
                                    &event_tx,
                                    &events_log,
                                    backend.as_ref(),
                                );
                            }
                        }
                    }
                }
            }
        })
    }
}

/// Best-effort attribution of a single file edit. See the module-level
/// docs for the strategy order: PID lookup, then single-agent fallback,
/// then "unattributed" (logged to stderr; the audit sink is wired in
/// Task 7).
fn attribute_edit(
    path: &Path,
    presence: &PresenceRegistry,
    occupancy: &OccupancyMap,
    event_tx: &broadcast::Sender<(u64, PresenceEvent)>,
    events_log: &crate::server::events_log::EventsLog,
    backend: &dyn AttributionBackend,
) {
    // 1. Try PID attribution via the injected backend.
    let pid = backend.lookup_writer_pid(path);
    let agent_id = if let Some(pid) = pid {
        presence
            .list_active(true)
            .into_iter()
            .find(|s| s.pid == Some(pid))
            .map(|s| s.id)
    } else {
        None
    };

    // 2. Fallback: single interactive agent heuristic.
    let agent_id = agent_id.or_else(|| {
        let active: Vec<_> = presence.list_active(false);
        if active.len() == 1 {
            Some(active[0].id.clone())
        } else {
            None
        }
    });

    if let Some(agent_id) = agent_id {
        let result = occupancy.claim(
            &agent_id,
            vec![ClaimRequest {
                path: path.to_path_buf(),
                symbols: vec![],
                intent: ClaimIntent::Edit,
                ttl_seconds: None,
                plan_revision: None,
            }],
        );
        if !result.granted.is_empty() {
            let ev = PresenceEvent::ClaimGranted {
                agent_id: agent_id.clone(),
                path: path.to_path_buf(),
            };
            let eid = events_log.append(&ev);
            let _ = event_tx.send((eid, ev));
        }
    } else {
        // Unattributed edit — log to audit (Task 7 wires the sink).
        eprintln!("[attribution] unattributed edit: {}", path.display());
    }
}
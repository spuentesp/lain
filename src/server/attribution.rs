//! Defensive attribution: inotify watcher on the workspace + `/proc/<pid>/fd`
//! to attribute file changes to registered agents.
//!
//! When the workspace filesystem reports a Modify/Create event for a file,
//! the watcher tries to figure out which registered agent actually wrote
//! the change. Two strategies are tried in order:
//!
//! 1. **PID lookup (Linux only).** Walk `/proc/<pid>/fd` and look for any
//!    process that has the target file open with a write FD. If that PID
//!    matches a registered agent's `pid` field, the edit is attributed to
//!    that agent.
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
use std::sync::Arc;
use tokio::sync::broadcast;

/// Background watcher that attributes workspace file changes to the
/// registered agents that wrote them.
///
/// Construct via `AttributionWatcher::new` and call `start` to spawn the
/// watcher thread. The returned `JoinHandle` can be dropped — the thread
/// exits when its `notify::RecommendedWatcher` is dropped (which happens
/// when the closure channel closes).
pub struct AttributionWatcher {
    presence: Arc<PresenceRegistry>,
    occupancy: Arc<OccupancyMap>,
    event_tx: broadcast::Sender<PresenceEvent>,
    config_dir: PathBuf,
}

impl AttributionWatcher {
    pub fn new(
        presence: Arc<PresenceRegistry>,
        occupancy: Arc<OccupancyMap>,
        event_tx: broadcast::Sender<PresenceEvent>,
        config_dir: PathBuf,
    ) -> Self {
        Self {
            presence,
            occupancy,
            event_tx,
            config_dir,
        }
    }

    /// Watch the config directory's parent (the project root) for file
    /// changes. Spawn a watcher thread; auto-attribute each change to the
    /// agent whose PID has the file open for write.
    pub fn start(self) -> std::thread::JoinHandle<()> {
        let presence = self.presence;
        let occupancy = self.occupancy;
        let event_tx = self.event_tx;
        let root = self.config_dir;

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
                                attribute_edit(&path, &presence, &occupancy, &event_tx);
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
    event_tx: &broadcast::Sender<PresenceEvent>,
) {
    // 1. Try PID attribution (Linux only).
    let pid = lookup_writer_pid(path);
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
            }],
        );
        if !result.granted.is_empty() {
            let _ = event_tx.send(PresenceEvent::ClaimGranted {
                agent_id: agent_id.clone(),
                path: path.to_path_buf(),
            });
        }
    } else {
        // Unattributed edit — log to audit (Task 7 wires the sink).
        eprintln!("[attribution] unattributed edit: {}", path.display());
    }
}

/// Walk `/proc/<pid>/fd` and look for any process that has `path` open
/// for write. This is the conservative approach — we don't have a fast
/// way to know which PID "owns" a file, so we look for any writer.
///
/// Returns `None` on non-Linux platforms (the `notify` watcher still
/// fires events, but attribution silently degrades to the single-agent
/// fallback in `attribute_edit`).
#[cfg(target_os = "linux")]
pub fn lookup_writer_pid(path: &Path) -> Option<u32> {
    use std::fs;
    use std::os::unix::fs::MetadataExt;

    let entries = fs::read_dir("/proc").ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let pid: u32 = name.to_str()?.parse().ok()?;
        let fd_dir = entry.path().join("fd");
        let fds = fs::read_dir(fd_dir).ok()?;
        for fd in fds.flatten() {
            let target = fs::read_link(fd.path()).ok()?;
            if target == path {
                // Check the file mode — only consider write FDs.
                if let Ok(meta) = fs::metadata(fd.path()) {
                    let mode = meta.mode();
                    // O_WRONLY = 0o1, O_RDWR = 0o2
                    if mode & 0o3 != 0 {
                        return Some(pid);
                    }
                }
            }
        }
    }
    None
}

#[cfg(not(target_os = "linux"))]
pub fn lookup_writer_pid(_path: &Path) -> Option<u32> {
    None
}
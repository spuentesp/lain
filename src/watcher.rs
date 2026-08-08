//! File system watcher for real-time graph updates
//!
//! Watches for file changes and updates the volatile overlay via LSP symbol extraction.

use crate::git::GitSensor;
use crate::LainServer;
use notify::{event::CreateKind, Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use parking_lot::Mutex;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

/// File extensions to watch (source code files)
const WATCHED_EXTENSIONS: &[&str] = &[
    "rs", "py", "ts", "tsx", "js", "jsx", "go", "java", "c", "cpp", "h", "hpp",
    "cs", "rb", "swift", "kt", "scala", "vue", "svelte",
];

/// Debounce window for rapid file changes
const DEBOUNCE_MS: u64 = 100;

/// Commands the watcher callback can dispatch back to the watcher
/// thread.
///
/// The watcher thread owns the receiver; any number of senders can
/// push commands through the same channel. Production only ever sends
/// `AddDirectory` (from the notify callback); `Shutdown` is the
/// clean-stop hook used by tests that own the channel and want to
/// drain the thread without leaking it.
enum WatchCommand {
    /// Register a directory-create path the notify callback just saw.
    AddDirectory(PathBuf),
    /// Stop the watcher thread's command-receive loop.
    Shutdown,
}

/// File system watcher that updates the volatile overlay on file changes
pub struct FileWatcher {
    /// Channel to send file paths that need processing
    sender: mpsc::Sender<PathBuf>,
    /// Channel to receive file paths that need processing
    receiver: mpsc::Receiver<PathBuf>,
}

impl FileWatcher {
    /// Create a new file watcher
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::channel(100);
        Self { sender, receiver }
    }

    /// Start watching the workspace directory
    pub fn start(self, workspace: PathBuf, server: LainServer) {
        let file_sender = self.sender.clone();
        let receiver = self.receiver;
        let git = Arc::clone(&server.git);

        // The watcher thread body lives in `run_watcher_thread` so
        // production *and* tests share one closure and one command
        // dispatch path. Production has no use for the test-only
        // readiness/command-done hooks, so both are passed as `None`.
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<WatchCommand>();
        let _join = run_watcher_thread(
            workspace,
            file_sender,
            git,
            cmd_tx,
            cmd_rx,
            None,
            None,
        );

        // Spawn the event processor task
        tokio::spawn(async move {
            let mut pending: HashSet<PathBuf> = HashSet::new();
            let mut receiver = receiver;
            const BATCH_SIZE: usize = 20;

            loop {
                tokio::time::sleep(Duration::from_millis(DEBOUNCE_MS)).await;

                // Collect pending paths
                match receiver.try_recv() {
                    Ok(path) => { pending.insert(path); }
                    Err(mpsc::error::TryRecvError::Disconnected) => {
                        warn!("FileWatcher: channel disconnected, stopping processor");
                        break;
                    }
                    Err(mpsc::error::TryRecvError::Empty) => {}
                }

                if pending.is_empty() {
                    continue;
                }

                // Process batch
                let batch: Vec<_> = pending.drain().take(BATCH_SIZE).collect();
                let remaining = pending.len();

                debug!(
                    "FileWatcher: processing {} files ({} remaining)",
                    batch.len(),
                    remaining
                );

                for path in &batch {
                    if let Err(e) = process_file(&server, path).await {
                        warn!("FileWatcher: failed to process {:?}: {}", path, e);
                    }
                }
            }
        });
    }
}

impl Default for FileWatcher {
    fn default() -> Self {
        Self::new()
    }
}

/// Discover every directory under `workspace` that should be watched.
///
/// Uses the `ignore` crate so the traversal honours `.gitignore`,
/// `.git/info/exclude`, the global Git ignore file, and hidden entries —
/// the same exclusions the rest of the indexer applies. Every candidate is
/// additionally probed with `read_dir` so a directory we cannot actually
/// list is dropped here rather than failing later inside `notify`.
fn discover_watch_directories(workspace: &Path) -> Vec<PathBuf> {
    let walker = ignore::WalkBuilder::new(workspace)
        .hidden(true)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(true)
        .build();

    let mut directories = Vec::new();

    // Deliberately not `.flatten()`: a flattened iterator would silently
    // swallow the permission errors this function exists to diagnose.
    for entry in walker {
        match entry {
            Ok(entry) => {
                let path = entry.path();
                if !entry.file_type().is_some_and(|file_type| file_type.is_dir()) {
                    continue;
                }
                if is_readable_directory(path) {
                    directories.push(path.to_path_buf());
                }
            }
            Err(error) => {
                warn!("FileWatcher: walk error: {}", error);
            }
        }
    }

    directories
}

/// Probe a directory for listability, logging and reporting `false` on failure.
fn is_readable_directory(path: &Path) -> bool {
    match std::fs::read_dir(path) {
        Ok(entries) => {
            drop(entries);
            true
        }
        Err(error) => {
            warn!("FileWatcher: skipping directory {:?}: {}", path, error);
            false
        }
    }
}

/// Register a single directory non-recursively, deduplicating against
/// `watched`.
///
/// A registration failure is logged and swallowed: one bad directory must
/// never terminate the watcher thread.
fn register_directory(
    watcher: &mut RecommendedWatcher,
    watched: &mut HashSet<PathBuf>,
    directory: PathBuf,
) {
    // `insert` returning false means the directory is already registered.
    if !watched.insert(directory.clone()) {
        return;
    }

    match watcher.watch(&directory, RecursiveMode::NonRecursive) {
        Ok(()) => debug!("FileWatcher: registered {:?}", directory),
        Err(error) => {
            watched.remove(&directory);
            warn!("FileWatcher: failed to watch {:?}: {}", directory, error);
        }
    }
}

/// Spawn the production watcher thread. Both `FileWatcher::start` and
/// the test suite call this, so both exercise the *same* notify
/// callback, command-receive loop, and registration path.
///
/// Arguments:
///
/// - `workspace` — root directory for initial `discover_watch_directories`.
/// - `file_sender` — Tokio mpsc sender file events are pushed through.
///   This is the same channel `FileWatcher::start` uses to feed the
///   overlay processor; tests wire a private receiver onto it.
/// - `git` — shared `GitSensor` for Git-ignore checks.
/// - `command_sender` — cloned into the notify callback so directory-
///   create events become `WatchCommand::AddDirectory` messages.
/// - `command_receiver` — consumed by the thread's receive loop.
/// - `ready_signal` — if `Some`, the thread sends the initial
///   `watched.len()` through it once startup registration is done.
///   `FileWatcher::start` passes `None`; tests pass `Some(...)` to
///   deterministically gate the test body on startup completion.
/// - `command_done` — if `Some`, the thread sends `()` after each
///   `handle_watch_command` call so tests can wait for dynamic
///   registration to finish before exercising downstream behavior.
///   `FileWatcher::start` passes `None`.
fn run_watcher_thread(
    workspace: PathBuf,
    file_sender: mpsc::Sender<PathBuf>,
    git: Arc<Mutex<GitSensor>>,
    command_sender: std::sync::mpsc::Sender<WatchCommand>,
    command_receiver: std::sync::mpsc::Receiver<WatchCommand>,
    ready_signal: Option<tokio::sync::oneshot::Sender<usize>>,
    command_done: Option<tokio::sync::mpsc::Sender<()>>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let cb_command_sender = command_sender;
        let cb_git = Arc::clone(&git);

        let mut watcher = RecommendedWatcher::new(
            move |res: Result<Event, notify::Error>| {
                match res {
                    Ok(event) => {
                        for path in &event.paths {
                            if is_created_directory_event(&event, path) {
                                let _ = cb_command_sender
                                    .send(WatchCommand::AddDirectory(path.clone()));
                            }
                        }
                        if let Some(file) = filter_event(&event, &cb_git) {
                            if let Err(error) = file_sender.blocking_send(file) {
                                debug!("FileWatcher: failed to send path: {}", error);
                            }
                        }
                    }
                    Err(error) => {
                        warn!("FileWatcher: notify callback error: {}", error);
                    }
                }
            },
            Config::default(),
        )
        .expect("Failed to create file watcher");

        // Each directory is registered non-recursively so the watcher
        // thread can add new subdirectories on demand without a
        // recursive watch (which would force us to also start watching
        // denied subtrees).
        let mut watched: HashSet<PathBuf> = HashSet::new();
        for directory in discover_watch_directories(&workspace) {
            register_directory(&mut watcher, &mut watched, directory);
        }

        if watched.is_empty() {
            warn!(
                "FileWatcher: no watchable directories under {:?}; \
                 watcher thread will idle until a command arrives",
                workspace
            );
        } else {
            info!(
                "FileWatcher: watching {} directories under {:?}",
                watched.len(),
                workspace
            );
        }

        // Startup-done signal — tests await this to avoid racing the
        // initial registration. Production passes `None` and skips it.
        if let Some(ready) = ready_signal {
            let _ = ready.send(watched.len());
        }

        // Keep the watcher alive and process dynamic registration
        // requests. The callback forwards directory-create events;
        // here we validate each one and call register_directory with
        // the deduplication set. Per-directory failures (EACCES, hidden
        // paths, Git-ignored subtrees, races) are logged and dropped
        // without ever returning from this loop — the watcher thread
        // must stay alive for as long as the file sender does.
        while let Ok(command) = command_receiver.recv() {
            match command {
                WatchCommand::AddDirectory(path) => {
                    handle_watch_command(path, &mut watcher, &mut watched, &git);
                    if let Some(ref done) = command_done {
                        // Best-effort: the test may have already
                        // dropped its receiver after asserting. A
                        // closed-receiver error is expected and benign.
                        let _ = done.try_send(());
                    }
                }
                WatchCommand::Shutdown => break,
            }
        }

        debug!("FileWatcher: command channel closed, watcher thread exiting");
    })
}

/// Decide whether `(event, path)` describes the creation of a directory
/// that we might want to register.
///
/// We only forward create events whose path is a directory and is not
/// hidden — hidden subtrees would be filtered by `is_watched_file`
/// anyway, so queueing them would just produce a wasted command. We
/// require `path.is_dir()` for every folder-shaped create kind rather
/// than trusting the backend's `CreateKind` tag: the tag is sometimes
/// imprecise (`Any`/`Other`), and on Linux inotify the syscall
/// ordering means `is_dir()` is always true by the time the callback
/// runs.
fn is_created_directory_event(event: &Event, path: &Path) -> bool {
    if !matches!(event.kind, EventKind::Create(_)) {
        return false;
    }
    if path
        .components()
        .any(|c| c.as_os_str().to_string_lossy().starts_with('.'))
    {
        return false;
    }
    match event.kind {
        EventKind::Create(CreateKind::Folder)
        | EventKind::Create(CreateKind::Any)
        | EventKind::Create(CreateKind::Other) => path.is_dir(),
        _ => false,
    }
}

/// Validate a directory-create command and, if it passes, register it
/// with the watcher.
///
/// Rejects non-directories, hidden paths, and Git-ignored paths. On
/// rejection or registration failure we log and return — the caller (the
/// watcher's command loop) must keep going so a single bad directory never
/// halts registration.
fn handle_watch_command(
    path: PathBuf,
    watcher: &mut RecommendedWatcher,
    watched: &mut HashSet<PathBuf>,
    git: &Arc<Mutex<GitSensor>>,
) {
    if !path.is_dir() {
        debug!("FileWatcher: ignoring non-directory command {:?}", path);
        return;
    }
    if path
        .components()
        .any(|c| c.as_os_str().to_string_lossy().starts_with('.'))
    {
        debug!("FileWatcher: ignoring hidden directory {:?}", path);
        return;
    }
    if is_git_ignored(&path, git) {
        debug!("FileWatcher: ignoring git-ignored directory {:?}", path);
        return;
    }
    register_directory(watcher, watched, path);
}

/// Filter notify events to only relevant file changes
fn filter_event(event: &Event, git: &Arc<Mutex<GitSensor>>) -> Option<PathBuf> {
    match event.kind {
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) => {
            // Get the first path from the event
            event.paths.iter().find_map(|p| {
                if is_watched_file(p) && !is_git_ignored(p, git) {
                    Some(p.clone())
                } else {
                    None
                }
            })
        }
        _ => None,
    }
}

/// Ask the shared `GitSensor` whether `path` is Git-ignored.
///
/// A failed ignore check is treated as *not* ignored: a transient Git
/// metadata problem should degrade into extra work, never into silently
/// dropped live updates.
fn is_git_ignored(path: &Path, git: &Arc<Mutex<GitSensor>>) -> bool {
    match git.lock().is_ignored(path) {
        Ok(ignored) => ignored,
        Err(error) => {
            debug!(
                "FileWatcher: ignore check failed for {:?}, treating as not ignored: {}",
                path, error
            );
            false
        }
    }
}

/// Check if a path is a watched source file
fn is_watched_file(path: &Path) -> bool {
    // Skip hidden files and directories
    if path
        .components()
        .any(|c| c.as_os_str().to_string_lossy().starts_with('.'))
    {
        return false;
    }

    // Skip non-files
    if !path.is_file() {
        return false;
    }

    // Check extension
    path.extension()
        .and_then(|e| e.to_str())
        .map(|ext| WATCHED_EXTENSIONS.contains(&ext))
        .unwrap_or(false)
}

/// Process a single file change and update the overlay
async fn process_file(server: &LainServer, path: &Path) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let symbols = {
        let lsp = server.lsp_pool.next();
        let mut lsp = lsp.lock().await;
        match lsp.get_document_symbols_hierarchical(path).await {
            Ok(s) => s,
            Err(e) => {
                debug!("FileWatcher: No LSP symbols for {:?}: {}", path, e);
                return Ok(()); // Not an error - file might not have LSP support
            }
        }
    };

    let count = symbols.len();
    for mut symbol in symbols {
        symbol.node.last_lsp_sync = Some(now);
        server.overlay.insert_node(symbol.node);
    }

    debug!("FileWatcher: updated overlay with {} symbols from {:?}", count, path);
    Ok(())
}

#[cfg(test)]
mod tests {
    //! Regression tests for permission-tolerant directory discovery and
    //! Git-aware event filtering (task 2), plus the dynamic
    //! registration and command-channel behavior added in task 3.

    use super::*;
    use crate::git::GitSensor;
    use parking_lot::Mutex;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    /// RAII guard that restores a directory's mode to `0o755` on drop, so the
    /// `TempDir` cleanup can recurse into it even if a test body panics.
    #[cfg(unix)]
    struct PermissionGuard {
        path: PathBuf,
    }

    #[cfg(unix)]
    impl PermissionGuard {
        fn new(path: &Path) -> Self {
            Self { path: path.to_path_buf() }
        }
    }

    #[cfg(unix)]
    impl Drop for PermissionGuard {
        fn drop(&mut self) {
            use std::os::unix::fs::PermissionsExt;
            // Best-effort: if the directory was already removed, ignore.
            let _ = fs::set_permissions(
                &self.path,
                fs::Permissions::from_mode(0o755),
            );
        }
    }

    /// Build a temp git repo with the standard layout the brief specifies:
    ///
    /// ```text
    /// repo/
    ///   .gitignore       # contains `ignored/`
    ///   visible/
    ///     source.rs
    ///   ignored/
    ///     ignored.rs
    ///   blocked/
    ///     blocked.rs     # chmod 000 on Unix (set by caller)
    /// ```
    ///
    /// The temp directory uses an explicit non-hidden prefix so the
    /// production hidden-component filter (which rejects any path whose
    /// components start with `.`) does not accidentally reject the
    /// fixture itself.
    fn build_repo_layout() -> (tempfile::TempDir, PathBuf) {
        // Non-hidden prefix on purpose — see `tempfile::tempdir()` docs:
        // the default prefix is `.tmp...`, and the production filter
        // rejects paths whose components start with `.`.
        let tmp = tempfile::Builder::new()
            .prefix("lain-watcher-test")
            .tempdir()
            .expect("tempdir");
        let repo_path = tmp.path().to_path_buf();
        let repo = git2::Repository::init(&repo_path).expect("git init");

        fs::write(repo_path.join(".gitignore"), "ignored/\n").expect("write gitignore");

        let visible = repo_path.join("visible");
        fs::create_dir(&visible).expect("mkdir visible");
        fs::write(visible.join("source.rs"), "// visible\n").expect("write visible");

        let ignored = repo_path.join("ignored");
        fs::create_dir(&ignored).expect("mkdir ignored");
        fs::write(ignored.join("ignored.rs"), "// ignored\n").expect("write ignored");

        let blocked = repo_path.join("blocked");
        fs::create_dir(&blocked).expect("mkdir blocked");
        fs::write(blocked.join("blocked.rs"), "// blocked\n").expect("write blocked");

        // Touch the index so the repo is in a normal state; some git2
        // versions are picky about an empty repo when probing ignore state.
        let mut index = repo.index().expect("repo index");
        index
            .add_path(std::path::Path::new("visible/source.rs"))
            .expect("add visible");
        index.write().expect("write index");

        (tmp, repo_path)
    }

    /// Step 1: discover_watch_directories must skip Git-ignored and
    /// permission-blocked directories while still returning readable
    /// siblings. Unix-only because the chmod 000 trick is meaningless on
    /// Windows ACLs.
    #[cfg(unix)]
    #[test]
    fn directory_discovery_skips_ignored_and_inaccessible() {
        use std::os::unix::fs::PermissionsExt;

        let (tmp, repo) = build_repo_layout();
        let blocked = repo.join("blocked");

        // Make `blocked` unreadable. RAII guard restores mode to `0o755`
        // on drop (including panic-unwind), so cleanup remains reliable.
        fs::set_permissions(&blocked, fs::Permissions::from_mode(0o000))
            .expect("chmod 000");
        let _guard = PermissionGuard::new(&blocked);

        // chmod 000 is not a reliable EACCES signal under root or with
        // CAP_DAC_OVERRIDE — the kernel bypasses DAC for such processes.
        // If we can still list the directory, the test cannot exercise
        // the EACCES path; skip with a clear message instead of failing
        // for the wrong reason.
        if fs::read_dir(&blocked).is_ok() {
            eprintln!(
                "directory_discovery_skips_ignored_and_inaccessible: SKIPPED \
                 (process can still read `blocked/` after chmod 000 — likely \
                 running as root or with CAP_DAC_OVERRIDE)"
            );
            return;
        }

        // The explicit annotation pins the result type so any future
        // signature drift surfaces here rather than downstream.
        let watched: Vec<PathBuf> = discover_watch_directories(&repo);

        assert!(
            watched.contains(&repo),
            "workspace root itself should be watched; got {:?}",
            watched,
        );
        // Closure parameters are explicitly typed as `&PathBuf` so the
        // missing-helper diagnostic stays focused on E0425 instead of
        // cascading into "type annotations needed" errors.
        assert!(
            watched.iter().any(|p: &PathBuf| p.ends_with("visible")),
            "readable sibling should be discovered; got {:?}",
            watched,
        );
        assert!(
            !watched.iter().any(|p: &PathBuf| p.ends_with("ignored")),
            "Git-ignored directory must be excluded; got {:?}",
            watched,
        );
        assert!(
            !watched.iter().any(|p: &PathBuf| p.ends_with("blocked")),
            "EACCES directory must be excluded; got {:?}",
            watched,
        );

        // Scope-end drop order runs `PermissionGuard` *before* `tmp`,
        // restoring `0o755` on `blocked/` first, then `TempDir` cleanup
        // recurses into the now-readable directory. (An explicit
        // `drop(tmp)` here would force TempDir cleanup *first* and leak
        // the tree because `blocked/` is still `0o000` at that point.)
    }

    /// Step 3: filter_event must consult the git repo so events for
    /// Git-ignored source files are filtered out before they reach the
    /// overlay pipeline.
    #[test]
    fn ignored_source_events_are_filtered() {
        let (tmp, repo_path) = build_repo_layout();

        let visible_path = repo_path.join("visible").join("source.rs");
        let ignored_path = repo_path.join("ignored").join("ignored.rs");

        let visible_event = notify::Event {
            kind: notify::EventKind::Create(notify::event::CreateKind::File),
            paths: vec![visible_path.clone()],
            attrs: notify::event::EventAttributes::default(),
        };
        let ignored_event = notify::Event {
            kind: notify::EventKind::Create(notify::event::CreateKind::File),
            paths: vec![ignored_path.clone()],
            attrs: notify::event::EventAttributes::default(),
        };

        // Git-aware signature:
        //   filter_event(&Event, &Arc<parking_lot::Mutex<GitSensor>>)
        //       -> Option<PathBuf>
        // GitSensor is the production sensor type — we wrap it in the
        // Arc<Mutex<_>> handle production code already uses elsewhere.
        let sensor = Arc::new(Mutex::new(
            GitSensor::new(&repo_path).expect("GitSensor::new"),
        ));

        let kept = filter_event(&visible_event, &sensor);
        let dropped = filter_event(&ignored_event, &sensor);

        assert_eq!(
            kept,
            Some(visible_path.clone()),
            "non-ignored source event should be kept as the visible path",
        );
        assert_eq!(
            dropped, None,
            "Git-ignored source event must be filtered out",
        );

        // Drop the GitSensor handle before TempDir so any in-memory state
        // referencing the repo path is gone before auto-cleanup runs.
        drop(sensor);
        drop(tmp);
    }

    /// Step 4: a readable sibling directory must keep emitting events
    /// even when its neighbour is EACCES — i.e. one bad directory at
    /// startup must not abort registration for the rest of the
    /// workspace.
    ///
    /// This test exercises the **production** watcher thread spawned
    /// by `run_watcher_thread` (the same helper `FileWatcher::start`
    /// uses), with a private Tokio mpsc receiver standing in for the
    /// overlay processor. After waiting for the readiness signal, we
    /// touch a file in `visible/` and assert a notify event lands
    /// within a bounded timeout, proving the readable sibling's watch
    /// survived even though `blocked/` was EACCES.
    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn readable_events_after_inaccessible_sibling() {
        use std::os::unix::fs::PermissionsExt;

        let (tmp, repo) = build_repo_layout();
        let blocked = repo.join("blocked");
        fs::set_permissions(&blocked, fs::Permissions::from_mode(0o000))
            .expect("chmod 000");
        let _guard = PermissionGuard::new(&blocked);

        if fs::read_dir(&blocked).is_ok() {
            eprintln!(
                "readable_events_after_inaccessible_sibling: SKIPPED \
                 (process can still read `blocked/` after chmod 000 — likely \
                 running as root or with CAP_DAC_OVERRIDE)"
            );
            return;
        }

        let git = Arc::new(Mutex::new(
            GitSensor::new(&repo).expect("GitSensor::new"),
        ));

        // Production-shape channels:
        // - file events flow through a Tokio mpsc (same type as the
        //   overlay processor); the test awaits its receiver.
        // - watch commands flow through the std::sync::mpsc that
        //   `run_watcher_thread` expects.
        let (file_tx, mut file_rx) = tokio::sync::mpsc::channel::<PathBuf>(16);
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<WatchCommand>();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<usize>();

        let join = run_watcher_thread(
            repo.clone(),
            file_tx.clone(),
            git.clone(),
            cmd_tx.clone(),
            cmd_rx,
            Some(ready_tx),
            None,
        );

        // Wait for the watcher thread's startup registration to
        // complete. Without this gate, the test could race the
        // inotify watches being set up and miss the event entirely.
        let initial_count = tokio::time::timeout(Duration::from_secs(5), ready_rx)
            .await
            .expect("watcher thread should signal readiness within 5s")
            .expect("watcher thread dropped the readiness sender");

        assert!(
            initial_count > 0,
            "expected at least one directory registered; got {}",
            initial_count,
        );

        // Touch a file in the readable sibling. The notify callback
        // should deliver this through the test channel within the
        // bounded timeout. A 5-second budget is generous enough that
        // a busy CI won't flake while still failing fast if the
        // watcher thread silently died after EACCES.
        let visible = repo.join("visible").join("source.rs");
        fs::write(&visible, "// updated\n").expect("write visible");

        let received = tokio::time::timeout(Duration::from_secs(5), file_rx.recv())
            .await
            .expect("event for visible/source.rs within 5s")
            .expect("file event channel returned None");

        assert!(
            received.ends_with("visible/source.rs"),
            "expected visible/source.rs; got {:?}",
            received,
        );

        // Drive a clean shutdown so the watcher thread joins without
        // a leak. `join.join()` blocks, so it has to run on the
        // blocking thread pool — the runtime is `current_thread`,
        // which can't be blocked here.
        let _ = cmd_tx.send(WatchCommand::Shutdown);
        drop(cmd_tx);
        drop(file_tx);
        tokio::task::spawn_blocking(move || join.join())
            .await
            .expect("join task panicked")
            .expect("watcher thread should exit cleanly");

        drop(git);
    }

    /// Step 5: a directory created after startup must trigger the
    /// production callback's directory-request send, then be picked up
    /// by the command-receive loop and validated through
    /// `handle_watch_command`, after which events from files created
    /// inside the new directory must reach the callback.
    ///
    /// Determinism comes from `command_done`: the watcher thread sends
    /// `()` after each `handle_watch_command` call, so the test can
    /// await proof of registration before writing the new file
    /// (writing before registration would lose the notify event).
    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn newly_created_directory_is_registered() {
        let (tmp, repo) = build_repo_layout();

        let git = Arc::new(Mutex::new(
            GitSensor::new(&repo).expect("GitSensor::new"),
        ));

        let (file_tx, mut file_rx) = tokio::sync::mpsc::channel::<PathBuf>(16);
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<WatchCommand>();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<usize>();
        let (cmd_done_tx, mut cmd_done_rx) = tokio::sync::mpsc::channel::<()>(16);

        let join = run_watcher_thread(
            repo.clone(),
            file_tx.clone(),
            git.clone(),
            cmd_tx.clone(),
            cmd_rx,
            Some(ready_tx),
            Some(cmd_done_tx),
        );

        // Gate on the startup registration completing.
        let initial_count = tokio::time::timeout(Duration::from_secs(5), ready_rx)
            .await
            .expect("watcher thread should signal readiness within 5s")
            .expect("watcher thread dropped the readiness sender");
        assert!(
            initial_count > 0,
            "expected some directories registered at startup; got {}",
            initial_count,
        );

        // Create the child directory *after* startup is done. The
        // production notify callback should see the IN_CREATE for the
        // folder, run `is_created_directory_event` (now requiring
        // `path.is_dir()`), and dispatch a `WatchCommand::AddDirectory`
        // back to the watcher thread through the command channel.
        let child = repo.join("new_child");
        fs::create_dir(&child).expect("mkdir new_child");

        // The thread's command loop will receive the AddDirectory,
        // call `handle_watch_command`, then `try_send(())` on the
        // done channel. Awaiting that signal proves the callback,
        // loop, and validator all ran without the test having to
        // construct its own watcher or call `register_directory`
        // directly.
        tokio::time::timeout(Duration::from_secs(5), cmd_done_rx.recv())
            .await
            .expect("AddDirectory command should be processed within 5s")
            .expect("command_done channel closed unexpectedly");

        // Now write a file inside the newly-registered directory and
        // expect the notify callback to deliver it. The 5-second
        // budget is generous without becoming flaky.
        let new_file = child.join("new_source.rs");
        fs::write(&new_file, "// new\n").expect("write new_source.rs");

        let received = tokio::time::timeout(Duration::from_secs(5), file_rx.recv())
            .await
            .expect("event for new_child/new_source.rs within 5s")
            .expect("file event channel returned None");

        assert!(
            received.ends_with("new_child/new_source.rs"),
            "expected new_child/new_source.rs; got {:?}",
            received,
        );

        // Clean shutdown.
        let _ = cmd_tx.send(WatchCommand::Shutdown);
        drop(cmd_tx);
        drop(file_tx);
        tokio::task::spawn_blocking(move || join.join())
            .await
            .expect("join task panicked")
            .expect("watcher thread should exit cleanly");

        drop(git);
    }
}

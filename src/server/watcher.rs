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

/// Return the set of paths the reload-aware watcher should watch given
/// the path to `repos.yaml`. Always includes `repos.yaml`; adds
/// `workspaces.yaml` next to it if the file exists. The watcher
/// ignores directories and other files in the same parent.
pub fn watch_paths_for_config(repos_yaml: &Path) -> Vec<PathBuf> {
    let mut paths = vec![repos_yaml.to_path_buf()];
    let parent = repos_yaml.parent().unwrap_or_else(|| Path::new("."));
    let ws = parent.join("workspaces.yaml");
    if ws.exists() {
        paths.push(ws);
    }
    paths
}

/// Spawn a dedicated `notify` watcher for the config files
/// (`repos.yaml` + `workspaces.yaml`). On any Modify/Create/Remove
/// event against those exact paths, calls `bus.request_reload()`.
///
/// Returns a `JoinHandle` for the watcher thread. Tests can drop it
/// to terminate; production drops it at server shutdown.
///
/// Implementation note: this is a separate watcher from
/// `FileWatcher::start` (which watches source files for the volatile
/// overlay). The two never interact — different paths, different
/// handlers, different channels.
pub fn spawn_config_watcher(
    repos_yaml: &Path,
    bus: Arc<crate::server::reload::ReloadBus>,
) -> std::thread::JoinHandle<()> {
    use crate::server::reload::ReloadBus;

    let targets: HashSet<PathBuf> = watch_paths_for_config(repos_yaml)
        .into_iter()
        .collect();

    std::thread::spawn(move || {
        let bus_clone: Arc<ReloadBus> = Arc::clone(&bus);
        let targets_clone = targets.clone();

        let mut watcher = match RecommendedWatcher::new(
            move |res: Result<Event, notify::Error>| {
                if let Ok(event) = res {
                    for path in &event.paths {
                        if targets_clone.contains(path) {
                            if let Err(e) = bus_clone.request_reload() {
                                warn!(
                                    "FileWatcher (config): bus.request_reload() failed: {e}"
                                );
                            } else {
                                debug!(
                                    "FileWatcher (config): reload requested for {:?}",
                                    path
                                );
                            }
                        }
                    }
                }
            },
            Config::default(),
        ) {
            Ok(w) => w,
            Err(e) => {
                warn!("FileWatcher (config): failed to create watcher: {e}");
                return;
            }
        };

        let mut registered: HashSet<PathBuf> = HashSet::new();
        for path in &targets {
            // Watch the parent directory (non-recursive) so file
            // creates and deletes fire events; the callback filters
            // by exact path. Watching the file directly misses
            // atomic-rename sequences on some platforms.
            let watch_root = path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .to_path_buf();
            if registered.insert(watch_root.clone()) {
                if let Err(e) = watcher.watch(&watch_root, RecursiveMode::NonRecursive) {
                    warn!(
                        "FileWatcher (config): failed to watch {:?}: {e}",
                        watch_root
                    );
                }
            }
        }

        if registered.is_empty() {
            warn!("FileWatcher (config): no parent directories to watch");
        } else {
            info!(
                "FileWatcher (config): watching {} directories for config changes",
                registered.len()
            );
        }

        // Block forever; the watcher is dropped when this thread exits
        // (i.e. when the JoinHandle is dropped by the server shutdown
        // path).
        loop {
            std::thread::sleep(Duration::from_secs(3600));
        }
    })
}

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
pub(crate) enum WatchCommand {
    /// Register a directory-create path the notify callback just saw.
    AddDirectory(PathBuf),
    /// Stop the watcher thread's command-receive loop.
    #[allow(dead_code)]
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
        let _join = run_watcher_thread(WatcherThreadArgs::production(
            workspace,
            file_sender,
            git,
            (cmd_tx, cmd_rx),
        ));

        // Spawn the event processor task
        tokio::spawn(async move {
            let mut pending: HashSet<PathBuf> = HashSet::new();
            let mut receiver = receiver;
            const BATCH_SIZE: usize = 20;

            loop {
                tokio::time::sleep(Duration::from_millis(DEBOUNCE_MS)).await;

                // Drain everything queued during the debounce window, not
                // one path per tick. A single `try_recv` meant a save that
                // touched N files took N debounce intervals to catch up.
                let mut disconnected = false;
                loop {
                    match receiver.try_recv() {
                        Ok(path) => {
                            pending.insert(path);
                        }
                        Err(mpsc::error::TryRecvError::Disconnected) => {
                            warn!("FileWatcher: channel disconnected, stopping processor");
                            disconnected = true;
                            break;
                        }
                        Err(mpsc::error::TryRecvError::Empty) => break,
                    }
                }
                if disconnected {
                    break;
                }

                if pending.is_empty() {
                    continue;
                }

                // Process batch
                let batch = take_batch(&mut pending, BATCH_SIZE);
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

/// Take up to `limit` paths out of `pending`, leaving the rest queued.
///
/// This was `pending.drain().take(limit).collect()`. `HashSet::drain`
/// empties the set when the iterator is dropped regardless of how many
/// items were actually consumed, so a batch of 20 silently discarded
/// every further changed file — and `pending.len()` afterwards was always
/// 0, which is why the "N remaining" log never reported a backlog. A
/// checkout or a rename touching more than `limit` files lost the tail.
fn take_batch(pending: &mut HashSet<PathBuf>, limit: usize) -> Vec<PathBuf> {
    let batch: Vec<PathBuf> = pending.iter().take(limit).cloned().collect();
    for p in &batch {
        pending.remove(p);
    }
    batch
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
/// Optional hooks used by tests to gate on watcher lifecycle events.
/// `FileWatcher::start` passes `None` for every field; the test bodies
/// in `mod tests` populate what they need via [`WatcherThreadArgs::for_test`].
#[derive(Default)]
pub(crate) struct WatcherTestHooks {
    /// If `Some`, the watcher thread sends the number of registered
    /// directories through it once startup registration completes.
    /// Tests use this to avoid racing the initial notify registration.
    pub ready_signal: Option<tokio::sync::oneshot::Sender<usize>>,
    /// If `Some`, the watcher thread sends `()` after each
    /// `WatchCommand::AddDirectory` is processed. Tests use this to
    /// confirm a dynamic registration actually ran (without having to
    /// poll a file event or reconstruct the watcher themselves).
    pub command_done: Option<tokio::sync::mpsc::Sender<()>>,
}

/// Bundle the 7 parameters that `run_watcher_thread` historically
/// took positionally. Four of them are channels and easy to swap by
/// accident in call sites — the struct form + named constructors
/// (`production` / `for_test`) makes the intent explicit.
pub(crate) struct WatcherThreadArgs {
    pub workspace: PathBuf,
    pub file_sender: mpsc::Sender<PathBuf>,
    pub git: Arc<Mutex<GitSensor>>,
    pub command_pair: (
        std::sync::mpsc::Sender<WatchCommand>,
        std::sync::mpsc::Receiver<WatchCommand>,
    ),
    pub test_hooks: WatcherTestHooks,
}

impl WatcherThreadArgs {
    /// Production wiring: no test hooks. Used by
    /// `FileWatcher::start` and `spawn_config_watcher`.
    pub fn production(
        workspace: PathBuf,
        file_sender: mpsc::Sender<PathBuf>,
        git: Arc<Mutex<GitSensor>>,
        command_pair: (
            std::sync::mpsc::Sender<WatchCommand>,
            std::sync::mpsc::Receiver<WatchCommand>,
        ),
    ) -> Self {
        Self {
            workspace,
            file_sender,
            git,
            command_pair,
            test_hooks: WatcherTestHooks::default(),
        }
    }

    /// Test wiring: caller provides whichever hooks they need.
    #[cfg(test)]
    pub fn for_test(
        workspace: PathBuf,
        file_sender: mpsc::Sender<PathBuf>,
        git: Arc<Mutex<GitSensor>>,
        command_pair: (
            std::sync::mpsc::Sender<WatchCommand>,
            std::sync::mpsc::Receiver<WatchCommand>,
        ),
        hooks: WatcherTestHooks,
    ) -> Self {
        Self {
            workspace,
            file_sender,
            git,
            command_pair,
            test_hooks: hooks,
        }
    }
}

fn run_watcher_thread(args: WatcherThreadArgs) -> std::thread::JoinHandle<()> {
    let WatcherThreadArgs {
        workspace,
        file_sender,
        git,
        command_pair: (command_sender, command_receiver),
        test_hooks,
    } = args;
    let WatcherTestHooks {
        ready_signal,
        command_done,
    } = test_hooks;
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
        match lsp.get_document_symbols_hierarchical(path, &server.config.workspace).await {
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
        let node = symbol.node.clone();
        server.overlay.insert_node(symbol.node);
        // Broadcast the new node to any subscribed sidecar.
        server.broadcast_overlay_insert(node);
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

    /// `pending.drain().take(limit)` empties the whole set when the
    /// `Drain` is dropped, so everything past `limit` vanished and the
    /// "N remaining" log always printed 0. A branch switch touching more
    /// than `BATCH_SIZE` files silently lost the tail.
    #[test]
    fn take_batch_leaves_the_overflow_queued() {
        let mut pending: HashSet<PathBuf> = (0..25)
            .map(|i| PathBuf::from(format!("/src/f{i}.rs")))
            .collect();

        let batch = super::take_batch(&mut pending, 20);

        assert_eq!(batch.len(), 20, "takes up to the limit");
        assert_eq!(
            pending.len(),
            5,
            "the remaining 5 must stay queued, not be discarded"
        );
        for p in &batch {
            assert!(!pending.contains(p), "a taken path must not remain pending");
        }
    }

    #[test]
    fn take_batch_drains_everything_when_under_the_limit() {
        let mut pending: HashSet<PathBuf> =
            [PathBuf::from("/src/a.rs"), PathBuf::from("/src/b.rs")].into_iter().collect();
        let batch = super::take_batch(&mut pending, 20);
        assert_eq!(batch.len(), 2);
        assert!(pending.is_empty());
    }

    /// The whole `FileWatcher` type — thread, debounce loop, `process_file`,
    /// and its `overlay.insert_node` — was never constructed outside this
    /// test module, so the volatile overlay was dead in every running
    /// server while the README advertised live freshness. Nothing failed;
    /// there was simply no caller. Pin that both entrypoints start it.
    #[test]
    fn both_bootstraps_start_the_source_watcher() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        for f in ["src/cli/mcp.rs", "src/cli/server.rs"] {
            let src = std::fs::read_to_string(root.join(f)).unwrap();
            assert!(
                src.contains("start_source_watcher"),
                "{f} must start the source-file watcher, or the volatile \
                 overlay is never written to and freshness is a fiction"
            );
        }
    }

    /// Step 1: discover_watch_directories must skip Git-ignored and
    /// permission-blocked directories while still returning readable
    /// siblings. Unix-only because the chmod 000 trick is meaningless on
    /// Windows ACLs.
    #[cfg(unix)]
    #[test]
    fn directory_discovery_skips_ignored_and_inaccessible() {
        use std::os::unix::fs::PermissionsExt;

        let (_tmp, repo) = build_repo_layout();
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

        let (_tmp, repo) = build_repo_layout();
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

        let join = run_watcher_thread(WatcherThreadArgs::for_test(
            repo.clone(),
            file_tx.clone(),
            git.clone(),
            (cmd_tx.clone(), cmd_rx),
            WatcherTestHooks { ready_signal: Some(ready_tx), command_done: None },
        ));

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

    /// Step 4: `watch_paths_for_config` returns the right paths
    /// whether or not `workspaces.yaml` already exists.
    #[test]
    fn watch_paths_for_config_lists_existing_workspaces_yaml() {
        let tmp = tempfile::tempdir().unwrap();
        let repos = tmp.path().join("repos.yaml");
        std::fs::write(&repos, "repos: []").unwrap();
        // No workspaces.yaml yet — only repos.yaml.
        let paths = super::watch_paths_for_config(&repos);
        assert_eq!(paths, vec![repos.clone()]);

        // Add workspaces.yaml — now both files appear.
        let ws = tmp.path().join("workspaces.yaml");
        std::fs::write(&ws, "workspaces: []").unwrap();
        let paths = super::watch_paths_for_config(&repos);
        assert!(paths.contains(&repos));
        assert!(paths.contains(&ws));
        assert_eq!(paths.len(), 2);
    }

    /// Step 5: `spawn_config_watcher` reacts to `repos.yaml` modify
    /// events by calling `bus.request_reload()`. The watcher thread
    /// is dropped at the end of the test so the OS handles cleanup.
    #[tokio::test(flavor = "current_thread")]
    async fn config_watcher_triggers_reload_on_repos_yaml_modify() {
        let tmp = tempfile::tempdir().unwrap();
        let repos = tmp.path().join("repos.yaml");
        std::fs::write(&repos, "repos: []").unwrap();

        let bus = Arc::new(crate::server::reload::ReloadBus::new());
        let mut sub = bus.subscribe();
        let _join = super::spawn_config_watcher(&repos, Arc::clone(&bus));

        // Give the watcher a moment to register.
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Modify the file.
        std::fs::write(&repos, "repos:\n  - id: r1\n").unwrap();

        // The bus should see the reload request within 2s.
        let result = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if sub.try_recv().is_ok() {
                    return true;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        assert!(result.unwrap_or(false), "expected reload request within 2s");
    }

    /// Step 6: `spawn_config_watcher` also reacts to `workspaces.yaml`
    /// modify events. We create the file before the watcher so it's in
    /// the watched set from startup.
    #[tokio::test(flavor = "current_thread")]
    async fn config_watcher_triggers_reload_on_workspaces_yaml_modify() {
        let tmp = tempfile::tempdir().unwrap();
        let repos = tmp.path().join("repos.yaml");
        let ws = tmp.path().join("workspaces.yaml");
        std::fs::write(&repos, "repos: []").unwrap();
        std::fs::write(&ws, "workspaces: []").unwrap();

        let bus = Arc::new(crate::server::reload::ReloadBus::new());
        let mut sub = bus.subscribe();
        let _join = super::spawn_config_watcher(&repos, Arc::clone(&bus));

        tokio::time::sleep(Duration::from_millis(100)).await;

        std::fs::write(&ws, "workspaces:\n  - name: w1\n    members: [r1]\n").unwrap();

        let result = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if sub.try_recv().is_ok() {
                    return true;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        assert!(result.unwrap_or(false), "expected reload request within 2s");
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
        let (_tmp, repo) = build_repo_layout();

        let git = Arc::new(Mutex::new(
            GitSensor::new(&repo).expect("GitSensor::new"),
        ));

        let (file_tx, mut file_rx) = tokio::sync::mpsc::channel::<PathBuf>(16);
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<WatchCommand>();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<usize>();
        let (cmd_done_tx, mut cmd_done_rx) = tokio::sync::mpsc::channel::<()>(16);

        let join = run_watcher_thread(WatcherThreadArgs::for_test(
            repo.clone(),
            file_tx.clone(),
            git.clone(),
            (cmd_tx.clone(), cmd_rx),
            WatcherTestHooks { ready_signal: Some(ready_tx), command_done: Some(cmd_done_tx) },
        ));

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

//! File system watcher for real-time graph updates
//!
//! Watches for file changes and updates the volatile overlay via LSP symbol extraction.

use crate::LainServer;
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

/// File extensions to watch (source code files)
const WATCHED_EXTENSIONS: &[&str] = &[
    "rs", "py", "ts", "tsx", "js", "jsx", "go", "java", "c", "cpp", "h", "hpp",
    "cs", "rb", "swift", "kt", "scala", "vue", "svelte",
];

/// Debounce window for rapid file changes
const DEBOUNCE_MS: u64 = 100;

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
        let sender = self.sender.clone();
        let receiver = self.receiver;

        // Spawn the notify watcher in its own thread
        std::thread::spawn(move || {
            let mut watcher = RecommendedWatcher::new(
                move |res: Result<Event, notify::Error>| {
                    if let Ok(event) = res {
                        if let Some(path) = filter_event(&event) {
                            if let Err(e) = sender.blocking_send(path) {
                                debug!("FileWatcher: failed to send path: {}", e);
                            }
                        }
                    }
                },
                Config::default(),
            )
            .expect("Failed to create file watcher");

            if let Err(e) = watcher.watch(&workspace, RecursiveMode::Recursive) {
                error!("FileWatcher: failed to watch workspace {:?}: {}", workspace, e);
                return;
            }

            info!("FileWatcher: watching {:?} recursively", workspace);

            // Keep watcher alive - block the thread
            loop {
                std::thread::sleep(Duration::from_secs(60));
            }
        });

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

/// Filter notify events to only relevant file changes
fn filter_event(event: &Event) -> Option<PathBuf> {
    match event.kind {
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) => {
            // Get the first path from the event
            event.paths.iter().find_map(|p| {
                if is_watched_file(p) {
                    Some(p.clone())
                } else {
                    None
                }
            })
        }
        _ => None,
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

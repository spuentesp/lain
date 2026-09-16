//! Hot reload — signal bus shared by watcher, listener, MCP handler, rebuild task.
//!
//! Extracted from `LainServer` in PR 3.7a. `LainServer` will hold an
//! `Arc<HotReloadBus>` in PR 3.7b and forward `reload_bus()` through.

use crate::server::reload::ReloadBus;
use std::sync::Arc;

/// Hot-reload signal bus. Always allocated (single-workspace and
/// federation-mode servers both hold one); the actual rebuild loop
/// is only spawned in federation mode.
pub struct HotReloadBus {
    pub(crate) reload_bus: Arc<ReloadBus>,
}

impl HotReloadBus {
    pub fn new(reload_bus: Arc<ReloadBus>) -> Self {
        Self { reload_bus }
    }

    /// Shared reload bus accessor. Always returns a bus; the bus is
    /// lazily initialized on the first call when the field is missing
    /// (only the case for `LainServer::new` before this commit — we
    /// construct it eagerly today but the accessor is the contract).
    pub fn reload_bus(&self) -> Arc<ReloadBus> {
        Arc::clone(&self.reload_bus)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_a_clone_of_the_inner_bus() {
        let bus = Arc::new(ReloadBus::default());
        let handle = HotReloadBus::new(Arc::clone(&bus));
        let returned = handle.reload_bus();
        assert!(Arc::ptr_eq(&returned, &bus));
    }
}

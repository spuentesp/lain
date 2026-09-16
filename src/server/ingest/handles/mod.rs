//! Partition handles extracted from `LainServer`.
//!
//! Each handle owns the fields and methods for one of the 9 partitions
//! the audit carved out of the LainServer god struct (Ingest, Refresh,
//! Federation, Presence, Audit, Hot reload, Auth, Attribution,
//! Lifecycle). PR 3.7a introduces them as standalone types so the
//! partition shape can be validated in isolation. PR 3.7b will swap
//! the field/method ownership on LainServer itself.
//!
//! The handles are concrete structs (no `dyn Trait`) per the audit's
//! recommendation and the `m04-zero-cost` skill: static dispatch, Arc
//! cloning for the cheap reference-shared fields, zero runtime
//! overhead beyond the inherent `Arc` increments.

pub mod attribution;
pub mod audit;
pub mod auth;
pub mod federation;
pub mod hot_reload;
pub mod ingest;
pub mod lifecycle;
pub mod presence;
pub mod refresh;

pub use attribution::AttributionState;
pub use audit::AuditState;
pub use auth::AuthHandle;
pub use federation::FederationHandle;
pub use hot_reload::HotReloadBus;
pub use ingest::IngestHandle;
pub use lifecycle::LifecycleInfo;
pub use presence::PresenceLayer;
pub use refresh::RefreshState;

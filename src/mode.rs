//! Mode the binary runs in.
//!
//! - `owner`   : holds the workspace lock, owns the graph, runs ingestion.
//! - `sidecar` : read-only client that subscribes to the owner's overlay stream.
//! - `auto`    : try to become owner; if another owner already holds the lock,
//!               fall back to sidecar. This is the default so multiple agents
//!               can share a single long-running owner without manual mode flags.

use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LainMode { Auto, Owner, Sidecar }

impl Default for LainMode { fn default() -> Self { LainMode::Auto } }

impl FromStr for LainMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "auto" => Ok(LainMode::Auto),
            "owner" => Ok(LainMode::Owner),
            "sidecar" => Ok(LainMode::Sidecar),
            other => Err(format!("unknown lain mode: {other}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::LainMode;
    #[test]
    fn parse_auto() {
        assert_eq!("auto".parse::<LainMode>().unwrap(), LainMode::Auto);
    }
    #[test]
    fn parse_owner() {
        assert_eq!("owner".parse::<LainMode>().unwrap(), LainMode::Owner);
    }
    #[test]
    fn parse_sidecar() {
        assert_eq!("sidecar".parse::<LainMode>().unwrap(), LainMode::Sidecar);
    }
    #[test]
    fn default_is_auto() {
        assert_eq!(LainMode::default(), LainMode::Auto);
    }
}

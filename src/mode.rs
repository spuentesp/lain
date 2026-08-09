//! Mode the binary runs in: owner (the default, today) or sidecar
//! (a read-only client that subscribes to the owner's overlay stream).

use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LainMode { Owner, Sidecar }

impl Default for LainMode { fn default() -> Self { LainMode::Owner } }

impl FromStr for LainMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
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
    fn parse_owner() {
        assert_eq!("owner".parse::<LainMode>().unwrap(), LainMode::Owner);
    }
    #[test]
    fn parse_sidecar() {
        assert_eq!("sidecar".parse::<LainMode>().unwrap(), LainMode::Sidecar);
    }
    #[test]
    fn default_is_owner() {
        assert_eq!(LainMode::default(), LainMode::Owner);
    }
}

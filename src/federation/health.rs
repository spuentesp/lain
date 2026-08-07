use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RepoHealth {
    Ready,
    Indexing,
    Degraded,
    Unavailable,
    Missing,
}

impl RepoHealth {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Indexing => "indexing",
            Self::Degraded => "degraded",
            Self::Unavailable => "unavailable",
            Self::Missing => "missing",
        }
    }
    pub fn is_serving(&self) -> bool {
        matches!(self, Self::Ready | Self::Indexing)
    }
}

impl std::fmt::Display for RepoHealth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn as_str_matches_variant() {
        assert_eq!(RepoHealth::Ready.as_str(), "ready");
        assert_eq!(RepoHealth::Indexing.as_str(), "indexing");
        assert_eq!(RepoHealth::Degraded.as_str(), "degraded");
        assert_eq!(RepoHealth::Unavailable.as_str(), "unavailable");
        assert_eq!(RepoHealth::Missing.as_str(), "missing");
    }

    #[test]
    fn is_serving_for_ready_and_indexing() {
        assert!(RepoHealth::Ready.is_serving());
        assert!(RepoHealth::Indexing.is_serving());
    }

    #[test]
    fn is_not_serving_for_terminal_states() {
        assert!(!RepoHealth::Degraded.is_serving());
        assert!(!RepoHealth::Unavailable.is_serving());
        assert!(!RepoHealth::Missing.is_serving());
    }

    #[test]
    fn serde_roundtrip() {
        for h in [RepoHealth::Ready, RepoHealth::Indexing, RepoHealth::Degraded, RepoHealth::Unavailable, RepoHealth::Missing] {
            let s = serde_json::to_string(&h).unwrap();
            let back: RepoHealth = serde_json::from_str(&s).unwrap();
            assert_eq!(h, back);
        }
    }
}
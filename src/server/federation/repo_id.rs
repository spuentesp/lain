use crate::schema::NodeType;

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct RepoId(String);

impl RepoId {
    pub fn new(s: &str) -> Result<Self, crate::error::LainError> {
        if s.is_empty() || s.contains(':') || s.contains('/') {
            return Err(crate::error::LainError::InvalidRepoId(s.to_string()));
        }
        Ok(Self(s.to_string()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RepoId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct GlobalId(String);

impl GlobalId {
    pub fn new(
        repo: &RepoId,
        kind: NodeType,
        path: &str,
        name: &str,
        line_start: Option<u32>,
    ) -> Self {
        Self(format!(
            "{}:{:?}:{}:{}:{}",
            repo.as_str(),
            kind,
            path,
            name,
            line_start.unwrap_or(0),
        ))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn repo_id(&self) -> &str {
        self.0.split(':').next().unwrap_or("")
    }
    pub fn parse(s: &str) -> Result<Self, crate::error::LainError> {
        let parts: Vec<&str> = s.split(':').collect();
        // Bumped from 4 to 5 segments: format is now
        // `repo:Kind:path:name:line_start`. Legacy 4-segment ids are
        // pre-bump and rejected here so downstream code never sees them.
        if parts.len() < 5 {
            return Err(crate::error::LainError::InvalidGlobalId(s.to_string()));
        }
        Ok(Self(s.to_string()))
    }

    /// Parse out the node-type component of a global id, e.g.
    /// `Function` from `"auth-svc:Function:src/auth.rs:verify_token:0"`.
    /// `None` if the id is malformed (which `parse` would already have
    /// rejected, but the helper is independent so callers don't have
    /// to re-validate).
    pub fn node_kind_str(&self) -> Option<&str> {
        // Format: `repo:Kind:path:name:line_start`. With `NodeType`'s
        // `Debug` impl producing no colons (variants are bare
        // identifiers), the second `:` is the boundary between `Kind`
        // and `path`.
        let after_repo = self.0.split_once(':')?.1;
        let (kind, _rest) = after_repo.split_once(':')?;
        Some(kind)
    }

    /// Parse the workspace-relative file path (3rd segment) of a global
    /// id, e.g. `src/auth.rs` from
    /// `"auth-svc:Function:src/auth.rs:verify_token:42"`. Returns
    /// `None` for any id that does not have the canonical 5-segment
    /// shape; paths that themselves contain `:` are not representable
    /// and would also fail.
    pub fn path(&self) -> Option<&str> {
        let parts: Vec<&str> = self.0.split(':').collect();
        parts.get(2).copied()
    }

    /// Parse the symbol name (4th segment) of a global id, e.g.
    /// `verify_token` from
    /// `"auth-svc:Function:src/auth.rs:verify_token:42"`. Returns
    /// `None` for any id that does not have the canonical 5-segment
    /// shape.
    pub fn name(&self) -> Option<&str> {
        let parts: Vec<&str> = self.0.split(':').collect();
        parts.get(3).copied()
    }

    /// Parse the line-start (5th segment) of a global id, e.g.
    /// `Some(42)` from `"auth-svc:Function:src/auth.rs:verify_token:42"`.
    /// Returns `None` for any id that does not have the canonical
    /// 5-segment shape, or whose 5th segment is not a valid `u32`.
    pub fn line_start(&self) -> Option<u32> {
        let parts: Vec<&str> = self.0.split(':').collect();
        parts.get(4)?.parse().ok()
    }
}

impl std::fmt::Display for GlobalId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::LainError;

    #[test]
    fn repo_id_rejects_empty() {
        assert!(RepoId::new("").is_err());
    }

    #[test]
    fn repo_id_rejects_colon() {
        assert!(RepoId::new("foo:bar").is_err());
    }

    #[test]
    fn repo_id_rejects_slash() {
        assert!(RepoId::new("foo/bar").is_err());
    }

    #[test]
    fn repo_id_accepts_valid() {
        let id = RepoId::new("auth-svc").unwrap();
        assert_eq!(id.as_str(), "auth-svc");
        assert_eq!(id.to_string(), "auth-svc");
    }

    #[test]
    fn global_id_format_is_stable() {
        let repo = RepoId::new("auth-svc").unwrap();
        let id = GlobalId::new(&repo, NodeType::Function, "src/auth.rs", "verify_token", None);
        assert_eq!(id.as_str(), "auth-svc:Function:src/auth.rs:verify_token:0");
    }

    #[test]
    fn global_id_roundtrip() {
        let repo = RepoId::new("billing-svc").unwrap();
        let id = GlobalId::new(&repo, NodeType::Method, "src/invoice.py", "calc_total", Some(42));
        let parsed = GlobalId::parse(id.as_str()).unwrap();
        assert_eq!(parsed, id);
        assert_eq!(parsed.repo_id(), "billing-svc");
    }

    #[test]
    fn global_id_parse_rejects_too_few_parts() {
        assert!(GlobalId::parse("foo:bar").is_err());
    }

    #[test]
    fn global_id_new_with_line_start_round_trips() {
        let repo = RepoId::new("bytes").unwrap();
        let cases = [
            (None, 0u32),
            (Some(0), 0),
            (Some(1), 1),
            (Some(12345), 12345),
        ];
        for (input, expected_line) in cases {
            let gid = GlobalId::new(
                &repo,
                NodeType::Function,
                "src/lib.rs",
                "foo",
                input,
            );
            let parsed = GlobalId::parse(gid.as_str()).unwrap();
            // Round-trip preserves the string form; line_start is the
            // last `:`-delimited segment.
            let last = parsed.as_str().rsplit(':').next().unwrap();
            assert_eq!(last.parse::<u32>().unwrap(), expected_line);
        }
    }

    #[test]
    fn global_id_parse_rejects_pre_bump_format() {
        // Pre-bump format: 4 segments (no line_start).
        let legacy = "bytes:Function:src/lib.rs:foo";
        let err = GlobalId::parse(legacy).unwrap_err();
        assert!(matches!(err, LainError::InvalidGlobalId(_)));
    }

    #[test]
    fn global_id_accessors_extract_path_name_line_start() {
        let repo = RepoId::new("auth-svc").unwrap();
        let gid = GlobalId::new(
            &repo,
            NodeType::Function,
            "src/auth.rs",
            "verify_token",
            Some(42),
        );
        assert_eq!(gid.path(), Some("src/auth.rs"));
        assert_eq!(gid.name(), Some("verify_token"));
        assert_eq!(gid.line_start(), Some(42));

        let zero = GlobalId::new(
            &repo,
            NodeType::Module,
            "src/lib.rs",
            "root",
            None,
        );
        assert_eq!(zero.path(), Some("src/lib.rs"));
        assert_eq!(zero.name(), Some("root"));
        assert_eq!(zero.line_start(), Some(0));
    }

    #[test]
    fn global_id_accessors_handle_noncanonical_shapes() {
        let repo = RepoId::new("auth-svc").unwrap();
        let gid = GlobalId::new(
            &repo,
            NodeType::Function,
            "src/auth.rs",
            "verify_token",
            Some(42),
        );
        let parsed = GlobalId::parse(gid.as_str()).unwrap();

        // 5-segment ids parse; the accessors should agree with the
        // constructor's input.
        assert_eq!(parsed.path(), Some("src/auth.rs"));
        assert_eq!(parsed.name(), Some("verify_token"));

        // 4-segment legacy id (pre-bump) is rejected by parse, but if
        // a malformed id leaks through some other way the accessors
        // should return None rather than panic.
        let four_segment = "repo:Kind:path:name";
        let parsed_four = GlobalId::parse(four_segment).unwrap_err();
        assert!(matches!(parsed_four, LainError::InvalidGlobalId(_)));
    }
}

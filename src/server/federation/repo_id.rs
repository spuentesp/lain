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
    pub fn new(repo: &RepoId, kind: NodeType, path: &str, name: &str) -> Self {
        Self(format!("{}:{:?}:{}:{}", repo.as_str(), kind, path, name))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn repo_id(&self) -> &str {
        self.0.split(':').next().unwrap_or("")
    }
    pub fn parse(s: &str) -> Result<Self, crate::error::LainError> {
        let parts: Vec<&str> = s.split(':').collect();
        if parts.len() < 4 {
            return Err(crate::error::LainError::InvalidGlobalId(s.to_string()));
        }
        // Validate the `Kind` segment is a real NodeType. Without this
        // check, `repo:Foo:src/main.rs:hello` parses as a GlobalId that
        // no graph lookup will ever resolve — the caller sees a
        // `NotFound` that's indistinguishable from a genuinely missing
        // symbol, and debugging requires reading the helper.
        let kind_str = parts[1];
        if !Self::is_known_node_kind(kind_str) {
            return Err(crate::error::LainError::InvalidGlobalId(format!(
                "{s}: unknown node kind `{kind_str}`"
            )));
        }
        Ok(Self(s.to_string()))
    }

    /// True iff `s` is the `Debug` rendering of a known [`NodeType`]
    /// variant. `GlobalId::new` formats the kind with `{:?}`, which
    /// yields the bare variant name (`Function`, `Struct`, `Method`,
    /// …) — so a string match against the static list is sufficient
    /// and avoids dragging a `FromStr` impl into the schema.
    fn is_known_node_kind(s: &str) -> bool {
        matches!(
            s,
            "File"
                | "Namespace"
                | "Module"
                | "Package"
                | "Class"
                | "Interface"
                | "Struct"
                | "Enum"
                | "Trait"
                | "Function"
                | "Method"
                | "Property"
                | "Variable"
                | "Constant"
                | "HttpRoute"
                | "Topic"
                | "Resource"
                | "Schema"
                | "Synthetic"
        )
    }

    /// Parse out the node-type component of a global id, e.g.
    /// `Function` from `"auth-svc:Function:src/auth.rs:verify_token"`.
    /// `None` if the id is malformed (which `parse` would already have
    /// rejected, but the helper is independent so callers don't have
    /// to re-validate).
    pub fn node_kind_str(&self) -> Option<&str> {
        // Format: `repo:Kind:path:name`. With `NodeType`'s `Debug`
        // impl producing no colons (variants are bare identifiers),
        // the second `:` is the boundary between `Kind` and `path`.
        let after_repo = self.0.split_once(':')?.1;
        let (kind, _rest) = after_repo.split_once(':')?;
        Some(kind)
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
        let id = GlobalId::new(&repo, NodeType::Function, "src/auth.rs", "verify_token");
        assert_eq!(id.as_str(), "auth-svc:Function:src/auth.rs:verify_token");
    }

    #[test]
    fn global_id_roundtrip() {
        let repo = RepoId::new("billing-svc").unwrap();
        let id = GlobalId::new(&repo, NodeType::Method, "src/invoice.py", "calc_total");
        let parsed = GlobalId::parse(id.as_str()).unwrap();
        assert_eq!(parsed, id);
        assert_eq!(parsed.repo_id(), "billing-svc");
    }

    #[test]
    fn global_id_parse_rejects_too_few_parts() {
        assert!(GlobalId::parse("foo:bar").is_err());
    }
}

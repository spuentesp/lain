//! Embedding models: the declared manifest, the install that fetches them, and
//! the lookup the embedder uses at runtime.
//!
//! Models are declared in `models.toml`, compiled into the binary, and may be
//! extended or re-pointed by a user file in the config directory. Installing
//! happens only when asked (`lain models install`, or the installers), never
//! from the server. Every file is checked against its SHA-256 before it is kept
//! or loaded, and a file that fails the check is discarded.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::server::error::LainError;

const BUILTIN: &str = include_str!("models.toml");

#[derive(Debug, Clone, Deserialize)]
pub struct FileSpec {
    /// Path in the repository.
    pub remote: String,
    /// File name inside the model directory.
    pub local: String,
    pub sha256: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ModelSpec {
    pub name: String,
    pub role: String,
    pub repo: String,
    pub revision: String,
    #[serde(default)]
    pub license: String,
    pub files: Vec<FileSpec>,
}

#[derive(Debug, Clone, Deserialize)]
struct Manifest {
    default: Option<String>,
    #[serde(default, rename = "model")]
    models: Vec<ModelSpec>,
}

/// The effective manifest: the built-in one, overlaid by the user's file.
#[derive(Debug, Clone)]
pub struct Catalog {
    pub default: String,
    pub models: Vec<ModelSpec>,
}

impl Catalog {
    fn parse(text: &str) -> Result<Manifest, LainError> {
        toml::from_str(text).map_err(|e| LainError::InvalidArgument(format!("models.toml: {e}")))
    }

    /// The built-in catalog, with the user file applied when it exists.
    pub fn load() -> Result<Self, LainError> {
        let user = match user_file().filter(|p| p.exists()) {
            Some(path) => {
                Some(fs::read_to_string(&path).map_err(|e| LainError::Io(e.to_string()))?)
            }
            None => None,
        };
        Self::merged(BUILTIN, user.as_deref())
    }

    /// `builtin` with `user` applied: its models replace same-named ones or
    /// are added, and its `default` wins when it sets one.
    pub fn merged(builtin: &str, user: Option<&str>) -> Result<Self, LainError> {
        let builtin = Self::parse(builtin)?;
        let mut models = builtin.models;
        let mut default = builtin.default.unwrap_or_default();
        if let Some(text) = user {
            let user = Self::parse(text)?;
            for m in user.models {
                match models.iter().position(|x| x.name == m.name) {
                    Some(i) => models[i] = m,
                    None => models.push(m),
                }
            }
            if let Some(d) = user.default {
                default = d;
            }
        }
        Ok(Self { default, models })
    }

    pub fn get(&self, name: &str) -> Option<&ModelSpec> {
        self.models.iter().find(|m| m.name == name)
    }

    pub fn default_model(&self) -> Option<&ModelSpec> {
        self.get(&self.default)
    }
}

fn user_file() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("lain").join("models.toml"))
}

/// Where models are installed: `LAIN_MODEL_DIR` if set, else the user data
/// directory under `lain/models`.
pub fn install_root() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("LAIN_MODEL_DIR") {
        return Some(PathBuf::from(dir));
    }
    dirs::data_dir().map(|d| d.join("lain").join("models"))
}

/// The directory one model installs into under `root`.
pub fn model_dir_in(root: &Path, spec: &ModelSpec) -> PathBuf {
    root.join(format!("{}-{}", spec.name, &spec.revision[..12]))
}

/// The directory one model installs into, under the install root.
pub fn model_dir(spec: &ModelSpec) -> Option<PathBuf> {
    install_root().map(|r| model_dir_in(&r, spec))
}

fn sha256_of(path: &Path) -> Result<String, LainError> {
    let mut file = fs::File::open(path).map_err(|e| LainError::Io(e.to_string()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| LainError::Io(e.to_string()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// True when every file of `spec` is in `dir` with its pinned hash.
pub fn is_installed(spec: &ModelSpec, dir: &Path) -> bool {
    spec.files.iter().all(|f| {
        sha256_of(&dir.join(&f.local))
            .map(|h| h == f.sha256)
            .unwrap_or(false)
    })
}

fn download(url: &str, dest: &Path, sha: &str) -> Result<(), LainError> {
    let tmp = dest.with_extension("partial");
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(30))
        .timeout(std::time::Duration::from_secs(900))
        .build()
        .map_err(|e| LainError::Nlp(e.to_string()))?;
    let mut resp = client
        .get(url)
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|e| LainError::Nlp(format!("download {url}: {e}")))?;
    {
        let mut out = fs::File::create(&tmp).map_err(|e| LainError::Io(e.to_string()))?;
        std::io::copy(&mut resp, &mut out).map_err(|e| LainError::Io(e.to_string()))?;
        out.flush().map_err(|e| LainError::Io(e.to_string()))?;
    }
    let got = sha256_of(&tmp)?;
    if got != sha {
        let _ = fs::remove_file(&tmp);
        return Err(LainError::Nlp(format!(
            "checksum mismatch for {url}: expected {sha}, got {got}"
        )));
    }
    fs::rename(&tmp, dest).map_err(|e| LainError::Io(e.to_string()))
}

/// Fetch every file of `spec` that is missing or damaged, and return the
/// model directory. Files already verified are not downloaded again.
pub fn install(spec: &ModelSpec) -> Result<PathBuf, LainError> {
    let root =
        install_root().ok_or_else(|| LainError::Nlp("no data directory for models".into()))?;
    install_into(spec, &root)
}

/// [`install`] into an explicit root.
pub fn install_into(spec: &ModelSpec, root: &Path) -> Result<PathBuf, LainError> {
    let dir = model_dir_in(root, spec);
    fs::create_dir_all(&dir).map_err(|e| LainError::Io(e.to_string()))?;
    for f in &spec.files {
        let dest = dir.join(&f.local);
        if sha256_of(&dest).map(|h| h == f.sha256).unwrap_or(false) {
            continue;
        }
        let url = format!(
            "https://huggingface.co/{}/resolve/{}/{}",
            spec.repo, spec.revision, f.remote
        );
        download(&url, &dest, &f.sha256)?;
    }
    Ok(dir)
}

/// The installed default model's `(model, tokenizer)` paths, when the
/// catalog's default is installed and verified. Never downloads.
pub fn installed_default() -> Option<(PathBuf, PathBuf)> {
    installed_default_in(&install_root()?)
}

/// [`installed_default`] under an explicit root.
pub fn installed_default_in(root: &Path) -> Option<(PathBuf, PathBuf)> {
    let catalog = Catalog::load().ok()?;
    let spec = catalog.default_model()?;
    let dir = model_dir_in(root, spec);
    if !is_installed(spec, &dir) {
        return None;
    }
    Some((dir.join("model.onnx"), dir.join("tokenizer.json")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn builtin_spec() -> ModelSpec {
        Catalog::parse(BUILTIN).unwrap().models.remove(0)
    }

    #[test]
    fn the_builtin_manifest_declares_a_pinned_default() {
        let c = Catalog::parse(BUILTIN).unwrap();
        assert_eq!(
            c.default.as_deref(),
            Some("granite-embedding-small-english-r2")
        );
        let m = &c.models[0];
        assert_eq!(m.revision.len(), 40, "pinned to a full commit");
        assert!(m.files.iter().all(|f| f.sha256.len() == 64));
    }

    #[test]
    fn sha256_matches_a_known_vector() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("abc");
        fs::write(&p, b"abc").unwrap();
        assert_eq!(
            sha256_of(&p).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn a_damaged_or_missing_model_is_not_installed() {
        let spec = builtin_spec();
        let d = tempfile::tempdir().unwrap();
        assert!(!is_installed(&spec, d.path()));
        for f in &spec.files {
            fs::write(d.path().join(&f.local), b"not the model").unwrap();
        }
        assert!(!is_installed(&spec, d.path()));
    }

    #[test]
    fn a_user_file_adds_models_and_can_change_the_default() {
        let user = r#"
default = "mine"

[[model]]
name = "mine"
role = "embedding"
repo = "example/mine"
revision = "0000000000000000000000000000000000000000"
files = []
"#;
        let c = Catalog::merged(BUILTIN, Some(user)).unwrap();
        assert_eq!(c.default, "mine");
        assert!(c.get("granite-embedding-small-english-r2").is_some());
        assert!(c.get("mine").is_some());
    }

    #[test]
    fn a_user_model_with_a_builtin_name_replaces_it() {
        let user = r#"
[[model]]
name = "granite-embedding-small-english-r2"
role = "embedding"
repo = "example/other"
revision = "1111111111111111111111111111111111111111"
files = []
"#;
        let c = Catalog::merged(BUILTIN, Some(user)).unwrap();
        assert_eq!(c.models.len(), 1);
        assert_eq!(
            c.get("granite-embedding-small-english-r2").unwrap().repo,
            "example/other"
        );
    }

    #[test]
    fn an_empty_root_has_no_installed_default_and_a_damaged_install_is_not_used() {
        let d = tempfile::tempdir().unwrap();
        assert!(installed_default_in(d.path()).is_none());
        let spec = builtin_spec();
        let dir = model_dir_in(d.path(), &spec);
        fs::create_dir_all(&dir).unwrap();
        for f in &spec.files {
            fs::write(dir.join(&f.local), b"not the model").unwrap();
        }
        assert!(installed_default_in(d.path()).is_none());
    }

    /// Network install into a temporary directory. Run with
    /// `cargo test --lib installs_the_default_model -- --ignored`.
    #[test]
    #[ignore]
    fn installs_the_default_model() {
        let d = tempfile::tempdir().unwrap();
        let spec = builtin_spec();
        let dir = install_into(&spec, d.path()).unwrap();
        assert!(is_installed(&spec, &dir));
        // A second install verifies and downloads nothing new.
        assert_eq!(install_into(&spec, d.path()).unwrap(), dir);
        assert!(installed_default_in(d.path()).is_some());
    }
}

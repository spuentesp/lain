//! The default embedding model, fetched on first use.
//!
//! Semantic search needs a model on disk, and the model is too large to ship
//! inside the binary. The first time the embedder is built without an explicit
//! model, the pinned files are downloaded into a per-user cache. Every file is
//! checked against its SHA-256 before it is used; a file that fails the check
//! is never kept.
//!
//! Pins are a commit of the Hugging Face repository plus each file's hash, so
//! the same model is loaded on every machine and an upstream change cannot
//! silently alter the embeddings.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::server::error::LainError;

const REPO: &str = "onnx-community/granite-embedding-small-english-r2-ONNX";
const REVISION: &str = "1dc7835ba0cb9c76a3618d0bf0c427c97671b3c8";
const CACHE_NAME: &str = "granite-embedding-small-english-r2-1dc7835b";

/// `(path in the repository, local file name, SHA-256)`. The model's weights
/// live in `model.onnx_data`, which the graph reads from its own directory, so
/// all three must sit side by side.
const FILES: &[(&str, &str, &str)] = &[
    (
        "onnx/model.onnx",
        "model.onnx",
        "cddb145cd1147ec24a3908b2ca2602b98b20a3d198365cff270b7cb26c98179e",
    ),
    (
        "onnx/model.onnx_data",
        "model.onnx_data",
        "86a3a705d4598615894d89540ea71a3d9bbdb17a315e79edcd5dfc737222834b",
    ),
    (
        "tokenizer.json",
        "tokenizer.json",
        "feeb83348dcb033bc6b9d2e1f7906ca9eb2d122845000c9416d894d7c2927149",
    ),
];

/// Where the model is cached: `LAIN_MODEL_CACHE` if set, else the user cache
/// directory under `lain/models`.
fn cache_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("LAIN_MODEL_CACHE") {
        return Some(PathBuf::from(dir).join(CACHE_NAME));
    }
    dirs::cache_dir().map(|d| d.join("lain").join("models").join(CACHE_NAME))
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

/// True when every pinned file is present in `dir` with the pinned hash.
fn is_complete(dir: &Path) -> bool {
    FILES.iter().all(|(_, local, sha)| {
        sha256_of(&dir.join(local))
            .map(|h| h == *sha)
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

/// The directory holding the default model, downloading any missing or
/// damaged file first. Returns the model file and the tokenizer file inside it.
pub fn ensure_default_model() -> Result<(PathBuf, PathBuf), LainError> {
    let dir = cache_dir()
        .ok_or_else(|| LainError::Nlp("no user cache directory for the model".into()))?;
    fs::create_dir_all(&dir).map_err(|e| LainError::Io(e.to_string()))?;
    if !is_complete(&dir) {
        tracing::info!(
            "downloading the embedding model ({REPO} at {}) to {}",
            &REVISION[..12],
            dir.display()
        );
        for (remote, local, sha) in FILES {
            let url = format!("https://huggingface.co/{REPO}/resolve/{REVISION}/{remote}");
            if sha256_of(&dir.join(local))
                .map(|h| h == *sha)
                .unwrap_or(false)
            {
                continue;
            }
            download(&url, &dir.join(local), sha)?;
        }
    }
    Ok((dir.join("model.onnx"), dir.join("tokenizer.json")))
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn an_empty_directory_is_not_a_complete_model() {
        let d = tempfile::tempdir().unwrap();
        assert!(!is_complete(d.path()));
    }

    #[test]
    fn a_file_with_the_wrong_content_is_not_a_complete_model() {
        let d = tempfile::tempdir().unwrap();
        for (_, local, _) in FILES {
            fs::write(d.path().join(local), b"not the model").unwrap();
        }
        assert!(!is_complete(d.path()));
    }

    /// Network download of the pinned model into a temporary cache. Run with
    /// `cargo test --lib fetches_and_verifies_the_default_model -- --ignored`.
    #[test]
    #[ignore]
    fn fetches_and_verifies_the_default_model() {
        let d = tempfile::tempdir().unwrap();
        std::env::set_var("LAIN_MODEL_CACHE", d.path());
        let (model, tokenizer) = ensure_default_model().unwrap();
        assert!(model.exists() && tokenizer.exists());
        assert!(is_complete(model.parent().unwrap()));
        // A second call finds the verified files and downloads nothing.
        assert_eq!(ensure_default_model().unwrap().0, model);
    }
}

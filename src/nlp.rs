//! NLP embedder for semantic search
//!
//! Generates embeddings for code symbols using all-MiniLM-L6-v2 ONNX model

use crate::error::LainError;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use parking_lot::{Mutex, RwLock};
use tracing::{debug, info, warn};

/// NLP embedder using all-MiniLM-L6-v2 ONNX model
pub struct NlpEmbedder {
    /// ONNX session for real embeddings (None if load failed)
    /// Mutex allows mutable access across threads
    session: Option<Arc<Mutex<ort::session::Session>>>,
    /// Tokenizer for text preprocessing
    tokenizer: Option<tokenizers::tokenizer::Tokenizer>,
    /// Cache of computed embeddings
    cache: Arc<RwLock<HashMap<String, Vec<f32>>>>,
    /// Fallback to hash-based embedder if ONNX fails
    fallback: HashEmbedder,
}

impl Clone for NlpEmbedder {
    fn clone(&self) -> Self {
        Self {
            session: self.session.clone(),
            tokenizer: self.tokenizer.clone(),
            cache: Arc::clone(&self.cache),
            fallback: self.fallback.clone(),
        }
    }
}

impl NlpEmbedder {
    /// Create a new NLP embedder, loading the ONNX model and tokenizer
    pub fn new() -> Result<Self, LainError> {
        // Robust path detection:
        // 1. Check environment variable (set by our NPM shim)
        // 2. Check local "models" directory
        let models_dir = if let Ok(env_path) = std::env::var("LAIN_MODELS_PATH") {
            PathBuf::from(env_path)
        } else {
            PathBuf::from("models")
        };

        Self::new_with_path(&models_dir)
    }

    /// Create with explicit model path (for testing)
    pub fn new_with_path(models_dir: &Path) -> Result<Self, LainError> {
        info!("Initializing NLP embedder with ONNX model");

        let model_path = models_dir.join("all-MiniLM-L6-v2.onnx");
        let tokenizer_path = models_dir.join("tokenizer.json");

        match Self::try_load(&model_path, &tokenizer_path) {
            Ok((session, tokenizer)) => {
                info!("ONNX model and tokenizer loaded successfully");
                Ok(Self {
                    session: Some(Arc::new(Mutex::new(session))),
                    tokenizer: Some(tokenizer),
                    cache: Arc::new(RwLock::new(HashMap::new())),
                    fallback: HashEmbedder::new(),
                })
            }
            Err(e) => {
                warn!("Failed to load ONNX model/tokenizer: {}, using hash fallback", e);
                Ok(Self {
                    session: None,
                    tokenizer: None,
                    cache: Arc::new(RwLock::new(HashMap::new())),
                    fallback: HashEmbedder::new(),
                })
            }
        }
    }

    fn try_load(
        model_path: &Path,
        tokenizer_path: &Path,
    ) -> Result<(ort::session::Session, tokenizers::tokenizer::Tokenizer), LainError> {
        // Load tokenizer
        let tokenizer = tokenizers::tokenizer::Tokenizer::from_file(tokenizer_path)
            .map_err(|e| LainError::Nlp(format!("Failed to load tokenizer: {}", e)))?;

        // Load ONNX model
        let session = ort::session::Session::builder()
            .map_err(|e| LainError::Nlp(format!("Failed to build session: {}", e)))?
            .commit_from_file(model_path)
            .map_err(|e| LainError::Nlp(format!("Failed to load model: {}", e)))?;

        Ok((session, tokenizer))
    }

    /// Generate an embedding for a text string
    pub fn embed(&self, text: &str) -> Result<Vec<f32>, LainError> {
        // Check cache first
        {
            let cache = self.cache.read();
            if let Some(cached) = cache.get(text) {
                return Ok(cached.clone());
            }
        }

        // Try ONNX inference; fall back to hash on failure
        let embedding = if let (Some(ref session), Some(ref tokenizer)) = (&self.session, &self.tokenizer) {
            let mut session_guard = session.lock();
            match self.embed_onnx(text, &mut session_guard, tokenizer) {
                Ok(emb) => emb,
                Err(e) => {
                    debug!("ONNX embedding failed: {}, falling back to hash", e);
                    self.fallback.embed(text)
                }
            }
        } else {
            self.fallback.embed(text)
        };

        // Cache result
        {
            let mut cache = self.cache.write();
            cache.insert(text.to_string(), embedding.clone());
        }

        Ok(embedding)
    }

    fn embed_onnx(
        &self,
        text: &str,
        session: &mut ort::session::Session,
        tokenizer: &tokenizers::tokenizer::Tokenizer,
    ) -> Result<Vec<f32>, LainError> {
        // Encode text with tokenizer
        let encoding = tokenizer.encode(text, false)
            .map_err(|e| LainError::Nlp(format!("Tokenization failed: {}", e)))?;

        let ids = encoding.get_ids();
        let mask = encoding.get_attention_mask();

        let seq_len = ids.len();
        let batch_size = 1;

        // Create input_ids tensor [batch, seq_len]
        let input_ids: ndarray::Array2<i64> = ndarray::Array2::from_shape_vec(
            (batch_size, seq_len),
            ids.iter().map(|&id| id as i64).collect(),
        ).map_err(|e| LainError::Nlp(format!("Array shape error: {}", e)))?;

        // Create attention_mask tensor [batch, seq_len]
        let attention_mask: ndarray::Array2<i64> = ndarray::Array2::from_shape_vec(
            (batch_size, seq_len),
            mask.iter().map(|&m| m as i64).collect(),
        ).map_err(|e| LainError::Nlp(format!("Array shape error: {}", e)))?;

        // Create ort values from arrays
        let input_ids_value = ort::value::Tensor::from_array(input_ids.into_dyn())
            .map_err(|e| LainError::Nlp(format!("Tensor creation error: {}", e)))?;
        let attention_mask_value = ort::value::Tensor::from_array(attention_mask.into_dyn())
            .map_err(|e| LainError::Nlp(format!("Tensor creation error: {}", e)))?;

        // Run inference
        let outputs = session
            .run(ort::inputs![
                "input_ids" => input_ids_value,
                "attention_mask" => attention_mask_value,
            ])
            .map_err(|e| LainError::Nlp(format!("Inference failed: {}", e)))?;

        // Get last_hidden_state output [batch, seq_len, hidden]
        let (shape, data) = outputs[0].try_extract_tensor::<f32>()
            .map_err(|e| LainError::Nlp(format!("Expected tensor output: {}", e)))?;

        // Reshape to [batch, seq_len, hidden]
        // shape elements are i64, convert to usize
        let b = shape[0] as usize;
        let s = shape[1] as usize;
        let h = shape[2] as usize;

        if shape.len() != 3 || b != 1 || h != 384 {
            return Err(LainError::Nlp(format!(
                "Unexpected output shape: {:?}, expected [1, {}, 384]", shape, seq_len
            )));
        }

        // Copy data into 3D array view
        let last_hidden = ndarray::Array3::<f32>::from_shape_vec((b, s, h), data.to_vec())
            .map_err(|e| LainError::Nlp(format!("Tensor reshape error: {}", e)))?;

        // Mean pooling over sequence dimension (ignore padding)
        let hidden_size = h;

        let mask_arr: ndarray::Array2<f32> = ndarray::Array2::from_shape_vec(
            (b, s),
            mask.iter().map(|&m| m as f32).collect(),
        ).map_err(|e| LainError::Nlp(format!("Mask array error: {}", e)))?;

        let mut embedding = vec![0.0f32; hidden_size];
        for i in 0..hidden_size {
            let mut sum = 0.0f32;
            let mut count = 0.0f32;
            for j in 0..s {
                if mask_arr[[0, j]] > 0.0 {
                    sum += last_hidden[[0, j, i]];
                    count += 1.0;
                }
            }
            embedding[i] = if count > 0.0 { sum / count } else { 0.0 };
        }

        debug!("Generated ONNX embedding for: {}", text);
        Ok(embedding)
    }

    /// Compute semantic similarity between two texts
    pub fn similarity(&self, text1: &str, text2: &str) -> Result<f32, LainError> {
        let emb1 = self.embed(text1)?;
        let emb2 = self.embed(text2)?;

        // Cosine similarity
        let dot_product: f32 = emb1.iter().zip(&emb2).map(|(a, b)| a * b).sum();
        let norm1: f32 = emb1.iter().map(|a| a * a).sum::<f32>().sqrt();
        let norm2: f32 = emb2.iter().map(|b| b * b).sum::<f32>().sqrt();

        if norm1 == 0.0 || norm2 == 0.0 {
            Ok(0.0)
        } else {
            Ok(dot_product / (norm1 * norm2))
        }
    }

    /// Find semantically similar texts from a list
    pub fn find_similar(&self, query: &str, candidates: &[&str], threshold: f32) -> Result<Vec<String>, LainError> {
        let mut results = Vec::new();

        for candidate in candidates {
            let sim = self.similarity(query, candidate)?;
            if sim >= threshold {
                results.push((candidate.to_string(), sim));
            }
        }

        // Sort by similarity descending
        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        Ok(results.into_iter().map(|(text, _)| text).collect())
    }
}

/// Hash-based fallback embedder when ONNX is unavailable
#[derive(Clone)]
pub struct HashEmbedder {
    dim: usize,
}

impl HashEmbedder {
    pub fn new() -> Self {
        Self { dim: 384 }
    }

    pub fn embed(&self, text: &str) -> Vec<f32> {
        let mut embedding = vec![0.0f32; self.dim];
        let hash = text.len() as u32;
        for (i, chunk) in text.as_bytes().chunks(4).enumerate() {
            let val = chunk.iter().fold(0u32, |acc, &b| acc.wrapping_mul(31).wrapping_add(b as u32));
            if i < embedding.len() {
                embedding[i] = ((val ^ hash) % 1000) as f32 / 500.0 - 1.0;
            }
        }
        embedding
    }
}

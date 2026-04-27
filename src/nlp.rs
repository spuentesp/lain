//! High-performance NLP embedding using ONNX Runtime
//!
//! Supports any ONNX model that produces fixed-dimension sentence embeddings.
//! Tested with sentence-transformers (all-MiniLM-L6-v2, paraphrase-multilingual, etc.)

use crate::error::LainError;
use ort::session::Session;
use ort::value::Tensor;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use parking_lot::Mutex;
use tokenizers::Tokenizer;

#[derive(Clone)]
enum EmbedInner {
    Onnx {
        session: Arc<Mutex<Session>>,
        tokenizer: Arc<Tokenizer>,
        embedding_dim: usize,
    },
    Stub { embedding_dim: usize },
}

#[derive(Clone)]
pub struct NlpEmbedder {
    inner: EmbedInner,
}

impl NlpEmbedder {
    /// Initialize with default paths (models/all-MiniLM-L6-v2.onnx)
    pub fn new() -> Result<Self, LainError> {
        // Check env var first, then fall back to relative path
        let (model_path, tokenizer_path) = if let Some(model_env) = std::env::var_os("LAIN_EMBEDDING_MODEL") {
            let model_path = Path::new(&model_env).to_path_buf();
            let tokenizer_path = model_path.parent()
                .map(|p| p.join("tokenizer.json"))
                .unwrap_or_else(|| PathBuf::from("tokenizer.json"));
            (model_path, tokenizer_path)
        } else {
            (Path::new("models/all-MiniLM-L6-v2.onnx").to_path_buf(),
             Path::new("models/tokenizer.json").to_path_buf())
        };

        if !model_path.exists() || !tokenizer_path.exists() {
            tracing::warn!("NLP model files not found at {:?}, using stub embedder", model_path);
            return Ok(Self::new_stub());
        }

        // Initialize ORT global logging once
        if !ort::init()
            .with_name("lain-nlp")
            .with_execution_providers([ort::execution_providers::CPUExecutionProvider::default().build()])
            .commit()
        {
            tracing::warn!("ORT initialization returned false - may indicate already initialized");
        }

        Self::new_with_paths(&model_path, &tokenizer_path)
    }

    pub fn new_with_paths(model_path: &Path, tokenizer_path: &Path) -> Result<Self, LainError> {
        if !model_path.exists() {
            return Err(LainError::Nlp(format!("Model file not found: {:?}", model_path)));
        }
        if !tokenizer_path.exists() {
            return Err(LainError::Nlp(format!("Tokenizer file not found: {:?}", tokenizer_path)));
        }

        let tokenizer = Tokenizer::from_file(tokenizer_path)
            .map_err(|e| LainError::Nlp(format!("Failed to load tokenizer: {}", e)))?;

        // Infer embedding dimension from model output shape via dummy inference on first session
        let mut session = Session::builder()?
            .with_intra_threads(1)?
            .commit_from_file(model_path)?;
        let embedding_dim = Self::detect_embedding_dim(&mut session)?;

        Ok(Self {
            inner: EmbedInner::Onnx {
                session: Arc::new(Mutex::new(session)),
                tokenizer: Arc::new(tokenizer),
                embedding_dim,
            },
        })
    }

    /// Detect embedding dimension by running a dummy inference
    fn detect_embedding_dim(session: &mut Session) -> Result<usize, LainError> {
        let dummy_ids = vec![1_i64, 2_i64, 3_i64];
        let dummy_mask = vec![1_i64, 1_i64, 1_i64];
        let dummy_types = vec![0_i64, 0_i64, 0_i64];

        let ids_tensor = Tensor::from_array(([1, 3], dummy_ids)).map_err(|e| LainError::Nlp(e.to_string()))?;
        let mask_tensor = Tensor::from_array(([1, 3], dummy_mask)).map_err(|e| LainError::Nlp(e.to_string()))?;
        let type_tensor = Tensor::from_array(([1, 3], dummy_types)).map_err(|e| LainError::Nlp(e.to_string()))?;

        let inputs = ort::inputs![
            "input_ids" => ids_tensor,
            "attention_mask" => mask_tensor,
            "token_type_ids" => type_tensor,
        ];

        let outputs = session.run(inputs).map_err(|e| LainError::Nlp(e.to_string()))?;
        let last_hidden_state = outputs["last_hidden_state"].try_extract_tensor::<f32>()
            .map_err(|e| LainError::Nlp(e.to_string()))?;

        let shape = last_hidden_state.0;
        Ok(shape.get(2).copied().unwrap_or(384) as usize)
    }

    #[doc(hidden)]
    pub fn new_stub() -> Self {
        Self { inner: EmbedInner::Stub { embedding_dim: 384 } }
    }

    /// Returns true if this embedder is a stub (no actual model loaded)
    pub fn is_stub(&self) -> bool {
        matches!(self.inner, EmbedInner::Stub { .. })
    }

    /// Returns the embedding dimension this model produces
    pub fn embedding_dim(&self) -> usize {
        match &self.inner {
            EmbedInner::Stub { embedding_dim } => *embedding_dim,
            EmbedInner::Onnx { embedding_dim, .. } => *embedding_dim,
        }
    }

    /// Generate a fixed-dimension embedding vector for the given text
    pub fn embed(&self, text: &str) -> Result<Vec<f32>, LainError> {
        let (session, tokenizer, embedding_dim) = match &self.inner {
            EmbedInner::Stub { embedding_dim } => return Ok(vec![0.0f32; *embedding_dim]),
            EmbedInner::Onnx { session, tokenizer, embedding_dim } => (session, tokenizer, *embedding_dim),
        };

        let encoding = tokenizer.encode(text, true)
            .map_err(|e| LainError::Nlp(format!("Tokenization error: {}", e)))?;

        let input_ids: Vec<i64> = encoding.get_ids().iter().map(|&x| x as i64).collect();
        let attention_mask: Vec<i64> = encoding.get_attention_mask().iter().map(|&x| x as i64).collect();
        let token_type_ids: Vec<i64> = encoding.get_type_ids().iter().map(|&x| x as i64).collect();
        let seq_len = input_ids.len();

        let ids_tensor = Tensor::from_array(([1, seq_len], input_ids))?;
        let mask_tensor = Tensor::from_array(([1, seq_len], attention_mask))?;
        let type_tensor = Tensor::from_array(([1, seq_len], token_type_ids))?;

        let inputs = ort::inputs![
            "input_ids" => ids_tensor,
            "attention_mask" => mask_tensor,
            "token_type_ids" => type_tensor,
        ];

        let mut session = session.lock();
        let outputs = session.run(inputs)?;

        let last_hidden_state = outputs["last_hidden_state"]
            .try_extract_tensor::<f32>()?;

        let shape = last_hidden_state.0;
        let data = last_hidden_state.1;

        let seq_len = shape[1] as usize;
        let hidden_dim = shape[2] as usize;

        let mut embedding = vec![0.0f32; embedding_dim];
        let mut count = 0;

        for i in 0..seq_len {
            if i < encoding.get_attention_mask().len() && encoding.get_attention_mask()[i] > 0 {
                let row_start = i * hidden_dim;
                for (j, val) in data.iter().skip(row_start).take(hidden_dim.min(embedding_dim)).enumerate() {
                    embedding[j] += val;
                }
                count += 1;
            }
        }

        if count > 0 {
            for elem in embedding.iter_mut() {
                *elem /= count as f32;
            }
        }

        let norm = embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in embedding.iter_mut() {
                *x /= norm;
            }
        }

        Ok(embedding)
    }
}

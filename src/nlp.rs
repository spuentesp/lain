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
use tokenizers::{Encoding, Tokenizer};

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
    /// Initialize with default paths (models/all-MiniLM-L6-v2.onnx).
    /// Reads `LAIN_EMBEDDING_MODEL` env var if set, else relative path.
    /// `max_threads` follows the same 0 = auto convention as with_max_threads.
    pub fn new() -> Result<Self, LainError> {
        Self::new_with_threads(0)
    }

    /// Like `new()` but with explicit intra-op thread cap (0 = auto).
    pub fn new_with_threads(max_threads: usize) -> Result<Self, LainError> {
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

        Self::with_max_threads(&model_path, &tokenizer_path, max_threads)
    }

    pub fn new_with_paths(model_path: &Path, tokenizer_path: &Path) -> Result<Self, LainError> {
        Self::with_max_threads(model_path, tokenizer_path, 0)
    }

    /// Like new_with_paths but lets the caller cap intra-op threads.
    /// max_threads = 0 means auto-detect: min(system cores, 4).
    pub fn with_max_threads(
        model_path: &Path,
        tokenizer_path: &Path,
        max_threads: usize,
    ) -> Result<Self, LainError> {
        if !model_path.exists() {
            return Err(LainError::Nlp(format!("Model file not found: {:?}", model_path)));
        }
        if !tokenizer_path.exists() {
            return Err(LainError::Nlp(format!("Tokenizer file not found: {:?}", tokenizer_path)));
        }

        let tokenizer = Tokenizer::from_file(tokenizer_path)
            .map_err(|e| LainError::Nlp(format!("Failed to load tokenizer: {}", e)))?;

        let threads = resolve_intra_threads(max_threads);
        tracing::info!("NLP embedder: using {} intra-op thread(s) per call", threads);
        let mut session = Session::builder()?
            .with_intra_threads(threads)?
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
        let mut results = self.embed_batch(&[text])?;
        Ok(results.remove(0))
    }

    /// Embed a batch of texts in a single ONNX forward pass. Returns
    /// one embedding per input text, in the same order.
    ///
    /// Why batch: 200 sequential `embed()` calls on a 3850-node corpus
    /// took ~5 s. With `batch=16` and a single forward pass per batch,
    /// the same 200 nodes finish in ~600-800 ms — a 6-8x speedup. The
    /// per-token matmul cost dominates, so amortizing across 16 inputs
    /// nearly eliminates per-call overhead.
    ///
    /// Each text is independently tokenized and right-truncated to
    /// `max_len` (512 for bge, 256 for MiniLM). The batch is then
    /// right-padded to the longest input in the batch with the [PAD]
    /// token (id 0); attention_mask=0 marks those positions so
    /// mean-pool skips them.
    pub fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>, LainError> {
        let n = texts.len();
        if n == 0 {
            return Ok(Vec::new());
        }
        let (session, tokenizer, embedding_dim) = match &self.inner {
            EmbedInner::Stub { embedding_dim } => {
                return Ok((0..n).map(|_| vec![0.0f32; *embedding_dim]).collect());
            }
            EmbedInner::Onnx { session, tokenizer, embedding_dim } => {
                (session, tokenizer, *embedding_dim)
            }
        };

        let max_len: usize = 512;
        let pad_id: i64 = tokenizer
            .token_to_id("[PAD]")
            .map(|v| v as i64)
            .unwrap_or(0);

        // 1. Tokenize each text independently (cheap; no model call yet).
        //    Truncate each to max_len tokens. If the source string is
        //    obviously huge, char-truncate first to avoid tokenizing a
        //    string we'd just throw away.
        let mut per_text_lens: Vec<usize> = Vec::with_capacity(n);
        let mut all_ids: Vec<Vec<i64>> = Vec::with_capacity(n);
        let mut all_masks: Vec<Vec<i64>> = Vec::with_capacity(n);
        let mut all_types: Vec<Vec<i64>> = Vec::with_capacity(n);

        for raw in texts {
            let encoded = if raw.len() > max_len * 6 {
                let truncated: String = raw.chars().take(max_len * 6).collect();
                tokenizer
                    .encode(truncated, true)
                    .map_err(|e| LainError::Nlp(format!("Tokenization error: {}", e)))?
            } else {
                tokenizer
                    .encode(*raw, true)
                    .map_err(|e| LainError::Nlp(format!("Tokenization error: {}", e)))?
            };
            let mut encoded = encoded;
            if encoded.get_ids().len() > max_len {
                encoded.truncate(max_len, 0, tokenizers::TruncationDirection::Right);
            }
            per_text_lens.push(encoded.get_ids().len());
            all_ids.push(encoded.get_ids().iter().map(|&x| x as i64).collect());
            all_masks.push(encoded.get_attention_mask().iter().map(|&x| x as i64).collect());
            all_types.push(encoded.get_type_ids().iter().map(|&x| x as i64).collect());
        }

        // 2. Right-pad every input to the longest one in the batch.
        let batch_seq_len = *per_text_lens.iter().max().unwrap_or(&1);
        let mut ids_flat = Vec::with_capacity(n * batch_seq_len);
        let mut masks_flat = Vec::with_capacity(n * batch_seq_len);
        let mut types_flat = Vec::with_capacity(n * batch_seq_len);
        for i in 0..n {
            let len = per_text_lens[i];
            ids_flat.extend_from_slice(&all_ids[i]);
            ids_flat.extend(std::iter::repeat(pad_id).take(batch_seq_len - len));
            masks_flat.extend_from_slice(&all_masks[i]);
            masks_flat.extend(std::iter::repeat(0_i64).take(batch_seq_len - len));
            types_flat.extend_from_slice(&all_types[i]);
            types_flat.extend(std::iter::repeat(0_i64).take(batch_seq_len - len));
        }

        // 3. Build [batch, seq_len] tensors and run ONNX once.
        let ids_tensor = Tensor::from_array(([n, batch_seq_len], ids_flat))?;
        let mask_tensor = Tensor::from_array(([n, batch_seq_len], masks_flat))?;
        let type_tensor = Tensor::from_array(([n, batch_seq_len], types_flat))?;

        let inputs = ort::inputs![
            "input_ids" => ids_tensor,
            "attention_mask" => mask_tensor,
            "token_type_ids" => type_tensor,
        ];

        let mut session = session.lock();
        let outputs = session.run(inputs)?;

        let last_hidden_state = outputs["last_hidden_state"].try_extract_tensor::<f32>()?;
        let shape = last_hidden_state.0;
        let data = last_hidden_state.1;
        let hidden_dim = shape[2] as usize;

        // 4. Per-batch-item: mean-pool non-padded positions, L2-normalize.
        // Use the model's actual output seq_len (`shape[1]`) as the bound,
        // not `batch_seq_len` from the input. They should match, but in
        // edge cases the model can return a slightly different length
        // (e.g. some BPE models strip a trailing token). Using the model's
        // length avoids OOB panics.
        let out_seq_len = shape[1] as usize;
        let mut out = Vec::with_capacity(n);
        for b in 0..n {
            let mut emb = vec![0.0f32; embedding_dim];
            let mut count = 0usize;
            for i in 0..out_seq_len {
                // attention_mask==0 at this position means [PAD] — skip.
                // all_masks[b] may be shorter than out_seq_len for short
                // inputs; in that case treat as padded.
                let is_padded = i >= all_masks[b].len() || all_masks[b][i] == 0;
                if is_padded {
                    continue;
                }
                let row_start = (b * out_seq_len + i) * hidden_dim;
                for (j, val) in data
                    .iter()
                    .skip(row_start)
                    .take(hidden_dim.min(embedding_dim))
                    .enumerate()
                {
                    emb[j] += val;
                }
                count += 1;
            }
            if count > 0 {
                for elem in emb.iter_mut() {
                    *elem /= count as f32;
                }
            }
            let norm = emb.iter().map(|x| x * x).sum::<f32>().sqrt();
            if norm > 0.0 {
                for x in emb.iter_mut() {
                    *x /= norm;
                }
            }
            out.push(emb);
        }
        Ok(out)
    }
}

// ─── Cross-encoder reranker ─────────────────────────────────────────────
//
// Cross-encoders encode the (query, document) pair jointly and produce a
// relevance logit, which is much more accurate than a bi-encoder's cosine
// similarity but ~100x slower. We use them as a second-pass reranker on
// the top-K candidates from the bi-encoder — typically K=20, so the
// per-query cost stays around ~50ms.

#[derive(Clone)]
pub struct CrossEncoder {
    inner: Option<CrossInner>,
}

#[derive(Clone)]
struct CrossInner {
    session: Arc<Mutex<Session>>,
    tokenizer: Arc<Tokenizer>,
}

impl CrossEncoder {
    /// Load from model.onnx + tokenizer.json in `dir` with auto-detected
    /// thread count. Use from_dir_with_threads to override.
    pub fn from_dir(dir: &Path) -> Self {
        Self::from_dir_with_threads(dir, 0)
    }

    /// Load from model.onnx + tokenizer.json in `dir`.
    /// Returns a stub (no-op scorer returning 0.0) if the model files
    /// aren't found — search.rs treats a stub as "skip reranking".
    /// `max_threads` follows the same convention as NlpEmbedder: 0 = auto.
    pub fn from_dir_with_threads(dir: &Path, max_threads: usize) -> Self {
        let model_path = dir.join("model.onnx");
        let tok_path = dir.join("tokenizer.json");
        if !model_path.exists() || !tok_path.exists() {
            tracing::warn!(
                "Cross-encoder model not found at {:?}, reranking disabled",
                dir
            );
            return Self { inner: None };
        }

        let tokenizer = match Tokenizer::from_file(&tok_path) {
            Ok(t) => Arc::new(t),
            Err(e) => {
                tracing::warn!("Failed to load cross-encoder tokenizer: {}", e);
                return Self { inner: None };
            }
        };

        let threads = resolve_intra_threads(max_threads);
        tracing::info!("Cross-encoder: using {} intra-op thread(s) per call", threads);
        let session = match Session::builder() {
            Ok(b) => match b.with_intra_threads(threads) {
                Ok(mut b) => match b.commit_from_file(&model_path) {
                    Ok(s) => Arc::new(Mutex::new(s)),
                    Err(e) => {
                        tracing::warn!("Failed to commit cross-encoder ONNX model: {}", e);
                        return Self { inner: None };
                    }
                },
                Err(e) => {
                    tracing::warn!("Failed to configure cross-encoder threads: {}", e);
                    return Self { inner: None };
                }
            },
            Err(e) => {
                tracing::warn!("Failed to create cross-encoder session builder: {}", e);
                return Self { inner: None };
            }
        };

        // Same global ORT init as the embedder; second call is a no-op.
        if !ort::init()
            .with_name("lain-cross-encoder")
            .with_execution_providers([
                ort::execution_providers::CPUExecutionProvider::default().build(),
            ])
            .commit()
        {
            tracing::debug!("ORT init returned false for cross-encoder (may be already initialized)");
        }

        tracing::info!("Cross-encoder reranker loaded from {:?}", dir);
        Self {
            inner: Some(CrossInner { session, tokenizer }),
        }
    }

    /// True iff this is a real model (vs. a no-op stub).
    pub fn is_active(&self) -> bool {
        self.inner.is_some()
    }

    /// Score a single (query, document) pair. Returns 0.0 if no model
    /// is loaded.
    pub fn score(&self, query: &str, document: &str) -> Result<f32, LainError> {
        let inner = match &self.inner {
            Some(i) => i,
            None => return Ok(0.0),
        };

        // Tokenize the (query, document) pair jointly. The tokenizer
        // returns type IDs that mark which tokens belong to query vs
        // document — the model uses these as segment embeddings.
        let encoding: Encoding = inner
            .tokenizer
            .encode((query, document), true)
            .map_err(|e| LainError::Nlp(format!("Cross-encoder tokenization: {}", e)))?;

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

        let mut session = inner.session.lock();
        let outputs = session.run(inputs)?;

        // ms-marco-MiniLM-L-6-v2 outputs shape [1, 1] — a single logit.
        let logits = outputs["logits"]
            .try_extract_tensor::<f32>()
            .map_err(|e| LainError::Nlp(format!("Cross-encoder output: {}", e)))?;
        let data = logits.1;
        Ok(data.first().copied().unwrap_or(0.0))
    }
}

/// Resolve the intra-op thread count for an ONNX session.
///
/// `max_threads` of 0 means "auto": use min(system cores, 4). The cap
/// at 4 is intentional — bge-small/bge-base inference doesn't benefit
/// from more than 4 threads per call (matmul/attention are
/// already-parallelized internally up to a point), and asking for
/// more just causes thread contention.
///
/// Non-zero `max_threads` is honored as-is (subject to system
/// availability), letting ops cap usage when sharing the box.
pub fn resolve_intra_threads(max_threads: usize) -> usize {
    if max_threads == 0 {
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        cores.min(4).max(1)
    } else {
        max_threads.max(1)
    }
}

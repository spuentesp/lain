//! Real ONNX inference without an external model or shared environment changes.
use lain::server::nlp::{CrossEncoder, NlpEmbedder};

#[test]
fn embedding_and_reranking_use_real_sessions_in_one_process() {
    if std::env::var_os("LAIN_NLP_RUNTIME_CHILD").is_some() {
        let dir = std::path::PathBuf::from(std::env::var_os("LAIN_NLP_RUNTIME_CHILD").unwrap());
        let embedder = NlpEmbedder::new_with_threads(1).unwrap();
        assert!(!embedder.is_stub(), "a valid model must load");
        assert_eq!(embedder.embedding_dim(), 2);
        let embeddings = embedder.embed_batch(&["hello", "world"]).unwrap();
        assert_eq!(embeddings.len(), 2);
        assert!(embeddings.iter().all(|vector| vector.len() == 2));
        assert_ne!(
            embeddings[0], embeddings[1],
            "different tokens produce different vectors"
        );
        assert!(embeddings.iter().flatten().any(|value| *value != 0.0));
        let reranker = CrossEncoder::from_dir_with_threads(&dir, 1);
        assert!(
            reranker.score("hello", "world").unwrap() > 0.0,
            "the reranker must share the environment and perform inference"
        );
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("model.onnx"),
        include_bytes!("fixtures/nlp/runtime.onnx"),
    )
    .unwrap();
    let vocab = [
        ("[UNK]".into(), 0),
        ("hello".into(), 1),
        ("world".into(), 2),
    ]
    .into_iter()
    .collect();
    let model = tokenizers::models::wordlevel::WordLevel::builder()
        .vocab(vocab)
        .unk_token("[UNK]".into())
        .build()
        .unwrap();
    let mut tokenizer = tokenizers::Tokenizer::new(model);
    tokenizer.with_pre_tokenizer(Some(tokenizers::pre_tokenizers::whitespace::Whitespace));
    tokenizer
        .save(dir.path().join("tokenizer.json"), false)
        .unwrap();
    // Only the child sees this model selection; parallel tests keep their
    // ambient environment and installed-model catalog untouched.
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "embedding_and_reranking_use_real_sessions_in_one_process",
            "--nocapture",
        ])
        .env("LAIN_NLP_RUNTIME_CHILD", dir.path())
        .env("LAIN_EMBEDDING_MODEL", dir.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

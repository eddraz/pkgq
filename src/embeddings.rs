//! Pure-Rust embedding engine: bge-m3 (XLM-RoBERTa) via candle.
//!
//! Weights are loaded from Hugging Face safetensors; tokenization uses the
//! Hugging Face `tokenizers` crate.  The model runs entirely in-process on
//! the CPU: CLS-token pooling followed by L2 normalization, 1024 dimensions.

use std::path::PathBuf;

use candle_core::{DType, Device, IndexOp, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::xlm_roberta::Config as XlmRobertaConfig;
use candle_transformers::models::xlm_roberta::XLMRobertaModel;
use hf_hub::api::sync::ApiBuilder;
use tokenizers::tokenizer::{PaddingParams, TruncationParams, Tokenizer};

/// Stable identifier for this embedding backend.  Written into the semantic
/// index so that old indexes (e.g. llama-server indexes without this field)
/// are treated as stale and rebuilt by `pkgq index`.
pub const ENGINE_ID: &str = "candle-bge-m3-v1";

/// Default Hugging Face repo id for the bge-m3 weights and tokenizer.
pub const DEFAULT_MODEL_REPO: &str = "BAAI/bge-m3";

/// Environment variable that overrides [`DEFAULT_MODEL_REPO`].
pub const MODEL_REPO_ENV: &str = "PKGQ_EMBED_MODEL_REPO";

/// Maximum tokens per input (XLM-R can support longer, but package
/// descriptions are short and a 512 cap keeps memory bounded).
pub const MAX_TOKENS: usize = 512;

/// Batch size for a single forward pass.  Padded attention masks let the
/// model process a whole chunk at once; this is the same call signature the
/// XLM-RoBERTa `forward` already accepts.
pub const BATCH_SIZE: usize = 8;

/// Return the configured model repo id.
pub fn model_repo() -> String {
    std::env::var(MODEL_REPO_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(|value| value.trim().to_string())
        .unwrap_or_else(|| DEFAULT_MODEL_REPO.to_string())
}

/// Compute the Hugging Face Hub cache directory, honoring the same
/// environment variables as `hf-hub` plus `HF_HUB_CACHE`.
///
/// Priority:
/// 1. `HF_HUB_CACHE` (used verbatim)
/// 2. `HF_HOME/hub`
/// 3. `~/.cache/huggingface/hub`
pub fn hf_cache_dir() -> PathBuf {
    if let Ok(value) = std::env::var("HF_HUB_CACHE") {
        if !value.is_empty() {
            return PathBuf::from(value);
        }
    }
    if let Ok(value) = std::env::var("HF_HOME") {
        if !value.is_empty() {
            return PathBuf::from(value).join("hub");
        }
    }
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".to_string()))
        .join(".cache")
        .join("huggingface")
        .join("hub")
}

/// Escape a repo id into the directory name used by the HF cache.
/// `BAAI/bge-m3` becomes `models--BAAI--bge-m3`.
pub fn repo_cache_name(repo_id: &str) -> String {
    format!("models--{}", repo_id.replace('/', "--"))
}

/// Directory where a given repo is cached.
pub fn repo_cache_dir(repo_id: &str) -> PathBuf {
    hf_cache_dir().join(repo_cache_name(repo_id))
}

/// Resolve paths to the bge-m3 weights and tokenizer from the HF cache.
/// Returns `None` when either file is missing.
pub fn cached_model_files() -> Option<(PathBuf, PathBuf)> {
    let weights = find_cached_file(&model_repo(), "model.safetensors")?;
    let tokenizer = find_cached_file(&model_repo(), "tokenizer.json")?;
    Some((weights, tokenizer))
}

/// Look for `filename` anywhere under the repo's snapshot directories.
fn find_cached_file(repo_id: &str, filename: &str) -> Option<PathBuf> {
    let repo_dir = repo_cache_dir(repo_id);
    let snapshots = repo_dir.join("snapshots");
    let entries = std::fs::read_dir(&snapshots).ok()?;
    for entry in entries.flatten() {
        let candidate = entry.path().join(filename);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Hard-coded bge-m3 / XLM-RoBERTa configuration.  bge-m3 is distributed
/// with a `config.json` that matches these values; keeping them inline avoids
/// an extra required file at runtime.
fn xlm_roberta_config() -> XlmRobertaConfig {
    XlmRobertaConfig {
        hidden_size: 1024,
        layer_norm_eps: 1e-5,
        attention_probs_dropout_prob: 0.1,
        hidden_dropout_prob: 0.1,
        num_attention_heads: 16,
        position_embedding_type: "absolute".to_string(),
        intermediate_size: 4096,
        hidden_act: candle_nn::Activation::Gelu,
        num_hidden_layers: 24,
        vocab_size: 250_002,
        max_position_embeddings: 8194,
        type_vocab_size: 1,
        pad_token_id: 1,
    }
}

/// Embed a slice of texts using the native candle bge-m3 engine.
///
/// Returns one L2-normalized 1024-dimensional vector per input text, or a
/// clear error string.  This function never downloads inside unit tests; any
/// test that needs the real model must be `#[ignore]`-gated.
pub fn embed_texts(texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
    if texts.is_empty() {
        return Ok(Vec::new());
    }

    let (weights_path, tokenizer_path) = cached_model_files().ok_or_else(|| {
        "bge-m3 weights not found in HF cache (run once with network, or unset PKGQ_NO_BOOTSTRAP)"
            .to_string()
    })?;

    let device = Device::Cpu;
    let vb = unsafe {
        // Safety: inherited from `memmap2`.  The weights file is read-only
        // and remains unchanged for the lifetime of the process.
        VarBuilder::from_mmaped_safetensors(&[&weights_path], DType::F32, &device)
    }
    .map_err(|e| format!("failed to load safetensors: {e}"))?;

    let config = xlm_roberta_config();
    let model = XLMRobertaModel::new(&config, vb)
        .map_err(|e| format!("failed to build XLM-RoBERTa model: {e}"))?;

    let tokenizer = Tokenizer::from_file(&tokenizer_path)
        .map_err(|e| format!("failed to load tokenizer: {e}"))?;

    // Truncate to MAX_TOKENS and pad each batch to its longest sequence.
    let pad_id = tokenizer
        .token_to_id("<pad>")
        .unwrap_or(config.pad_token_id);
    let mut tokenizer = tokenizer;
    tokenizer
        .with_truncation(Some(TruncationParams {
            max_length: MAX_TOKENS,
            ..Default::default()
        }))
        .map_err(|e| format!("failed to configure truncation: {e}"))?;
    tokenizer.with_padding(Some(PaddingParams {
        pad_id,
        pad_type_id: 0,
        pad_token: "<pad>".to_string(),
        ..Default::default()
    }));

    let mut embeddings: Vec<Vec<f32>> = Vec::with_capacity(texts.len());

    for (start, end) in batch_ranges(texts.len(), BATCH_SIZE) {
        let chunk = &texts[start..end];
        let inputs: Vec<tokenizers::tokenizer::EncodeInput> = chunk
            .iter()
            .map(|text| tokenizers::EncodeInput::from(text.as_str()))
            .collect();
        let encodings = tokenizer
            .encode_batch(inputs, true)
            .map_err(|e| format!("tokenization failed: {e}"))?;

        let max_len = encodings.iter().map(|encoding| encoding.len()).max().unwrap_or(0);
        let batch = encodings.len();

        let mut input_ids = Vec::with_capacity(batch * max_len);
        let mut attention_mask = Vec::with_capacity(batch * max_len);
        for encoding in &encodings {
            let ids = encoding.get_ids();
            let mask = encoding.get_attention_mask();
            input_ids.extend(ids.iter().copied());
            attention_mask.extend(mask.iter().copied());
            // Right padding is used; the loop naturally repeats zeros.
            for _ in ids.len()..max_len {
                input_ids.push(pad_id);
                attention_mask.push(0);
            }
        }

        let input_ids = Tensor::new(input_ids, &device)
            .and_then(|tensor| tensor.reshape((batch, max_len)))
            .map_err(|e| format!("failed to build input tensor: {e}"))?;
        let attention_mask = Tensor::new(attention_mask, &device)
            .and_then(|tensor| tensor.reshape((batch, max_len)))
            .map_err(|e| format!("failed to build attention mask: {e}"))?;
        let token_type_ids = Tensor::zeros(input_ids.shape(), DType::U32, &device)
            .map_err(|e| format!("failed to build token type ids: {e}"))?;

        let output = model
            .forward(&input_ids, &attention_mask, &token_type_ids, None, None, None)
            .map_err(|e| format!("model forward failed: {e}"))?;

        // CLS pooling: first token of every sequence.
        let cls = output
            .i((.., 0usize, ..))
            .map_err(|e| format!("failed to extract CLS tokens: {e}"))?;
        let matrix: Vec<Vec<f32>> = cls
            .to_vec2::<f32>()
            .map_err(|e| format!("failed to convert embeddings: {e}"))?;

        embeddings.extend(matrix.iter().map(|vector| l2_normalize(vector)));
    }

    Ok(embeddings)
}

/// L2-normalize a vector.  The zero vector is returned unchanged.
pub fn l2_normalize(vector: &[f32]) -> Vec<f32> {
    let norm_sq: f32 = vector.iter().map(|value| value * value).sum();
    let norm = norm_sq.sqrt();
    if norm == 0.0 {
        return vector.to_vec();
    }
    vector.iter().map(|value| value / norm).collect()
}

/// Ranges covering `total` items in fixed-size batches.
pub fn batch_ranges(total: usize, batch_size: usize) -> Vec<(usize, usize)> {
    if batch_size == 0 {
        return Vec::new();
    }
    (0..total)
        .step_by(batch_size)
        .map(|start| (start, (start + batch_size).min(total)))
        .collect()
}

/// Ensure the model and tokenizer are present in the HF cache.
///
/// Returns progress/warning messages; the operation is warnings-only and
/// never fails the caller.
pub fn prewarm_cache() -> Vec<String> {
    let mut messages = Vec::new();
    let repo_id = model_repo();

    if cached_model_files().is_some() {
        // Silent success: a fully cached setup must not add stderr noise to
        // every single pkgq invocation.
        return messages;
    }

    messages.push(format!(
        "bootstrap: prewarming HF cache for {repo_id} ..."
    ));

    let cache_dir = hf_cache_dir();
    let api = match ApiBuilder::new()
        .with_cache_dir(cache_dir)
        .build()
        .map_err(|e| e.to_string())
    {
        Ok(api) => api,
        Err(e) => {
            messages.push(format!(
                "WARNING: failed to initialize Hugging Face API ({e}); semantic search setup incomplete"
            ));
            return messages;
        }
    };

    let repo = api.model(repo_id.clone());
    for filename in ["model.safetensors", "tokenizer.json"] {
        match repo.get(filename) {
            Ok(path) => messages.push(format!(
                "bootstrap: cached {filename} at {}",
                path.display()
            )),
            Err(e) => messages.push(format!(
                "WARNING: failed to cache {filename} for {repo_id} ({e}); semantic search setup incomplete"
            )),
        }
    }

    messages
}

#[cfg(test)]
pub(crate) static ENV_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn l2_normalize_produces_unit_vectors() {
        let normalized = l2_normalize(&[3.0, 4.0]);
        assert!((normalized[0] - 0.6).abs() < 1e-6);
        assert!((normalized[1] - 0.8).abs() < 1e-6);
        let norm: f32 = normalized.iter().map(|value| value * value).sum();
        assert!((norm - 1.0).abs() < 1e-6);
    }

    #[test]
    fn l2_normalize_leaves_zero_vector_unchanged() {
        assert_eq!(l2_normalize(&[0.0, 0.0, 0.0]), vec![0.0, 0.0, 0.0]);
        assert!(l2_normalize(&[]).is_empty());
    }

    #[test]
    fn batch_ranges_cover_all_items() {
        assert_eq!(batch_ranges(0, 8), &[]);
        assert_eq!(batch_ranges(5, 8), &[(0, 5)]);
        assert_eq!(batch_ranges(8, 8), &[(0, 8)]);
        assert_eq!(batch_ranges(17, 8), &[(0, 8), (8, 16), (16, 17)]);
    }

    #[test]
    fn batch_ranges_is_empty_for_zero_batch_size() {
        assert!(batch_ranges(10, 0).is_empty());
    }

    #[test]
    fn repo_cache_name_escapes_slashes() {
        assert_eq!(repo_cache_name("BAAI/bge-m3"), "models--BAAI--bge-m3");
        assert_eq!(repo_cache_name("org/model"), "models--org--model");
    }

    #[test]
    fn engine_id_is_constant() {
        assert_eq!(ENGINE_ID, "candle-bge-m3-v1");
        assert_eq!(model_repo(), DEFAULT_MODEL_REPO);
    }

    #[test]
    fn default_cache_dir_respects_hf_home() {
        let _guard = ENV_TEST_LOCK.lock().unwrap();
        let original = std::env::var("HF_HOME").ok();
        let original_hub = std::env::var("HF_HUB_CACHE").ok();
        std::env::remove_var("HF_HUB_CACHE");
        std::env::set_var("HF_HOME", "/tmp/pkgq-hf-home");
        assert_eq!(hf_cache_dir(), PathBuf::from("/tmp/pkgq-hf-home/hub"));
        match original {
            Some(value) => std::env::set_var("HF_HOME", value),
            None => std::env::remove_var("HF_HOME"),
        }
        if let Some(value) = original_hub {
            std::env::set_var("HF_HUB_CACHE", value);
        }
    }

    #[test]
    fn hub_cache_env_overrides_hf_home() {
        let _guard = ENV_TEST_LOCK.lock().unwrap();
        let original = std::env::var("HF_HUB_CACHE").ok();
        let original_home = std::env::var("HF_HOME").ok();
        std::env::set_var("HF_HUB_CACHE", "/tmp/pkgq-hub-cache");
        std::env::set_var("HF_HOME", "/tmp/pkgq-hf-home");
        assert_eq!(hf_cache_dir(), PathBuf::from("/tmp/pkgq-hub-cache"));
        match original {
            Some(value) => std::env::set_var("HF_HUB_CACHE", value),
            None => std::env::remove_var("HF_HUB_CACHE"),
        }
        match original_home {
            Some(value) => std::env::set_var("HF_HOME", value),
            None => std::env::remove_var("HF_HOME"),
        }
    }

    #[test]
    #[ignore = "downloads real model weights (network + disk)"]
    fn embed_real_texts() {
        let texts = vec![
            "A text editor for programmers.".to_string(),
            "Un éditeur de texte.".to_string(),
        ];
        let embeddings = embed_texts(&texts).expect("real embedding should succeed");
        assert_eq!(embeddings.len(), 2);
        assert_eq!(embeddings[0].len(), 1024);
        assert_eq!(embeddings[1].len(), 1024);
    }
}

//! Optional semantic layer: an embedding index built with a local
//! llama-server (OpenAI-compatible `/v1/embeddings`, e.g. bge-m3).
//!
//! `pkgq index` embeds every inventory description once into
//! `~/.cache/pkgq/index.json`; `search` blends lexical and semantic scores
//! when the index exists and the server responds, and silently falls back to
//! lexical-only otherwise. The embeddings call is one `curl` per batch
//! through the bash adapter — zero extra Rust dependencies.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::model::{ManagerError, ManagerKind};
use crate::run;
use crate::shell;

const DEFAULT_EMBED_URL: &str = "http://127.0.0.1:8080/v1/embeddings";
const DEFAULT_EMBED_MODEL: &str = "bge-m3";
const BATCH_SIZE: usize = 16;
const EMBED_TIMEOUT_SECS: u64 = 120;
/// Semantic dominates for cross-language queries; lexical keeps precision.
const SEMANTIC_WEIGHT: f64 = 0.6;
const LEXICAL_WEIGHT: f64 = 0.4;

fn embed_url() -> String {
    std::env::var("PKGQ_EMBED_URL").unwrap_or_else(|_| DEFAULT_EMBED_URL.to_string())
}

fn embed_model() -> String {
    std::env::var("PKGQ_EMBED_MODEL").unwrap_or_else(|_| DEFAULT_EMBED_MODEL.to_string())
}

fn cache_path() -> PathBuf {
    let base = std::env::var("XDG_CACHE_HOME")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            std::env::var("HOME")
                .map(|home| format!("{home}/.cache"))
                .unwrap_or_else(|_| "/tmp".to_string())
        });
    PathBuf::from(base).join("pkgq").join("index.json")
}

/// One indexed application: the embedded text plus its identity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexItem {
    pub name: String,
    pub manager: ManagerKind,
    pub text: String,
    pub embedding: Vec<f32>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub installed: bool,
}

/// On-disk semantic index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticIndex {
    pub model: String,
    pub generated_at: String,
    pub items: Vec<IndexItem>,
}

/// Cosine similarity between two vectors; 0 when either is degenerate.
pub fn cosine(a: &[f32], b: &[f32]) -> f64 {
    let dot: f64 = a
        .iter()
        .zip(b)
        .map(|(x, y)| (*x as f64) * (*y as f64))
        .sum();
    let norm_a: f64 = a.iter().map(|x| (*x as f64).powi(2)).sum::<f64>().sqrt();
    let norm_b: f64 = b.iter().map(|y| (*y as f64).powi(2)).sum::<f64>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        0.0
    } else {
        dot / (norm_a * norm_b)
    }
}

/// Embed texts through the configured llama-server endpoint, in batches.
pub fn embed_texts(texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
    if texts.is_empty() {
        return Ok(Vec::new());
    }
    let url = embed_url();
    let model = embed_model();
    let mut out: Vec<Vec<f32>> = Vec::with_capacity(texts.len());
    for (batch_index, batch) in texts.chunks(BATCH_SIZE).enumerate() {
        let body = serde_json::json!({ "model": model, "input": batch }).to_string();
        let body_file = std::env::temp_dir().join(format!(
            "pkgq-embed-{}-{batch_index}.json",
            std::process::id()
        ));
        std::fs::write(&body_file, body).map_err(|e| e.to_string())?;
        let cmd = format!(
            "curl -s -m {EMBED_TIMEOUT_SECS} -X POST {} -H 'Content-Type: application/json' --data-binary @{}",
            shell::quote(&url),
            shell::quote(&body_file.to_string_lossy())
        );
        let response = shell::run(&cmd).map_err(|e| e.to_string());
        let _ = std::fs::remove_file(&body_file);
        let response = response?;
        let value: serde_json::Value = serde_json::from_str(response.trim())
            .map_err(|e| format!("non-JSON embeddings response: {e}"))?;
        let mut data: Vec<(usize, Vec<f32>)> = value
            .get("data")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| "embeddings response has no 'data' array".to_string())?
            .iter()
            .map(|item| {
                let index = item
                    .get("index")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0) as usize;
                let embedding = item
                    .get("embedding")
                    .and_then(serde_json::Value::as_array)
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(serde_json::Value::as_f64)
                            .map(|value| value as f32)
                            .collect()
                    })
                    .unwrap_or_default();
                (index, embedding)
            })
            .collect();
        data.sort_by_key(|(index, _)| *index);
        out.extend(data.into_iter().map(|(_, embedding)| embedding));
    }
    Ok(out)
}

/// Build the semantic index over the (optionally filtered) local inventory.
pub fn build_index(
    selected: Option<&[ManagerKind]>,
) -> Result<serde_json::Value, Vec<ManagerError>> {
    let output = run::run_list(selected);
    let texts: Vec<String> = output
        .results
        .iter()
        .map(|app| match &app.description {
            Some(description) => format!("{}. {description}", app.name),
            None => app.name.clone(),
        })
        .collect();
    let embeddings = embed_texts(&texts).map_err(|e| {
        vec![ManagerError {
            manager: ManagerKind::Apt,
            message: format!("semantic index: {e} (is llama-server running?)"),
        }]
    })?;

    let index = SemanticIndex {
        model: embed_model(),
        generated_at: crate::timefmt::now_rfc3339_utc(),
        items: output
            .results
            .iter()
            .zip(texts.iter())
            .zip(embeddings.iter())
            .map(|((app, text), embedding)| IndexItem {
                name: app.name.clone(),
                manager: app.manager,
                text: text.clone(),
                embedding: embedding.clone(),
                description: app.description.clone(),
                installed: app.installed,
            })
            .collect(),
    };

    let path = cache_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let serialized = serde_json::to_string_pretty(&index).unwrap_or_default();
    std::fs::write(&path, serialized).map_err(|e| {
        vec![ManagerError {
            manager: ManagerKind::Apt,
            message: e.to_string(),
        }]
    })?;

    Ok(serde_json::json!({
        "command": "index",
        "indexed": index.items.len(),
        "model": index.model,
        "cache": path.to_string_lossy(),
        "generated_at": index.generated_at,
    }))
}

/// Load the semantic index if it exists on disk.
pub fn load_index() -> Option<SemanticIndex> {
    let content = std::fs::read_to_string(cache_path()).ok()?;
    serde_json::from_str(&content).ok()
}

/// Blended score for a search result: semantic similarity (when an index is
/// available) plus the normalized lexical score.
pub fn blended_score(lexical_confidence: f64, similarity: f64) -> f64 {
    SEMANTIC_WEIGHT * similarity + LEXICAL_WEIGHT * lexical_confidence
}

/// An embedding lookup keyed by (app name, manager), prebuilt once per query.
pub type EmbeddingLookup = HashMap<(String, ManagerKind), Vec<f32>>;

/// Build the O(1) similarity lookup from an index.
pub fn embedding_lookup(index: &SemanticIndex) -> EmbeddingLookup {
    index
        .items
        .iter()
        .map(|item| ((item.name.clone(), item.manager), item.embedding.clone()))
        .collect()
}

/// Cosine similarity against the lookup; 0 when the app is not indexed.
pub fn similarity(
    lookup: &EmbeddingLookup,
    name: &str,
    manager: ManagerKind,
    query: &[f32],
) -> f64 {
    lookup
        .get(&(name.to_string(), manager))
        .map(|embedding| cosine(embedding, query))
        .unwrap_or(0.0)
}

/// Minimum similarity for the semantic layer to rescue a candidate that the
/// lexical score rejected (cross-language / synonym queries).
pub const SEMANTIC_RESCUE_THRESHOLD: f64 = 0.55;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_of_known_vectors() {
        assert!((cosine(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-9);
        assert!(cosine(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-9);
        assert!((cosine(&[1.0, 0.0], &[-1.0, 0.0]) + 1.0).abs() < 1e-9);
        assert_eq!(cosine(&[], &[]), 0.0);
    }

    #[test]
    fn index_roundtrip_through_json() {
        let index = SemanticIndex {
            model: "bge-m3".into(),
            generated_at: "2026-09-20T00:00:00Z".into(),
            items: vec![IndexItem {
                name: "Drift".into(),
                manager: ManagerKind::Flatpak,
                text: "Drift. Edit and export videos easily".into(),
                embedding: vec![0.1, 0.2, 0.3],
                description: Some("Edit and export videos easily".into()),
                installed: true,
            }],
        };
        let serialized = serde_json::to_string(&index).unwrap();
        let parsed: SemanticIndex = serde_json::from_str(&serialized).unwrap();
        assert_eq!(parsed.items[0].name, "Drift");
        assert_eq!(parsed.items[0].embedding, vec![0.1, 0.2, 0.3]);
    }

    #[test]
    fn similarity_is_zero_for_missing_items() {
        let index = SemanticIndex {
            model: "m".into(),
            generated_at: String::new(),
            items: vec![],
        };
        let lookup = embedding_lookup(&index);
        assert_eq!(
            similarity(&lookup, "anything", ManagerKind::Snap, &[0.1]),
            0.0
        );
    }

    #[test]
    fn blended_score_respects_weights() {
        // Full semantic match with no lexical signal.
        assert!((blended_score(0.0, 1.0) - SEMANTIC_WEIGHT).abs() < 1e-9);
        // Full lexical match with no semantic signal.
        assert!((blended_score(1.0, 0.0) - LEXICAL_WEIGHT).abs() < 1e-9);
        assert!(blended_score(1.0, 1.0) > blended_score(0.5, 0.5));
    }
}

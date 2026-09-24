# Feature: native-embeddings (candle bge-m3, in-process)

Status: merged to master (PR #2)

## Goal

Replace the external llama-server embedding path with an in-process,
pure-Rust engine: candle-transformers XLM-RoBERTa running bge-m3
safetensors + HF tokenizer. No llama.cpp fork, no GGUF, no server, no curl
for embeddings.

## Decisions (user-confirmed)

- Keep model: bge-m3 (CLS dense pooling + L2 normalization, 1024 dims).
- Engine: candle (pure Rust), in-process; no C/C++ dependency.
- Remove the llama-server client path and the llama.cpp/GGUF bootstrap.

## Design notes

- Weights: BAAI/bge-m3 model.safetensors (~2.3 GB fp32) + tokenizer.json via
  hf-hub standard cache; `PKGQ_EMBED_MODEL_REPO` env override.
- bootstrap.rs becomes a cheap HF-cache check + one-time prewarm
  (warnings-only, PKGQ_NO_BOOTSTRAP=1 still skips).
- semantic.rs: embed_texts keeps its signature (same call sites); batching
  stays; SemanticIndex gains `engine` field — missing/mismatched engine
  (e.g. old llama-server indexes) means stale: ignored for blending,
  rebuilt by `pkgq index`.
- Trade-offs accepted: cold start (model load per CLI invocation, seconds),
  2.3 GB one-time download, CPU-only first (no cuda/metal features).

## Tasks

- [x] T1: Cargo.toml deps (candle-core/candle-nn/candle-transformers 0.9.2, tokenizers 0.21.4 fancy-regex, hf-hub 0.4.3 ureq/rustls)
- [x] T2: src/embeddings.rs — candle engine (hf-hub fetch, tokenize, XLM-R, CLS pool, L2 norm, batch 8 con máscara)
- [x] T3: bootstrap.rs — HF-cache check + prewarm (warnings-only), PKGQ_NO_BOOTSTRAP intacto
- [x] T4: semantic.rs — backend swap, campo engine + staleness (índices viejos ignorados), path curl eliminado
- [x] T5: 94 tests green offline (1 #[ignore]: embed_real_texts con pesos reales)
- [x] T6: README — engine nativo, tamaños, env vars, re-index

## Evidence

- Work-unit commit: feat/native-embeddings — `feat(embeddings): native candle bge-m3 engine, drop llama-server path`
- Parent fix: prewarm_cache silent when cached (stderr noise regression caught in parent review)
- Independent verification (gentle-ai-verify) 7/7 PASS: offline tests guaranteed, no onig/openssl in tree, rustls TLS, no cert-weakening, no llama-server leftovers in code, staleness confirmed, stderr clean, `list --compact` valid JSON (1855 items)
- Unverified by design: real-model end-to-end (#[ignore] test; requires 2.3 GB download) — first real `pkgq index` run exercises it
- Follow-up (bootstrap evolution, user-directed): first-run validation order — llama-server detection with port reuse (43210 embeddings / 43211 LFM2.5 / 43212 K2), candle in-process fallback verifying/downloading native weights into ~/models/bge-m3/; GGUF curl bootstrap restored earlier on master

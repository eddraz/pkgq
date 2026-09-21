//! First-run bootstrap: ensures the native embedding assets exist in the
//! Hugging Face cache.
//!
//! Verification is a cheap filesystem scan; the (potentially slow) download
//! only happens when `model.safetensors` or `tokenizer.json` is missing.  Set
//! `PKGQ_NO_BOOTSTRAP=1` to skip entirely.

use crate::embeddings;

/// Ensure the configured embedding model and tokenizer are cached.
///
/// Returns human-readable progress/warning messages; the operation is
/// warnings-only and never fails the caller.
pub fn ensure() -> Vec<String> {
    embeddings::prewarm_cache()
}

/// Entry point called at the start of every binary execution. Prints
/// progress/warnings to stderr; never fails the command.
pub fn run_if_enabled() {
    if std::env::var("PKGQ_NO_BOOTSTRAP")
        .map(|value| value == "1")
        .unwrap_or(false)
    {
        return;
    }
    for message in ensure() {
        eprintln!("pkgq: {message}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn with_env(key: &str, value: Option<&str>) -> Option<String> {
        let previous = std::env::var(key).ok();
        match value {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
        previous
    }

    fn restore_env(key: &str, previous: Option<String>) {
        match previous {
            Some(previous) => std::env::set_var(key, previous),
            None => std::env::remove_var(key),
        }
    }

    #[test]
    fn cache_dir_defaults_under_home() {
        let _guard = embeddings::ENV_TEST_LOCK.lock().unwrap();
        let prev_home = with_env("HF_HOME", None);
        let prev_hub = with_env("HF_HUB_CACHE", None);
        let home = std::env::var("HOME").unwrap_or_else(|_| "/home/user".to_string());
        assert_eq!(
            embeddings::hf_cache_dir(),
            PathBuf::from(home).join(".cache/huggingface/hub")
        );
        restore_env("HF_HOME", prev_home);
        restore_env("HF_HUB_CACHE", prev_hub);
    }

    #[test]
    fn cache_dir_respects_hf_home() {
        let _guard = embeddings::ENV_TEST_LOCK.lock().unwrap();
        let prev_home = with_env("HF_HOME", Some("/tmp/pkgq-hf-home"));
        let prev_hub = with_env("HF_HUB_CACHE", None);
        assert_eq!(
            embeddings::hf_cache_dir(),
            PathBuf::from("/tmp/pkgq-hf-home/hub")
        );
        restore_env("HF_HOME", prev_home);
        restore_env("HF_HUB_CACHE", prev_hub);
    }

    #[test]
    fn cache_dir_respects_hf_hub_cache() {
        let _guard = embeddings::ENV_TEST_LOCK.lock().unwrap();
        let prev_home = with_env("HF_HOME", Some("/tmp/pkgq-hf-home"));
        let prev_hub = with_env("HF_HUB_CACHE", Some("/tmp/pkgq-hub-cache"));
        assert_eq!(
            embeddings::hf_cache_dir(),
            PathBuf::from("/tmp/pkgq-hub-cache")
        );
        restore_env("HF_HOME", prev_home);
        restore_env("HF_HUB_CACHE", prev_hub);
    }

    #[test]
    fn repo_cache_name_replaces_slashes() {
        assert_eq!(
            embeddings::repo_cache_name("BAAI/bge-m3"),
            "models--BAAI--bge-m3"
        );
    }
}

//! First-run bootstrap: ensures the bge-m3 embedding model exists in `~/models/`.
//!
//! Download happens with `curl` from Hugging Face GGML (bge-m3-q8_0.gguf).
//! Set `PKGQ_NO_BOOTSTRAP=1` to skip entirely.

use std::path::{Path, PathBuf};

use crate::shell;

pub const MODEL_FILE_NAME: &str = "bge-m3-q8_0.gguf";
pub const MODEL_URL: &str =
    "https://huggingface.co/ggml-org/bge-m3-Q8_0-GGUF/resolve/main/bge-m3-q8_0.gguf?download=true";

pub fn home_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".to_string()))
}

pub fn models_dir(home: &Path) -> PathBuf {
    home.join("models")
}

pub fn model_path(home: &Path) -> PathBuf {
    models_dir(home).join(MODEL_FILE_NAME)
}

/// Whether the model exists in `~/models` or `~/models/bge-m3`,
/// checking for either `bge-m3-q8_0.gguf` (case-insensitively) or
/// native weights (`pytorch_model.bin` / `model.safetensors` + `tokenizer.json`).
pub fn model_present(home: &Path) -> bool {
    let candidate_dirs = [models_dir(home), models_dir(home).join("bge-m3")];

    for dir in candidate_dirs {
        if !dir.is_dir() {
            continue;
        }
        if let Ok(entries) = dir.read_dir() {
            for entry in entries.flatten() {
                if entry
                    .file_name()
                    .to_string_lossy()
                    .eq_ignore_ascii_case(MODEL_FILE_NAME)
                {
                    return true;
                }
            }
        }
        let has_weights = dir.join("model.safetensors").is_file() || dir.join("pytorch_model.bin").is_file();
        let has_tokenizer = dir.join("tokenizer.json").is_file();
        if has_weights && has_tokenizer {
            return true;
        }
    }

    false
}

/// Ensure the model asset exists, downloading with curl if missing.
pub fn ensure(home: &Path) -> Vec<String> {
    let mut messages: Vec<String> = Vec::new();

    if !model_present(home) {
        let target = model_path(home);
        messages.push(format!(
            "bootstrap: downloading bge-m3 model into {} ...",
            target.display()
        ));
        let cmd = format!(
            "curl -fL --create-dirs -o {} {}",
            shell::quote(&target.to_string_lossy()),
            shell::quote(MODEL_URL)
        );
        match shell::run(&cmd) {
            Ok(_) => messages.push(format!("model saved to {}", target.display())),
            Err(e) => messages.push(format!(
                "WARNING: model download failed ({}); semantic search setup incomplete",
                e
            )),
        }
    }

    messages
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
    let home = home_dir();
    for message in ensure(&home) {
        eprintln!("pkgq: {message}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_live_under_home() {
        let home = Path::new("/home/tester");
        assert_eq!(
            model_path(home),
            PathBuf::from("/home/tester/models/bge-m3-q8_0.gguf")
        );
    }

    #[test]
    fn model_detection_is_case_insensitive() {
        let base = std::env::temp_dir().join(format!("pkgq-bootstrap-{}", std::process::id()));
        let models = models_dir(&base);
        std::fs::create_dir_all(&models).unwrap();
        assert!(!model_present(&base));
        std::fs::write(models.join("bge-m3-Q8_0.gguf"), b"x").unwrap();
        assert!(model_present(&base));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn missing_models_dir_is_not_present() {
        let base = std::env::temp_dir().join(format!("pkgq-bootstrap-none-{}", std::process::id()));
        assert!(!model_present(&base));
    }

    #[test]
    fn native_model_detection_recognizes_pytorch_weights() {
        let base = std::env::temp_dir().join(format!("pkgq-bootstrap-native-{}", std::process::id()));
        let bge_dir = models_dir(&base).join("bge-m3");
        std::fs::create_dir_all(&bge_dir).unwrap();
        assert!(!model_present(&base));
        std::fs::write(bge_dir.join("pytorch_model.bin"), b"dummy").unwrap();
        assert!(!model_present(&base)); // still missing tokenizer.json
        std::fs::write(bge_dir.join("tokenizer.json"), b"dummy").unwrap();
        assert!(model_present(&base));
        let _ = std::fs::remove_dir_all(&base);
    }
}

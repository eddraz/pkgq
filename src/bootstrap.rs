//! First-run bootstrap: verifies model serving infrastructure and local assets.
//!
//! Startup validation order:
//! 1. Checks if `llama-server` exists.
//! 2. If `llama-server` exists:
//!    - Checks if port 43210 (embeddings), 43211 (LFM2.5), 43212 (K2) are active
//!      to avoid starting/calling duplicate instances.
//!    - If not active, ensures the corresponding GGUF models exist in `~/models/`.
//! 3. If `llama-server` does NOT exist:
//!    - Configures fallback to Candle in-process.
//!    - Verifies native model weights exist in `~/models/` (`model.safetensors` or `pytorch_model.bin` + `tokenizer.json`).
//!    - Downloads weights for Candle into `~/models/bge-m3/` if missing.
//!
//! Set `PKGQ_NO_BOOTSTRAP=1` to skip entirely; `PKGQ_FORCE_BOOTSTRAP=1` to force
//! verbose validation output even if already bootstrapped.

use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::shell;

pub const EMBEDDINGS_PORT: u16 = 43210;
pub const LFM25_PORT: u16 = 43211;
pub const K2_PORT: u16 = 43212;

pub const MODEL_FILE_NAME: &str = "bge-m3-q8_0.gguf";
pub const MODEL_URL: &str =
    "https://huggingface.co/ggml-org/bge-m3-Q8_0-GGUF/resolve/main/bge-m3-q8_0.gguf?download=true";

pub const LFM25_FILE_NAME: &str = "LFM2.5-230M-F16.gguf";
pub const K2_FILE_NAME: &str = "K2-Horizon-1B-BF16.gguf";

pub const CANDLE_TOKENIZER_URL: &str =
    "https://huggingface.co/BAAI/bge-m3/resolve/main/tokenizer.json";
pub const CANDLE_WEIGHTS_URL: &str =
    "https://huggingface.co/Shitao/bge-m3/resolve/main/model.safetensors";

pub fn home_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".to_string()))
}

pub fn models_dir(home: &Path) -> PathBuf {
    home.join("models")
}

pub fn model_path(home: &Path) -> PathBuf {
    models_dir(home).join(MODEL_FILE_NAME)
}

/// Check if a local TCP port is already listening on 127.0.0.1.
pub fn is_port_active(port: u16) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&addr, Duration::from_millis(50)).is_ok()
}

/// Check whether `llama-server` (or `llama-k2-server`) is installed and available.
#[allow(dead_code)]
pub fn llama_server_present() -> bool {
    llama_server_present_in(&home_dir())
}

/// Check whether `llama-server` is present given a specific home path.
pub fn llama_server_present_in(home: &Path) -> bool {
    if shell::which("llama-server") || shell::which("llama-k2-server") {
        return true;
    }
    let candidates = [
        home.join(".local").join("bin").join("llama-server"),
        home.join(".local").join("bin").join("llama-k2-server"),
        home.join("apps")
            .join("llama.cpp")
            .join("build")
            .join("bin")
            .join("llama-server"),
        PathBuf::from("/usr/local/bin/llama-server"),
        PathBuf::from("/usr/bin/llama-server"),
    ];
    candidates.iter().any(|p| p.is_file())
}

/// Whether a specific GGUF model exists under `~/models` or `~/models/<subdir>` (case-insensitively).
pub fn has_gguf_model(home: &Path, filename: &str) -> bool {
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
                    .eq_ignore_ascii_case(filename)
                {
                    return true;
                }
            }
        }
    }
    false
}

/// Whether Candle native weights (`model.safetensors` or `pytorch_model.bin` + `tokenizer.json`)
/// exist in `~/models`, `~/models/bge-m3`, or in the HF cache.
pub fn has_candle_weights(home: &Path) -> bool {
    let candidate_dirs = [models_dir(home), models_dir(home).join("bge-m3")];
    for dir in candidate_dirs {
        if !dir.is_dir() {
            continue;
        }
        let has_weights =
            dir.join("model.safetensors").is_file() || dir.join("pytorch_model.bin").is_file();
        let has_tokenizer = dir.join("tokenizer.json").is_file();
        if has_weights && has_tokenizer {
            return true;
        }
    }
    crate::embeddings::cached_model_files().is_some()
}

/// Whether the bge-m3 model exists locally as either GGUF or Candle weights.
#[allow(dead_code)]
pub fn model_present(home: &Path) -> bool {
    has_gguf_model(home, MODEL_FILE_NAME) || has_candle_weights(home)
}

/// Path to the first-run completion marker.
pub fn bootstrap_marker_path(home: &Path) -> PathBuf {
    let cache_base = std::env::var("XDG_CACHE_HOME")
        .ok()
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".cache"));
    cache_base.join("pkgq").join(".bootstrapped")
}

/// Check if this is the first execution of the application.
pub fn is_first_run(home: &Path) -> bool {
    !bootstrap_marker_path(home).is_file()
}

/// Mark the bootstrap as completed so subsequent executions remain fast and silent.
pub fn mark_bootstrapped(home: &Path) {
    let path = bootstrap_marker_path(home);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, crate::timefmt::now_rfc3339_utc());
}

fn download_file(target: &Path, url: &str, label: &str, messages: &mut Vec<String>) {
    messages.push(format!(
        "bootstrap: descargando {label} en {} ...",
        target.display()
    ));
    let cmd = format!(
        "curl -fL --create-dirs -o {} {}",
        shell::quote(&target.to_string_lossy()),
        shell::quote(url)
    );
    match shell::run(&cmd) {
        Ok(_) => messages.push(format!("{label} guardado en {}", target.display())),
        Err(e) => messages.push(format!(
            "WARNING: error al descargar {label} ({e}); configuración incompleta"
        )),
    }
}

/// Main bootstrap validation:
/// Checks `llama-server` existence, port availability (43210, 43211, 43212),
/// or prepares Candle fallback with downloaded weights if missing.
pub fn ensure(home: &Path) -> Vec<String> {
    let first_run = is_first_run(home)
        || std::env::var("PKGQ_FORCE_BOOTSTRAP")
            .map(|v| v == "1")
            .unwrap_or(false);

    let mut messages: Vec<String> = Vec::new();
    let llama_exists = llama_server_present_in(home);

    if first_run {
        messages.push(
            "bootstrap: validando entorno de inferencia (llama-server / Candle)...".to_string(),
        );
    }

    if llama_exists {
        let embed_active = is_port_active(EMBEDDINGS_PORT);
        let lfm25_active = is_port_active(LFM25_PORT);
        let k2_active = is_port_active(K2_PORT);

        // 1. Embeddings (Port 43210)
        if embed_active {
            if first_run {
                messages.push(format!(
                    "llama-server: puerto {EMBEDDINGS_PORT} para embeddings activo (evitando doble llamada)"
                ));
            }
        } else {
            if !has_gguf_model(home, MODEL_FILE_NAME) {
                download_file(
                    &model_path(home),
                    MODEL_URL,
                    "modelo GGUF bge-m3",
                    &mut messages,
                );
            } else if first_run {
                messages.push(format!(
                    "llama-server: puerto {EMBEDDINGS_PORT} inactivo; modelo GGUF listo en ~/models/{MODEL_FILE_NAME}"
                ));
            }
        }

        // 2. LFM2.5 (Port 43211)
        if lfm25_active {
            if first_run {
                messages.push(format!(
                    "llama-server: puerto {LFM25_PORT} para LFM2.5 activo (evitando doble llamada)"
                ));
            }
        } else if first_run {
            let present = has_gguf_model(home, LFM25_FILE_NAME);
            messages.push(format!(
                "llama-server: puerto {LFM25_PORT} (LFM2.5) inactivo; GGUF {LFM25_FILE_NAME} (presente: {present})"
            ));
        }

        // 3. K2 (Port 43212)
        if k2_active {
            if first_run {
                messages.push(format!(
                    "llama-server: puerto {K2_PORT} para K2 activo (evitando doble llamada)"
                ));
            }
        } else if first_run {
            let present = has_gguf_model(home, K2_FILE_NAME);
            messages.push(format!(
                "llama-server: puerto {K2_PORT} (K2) inactivo; GGUF {K2_FILE_NAME} (presente: {present})"
            ));
        }
    } else {
        // llama-server does not exist -> consume with Candle
        if first_run {
            messages.push(
                "llama-server no encontrado; configurando consumo vía Candle in-process"
                    .to_string(),
            );
        }
        if !has_candle_weights(home) {
            let bge_dir = models_dir(home).join("bge-m3");
            download_file(
                &bge_dir.join("tokenizer.json"),
                CANDLE_TOKENIZER_URL,
                "tokenizer bge-m3",
                &mut messages,
            );
            download_file(
                &bge_dir.join("model.safetensors"),
                CANDLE_WEIGHTS_URL,
                "pesos safetensors bge-m3",
                &mut messages,
            );
        } else if first_run {
            messages.push("Candle: pesos y tokenizer nativos verificados en local".to_string());
        }
    }

    if first_run {
        mark_bootstrapped(home);
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
        let base =
            std::env::temp_dir().join(format!("pkgq-bootstrap-native-{}", std::process::id()));
        let bge_dir = models_dir(&base).join("bge-m3");
        std::fs::create_dir_all(&bge_dir).unwrap();
        assert!(!model_present(&base));
        std::fs::write(bge_dir.join("pytorch_model.bin"), b"dummy").unwrap();
        assert!(!model_present(&base)); // still missing tokenizer.json
        std::fs::write(bge_dir.join("tokenizer.json"), b"dummy").unwrap();
        assert!(model_present(&base));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn is_port_active_detects_listening_and_closed_ports() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(is_port_active(port));
        drop(listener);
        assert!(!is_port_active(port));
    }

    #[test]
    fn first_run_marker_lifecycle() {
        let base =
            std::env::temp_dir().join(format!("pkgq-bootstrap-marker-{}", std::process::id()));
        assert!(is_first_run(&base));
        mark_bootstrapped(&base);
        assert!(!is_first_run(&base));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn llama_server_detection_in_mock_home() {
        let base =
            std::env::temp_dir().join(format!("pkgq-bootstrap-llama-{}", std::process::id()));
        let bin_dir = base.join(".local").join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        assert!(!llama_server_present_in(&base) || shell::which("llama-server"));
        std::fs::write(bin_dir.join("llama-server"), b"#!/bin/sh\n").unwrap();
        assert!(llama_server_present_in(&base));
        let _ = std::fs::remove_dir_all(&base);
    }
}

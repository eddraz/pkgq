//! Self-update: download and replace the pkgq binary from GitHub Releases.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde_json::json;

use crate::shell;

// Repository slug; kept as a named constant even though the URL below embeds
// it, so callers/readers can locate the canonical source at a glance.
#[allow(dead_code)]
const REPO: &str = "eddraz/pkgq";

/// GitHub release API endpoint for the latest release.
const RELEASE_URL: &str = "https://api.github.com/repos/eddraz/pkgq/releases/latest";

/// Known release targets. `target_triple()` must always return one of these.
#[allow(dead_code)]
const KNOWN_TARGETS: &[&str] = &[
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
];

/// Map the current platform to a Rust-style target triple.
pub fn target_triple() -> &'static str {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        "x86_64-unknown-linux-gnu"
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        "aarch64-unknown-linux-gnu"
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        "x86_64-apple-darwin"
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        "aarch64-apple-darwin"
    }
}

/// Compare two dot-separated version strings.
///
/// Numeric segments are compared as numbers; non-numeric segments fall back to
/// string inequality. Shorter versions are padded with zeros.
pub fn is_newer(latest: &str, current: &str) -> bool {
    let latest_parts: Vec<&str> = latest.split('.').collect();
    let current_parts: Vec<&str> = current.split('.').collect();
    let max_len = latest_parts.len().max(current_parts.len());

    for i in 0..max_len {
        let l = latest_parts.get(i).unwrap_or(&"0");
        let c = current_parts.get(i).unwrap_or(&"0");

        match (l.parse::<u64>(), c.parse::<u64>()) {
            (Ok(ln), Ok(cn)) => {
                if ln != cn {
                    return ln > cn;
                }
            }
            _ => {
                if l != c {
                    return l > c;
                }
            }
        }
    }

    false
}

/// Fetch the latest release metadata from GitHub.
pub fn fetch_latest_release() -> Result<serde_json::Value, String> {
    let command = format!(
        "curl -fsSL {} -H {} -H {}",
        shell::quote(RELEASE_URL),
        shell::quote("User-Agent: pkgq"),
        shell::quote("Accept: application/vnd.github+json")
    );

    let stdout = shell::run(&command).map_err(|e| format!("failed to fetch latest release: {e}"))?;

    serde_json::from_str(&stdout)
        .map_err(|e| format!("failed to parse GitHub release JSON: {e}"))
}

/// Strip a leading 'v' from a tag name to get the plain version.
fn strip_v(tag: &str) -> &str {
    tag.strip_prefix('v').unwrap_or(tag)
}

/// Select the asset that matches the current target triple.
///
/// Returns `Some((version, download_url))` where `version` has any leading
/// `v` removed from the tag name.
pub fn select_asset(release_json: &serde_json::Value, target: &str) -> Option<(String, String)> {
    let tag = release_json.get("tag_name")?.as_str()?;
    let version = strip_v(tag).to_string();
    let expected_name = format!("pkgq-{version}-{target}.tar.gz");

    let assets = release_json.get("assets")?.as_array()?;
    for asset in assets {
        let name = asset.get("name")?.as_str()?;
        if name == expected_name {
            let url = asset.get("browser_download_url")?.as_str()?;
            return Some((version, url.to_string()));
        }
    }

    None
}

/// Locate the extracted `pkgq` binary inside `dir`.
fn find_extracted_binary(dir: &Path) -> Option<PathBuf> {
    let entries = fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            if let Some(name) = path.file_name() {
                if name == "pkgq" {
                    return Some(path);
                }
            }
        }
    }
    None
}

/// Copy unix permissions from `source` to `dest`.
#[cfg(unix)]
fn copy_permissions(source: &Path, dest: &Path) -> Result<(), String> {
    let mode = fs::metadata(source)
        .map_err(|e| format!("failed to read permissions of current binary: {e}"))?
        .permissions()
        .mode();
    let mut perms = fs::metadata(dest)
        .map_err(|e| format!("failed to read permissions of new binary: {e}"))?
        .permissions();
    perms.set_mode(mode);
    fs::set_permissions(dest, perms)
        .map_err(|e| format!("failed to set permissions on new binary: {e}"))?;
    Ok(())
}

#[cfg(not(unix))]
fn copy_permissions(_source: &Path, _dest: &Path) -> Result<(), String> {
    Ok(())
}

/// Download the tarball, extract it, and replace the running binary.
pub fn download_and_replace(url: &str) -> Result<PathBuf, String> {
    let current_exe = std::env::current_exe()
        .map_err(|e| format!("failed to determine current executable path: {e}"))?;

    let temp_dir = shell::run("mktemp -d")
        .map_err(|e| format!("failed to create temporary directory: {e}"))?
        .trim()
        .to_string();
    let temp_path = PathBuf::from(&temp_dir);

    let archive_path = temp_path.join("pkgq.tar.gz");
    let archive_quoted = shell::quote(archive_path.to_str().unwrap_or("pkgq.tar.gz"));
    let url_quoted = shell::quote(url);

    shell::run(&format!("curl -fsSL -o {archive_quoted} {url_quoted}"))
        .map_err(|e| format!("failed to download update archive: {e}"))?;

    let temp_quoted = shell::quote(temp_path.to_str().unwrap_or("/tmp"));
    shell::run(&format!("tar xzf {archive_quoted} -C {temp_quoted}"))
        .map_err(|e| format!("failed to extract update archive: {e}"))?;

    let extracted = find_extracted_binary(&temp_path)
        .ok_or("extracted archive does not contain a 'pkgq' binary")?;

    let new_path = current_exe.with_extension("new");
    fs::copy(&extracted, &new_path)
        .map_err(|e| map_replace_error(&current_exe, e))?;

    copy_permissions(&current_exe, &new_path)?;

    fs::rename(&new_path, &current_exe)
        .map_err(|e| map_replace_error(&current_exe, e))?;

    Ok(current_exe)
}

fn map_replace_error(path: &Path, e: std::io::Error) -> String {
    if e.kind() == std::io::ErrorKind::PermissionDenied {
        format!(
            "cannot replace {}: permission denied (try sudo or install to a writable location)",
            path.display()
        )
    } else {
        format!("cannot replace {}: {e}", path.display())
    }
}

/// Entry point for the `pkgq update` command.
///
/// On success returns a JSON value; on fatal error returns an error JSON value.
pub fn run_update() -> Result<serde_json::Value, serde_json::Value> {
    let current_version = env!("CARGO_PKG_VERSION");
    let target = target_triple();

    let release_json = match fetch_latest_release() {
        Ok(v) => v,
        Err(msg) => {
            return Err(json!({
                "command": "update",
                "error": msg,
            }));
        }
    };

    let latest_version = release_json
        .get("tag_name")
        .and_then(|v| v.as_str())
        .map(strip_v)
        .unwrap_or("unknown")
        .to_string();

    if !is_newer(&latest_version, current_version) {
        return Ok(json!({
            "command": "update",
            "current_version": current_version,
            "latest_version": latest_version,
            "target": target,
            "binary_path": std::env::current_exe().map(|p| p.to_string_lossy().to_string()).unwrap_or_default(),
            "updated": false,
            "message": "already up to date",
        }));
    }

    let (version, url) = match select_asset(&release_json, target) {
        Some(v) => v,
        None => {
            return Err(json!({
                "command": "update",
                "current_version": current_version,
                "latest_version": latest_version,
                "target": target,
                "error": format!("no release asset found for target {target}"),
            }));
        }
    };

    match download_and_replace(&url) {
        Ok(binary_path) => Ok(json!({
            "command": "update",
            "current_version": current_version,
            "latest_version": version,
            "target": target,
            "binary_path": binary_path.to_string_lossy(),
            "updated": true,
        })),
        Err(msg) => Err(json!({
            "command": "update",
            "current_version": current_version,
            "latest_version": version,
            "target": target,
            "error": msg,
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_newer_equal() {
        assert!(!is_newer("0.1.1", "0.1.1"));
    }

    #[test]
    fn is_newer_patch_bump() {
        assert!(is_newer("0.1.2", "0.1.1"));
        assert!(!is_newer("0.1.1", "0.1.2"));
    }

    #[test]
    fn is_newer_minor_bump() {
        assert!(is_newer("0.2.0", "0.1.5"));
        assert!(!is_newer("0.1.5", "0.2.0"));
    }

    #[test]
    fn is_newer_major_bump() {
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(!is_newer("0.9.9", "1.0.0"));
    }

    #[test]
    fn is_newer_non_numeric_segment() {
        assert!(is_newer("0.1.1-beta", "0.1.1-alpha"));
        assert!(!is_newer("0.1.1-alpha", "0.1.1-beta"));
    }

    #[test]
    fn is_newer_unequal_lengths() {
        assert!(is_newer("0.1.1.1", "0.1.1"));
        assert!(!is_newer("0.1.1", "0.1.1.1"));
        assert!(is_newer("0.1", "0.0.9"));
    }

    #[test]
    fn target_triple_is_known() {
        let triple = target_triple();
        assert!(
            KNOWN_TARGETS.contains(&triple),
            "unknown target triple: {triple}"
        );
    }

    #[test]
    fn select_asset_finds_match() {
        let json = json!({
            "tag_name": "v0.2.0",
            "assets": [
                {
                    "name": "pkgq-0.2.0-x86_64-unknown-linux-gnu.tar.gz",
                    "browser_download_url": "https://example.com/pkgq-0.2.0-linux-x86_64.tar.gz"
                }
            ]
        });
        let (version, url) = select_asset(&json, "x86_64-unknown-linux-gnu").unwrap();
        assert_eq!(version, "0.2.0");
        assert_eq!(url, "https://example.com/pkgq-0.2.0-linux-x86_64.tar.gz");
    }

    #[test]
    fn select_asset_missing_asset() {
        let json = json!({
            "tag_name": "v0.2.0",
            "assets": [
                {
                    "name": "pkgq-0.2.0-x86_64-apple-darwin.tar.gz",
                    "browser_download_url": "https://example.com/darwin.tar.gz"
                }
            ]
        });
        assert!(select_asset(&json, "x86_64-unknown-linux-gnu").is_none());
    }

    #[test]
    fn select_asset_strips_leading_v_from_tag() {
        let json = json!({
            "tag_name": "0.3.0",
            "assets": [
                {
                    "name": "pkgq-0.3.0-aarch64-apple-darwin.tar.gz",
                    "browser_download_url": "https://example.com/pkgq-0.3.0-darwin-arm64.tar.gz"
                }
            ]
        });
        let (version, url) = select_asset(&json, "aarch64-apple-darwin").unwrap();
        assert_eq!(version, "0.3.0");
        assert_eq!(url, "https://example.com/pkgq-0.3.0-darwin-arm64.tar.gz");
    }
}

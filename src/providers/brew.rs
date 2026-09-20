//! Homebrew provider: installed formulae/casks and catalog search.

use std::collections::HashMap;
use std::collections::HashSet;

use crate::model::{App, ManagerError, ManagerKind};
use crate::provider::Provider;
use crate::shell;

pub struct Brew;

/// Parse `brew list --versions` output into (name, version) pairs.
pub(crate) fn parse_list_versions(output: &str) -> Vec<(String, String)> {
    output
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let name = fields.next()?.trim();
            if name.is_empty() {
                return None;
            }
            // Multiple installed versions (kegs) are space-separated; keep the first.
            let version = fields.next().unwrap_or("").trim();
            Some((
                name.to_string(),
                if version.is_empty() {
                    "".to_string()
                } else {
                    version.to_string()
                },
            ))
        })
        .collect()
}

/// Parse `brew search` output: candidate names, ignoring `==>` section headers.
pub(crate) fn parse_search_output(output: &str) -> Vec<String> {
    output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("==>") && !line.starts_with(' '))
        .map(str::to_string)
        .collect()
}

/// Description and stable version per name from batched `brew info --json=v2`.
pub(crate) fn info_map(names: &[&str]) -> HashMap<String, (Option<String>, Option<String>)> {
    if names.is_empty() {
        return HashMap::new();
    }
    let args = names
        .iter()
        .map(|n| shell::quote(n))
        .collect::<Vec<_>>()
        .join(" ");
    let cmd = format!("LC_ALL=C brew info --json=v2 {args} 2>/dev/null || true");
    let Some(json) = shell::run(&cmd)
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
    else {
        return HashMap::new();
    };
    let mut map = HashMap::new();
    let entries = ["formulae", "casks"]
        .iter()
        .filter_map(|key| json.get(*key).and_then(serde_json::Value::as_array))
        .flatten();
    for entry in entries {
        let Some(name) = entry.get("name").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let desc = entry
            .get("desc")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let version = entry
            .pointer("/versions/stable")
            .and_then(serde_json::Value::as_str)
            .or_else(|| entry.get("version").and_then(serde_json::Value::as_str))
            .map(str::to_string);
        map.insert(name.to_string(), (desc, version));
    }
    map
}

/// Installed names and versions across formulae and casks (two brew calls).
pub(crate) fn installed_pairs() -> Result<Vec<(String, String)>, ManagerError> {
    let cmd = "LC_ALL=C brew list --versions --formula 2>/dev/null; LC_ALL=C brew list --versions --cask 2>/dev/null";
    let output = shell::run_managed(ManagerKind::Brew, cmd)?;
    Ok(parse_list_versions(&output))
}

fn to_app(name: &str, version: &str, description: Option<String>, installed: bool) -> App {
    App {
        usage: Some(name.to_string()),
        install: Some(format!("brew install {name}")),
        name: name.to_string(),
        manager: ManagerKind::Brew,
        installed,
        version: (!version.is_empty()).then(|| version.to_string()),
        description,
    }
}

impl Provider for Brew {
    fn kind(&self) -> ManagerKind {
        ManagerKind::Brew
    }

    fn list_installed(&self) -> Result<Vec<App>, ManagerError> {
        let pairs = installed_pairs()?;
        let names: Vec<&str> = pairs.iter().map(|(name, _)| name.as_str()).collect();
        let info = info_map(&names);
        Ok(pairs
            .iter()
            .map(|(name, version)| {
                let description = info.get(name).and_then(|(desc, _)| desc.clone());
                to_app(name, version, description, true)
            })
            .collect())
    }

    fn search(&self, query: &str) -> Result<Vec<App>, ManagerError> {
        let cmd = format!(
            "LC_ALL=C brew search {} 2>/dev/null || true",
            shell::quote(query)
        );
        let output = shell::run_managed(ManagerKind::Brew, &cmd)?;
        let names = parse_search_output(&output);
        if names.is_empty() {
            return Ok(Vec::new());
        }
        let installed: HashMap<String, String> = installed_pairs()?.into_iter().collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let info = info_map(&refs);
        Ok(names
            .iter()
            .map(|name| {
                let (description, catalog_version) =
                    info.get(name).cloned().unwrap_or((None, None));
                let is_installed = installed.contains_key(name);
                let version = if is_installed {
                    installed.get(name).cloned()
                } else {
                    None
                }
                .or(catalog_version)
                .unwrap_or_default();
                to_app(name, &version, description, is_installed)
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST_FIXTURE: &str =
        concat!("curl 8.22.0\n", "wget 1.25.0 1.24.5\n", "openssl@3 3.5.2\n",);

    const SEARCH_FIXTURE: &str = concat!(
        "==> Formulae\n",
        "awscurl\n",
        "curl\n",
        "curlcpp\n",
        "\n",
        "==> Casks\n",
        "carl\n",
        "cursr\n",
    );

    const INFO_JSON: &str = r#"{
        "formulae": [{"name": "curl", "desc": "Get a file from an HTTP, HTTPS or FTP server", "versions": {"stable": "8.22.0"}}],
        "casks": [{"name": "drift", "desc": "Edit and export videos", "version": "0.6.0"}]
    }"#;

    #[test]
    fn parses_list_versions_keeping_first_keg() {
        let rows = parse_list_versions(LIST_FIXTURE);
        assert_eq!(rows[0], ("curl".to_string(), "8.22.0".to_string()));
        assert_eq!(rows[1].1, "1.25.0");
        assert_eq!(rows[2].0, "openssl@3");
    }

    #[test]
    fn parses_search_names_ignoring_headers_and_indentation() {
        let names = parse_search_output(SEARCH_FIXTURE);
        assert_eq!(names, vec!["awscurl", "curl", "curlcpp", "carl", "cursr"]);
    }

    #[test]
    fn parses_info_json_for_formulae_and_casks() {
        let json: serde_json::Value = serde_json::from_str(INFO_JSON).unwrap();
        let mut map = HashMap::new();
        for entry in ["formulae", "casks"]
            .iter()
            .filter_map(|key| json.get(*key).and_then(serde_json::Value::as_array))
            .flatten()
        {
            let name = entry
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap()
                .to_string();
            let desc = entry
                .get("desc")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            let version = entry
                .pointer("/versions/stable")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            map.insert(name, (desc, version));
        }
        assert_eq!(map.get("curl").unwrap().1.as_deref(), Some("8.22.0"));
        assert_eq!(
            map.get("drift").unwrap().0.as_deref(),
            Some("Edit and export videos")
        );
    }
}

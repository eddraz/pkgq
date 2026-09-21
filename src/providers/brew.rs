//! Homebrew provider: installed formulae/casks and catalog search.

use std::collections::HashMap;

use crate::model::{App, ManagerError, ManagerKind};
use crate::provider::{app_matches_query, merge_installed_and_catalog, query_tokens, Provider};
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
/// Description, version and metadata for one formula/cask.
#[derive(Debug, Clone, Default)]
pub(crate) struct BrewInfo {
    pub description: Option<String>,
    pub version: Option<String>,
    pub homepage: Option<String>,
    pub license: Option<String>,
    pub origin: Option<String>,
    pub depends: Option<String>,
}

/// Brew licenses are either a string or an array of strings.
fn license_field(value: Option<&serde_json::Value>) -> Option<String> {
    match value? {
        serde_json::Value::String(text) => Some(text.clone()),
        serde_json::Value::Array(items) => {
            let joined = items
                .iter()
                .filter_map(serde_json::Value::as_str)
                .collect::<Vec<_>>()
                .join(", ");
            (!joined.is_empty()).then_some(joined)
        }
        _ => None,
    }
}

pub(crate) fn info_map(names: &[&str]) -> HashMap<String, BrewInfo> {
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
        let info = BrewInfo {
            description: entry
                .get("desc")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            version: entry
                .pointer("/versions/stable")
                .and_then(serde_json::Value::as_str)
                .or_else(|| entry.get("version").and_then(serde_json::Value::as_str))
                .map(str::to_string),
            homepage: entry
                .get("homepage")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            license: license_field(entry.get("license")),
            origin: entry
                .get("tap")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            depends: entry
                .get("dependencies")
                .and_then(serde_json::Value::as_array)
                .map(|deps| {
                    deps.iter()
                        .filter_map(serde_json::Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .filter(|joined| !joined.is_empty()),
        };
        map.insert(name.to_string(), info);
    }
    map
}

/// Installed names and versions across formulae and casks (two brew calls).
pub(crate) fn installed_pairs() -> Result<Vec<(String, String)>, ManagerError> {
    let cmd = "LC_ALL=C brew list --versions --formula 2>/dev/null; LC_ALL=C brew list --versions --cask 2>/dev/null";
    let output = shell::run_managed(ManagerKind::Brew, cmd)?;
    Ok(parse_list_versions(&output))
}

fn to_app(
    name: &str,
    version: &str,
    info: Option<&BrewInfo>,
    installed: bool,
    installed_bytes: Option<u64>,
) -> App {
    let brew_info = info.cloned().unwrap_or_default();
    App {
        usage: Some(name.to_string()),
        install: Some(format!("brew install {name}")),
        installed_bytes,
        download_bytes: None,
        homepage: brew_info.homepage,
        license: brew_info.license,
        origin: brew_info.origin,
        arch: None,
        maintainer: None,
        section: None,
        depends: brew_info.depends,
        install_date: None,
        available_version: None,
        name: name.to_string(),
        manager: ManagerKind::Brew,
        installed,
        version: (!version.is_empty()).then(|| version.to_string()),
        description: brew_info.description,
    }
}

/// On-disk size per installed formula/cask via one batched `du -sk` over the
/// Cellar and Caskroom directories.
pub(crate) fn installed_size_map(names: &[&str]) -> HashMap<String, u64> {
    if names.is_empty() {
        return HashMap::new();
    }
    let list = names
        .iter()
        .map(|n| shell::quote(n))
        .collect::<Vec<_>>()
        .join(" ");
    let cmd = format!(
        "for n in {list}; do p=\"$(brew --cellar)/$n\"; [ -d \"$p\" ] || p=\"$(brew --caskroom)/$n\"; [ -d \"$p\" ] && du -sk \"$p\" 2>/dev/null; done"
    );
    let output = shell::run(&cmd).unwrap_or_default();
    output
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(2, '\t');
            let kib = parts.next()?.trim().parse::<u64>().ok()?;
            let path = parts.next()?.trim();
            let name = path.rsplit('/').next()?;
            if name.is_empty() {
                return None;
            }
            Some((name.to_string(), kib * 1024))
        })
        .collect()
}

impl Provider for Brew {
    fn kind(&self) -> ManagerKind {
        ManagerKind::Brew
    }

    fn list_installed(&self) -> Result<Vec<App>, ManagerError> {
        let pairs = installed_pairs()?;
        let names: Vec<&str> = pairs.iter().map(|(name, _)| name.as_str()).collect();
        let info = info_map(&names);
        let sizes = installed_size_map(&names);
        Ok(pairs
            .iter()
            .map(|(name, version)| {
                let brew_info = info.get(name);
                to_app(name, version, brew_info, true, sizes.get(name).copied())
            })
            .collect())
    }

    fn search(&self, query: &str) -> Result<Vec<App>, ManagerError> {
        let tokens = query_tokens(query);
        if tokens.is_empty() {
            return Ok(Vec::new());
        }
        let installed: HashMap<String, String> = installed_pairs()?.into_iter().collect();
        // Installed kegs absent from catalog output still match by name.
        let matched: Vec<String> = installed
            .keys()
            .filter(|name| app_matches_query(name, None, &tokens))
            .cloned()
            .collect();
        let matched_names: Vec<&str> = matched.iter().map(String::as_str).collect();
        let sizes = installed_size_map(&matched_names);
        let matched_info = info_map(&matched_names);
        let installed_apps: Vec<App> = matched
            .iter()
            .map(|name| {
                let brew_info = matched_info.get(name);
                let size = sizes.get(name).copied();
                let version = installed.get(name).cloned().unwrap_or_default();
                to_app(name, &version, brew_info, true, size)
            })
            .collect();
        let cmd = format!(
            "LC_ALL=C brew search {} 2>/dev/null || true",
            shell::quote(&tokens.join(" "))
        );
        let output = shell::run_managed(ManagerKind::Brew, &cmd)?;
        let names = parse_search_output(&output);
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let info = info_map(&refs);
        let catalog: Vec<App> = names
            .iter()
            .map(|name| {
                let brew_info = info.get(name).cloned().unwrap_or_default();
                let is_installed = installed.contains_key(name);
                let version = if is_installed {
                    installed.get(name).cloned()
                } else {
                    None
                }
                .or(brew_info.version.clone())
                .unwrap_or_default();
                to_app(name, &version, Some(&brew_info), is_installed, None)
            })
            .collect();
        Ok(merge_installed_and_catalog(installed_apps, catalog))
    }
    fn outdated(&self) -> Result<Vec<App>, ManagerError> {
        let cmd = "LC_ALL=C brew outdated --json=v2 2>/dev/null || true";
        let output = shell::run_managed(ManagerKind::Brew, cmd)?;
        let trimmed = output.trim();
        if trimmed.is_empty() || trimmed == "[]" {
            return Ok(Vec::new());
        }
        let Ok(json) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            return Ok(Vec::new());
        };
        let mut apps = Vec::new();
        for section in ["formulae", "casks"] {
            let Some(entries) = json.get(section).and_then(serde_json::Value::as_array) else {
                continue;
            };
            for entry in entries {
                let Some(name) = entry.get("name").and_then(serde_json::Value::as_str) else {
                    continue;
                };
                let installed_version = entry
                    .get("installed")
                    .and_then(serde_json::Value::as_array)
                    .and_then(|versions| versions.first())
                    .and_then(|installed| installed.get("version"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string);
                let available_version = entry
                    .get("current_version")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string);
                let mut app = to_app(
                    name,
                    &installed_version.clone().unwrap_or_default(),
                    None,
                    true,
                    None,
                );
                app.available_version = available_version;
                apps.push(app);
            }
        }
        Ok(apps)
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

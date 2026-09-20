//! apt provider: search over the available Debian catalog.
//!
//! Installed debs belong to the dpkg provider (same database); apt adds
//! catalog search with install commands and candidate versions. All parsing
//! runs against `LC_ALL=C` output so labels never depend on the user locale.

use std::collections::HashMap;

use crate::model::{App, ManagerError, ManagerKind};
use crate::provider::Provider;
use crate::providers::dpkg;
use crate::shell;

pub struct Apt;

/// One `apt-cache search` hit: package name and short description.
pub(crate) fn parse_search_output(output: &str) -> Vec<(String, Option<String>)> {
    output
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                return None;
            }
            Some(match line.split_once(" - ") {
                Some((name, description)) => (name.to_string(), Some(description.to_string())),
                None => (line.to_string(), None),
            })
        })
        .collect()
}

/// Installed flag plus installed version (when present) per package.
pub(crate) fn deb_status_map(names: &[&str]) -> HashMap<String, (bool, Option<String>)> {
    if names.is_empty() {
        return HashMap::new();
    }
    let args = names
        .iter()
        .map(|n| shell::quote(n))
        .collect::<Vec<_>>()
        .join(" ");
    let cmd = format!(
        "LC_ALL=C dpkg-query -W -f='${{Package}}\\t${{db:Status-Abbrev}}\\t${{Version}}\\n' {args} 2>/dev/null || true"
    );
    let output = shell::run(&cmd).unwrap_or_default();
    parse_status_output(&output)
}

/// Parse the batched dpkg-query status output used by the apt provider.
pub(crate) fn parse_status_output(output: &str) -> HashMap<String, (bool, Option<String>)> {
    output
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, '\t');
            let name = parts.next()?.trim();
            if name.is_empty() {
                return None;
            }
            let installed = parts.next()?.trim().starts_with("ii");
            let version = parts
                .next()
                .map(str::trim)
                .filter(|v| !v.is_empty() && *v != "<none>")
                .map(str::to_string);
            Some((name.to_string(), (installed, version)))
        })
        .collect()
}

/// Candidate version per package from a batched `apt-cache policy` run.
pub(crate) fn apt_policy_map(names: &[&str]) -> HashMap<String, PolicyBlock> {
    if names.is_empty() {
        return HashMap::new();
    }
    let args = names
        .iter()
        .map(|n| shell::quote(n))
        .collect::<Vec<_>>()
        .join(" ");
    let cmd = format!("LC_ALL=C apt-cache policy {args} 2>/dev/null || true");
    let output = shell::run(&cmd).unwrap_or_default();
    parse_policy_output(&output)
}

/// Candidate version extracted from one policy block.
#[derive(Debug, Default, Clone)]
pub(crate) struct PolicyBlock {
    pub installed: Option<String>,
    pub candidate: Option<String>,
}

/// Parse batched `apt-cache policy` output into per-package blocks.
pub(crate) fn parse_policy_output(output: &str) -> HashMap<String, PolicyBlock> {
    let mut map: HashMap<String, PolicyBlock> = HashMap::new();
    let mut current: Option<String> = None;
    for line in output.lines() {
        if line.starts_with(' ') {
            let Some(name) = current.clone() else {
                continue;
            };
            let trimmed = line.trim();
            let Some(block) = map.get_mut(&name) else {
                continue;
            };
            if let Some(v) = trimmed.strip_prefix("Candidate:") {
                block.candidate = non_none(v.trim());
            } else if let Some(v) = trimmed.strip_prefix("Installed:") {
                block.installed = non_none(v.trim());
            }
        } else if let Some(name) = line.strip_suffix(':') {
            if !name.is_empty() {
                current = Some(name.to_string());
                map.entry(name.to_string()).or_default();
            }
        }
    }
    map
}

fn non_none(value: &str) -> Option<String> {
    (!value.is_empty() && value != "(none)").then(|| value.to_string())
}

impl Provider for Apt {
    fn kind(&self) -> ManagerKind {
        ManagerKind::Apt
    }

    fn list_installed(&self) -> Result<Vec<App>, ManagerError> {
        // The dpkg provider already inventories every installed deb; emitting
        // them again here would duplicate results.
        Ok(Vec::new())
    }

    fn search(&self, query: &str) -> Result<Vec<App>, ManagerError> {
        let cmd = format!(
            "LC_ALL=C apt-cache search {} 2>/dev/null || true",
            shell::quote(query)
        );
        let hits = parse_search_output(&shell::run_managed(ManagerKind::Apt, &cmd)?);
        if hits.is_empty() {
            return Ok(Vec::new());
        }

        let names: Vec<&str> = hits.iter().map(|(name, _)| name.as_str()).collect();
        let status = deb_status_map(&names);
        let policy = apt_policy_map(&names);
        let bins = dpkg::binary_map();

        Ok(hits
            .into_iter()
            .map(|(name, description)| {
                let (installed, installed_version) =
                    status.get(&name).cloned().unwrap_or((false, None));
                let candidate = || policy.get(&name).and_then(|b| b.candidate.clone());
                let version = if installed {
                    installed_version.or_else(candidate)
                } else {
                    candidate()
                };
                App {
                    usage: installed.then(|| bins.get(&name).cloned()).flatten(),
                    install: Some(format!("sudo apt install {name}")),
                    name,
                    manager: ManagerKind::Apt,
                    installed,
                    version,
                    description,
                }
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEARCH_FIXTURE: &str = concat!(
        "curl - command line tool for transferring data with URL syntax\n",
        "curlpp - C++ bindings for curl\n",
        "weirdpkg\n",
    );

    const STATUS_FIXTURE: &str = concat!("curl\tii \t8.14.1-2+deb13u5\n", "curlpp\tun \t<none>\n",);

    const POLICY_FIXTURE: &str = concat!(
        "curl:\n",
        "  Installed: 8.14.1-2+deb13u5\n",
        "  Candidate: 8.21.0-2~bpo13+1\n",
        "  Version table:\n",
        " *** 8.14.1-2+deb13u5 500\n",
        "vim:\n",
        "  Installed: (none)\n",
        "  Candidate: 2:9.1.1230-2\n",
    );

    #[test]
    fn parses_search_hits_with_and_without_description() {
        let hits = parse_search_output(SEARCH_FIXTURE);
        assert_eq!(hits.len(), 3);
        assert_eq!(hits[0].0, "curl");
        assert_eq!(
            hits[0].1.as_deref(),
            Some("command line tool for transferring data with URL syntax")
        );
        assert_eq!(hits[2].1, None);
    }

    #[test]
    fn parses_status_map_with_installed_flag() {
        let map = parse_status_output(STATUS_FIXTURE);
        assert_eq!(
            map.get("curl"),
            Some(&(true, Some("8.14.1-2+deb13u5".into())))
        );
        assert_eq!(map.get("curlpp"), Some(&(false, None)));
    }

    #[test]
    fn parses_policy_blocks_and_ignores_none() {
        let map = parse_policy_output(POLICY_FIXTURE);
        let curl = map.get("curl").unwrap();
        assert_eq!(curl.installed.as_deref(), Some("8.14.1-2+deb13u5"));
        assert_eq!(curl.candidate.as_deref(), Some("8.21.0-2~bpo13+1"));
        let vim = map.get("vim").unwrap();
        assert_eq!(vim.installed, None);
        assert_eq!(vim.candidate.as_deref(), Some("2:9.1.1230-2"));
    }

    #[test]
    fn version_table_line_is_not_confused_with_fields() {
        let map = parse_policy_output(POLICY_FIXTURE);
        // "Version table:" line starts with a space and must not create fields.
        assert!(map.get("curl").unwrap().candidate.as_deref() != Some("table:"));
    }
}

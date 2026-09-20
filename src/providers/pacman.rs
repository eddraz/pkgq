//! pacman provider (Arch): installed inventory and sync-database search.

use std::collections::HashMap;
use std::collections::HashSet;

use crate::model::{App, ManagerError, ManagerKind};
use crate::provider::Provider;
use crate::shell;

pub struct Pacman;

/// Parse `pacman -Q` output into (name, version) pairs.
pub(crate) fn parse_query_list(output: &str) -> Vec<(String, String)> {
    output
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let name = fields.next()?.trim();
            if name.is_empty() {
                return None;
            }
            let version = fields.next().unwrap_or("").trim();
            Some((name.to_string(), version.to_string()))
        })
        .collect()
}

/// One `pacman -Ss` hit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SyncHit {
    pub name: String,
    pub version: String,
    pub installed: bool,
    pub description: Option<String>,
}

/// Parse `pacman -Ss` output: `repo/name version [installed]` plus indented
/// description lines.
pub(crate) fn parse_sync_search(output: &str) -> Vec<SyncHit> {
    let mut hits: Vec<SyncHit> = Vec::new();
    for line in output.lines() {
        if line.starts_with(' ') || line.is_empty() {
            if let Some(last) = hits.last_mut() {
                if last.description.is_none() {
                    last.description = Some(line.trim().to_string());
                }
            }
            continue;
        }
        let Some((qualified, rest)) = line.split_once(' ') else {
            continue;
        };
        let Some((_, name)) = qualified.split_once('/') else {
            continue;
        };
        let rest = rest.trim();
        let (version, installed) = match rest.strip_prefix("[").and_then(|v| v.strip_suffix("]")) {
            Some(inner) if inner == "installed" => ("", true),
            _ => {
                let mut fields = rest.split_whitespace();
                let version = fields.next().unwrap_or("");
                let installed = rest.contains("[installed");
                (version, installed)
            }
        };
        hits.push(SyncHit {
            name: name.to_string(),
            version: version.to_string(),
            installed,
            description: None,
        });
    }
    hits
}

/// Short description per installed package, batched through one bash loop of
/// `pacman -Qi` calls.
pub(crate) fn description_map(names: &[&str]) -> HashMap<String, String> {
    if names.is_empty() {
        return HashMap::new();
    }
    let list = names
        .iter()
        .map(|n| shell::quote(n))
        .collect::<Vec<_>>()
        .join(" ");
    let cmd = format!(
        "for p in {list}; do echo \"== $p\"; LC_ALL=C pacman -Qi \"$p\" 2>/dev/null | sed -n 's/^Description[[:space:]]*:[[:space:]]*//p'; done"
    );
    let output = shell::run(&cmd).unwrap_or_default();
    parse_blocks_output(&output)
}

/// Parse `== name` / single-line value blocks.
pub(crate) fn parse_blocks_output(output: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let mut current: Option<String> = None;
    for line in output.lines() {
        if let Some(name) = line.strip_prefix("== ") {
            current = Some(name.trim().to_string());
        } else if let Some(name) = current.clone() {
            let value = line.trim();
            if !value.is_empty() {
                map.insert(name, value.to_string());
                current = None;
            }
        }
    }
    map
}

/// First executable per installed package, one grep over pacman's file lists.
pub(crate) fn binary_map() -> HashMap<String, String> {
    let cmd = "grep -H -m1 -E '/s?bin/.' /var/lib/pacman/local/*/files 2>/dev/null || true";
    let output = shell::run(&cmd).unwrap_or_default();
    parse_files_output(&output)
}

/// Parse `/var/lib/pacman/local/curl-8.14.1-1/files:usr/bin/curl` lines.
pub(crate) fn parse_files_output(output: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for line in output.lines() {
        let Some(rest) = line.strip_prefix("/var/lib/pacman/local/") else {
            continue;
        };
        let Some((dir, binary)) = rest.split_once("/files:") else {
            continue;
        };
        // Directory layout is `<name>-<pkgver>-<pkgrel>`; pkgver may itself
        // contain dashes, so strip exactly two trailing dash segments.
        let Some(pkg) = dir
            .rsplit_once('-')
            .and_then(|(rest, _rel)| rest.rsplit_once('-').map(|(name, _ver)| name))
        else {
            continue;
        };
        if pkg.is_empty() || binary.is_empty() {
            continue;
        }
        let Some(bin_name) = binary.rsplit('/').next().filter(|name| !name.is_empty()) else {
            continue;
        };
        map.entry(pkg.to_string())
            .or_insert_with(|| bin_name.to_string());
    }
    map
}

impl Provider for Pacman {
    fn kind(&self) -> ManagerKind {
        ManagerKind::Pacman
    }

    fn list_installed(&self) -> Result<Vec<App>, ManagerError> {
        let output = shell::run_managed(ManagerKind::Pacman, "LC_ALL=C pacman -Q")?;
        let rows = parse_query_list(&output);
        let names: Vec<&str> = rows.iter().map(|(name, _)| name.as_str()).collect();
        let descriptions = description_map(&names);
        let bins = binary_map();
        Ok(rows
            .into_iter()
            .map(|(name, version)| {
                let usage = bins.get(&name).cloned();
                let description = descriptions.get(&name).cloned();
                App {
                    install: Some(format!("sudo pacman -S {name}")),
                    name,
                    manager: ManagerKind::Pacman,
                    installed: true,
                    version: Some(version),
                    description,
                    usage,
                }
            })
            .collect())
    }

    fn search(&self, query: &str) -> Result<Vec<App>, ManagerError> {
        let cmd = format!(
            "LC_ALL=C pacman -Ss {} 2>/dev/null || true",
            shell::quote(query)
        );
        let output = shell::run_managed(ManagerKind::Pacman, &cmd)?;
        let hits = parse_sync_search(&output);
        if hits.is_empty() {
            return Ok(Vec::new());
        }
        let installed: HashSet<String> = parse_query_list(
            &shell::run("LC_ALL=C pacman -Q 2>/dev/null || true").unwrap_or_default(),
        )
        .into_iter()
        .map(|(name, _)| name)
        .collect();
        let bins = binary_map();
        Ok(hits
            .into_iter()
            .map(|hit| {
                // `[installed]` in -Ss output is authoritative when present;
                // otherwise cross-check the local query (partial upgrades).
                let installed = hit.installed || installed.contains(&hit.name);
                App {
                    usage: bins.get(&hit.name).cloned(),
                    install: Some(format!("sudo pacman -S {}", hit.name)),
                    name: hit.name,
                    manager: ManagerKind::Pacman,
                    installed,
                    version: (!hit.version.is_empty()).then_some(hit.version),
                    description: hit.description,
                }
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const QUERY_FIXTURE: &str = "curl 8.14.1-2\nbash 5.2.37-1\n";

    const SYNC_FIXTURE: &str = concat!(
        "core/curl 8.14.1-2 [installed: 8.14.1-1]\n",
        "    Command line tool and library for transferring data with URLs\n",
        "extra/curlsharp 1.0-3\n",
        "    A sharp wrapper\n",
        "extra/curlpp 1.2-1 [installed]\n",
        "    C++ bindings\n",
    );

    const FILES_FIXTURE: &str = concat!(
        "/var/lib/pacman/local/curl-8.14.1-1/files:usr/bin/\n",
        "/var/lib/pacman/local/curl-8.14.1-1/files:usr/bin/curl\n",
        "/var/lib/pacman/local/bzip2-1.0.8-3/files:usr/bin/bzip2\n",
    );

    #[test]
    fn parses_query_pairs() {
        let rows = parse_query_list(QUERY_FIXTURE);
        assert_eq!(rows[0], ("curl".to_string(), "8.14.1-2".to_string()));
    }

    #[test]
    fn parses_sync_hits_with_installed_markers() {
        let hits = parse_sync_search(SYNC_FIXTURE);
        assert_eq!(hits.len(), 3);
        assert!(hits[0].installed);
        assert_eq!(hits[0].version, "8.14.1-2");
        assert_eq!(
            hits[0].description.as_deref(),
            Some("Command line tool and library for transferring data with URLs")
        );
        assert!(!hits[1].installed);
        assert!(hits[2].installed);
    }

    #[test]
    fn parses_pacman_files_skipping_directories() {
        let map = parse_files_output(FILES_FIXTURE);
        // First line is the `usr/bin/` directory (trailing char rule drops it).
        assert_eq!(map.get("curl").map(String::as_str), Some("curl"));
        assert_eq!(map.get("bzip2").map(String::as_str), Some("bzip2"));
    }

    #[test]
    fn strips_version_from_package_directory() {
        let map = parse_files_output("/var/lib/pacman/local/vim-9.1.0821-1/files:usr/bin/vim\n");
        assert_eq!(map.get("vim").map(String::as_str), Some("vim"));
    }
}

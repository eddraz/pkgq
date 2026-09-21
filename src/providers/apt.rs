//! apt provider: search over the available Debian catalog.
//!
//! Installed debs belong to the dpkg provider (same database); apt adds
//! catalog search with install commands and candidate versions. All parsing
//! runs against `LC_ALL=C` output so labels never depend on the user locale.

use std::collections::HashMap;

use crate::model::{App, ManagerError, ManagerKind};
use crate::provider::{query_tokens, Provider};
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

/// Sizes and metadata per package from a batched `apt-cache show` run.
pub(crate) fn apt_show_details_map(names: &[&str]) -> HashMap<String, AptShowDetails> {
    if names.is_empty() {
        return HashMap::new();
    }
    let args = names
        .iter()
        .map(|n| shell::quote(n))
        .collect::<Vec<_>>()
        .join(" ");
    let cmd = format!("LC_ALL=C apt-cache show {args} 2>/dev/null || true");
    let output = shell::run(&cmd).unwrap_or_default();
    parse_show_details_output(&output)
}

/// Details parsed from one `apt-cache show` block.
#[derive(Debug, Clone, Default)]
pub(crate) struct AptShowDetails {
    pub installed_bytes: Option<u64>,
    pub download_bytes: Option<u64>,
    pub homepage: Option<String>,
    pub arch: Option<String>,
    pub maintainer: Option<String>,
    pub section: Option<String>,
    pub depends: Option<String>,
}

/// Parse `apt-cache show` blocks: `Package:` starts an entry, `Installed-Size:`
/// is reported in KiB and `Size:` in bytes; the first block per package wins.
pub(crate) fn parse_show_details_output(output: &str) -> HashMap<String, AptShowDetails> {
    let mut map: HashMap<String, AptShowDetails> = HashMap::new();
    let mut current: Option<String> = None;
    for line in output.lines() {
        if let Some(name) = line.strip_prefix("Package: ") {
            current = Some(name.trim().to_string());
            map.entry(name.trim().to_string()).or_default();
        } else if let Some(name) = current.clone() {
            let Some(entry) = map.get_mut(&name) else {
                continue;
            };
            let trimmed = line.trim();
            if let Some(kib) = trimmed.strip_prefix("Installed-Size: ") {
                if entry.installed_bytes.is_none() {
                    entry.installed_bytes = kib.trim().parse::<u64>().ok().map(|k| k * 1024);
                }
            } else if let Some(bytes) = trimmed.strip_prefix("Size: ") {
                if entry.download_bytes.is_none() {
                    entry.download_bytes = bytes.trim().parse::<u64>().ok();
                }
            }
            let text_field = |prefix: &str| {
                trimmed
                    .strip_prefix(prefix)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
            };
            if entry.homepage.is_none() {
                entry.homepage = text_field("Homepage: ");
            }
            if entry.arch.is_none() {
                entry.arch = text_field("Architecture: ");
            }
            if entry.maintainer.is_none() {
                entry.maintainer = text_field("Maintainer: ");
            }
            if entry.section.is_none() {
                entry.section = text_field("Section: ");
            }
            if entry.depends.is_none() {
                entry.depends = text_field("Depends: ");
            }
        }
    }
    map
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
        // Catalog queries use the expanded tokens (stopwords/synonyms applied)
        // so managers' own AND matching sees meaningful terms only.
        let tokens = query_tokens(query);
        if tokens.is_empty() {
            return Ok(Vec::new());
        }
        let cmd = format!(
            "LC_ALL=C apt-cache search {} 2>/dev/null || true",
            shell::quote(&tokens.join(" "))
        );
        let hits = parse_search_output(&shell::run_managed(ManagerKind::Apt, &cmd)?);
        if hits.is_empty() {
            return Ok(Vec::new());
        }

        let names: Vec<&str> = hits.iter().map(|(name, _)| name.as_str()).collect();
        let status = deb_status_map(&names);
        let policy = apt_policy_map(&names);
        let show = apt_show_details_map(&names);
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
                // Installed apps report their on-disk size, available ones the
                // download size of the .deb.
                let show_details = show.get(&name);
                let installed_bytes = if installed {
                    show_details.and_then(|d| d.installed_bytes)
                } else {
                    None
                };
                let download_bytes = if installed {
                    None
                } else {
                    show_details.and_then(|d| d.download_bytes)
                };
                App {
                    usage: installed.then(|| bins.get(&name).cloned()).flatten(),
                    install: Some(format!("sudo apt install {name}")),
                    installed_bytes,
                    download_bytes,
                    homepage: show_details.and_then(|d| d.homepage.clone()),
                    license: None,
                    origin: None,
                    arch: show_details.and_then(|d| d.arch.clone()),
                    maintainer: show_details.and_then(|d| d.maintainer.clone()),
                    section: show_details.and_then(|d| d.section.clone()),
                    depends: show_details.and_then(|d| d.depends.clone()),
                    install_date: None,
                    available_version: None,
                    matched_tokens: Vec::new(),
                    confidence: None,
                    name,
                    manager: ManagerKind::Apt,
                    installed,
                    version,
                    description,
                }
            })
            .collect())
    }
    fn outdated(&self) -> Result<Vec<App>, ManagerError> {
        let cmd = "LC_ALL=C apt list --upgradable 2>/dev/null || true";
        let output = shell::run_managed(ManagerKind::Apt, cmd)?;
        let upgrades = parse_upgradable_output(&output);
        if upgrades.is_empty() {
            return Ok(Vec::new());
        }
        let names: Vec<&str> = upgrades.iter().map(|u| u.name.as_str()).collect();
        let details = apt_show_details_map(&names);
        let bins = dpkg::binary_map();
        Ok(upgrades
            .into_iter()
            .map(|u| {
                let show = details.get(&u.name);
                App {
                    usage: bins.get(&u.name).cloned(),
                    install: Some(format!("sudo apt install {}", u.name)),
                    installed_bytes: show.and_then(|d| d.installed_bytes),
                    download_bytes: show.and_then(|d| d.download_bytes),
                    homepage: show.and_then(|d| d.homepage.clone()),
                    license: None,
                    origin: None,
                    arch: show.and_then(|d| d.arch.clone()),
                    maintainer: show.and_then(|d| d.maintainer.clone()),
                    section: show.and_then(|d| d.section.clone()),
                    depends: show.and_then(|d| d.depends.clone()),
                    install_date: None,
                    name: u.name,
                    manager: ManagerKind::Apt,
                    installed: true,
                    version: u.installed_version,
                    description: None,
                    available_version: u.available_version,
                    matched_tokens: Vec::new(),
                    confidence: None,
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

    const SHOW_FIXTURE: &str = concat!(
        "Package: curl\n",
        "Installed-Size: 518\n",
        "Size: 289096\n",
        "\n",
        "Package: curl\n",
        "Installed-Size: 519\n",
        "Size: 290000\n",
    );

    #[test]
    fn parses_show_sizes_keeping_first_block() {
        let map = parse_show_details_output(SHOW_FIXTURE);
        let curl = map.get("curl").unwrap();
        assert_eq!(curl.installed_bytes, Some(518 * 1024));
        assert_eq!(curl.download_bytes, Some(289096));
    }

    #[test]
    fn version_table_line_is_not_confused_with_fields() {
        let map = parse_policy_output(POLICY_FIXTURE);
        // "Version table:" line starts with a space and must not create fields.
        assert!(map.get("curl").unwrap().candidate.as_deref() != Some("table:"));
    }
}

/// One `apt list --upgradable` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Upgrade {
    pub name: String,
    pub installed_version: Option<String>,
    pub available_version: Option<String>,
}

/// Parse `apt list --upgradable` output, skipping the `Listing...` header.
pub(crate) fn parse_upgradable_output(output: &str) -> Vec<Upgrade> {
    output
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || !line.contains('/') || line.starts_with("Listing") {
                return None;
            }
            let name = line.split('/').next()?.trim().to_string();
            if name.is_empty() {
                return None;
            }
            let mut fields = line.split_whitespace();
            fields.next();
            let available_version = fields.next().map(str::to_string);
            let installed_version = line
                .split("[upgradable from: ")
                .nth(1)
                .and_then(|rest| rest.strip_suffix(']'))
                .map(str::to_string);
            Some(Upgrade {
                name,
                installed_version,
                available_version,
            })
        })
        .collect()
}

#[cfg(test)]
mod outdated_tests {
    use super::*;

    #[test]
    fn parses_upgradable_rows_with_from_versions() {
        let fixture = concat!(
            "Listing... Done\n",
            "curl/trixie 8.21.0 amd64 [upgradable from: 8.14.1-2+deb13u5]\n",
            "vim/stable 2:9.1.0 amd64\n",
        );
        let rows = parse_upgradable_output(fixture);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "curl");
        assert_eq!(
            rows[0].installed_version.as_deref(),
            Some("8.14.1-2+deb13u5")
        );
        assert_eq!(rows[0].available_version.as_deref(), Some("8.21.0"));
        assert_eq!(rows[1].installed_version, None);
    }
}

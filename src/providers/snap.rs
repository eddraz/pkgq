//! snap provider: installed snaps and the Snap Store catalog search.
//!
//! `snap list` and `snap find` emit space-aligned tables (with tabs possible
//! on some versions), so parsing splits on any whitespace: name, version,
//! publisher and notes are fixed-position fields and the rest of the line is
//! the summary/description. `snap list` prints chatter instead of a table
//! when no snaps exist, which parses as an empty inventory.

use std::collections::HashMap;
use std::collections::HashSet;

use crate::model::{parse_human_size, App, ManagerError, ManagerKind};
use crate::provider::{app_matches_query, merge_installed_and_catalog, query_tokens, Provider};
use crate::shell;

pub struct Snap;

const LIST_CMD: &str = "LC_ALL=C snap list";
const FIND_COLUMNS: usize = 4;

/// Skip the first `n` whitespace-separated fields and return the remainder.
pub(crate) fn rest_after_fields(line: &str, n: usize) -> &str {
    let mut rest = line.trim_start();
    for _ in 0..n {
        match rest.find(char::is_whitespace) {
            Some(end) => rest = rest[end..].trim_start(),
            None => return "",
        }
    }
    rest
}

/// Parse `snap list` output into (name, version) pairs.
pub(crate) fn parse_snap_list(output: &str) -> Vec<(String, String)> {
    let mut rows = Vec::new();
    let mut in_table = false;
    for line in output.lines() {
        if !in_table {
            // The table starts at the fixed header row; chatter before it is ignored.
            in_table = rest_after_fields(line, 1) == "Version"
                || line.starts_with("Name ")
                || line.starts_with("Name\t");
            continue;
        }
        let mut fields = line.split_whitespace();
        let Some(name) = fields.next() else { continue };
        let Some(version) = fields.next() else {
            continue;
        };
        rows.push((name.to_string(), version.to_string()));
    }
    rows
}

/// Parse `snap find` output into (name, version, summary) rows.
pub(crate) fn parse_snap_find(output: &str) -> Vec<(String, String, String)> {
    let mut rows = Vec::new();
    for line in output.lines() {
        let mut fields = line.split_whitespace();
        let Some(name) = fields.next() else { continue };
        if name == "Name" {
            continue; // header row
        }
        let Some(version) = fields.next() else {
            continue;
        };
        let summary = rest_after_fields(line, FIND_COLUMNS).trim();
        let summary = if summary.is_empty() {
            None
        } else {
            Some(summary)
        };
        if let Some(summary) = summary {
            rows.push((name.to_string(), version.to_string(), summary.to_string()));
        }
    }
    rows
}

/// Short summary per snap name, batched through a single bash invocation.
pub(crate) fn details_map(names: &[&str]) -> HashMap<String, SnapDetails> {
    if names.is_empty() {
        return HashMap::new();
    }
    let list = names
        .iter()
        .map(|n| shell::quote(n))
        .collect::<Vec<_>>()
        .join(" ");
    let cmd = format!(
        "for s in {list}; do echo \"== $s\"; info=$(LC_ALL=C snap info \"$s\" 2>/dev/null); echo \"S: $(echo \"$info\" | sed -n 's/^summary:[[:space:]]*//p')\"; echo \"I: $(echo \"$info\" | sed -n 's/^installed-size:[[:space:]]*//p')\"; echo \"D: $(echo \"$info\" | grep -oE '[0-9]+([.][0-9]+)? ?[kKMGTPE]?B' | head -n 1)\"; echo \"L: $(echo \"$info\" | sed -n 's/^license:[[:space:]]*//p')\"; echo \"P: $(echo \"$info\" | sed -n 's/^publisher:[[:space:]]*//p')\"; echo \"W: $(echo \"$info\" | sed -n 's/^store-url:[[:space:]]*//p')\"; done"
    );
    let output = shell::run(&cmd).unwrap_or_default();
    parse_details_output(&output)
}

/// Summary and sizes for one snap. `installed_bytes` comes from the
/// `installed-size` field (present on installed snaps); `download_bytes`
/// from the stable-channel listing.
#[derive(Debug, Clone, Default)]
pub(crate) struct SnapDetails {
    pub summary: Option<String>,
    pub installed_bytes: Option<u64>,
    pub download_bytes: Option<u64>,
    pub license: Option<String>,
    pub origin: Option<String>,
    pub homepage: Option<String>,
}

/// Parse the `== name` / `S:` / `I:` / `D:` blocks produced by [`details_map`].
pub(crate) fn parse_details_output(output: &str) -> HashMap<String, SnapDetails> {
    let mut map: HashMap<String, SnapDetails> = HashMap::new();
    let mut current: Option<String> = None;
    for line in output.lines() {
        if let Some(name) = line.strip_prefix("== ") {
            current = Some(name.trim().to_string());
            map.entry(name.trim().to_string()).or_default();
        } else if let Some(name) = current.clone() {
            let Some(entry) = map.get_mut(&name) else {
                continue;
            };
            if let Some(summary) = line.strip_prefix("S: ") {
                let summary = summary.trim();
                if !summary.is_empty() {
                    entry.summary = Some(summary.to_string());
                }
            } else if let Some(license) = line.strip_prefix("L: ") {
                let license = license.trim();
                if !license.is_empty() {
                    entry.license = Some(license.to_string());
                }
            } else if let Some(publisher) = line.strip_prefix("P: ") {
                let publisher = publisher.trim();
                if !publisher.is_empty() {
                    entry.origin = Some(publisher.to_string());
                }
            } else if let Some(website) = line.strip_prefix("W: ") {
                let website = website.trim();
                if !website.is_empty() {
                    entry.homepage = Some(website.to_string());
                }
            } else if let Some(installed) = line.strip_prefix("I: ") {
                let installed = installed.trim();
                let parsed = installed
                    .parse::<u64>()
                    .ok()
                    .or_else(|| parse_human_size(installed));
                if parsed.is_some() {
                    entry.installed_bytes = parsed;
                }
            } else if let Some(download) = line.strip_prefix("D: ") {
                entry.download_bytes = parse_human_size(download.trim());
            }
        }
    }
    map
}

impl Provider for Snap {
    fn kind(&self) -> ManagerKind {
        ManagerKind::Snap
    }

    fn list_installed(&self) -> Result<Vec<App>, ManagerError> {
        let rows = parse_snap_list(&shell::run_managed(ManagerKind::Snap, LIST_CMD)?);
        let names: Vec<&str> = rows.iter().map(|(name, _)| name.as_str()).collect();
        let details = details_map(&names);
        Ok(rows
            .into_iter()
            .map(|(name, version)| {
                let snap_details = details.get(&name);
                App {
                    usage: Some(name.clone()),
                    install: Some(format!("sudo snap install {name}")),
                    installed_bytes: snap_details.and_then(|d| d.installed_bytes),
                    download_bytes: None,
                    homepage: snap_details.and_then(|d| d.homepage.clone()),
                    license: snap_details.and_then(|d| d.license.clone()),
                    origin: snap_details.and_then(|d| d.origin.clone()),
                    arch: None,
                    maintainer: snap_details.and_then(|d| d.origin.clone()),
                    section: None,
                    depends: None,
                    install_date: None,
                    available_version: None,
                    name: name.clone(),
                    manager: ManagerKind::Snap,
                    installed: true,
                    version: Some(version),
                    description: snap_details.and_then(|d| d.summary.clone()),
                }
            })
            .collect())
    }

    fn search(&self, query: &str) -> Result<Vec<App>, ManagerError> {
        let tokens = query_tokens(query);
        if tokens.is_empty() {
            return Ok(Vec::new());
        }
        let rows = parse_snap_list(&shell::run_managed(ManagerKind::Snap, LIST_CMD)?);
        let installed_set: HashSet<String> = rows.iter().map(|(name, _)| name.clone()).collect();
        // Snap details require one `snap info` per snap; only pay it when
        // there is an inventory to describe.
        let details = if rows.is_empty() {
            HashMap::new()
        } else {
            let names: Vec<&str> = rows.iter().map(|(name, _)| name.as_str()).collect();
            details_map(&names)
        };
        let installed_apps: Vec<App> = rows
            .iter()
            .filter(|(name, _)| {
                app_matches_query(
                    name,
                    details.get(name).and_then(|d| d.summary.as_deref()),
                    &tokens,
                )
            })
            .map(|(name, version)| {
                let snap_details = details.get(name);
                App {
                    usage: Some(name.clone()),
                    install: Some(format!("sudo snap install {name}")),
                    installed_bytes: snap_details.and_then(|d| d.installed_bytes),
                    download_bytes: snap_details.and_then(|d| d.download_bytes),
                    homepage: snap_details.and_then(|d| d.homepage.clone()),
                    license: snap_details.and_then(|d| d.license.clone()),
                    origin: snap_details.and_then(|d| d.origin.clone()),
                    arch: None,
                    maintainer: snap_details.and_then(|d| d.origin.clone()),
                    section: None,
                    depends: None,
                    install_date: None,
                    available_version: None,
                    name: name.clone(),
                    manager: ManagerKind::Snap,
                    installed: true,
                    version: Some(version.clone()),
                    description: snap_details.and_then(|d| d.summary.clone()),
                }
            })
            .collect();
        let cmd = format!(
            "LC_ALL=C snap find {} 2>/dev/null || true",
            shell::quote(query)
        );
        let output = shell::run_managed(ManagerKind::Snap, &cmd)?;
        let catalog: Vec<App> = parse_snap_find(&output)
            .into_iter()
            .map(|(name, version, summary)| {
                let is_installed = installed_set.contains(&name);
                App {
                    usage: Some(name.clone()),
                    install: Some(format!("sudo snap install {name}")),
                    installed_bytes: None,
                    download_bytes: None,
                    homepage: None,
                    license: None,
                    origin: None,
                    arch: None,
                    maintainer: None,
                    section: None,
                    depends: None,
                    install_date: None,
                    available_version: None,
                    name,
                    manager: ManagerKind::Snap,
                    installed: is_installed,
                    version: Some(version),
                    description: Some(summary),
                }
            })
            .collect();
        Ok(merge_installed_and_catalog(installed_apps, catalog))
    }
    fn outdated(&self) -> Result<Vec<App>, ManagerError> {
        let cmd = "LC_ALL=C snap refresh --list 2>/dev/null || true";
        let output = shell::run_managed(ManagerKind::Snap, cmd)?;
        let updates = parse_refresh_list(&output);
        if updates.is_empty() {
            return Ok(Vec::new());
        }
        let current: HashMap<String, String> =
            parse_snap_list(&shell::run_managed(ManagerKind::Snap, LIST_CMD)?)
                .into_iter()
                .collect();
        Ok(updates
            .into_iter()
            .map(|(name, available_version)| App {
                usage: Some(name.clone()),
                install: Some(format!("sudo snap refresh {name}")),
                installed_bytes: None,
                download_bytes: None,
                homepage: None,
                license: None,
                origin: None,
                arch: None,
                maintainer: None,
                section: None,
                depends: None,
                install_date: None,
                name: name.clone(),
                manager: ManagerKind::Snap,
                installed: true,
                version: current.get(&name).cloned(),
                description: None,
                available_version: Some(available_version),
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST_FIXTURE: &str = concat!(
        "Name                        Version          Rev    Tracking       Publisher    Notes\n",
        "core22                      20250912         1900   latest/stable  canonical**  base\n",
        "firefox                     156.0-1          6420   latest/stable  mozilla**     -\n",
    );

    const EMPTY_LIST_FIXTURE: &str =
        "No snaps are installed yet. Try 'snap install hello-world'.\n";

    const FIND_FIXTURE: &str = concat!(
        "Name                         Version               Publisher                 Notes  Summary\n",
        "curl                         8.22.0                aoilinux                  -      command line tool and library for transferring data with URLs.(with HTTP3 support)\n",
        "curl-metalink                7.65.2+pkg-6ea8       brlin                     -      Download Metalinks with cURL for Debianish distros\n",
    );

    #[test]
    fn rest_after_fields_skips_exact_field_count() {
        let line = "curl   8.22.0   aoilinux   -   summary text here";
        assert_eq!(rest_after_fields(line, 4), "summary text here");
        assert_eq!(
            rest_after_fields(line, 1).split_whitespace().next(),
            Some("8.22.0")
        );
        assert_eq!(rest_after_fields("short", 2), "");
    }

    #[test]
    fn parses_snap_list_table_only_after_header() {
        let rows = parse_snap_list(LIST_FIXTURE);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], ("core22".to_string(), "20250912".to_string()));
        assert_eq!(rows[1].0, "firefox");
    }

    #[test]
    fn chatter_without_header_yields_empty_inventory() {
        assert!(parse_snap_list(EMPTY_LIST_FIXTURE).is_empty());
    }

    #[test]
    fn parses_snap_find_rows_with_summaries() {
        let rows = parse_snap_find(FIND_FIXTURE);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, "curl");
        assert_eq!(rows[0].1, "8.22.0");
        assert!(rows[0].2.starts_with("command line tool"));
        assert_eq!(
            rows[1].2,
            "Download Metalinks with cURL for Debianish distros"
        );
    }

    #[test]
    fn parses_details_blocks_with_size_precedence() {
        let fixture = concat!(
            "== core22\n",
            "S: Snap runtime environment\n",
            "I: 76543210\n",
            "D: 77MB\n",
            "L: Other Open Source\n",
            "P: Canonical**\n",
            "W: https://snapcraft.io/core22\n",
            "== firefox\n",
            "S: Browser\n",
            "D: 218MB\n",
        );
        let map = parse_details_output(fixture);
        let core = map.get("core22").unwrap();
        assert_eq!(core.summary.as_deref(), Some("Snap runtime environment"));
        // installed-size and channel download size are now separate fields.
        assert_eq!(core.installed_bytes, Some(76_543_210));
        assert_eq!(core.download_bytes, Some(77_000_000));
        assert_eq!(core.license.as_deref(), Some("Other Open Source"));
        assert_eq!(core.origin.as_deref(), Some("Canonical**"));
        assert_eq!(
            core.homepage.as_deref(),
            Some("https://snapcraft.io/core22")
        );
        let firefox = map.get("firefox").unwrap();
        // Without installed-size only the download size is known.
        assert_eq!(firefox.installed_bytes, None);
        assert_eq!(firefox.download_bytes, Some(218_000_000));
        assert!(!map.contains_key("missing"));
    }
}

/// Parse `snap refresh --list` into (name, new version) rows, ignoring the
/// header row and the "All snaps up to date." chatter.
pub(crate) fn parse_refresh_list(output: &str) -> Vec<(String, String)> {
    let mut in_table = false;
    let mut rows = Vec::new();
    for line in output.lines() {
        if !in_table {
            in_table = line.starts_with("Name ") || line.starts_with("Name\t");
            continue;
        }
        let mut fields = line.split_whitespace();
        let Some(name) = fields.next() else {
            continue;
        };
        let Some(new_version) = fields.next() else {
            continue;
        };
        rows.push((name.to_string(), new_version.to_string()));
    }
    rows
}

#[cfg(test)]
mod outdated_tests {
    use super::*;

    #[test]
    fn parses_refresh_list_rows_only() {
        let fixture = concat!(
            "All snaps up to date.\n",
            "Name      Version  Rev  Tracking  Publisher  Notes\n",
            "core22    20260901 1901 latest/stable canonical** base\n",
            "firefox   157.0    6500 latest/stable mozilla**   -\n",
        );
        let rows = parse_refresh_list(fixture);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, "core22");
        assert_eq!(rows[1].1, "157.0");
    }

    #[test]
    fn chatter_without_header_yields_no_updates() {
        assert!(parse_refresh_list("All snaps up to date.\n").is_empty());
    }
}

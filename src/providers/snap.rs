//! snap provider: installed snaps and the Snap Store catalog search.
//!
//! `snap list` and `snap find` emit space-aligned tables (with tabs possible
//! on some versions), so parsing splits on any whitespace: name, version,
//! publisher and notes are fixed-position fields and the rest of the line is
//! the summary/description. `snap list` prints chatter instead of a table
//! when no snaps exist, which parses as an empty inventory.

use std::collections::HashMap;
use std::collections::HashSet;

use crate::model::{App, ManagerError, ManagerKind};
use crate::provider::Provider;
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
pub(crate) fn summary_map(names: &[&str]) -> HashMap<String, String> {
    if names.is_empty() {
        return HashMap::new();
    }
    let list = names
        .iter()
        .map(|n| shell::quote(n))
        .collect::<Vec<_>>()
        .join(" ");
    let cmd = format!(
        "for s in {list}; do echo \"== $s\"; LC_ALL=C snap info \"$s\" 2>/dev/null | sed -n 's/^summary:[[:space:]]*//p'; done"
    );
    let output = shell::run(&cmd).unwrap_or_default();
    parse_summary_output(&output)
}

/// Parse the `== name` / summary blocks produced by [`summary_map`].
pub(crate) fn parse_summary_output(output: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let mut current: Option<String> = None;
    for line in output.lines() {
        if let Some(name) = line.strip_prefix("== ") {
            current = Some(name.trim().to_string());
        } else if let Some(name) = current.clone() {
            let summary = line.trim();
            if !summary.is_empty() {
                map.insert(name, summary.to_string());
                current = None;
            }
        }
    }
    map
}

/// Names of currently installed snaps (one snap call).
pub(crate) fn installed_names() -> Result<Vec<String>, ManagerError> {
    let output = shell::run_managed(ManagerKind::Snap, LIST_CMD)?;
    Ok(parse_snap_list(&output)
        .into_iter()
        .map(|(name, _)| name)
        .collect())
}

impl Provider for Snap {
    fn kind(&self) -> ManagerKind {
        ManagerKind::Snap
    }

    fn list_installed(&self) -> Result<Vec<App>, ManagerError> {
        let rows = parse_snap_list(&shell::run_managed(ManagerKind::Snap, LIST_CMD)?);
        let names: Vec<&str> = rows.iter().map(|(name, _)| name.as_str()).collect();
        let summaries = summary_map(&names);
        Ok(rows
            .into_iter()
            .map(|(name, version)| {
                let description = summaries.get(&name).cloned();
                App {
                    usage: Some(name.clone()),
                    install: Some(format!("sudo snap install {name}")),
                    name: name.clone(),
                    manager: ManagerKind::Snap,
                    installed: true,
                    version: Some(version),
                    description,
                }
            })
            .collect())
    }

    fn search(&self, query: &str) -> Result<Vec<App>, ManagerError> {
        let cmd = format!(
            "LC_ALL=C snap find {} 2>/dev/null || true",
            shell::quote(query)
        );
        let output = shell::run_managed(ManagerKind::Snap, &cmd)?;
        let rows = parse_snap_find(&output);
        if rows.is_empty() {
            return Ok(Vec::new());
        }
        let installed: HashSet<String> = installed_names()?.into_iter().collect();
        Ok(rows
            .into_iter()
            .map(|(name, version, summary)| {
                let installed = installed.contains(&name);
                App {
                    usage: Some(name.clone()),
                    install: Some(format!("sudo snap install {name}")),
                    name: name.clone(),
                    manager: ManagerKind::Snap,
                    installed,
                    version: Some(version),
                    description: Some(summary),
                }
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

    const SUMMARY_FIXTURE: &str =
        concat!("== core22\n", "Snap runtime environment\n", "== firefox\n",);

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
    fn parses_summary_blocks() {
        let map = parse_summary_output(SUMMARY_FIXTURE);
        assert_eq!(
            map.get("core22").map(String::as_str),
            Some("Snap runtime environment")
        );
        assert!(!map.contains_key("firefox"));
    }
}

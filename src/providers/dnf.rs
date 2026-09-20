//! dnf provider (Fedora/RHEL): installed inventory via rpm and repo search.

use std::collections::HashMap;
use std::collections::HashSet;

use crate::model::{App, ManagerError, ManagerKind};
use crate::provider::{app_matches_query, merge_installed_and_catalog, query_tokens, Provider};
use crate::shell;

pub struct Dnf;

/// One row of `rpm -qa` tab-separated output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RpmRow {
    pub name: String,
    pub version: String,
    pub description: Option<String>,
    pub size_bytes: Option<u64>,
}

/// Parse `rpm -qa --qf '%{NAME}\t%{VERSION}-%{RELEASE}\t%{SUMMARY}\t%{SIZE}\n'`
/// output (`SIZE` is bytes).
pub(crate) fn parse_rpm_qa(output: &str) -> Vec<RpmRow> {
    output
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(4, '\t');
            let name = parts.next()?.trim();
            if name.is_empty() {
                return None;
            }
            let version = parts.next()?.trim();
            let description = parts
                .next()
                .map(str::trim)
                .filter(|d| !d.is_empty() && *d != "(none)")
                .map(str::to_string);
            let size_bytes = parts.next().and_then(|s| s.trim().parse::<u64>().ok());
            Some(RpmRow {
                name: name.to_string(),
                version: version.to_string(),
                description,
                size_bytes,
            })
        })
        .collect()
}

/// Parse `dnf search` output lines shaped `name.arch : summary`.
pub(crate) fn parse_dnf_search(output: &str) -> Vec<(String, Option<String>)> {
    let mut hits: Vec<(String, Option<String>)> = Vec::new();
    for line in output.lines() {
        let Some((left, summary)) = line.split_once(" : ") else {
            continue;
        };
        let left = left.trim();
        if left.is_empty() || left.contains(' ') {
            continue; // section headers such as `=== Name Exactly Matched: ... ===`
        }
        let name = left.split('.').next().unwrap_or(left).to_string();
        if name.is_empty() {
            continue;
        }
        hits.push((name, Some(summary.trim().to_string())));
    }
    hits
}

/// Installed rows through rpm (present on every dnf system), one call.
pub(crate) fn installed_rows() -> Result<Vec<RpmRow>, ManagerError> {
    let cmd = "LC_ALL=C rpm -qa --qf '%{NAME}\\t%{VERSION}-%{RELEASE}\\t%{SUMMARY}\\t%{SIZE}\\n' 2>/dev/null";
    let output = shell::run_managed(ManagerKind::Dnf, cmd)?;
    Ok(parse_rpm_qa(&output))
}

/// First executable per package for a handful of names, batched rpm -ql loop.
pub(crate) fn binary_map(names: &[&str]) -> HashMap<String, String> {
    if names.is_empty() {
        return HashMap::new();
    }
    let list = names
        .iter()
        .map(|n| shell::quote(n))
        .collect::<Vec<_>>()
        .join(" ");
    let cmd = format!(
        "for p in {list}; do b=$(LC_ALL=C rpm -ql \"$p\" 2>/dev/null | grep -m1 -E '/s?bin/.'); [ -n \"$b\" ] && printf '%s\\t%s\\n' \"$p\" \"${{b##*/}}\"; done"
    );
    let output = shell::run(&cmd).unwrap_or_default();
    output
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(2, '\t');
            let name = parts.next()?.trim();
            let bin = parts.next()?.trim();
            if name.is_empty() || bin.is_empty() {
                return None;
            }
            Some((name.to_string(), bin.to_string()))
        })
        .collect()
}

impl Provider for Dnf {
    fn kind(&self) -> ManagerKind {
        ManagerKind::Dnf
    }

    fn list_installed(&self) -> Result<Vec<App>, ManagerError> {
        let rows = installed_rows()?;
        Ok(rows
            .into_iter()
            .map(|row| App {
                // rpm -ql per package would spawn thousands of processes;
                // usage stays null for the bulk inventory (documented).
                usage: None,
                install: Some(format!("sudo dnf install {}", row.name)),
                size_bytes: row.size_bytes,
                name: row.name,
                manager: ManagerKind::Dnf,
                installed: true,
                version: Some(row.version),
                description: row.description,
            })
            .collect())
    }

    fn search(&self, query: &str) -> Result<Vec<App>, ManagerError> {
        let tokens = query_tokens(query);
        if tokens.is_empty() {
            return Ok(Vec::new());
        }
        let local_rows = installed_rows()?;
        let installed_map: HashMap<String, String> = local_rows
            .iter()
            .map(|row| (row.name.clone(), row.version.clone()))
            .collect();
        let installed_names: HashSet<&str> = installed_map.keys().map(String::as_str).collect();
        let installed_apps: Vec<App> = local_rows
            .iter()
            .filter(|row| app_matches_query(&row.name, row.description.as_deref(), &tokens))
            .map(|row| App {
                usage: None,
                install: Some(format!("sudo dnf install {}", row.name)),
                size_bytes: row.size_bytes,
                name: row.name.clone(),
                manager: ManagerKind::Dnf,
                installed: true,
                version: Some(row.version.clone()),
                description: row.description.clone(),
            })
            .collect();
        let cmd = format!(
            "LC_ALL=C dnf search {} 2>/dev/null || true",
            shell::quote(query)
        );
        let output = shell::run_managed(ManagerKind::Dnf, &cmd)?;
        let hits = parse_dnf_search(&output);
        let bin_names: Vec<&str> = hits
            .iter()
            .filter(|(name, _)| installed_names.contains(name.as_str()))
            .map(|(name, _)| name.as_str())
            .collect();
        let bins = binary_map(&bin_names);
        let catalog: Vec<App> = hits
            .into_iter()
            .map(|(name, description)| {
                let installed = installed_names.contains(name.as_str());
                let version = installed_map.get(&name).cloned();
                App {
                    usage: bins.get(&name).cloned(),
                    install: Some(format!("sudo dnf install {name}")),
                    size_bytes: None,
                    name,
                    manager: ManagerKind::Dnf,
                    installed,
                    version,
                    description,
                }
            })
            .collect();
        Ok(merge_installed_and_catalog(installed_apps, catalog))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RPM_QA_FIXTURE: &str = concat!(
        "curl\t8.14.1-2.fc41\tA tool for transferring data from/to a network server\n",
        "glibc\t2.40-17.fc41\tGNU C Library\n",
        "mystery\t1.0-1\t(none)\n",
    );

    const SEARCH_FIXTURE: &str = concat!(
        "=== Name Exactly Matched: curl ===\n",
        "curl.x86_64 : A tool for transferring data from/to a network server\n",
        "=== Summary & Description Matched: ftp ===\n",
        "curlftpfs.x86_64 : Mount remote FTP hosts\n",
    );

    #[test]
    fn parses_rpm_qa_rows() {
        let rows = parse_rpm_qa(RPM_QA_FIXTURE);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].name, "curl");
        assert_eq!(rows[0].version, "8.14.1-2.fc41");
        assert_eq!(rows[2].description, None);
    }

    #[test]
    fn parses_dnf_search_hits_ignoring_headers() {
        let hits = parse_dnf_search(SEARCH_FIXTURE);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].0, "curl");
        assert_eq!(
            hits[0].1.as_deref(),
            Some("A tool for transferring data from/to a network server")
        );
        assert_eq!(hits[1].0, "curlftpfs");
    }

    #[test]
    fn arch_suffix_is_stripped() {
        let hits = parse_dnf_search("curl.x86_64 : summary here\n");
        assert_eq!(hits[0].0, "curl");
    }
}

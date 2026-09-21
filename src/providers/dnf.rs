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
    pub installed_bytes: Option<u64>,
    pub homepage: Option<String>,
    pub license: Option<String>,
    pub origin: Option<String>,
    pub arch: Option<String>,
    pub maintainer: Option<String>,
    pub section: Option<String>,
    pub install_date: Option<String>,
}

/// Parse `rpm -qa` output; `SIZE` is bytes and `INSTALLTIME` an epoch that is
/// normalized to RFC3339.
pub(crate) fn parse_rpm_qa(output: &str) -> Vec<RpmRow> {
    output
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(11, '\t');
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
            let installed_bytes = parts.next().and_then(|s| s.trim().parse::<u64>().ok());
            let field = |parts: &mut std::str::SplitN<'_, char>| {
                parts
                    .next()
                    .map(str::trim)
                    .filter(|value| !value.is_empty() && *value != "(none)")
                    .map(str::to_string)
            };
            let homepage = field(&mut parts);
            let license = field(&mut parts);
            let origin = field(&mut parts);
            let arch = field(&mut parts);
            let maintainer = field(&mut parts);
            let section = field(&mut parts);
            let install_date = parts
                .next()
                .and_then(|s| s.trim().parse::<u64>().ok())
                .map(crate::timefmt::format_rfc3339);
            Some(RpmRow {
                name: name.to_string(),
                version: version.to_string(),
                description,
                installed_bytes,
                homepage,
                license,
                origin,
                arch,
                maintainer,
                section,
                install_date,
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
    let cmd = "LC_ALL=C rpm -qa --qf '%{NAME}\\t%{VERSION}-%{RELEASE}\\t%{SUMMARY}\\t%{SIZE}\\t%{URL}\\t%{LICENSE}\\t%{VENDOR}\\t%{ARCH}\\t%{PACKAGER}\\t%{GROUP}\\t%{INSTALLTIME}\\n' 2>/dev/null";
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
                installed_bytes: row.installed_bytes,
                download_bytes: None,
                homepage: row.homepage,
                license: row.license,
                origin: row.origin,
                arch: row.arch,
                maintainer: row.maintainer,
                section: row.section,
                depends: None,
                install_date: row.install_date,
                available_version: None,
                matched_tokens: Vec::new(),
                confidence: None,
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
                installed_bytes: row.installed_bytes,
                download_bytes: None,
                homepage: row.homepage.clone(),
                license: row.license.clone(),
                origin: row.origin.clone(),
                arch: row.arch.clone(),
                maintainer: row.maintainer.clone(),
                section: row.section.clone(),
                depends: None,
                install_date: row.install_date.clone(),
                available_version: None,
                matched_tokens: Vec::new(),
                confidence: None,
                name: row.name.clone(),
                manager: ManagerKind::Dnf,
                installed: true,
                version: Some(row.version.clone()),
                description: row.description.clone(),
            })
            .collect();
        let cmd = format!(
            "LC_ALL=C dnf search {} 2>/dev/null || true",
            shell::quote(&tokens.join(" "))
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
                    matched_tokens: Vec::new(),
                    confidence: None,
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
    fn outdated(&self) -> Result<Vec<App>, ManagerError> {
        let output = shell::run("LC_ALL=C dnf check-update 2>/dev/null || true").map_err(|e| {
            ManagerError {
                manager: ManagerKind::Dnf,
                message: e.to_string(),
            }
        })?;
        let updates = parse_check_update(&output);
        if updates.is_empty() {
            return Ok(Vec::new());
        }
        let installed_map: HashMap<String, String> = installed_rows()?
            .into_iter()
            .map(|row| (row.name, row.version))
            .collect();
        Ok(updates
            .into_iter()
            .map(|(name, available_version)| App {
                usage: None,
                install: Some(format!("sudo dnf update {name}")),
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
                manager: ManagerKind::Dnf,
                installed: true,
                version: installed_map.get(&name).cloned(),
                description: None,
                available_version: Some(available_version),
                matched_tokens: Vec::new(),
                confidence: None,
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RPM_QA_FIXTURE: &str = concat!(
        "curl\t8.14.1-2.fc41\tA tool for transferring data from/to a network server\t1500000\thttps://curl.se\tMIT\tFedora Project\tx86_64\tFedora Project\tApplications/Internet\t1738000000\n",
        "glibc\t2.40-17.fc41\tGNU C Library\t12000000\t\t\tFedora Project\tx86_64\t\t\t1738000000\n",
        "mystery\t1.0-1\t(none)\t\t\t\t\t\t\t\t\n",
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
        assert_eq!(rows[0].installed_bytes, Some(1_500_000));
        assert_eq!(rows[0].homepage.as_deref(), Some("https://curl.se"));
        assert_eq!(rows[0].license.as_deref(), Some("MIT"));
        assert_eq!(rows[0].origin.as_deref(), Some("Fedora Project"));
        assert_eq!(rows[0].arch.as_deref(), Some("x86_64"));
        assert_eq!(rows[0].section.as_deref(), Some("Applications/Internet"));
        // INSTALLTIME epoch is normalized to RFC3339.
        assert_eq!(
            rows[0].install_date.as_deref(),
            Some(crate::timefmt::format_rfc3339(1_738_000_000).as_str())
        );
        // Empty and (none) fields parse as None.
        assert_eq!(rows[1].homepage, None);
        assert_eq!(rows[1].maintainer, None);
        assert_eq!(rows[2].description, None);
        assert_eq!(rows[2].installed_bytes, None);
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

/// Parse `dnf check-update` upgrade rows (`name.arch version repo`), skipping
/// headers, the obsoletes section and metadata lines.
pub(crate) fn parse_check_update(output: &str) -> Vec<(String, String)> {
    output
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('=') {
                return None;
            }
            let mut fields = line.split_whitespace();
            let package = fields.next()?;
            let version = fields.next()?;
            // Upgrade rows are `name.arch` followed by a numeric version.
            if !version.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                return None;
            }
            let name = package.split('.').next()?.to_string();
            if name.is_empty() {
                return None;
            }
            Some((name, version.to_string()))
        })
        .collect()
}

#[cfg(test)]
mod outdated_tests {
    use super::*;

    #[test]
    fn parses_check_update_rows_and_skips_sections() {
        let fixture = concat!(
            "Last metadata expiration check: 0:01:01 ago.\n",
            "curl.x86_64    8.15.0-1.fc41    updates\n",
            "vim.minimal.x86_64    9.1.0-1.fc41    updates\n",
            "Obsoleting Packages\n",
            "oldpkg.noarch    1.0-1    updates\n",
        );
        let rows = parse_check_update(fixture);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].0, "curl");
        assert_eq!(rows[0].1, "8.15.0-1.fc41");
        assert_eq!(rows[2].0, "oldpkg");
    }
}

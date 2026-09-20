//! dpkg provider: inventory of installed `.deb` packages.
//!
//! dpkg owns the installed-package database, so the dpkg provider is the
//! source of truth for `installed: true` deb entries. Searching the available
//! catalog is the apt provider's job; dpkg only inventories.

use std::collections::HashMap;

use crate::model::{App, ManagerError, ManagerKind};
use crate::provider::{app_matches_query, query_tokens, Provider};
use crate::shell;

pub struct Dpkg;

const QUERY_CMD: &str = "LC_ALL=C dpkg-query -W -f='${Package}\\t${Version}\\t${db:Status-Abbrev}\\t${binary:Summary}\\n'";
const LIST_FILES_GLOB: &str = "/var/lib/dpkg/info/*.list";

/// One parsed row of `dpkg-query -W`.
#[derive(Debug, Clone)]
pub(crate) struct DpkgRow {
    pub name: String,
    pub version: String,
    pub installed: bool,
    pub description: Option<String>,
}

/// Parse `dpkg-query -W` output with the four-column tab format.
pub(crate) fn parse_query_output(output: &str) -> Vec<DpkgRow> {
    output
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(4, '\t');
            let name = parts.next()?.trim();
            if name.is_empty() {
                return None;
            }
            let version = parts.next()?.trim();
            let status = parts.next()?.trim();
            let description = parts
                .next()
                .map(str::trim)
                .filter(|d| !d.is_empty())
                .map(str::to_string);
            Some(DpkgRow {
                name: name.to_string(),
                version: version.to_string(),
                installed: status.starts_with("ii"),
                description,
            })
        })
        .collect()
}

/// Every installed deb row; dpkg's status database is the source of truth.
pub(crate) fn installed_rows() -> Result<Vec<DpkgRow>, ManagerError> {
    let output = shell::run_managed(ManagerKind::Dpkg, QUERY_CMD)?;
    Ok(parse_query_output(&output)
        .into_iter()
        .filter(|row| row.installed)
        .collect())
}

/// Map package name -> first executable it ships, with one grep over dpkg's
/// `.list` files (no per-package process spawns).
pub(crate) fn binary_map() -> HashMap<String, String> {
    let cmd = format!("grep -H -m1 -E '/s?bin/' {LIST_FILES_GLOB} 2>/dev/null || true");
    let output = shell::run(&cmd).unwrap_or_default();
    parse_list_files_output(&output)
}

/// Parse `grep -H` lines such as
/// `/var/lib/dpkg/info/curl.list:/usr/bin/curl` and
/// `/var/lib/dpkg/info/libc6:amd64.list:/usr/lib/x.bin`.
pub(crate) fn parse_list_files_output(output: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for line in output.lines() {
        let Some(rest) = line.strip_prefix("/var/lib/dpkg/info/") else {
            continue;
        };
        let Some((file, binary)) = rest.split_once(".list:") else {
            continue;
        };
        // Drop an `:arch` suffix such as `libc6:amd64`.
        let pkg = file.split(':').next().unwrap_or(file);
        if pkg.is_empty() || binary.is_empty() {
            continue;
        }
        map.entry(pkg.to_string())
            .or_insert_with(|| binary.rsplit('/').next().unwrap_or(binary).to_string());
    }
    map
}

/// Convert an installed-deb row into an `App`, looking up its executable.
fn row_to_app(row: DpkgRow, bins: &HashMap<String, String>) -> App {
    let name = row.name.clone();
    App {
        usage: bins.get(&name).cloned(),
        install: Some(format!("sudo apt install {name}")),
        name,
        manager: ManagerKind::Dpkg,
        installed: true,
        version: Some(row.version),
        description: row.description,
    }
}

impl Provider for Dpkg {
    fn kind(&self) -> ManagerKind {
        ManagerKind::Dpkg
    }

    fn list_installed(&self) -> Result<Vec<App>, ManagerError> {
        let bins = binary_map();
        Ok(installed_rows()?
            .into_iter()
            .map(|row| row_to_app(row, &bins))
            .collect())
    }

    fn search(&self, query: &str) -> Result<Vec<App>, ManagerError> {
        // The dpkg side of search covers locally installed debs even when
        // apt-cache cannot surface them (e.g. local .deb installs missing
        // from any configured repository).
        let tokens = query_tokens(query);
        if tokens.is_empty() {
            return Ok(Vec::new());
        }
        let bins = binary_map();
        Ok(installed_rows()?
            .into_iter()
            .filter(|row| app_matches_query(&row.name, row.description.as_deref(), &tokens))
            .map(|row| row_to_app(row, &bins))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = concat!(
        "7zip\t25.01+dfsg-1~deb13u2\tii \t7-Zip file archiver with a high compression ratio\n",
        "adduser\t3.152\tii \tadd and remove users and groups\n",
        "broken-pkg\t1.0\trc \tremoved but config files remain\n",
        "no-desc\t2.0\tii \t\n",
    );

    const GREP_FIXTURE: &str = concat!(
        "/var/lib/dpkg/info/curl.list:/usr/bin/curl\n",
        "/var/lib/dpkg/info/libc6:amd64.list:/usr/lib/x86_64-linux-gnu/ld-linux-x86-64.so.2\n",
        "/var/lib/dpkg/info/libc6:amd64.list:/usr/bin/useless\n",
        "/var/lib/dpkg/info/vim.list:/usr/bin/vim.basic\n",
    );

    #[test]
    fn parses_rows_and_installed_flag() {
        let rows = parse_query_output(FIXTURE);
        assert_eq!(rows.len(), 4);
        assert!(rows[0].installed);
        assert_eq!(rows[0].name, "7zip");
        assert_eq!(rows[0].version, "25.01+dfsg-1~deb13u2");
        // `rc` status is not installed.
        assert!(!rows[2].installed);
        // Empty summary becomes None.
        assert_eq!(rows[3].description, None);
    }

    #[test]
    fn installed_rows_filter_keeps_only_ii() {
        let rows: Vec<_> = parse_query_output(FIXTURE)
            .into_iter()
            .filter(|r| r.installed)
            .collect();
        assert_eq!(
            rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            ["7zip", "adduser", "no-desc"]
        );
    }

    #[test]
    fn parses_grep_list_output_with_arch_suffix() {
        let map = parse_list_files_output(GREP_FIXTURE);
        assert_eq!(map.get("curl").map(String::as_str), Some("curl"));
        // First match wins; arch suffix stripped from the package name.
        assert_eq!(
            map.get("libc6").map(String::as_str),
            Some("ld-linux-x86-64.so.2")
        );
        assert_eq!(map.get("vim").map(String::as_str), Some("vim.basic"));
        assert_eq!(map.len(), 3);
    }

    #[test]
    fn ignores_malformed_grep_lines() {
        let map = parse_list_files_output("garbage\n/var/lib/dpkg/info/nomatch.txt:/bin/x\n");
        assert!(map.is_empty());
    }
}

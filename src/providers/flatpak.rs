//! flatpak provider: installed apps and remote catalog search.
//!
//! Both list and search use tab-separated `--columns` output; rows can be
//! duplicated when an app is exposed by more than one remote, so results are
//! deduplicated by application ID keeping the first row.

use std::collections::HashSet;

use crate::model::{parse_human_size, App, ManagerError, ManagerKind};
use crate::provider::{app_matches_query, merge_installed_and_catalog, query_tokens, Provider};
use crate::shell;

pub struct Flatpak;

const LIST_CMD: &str =
    "LC_ALL=C flatpak list --app --columns=application,name,version,description,size,origin,arch";
const SEARCH_COLUMNS: &str = "--columns=application,name,version,description";

/// One parsed flatpak row (already deduplicated). Installed rows carry size,
/// origin and arch; catalog search rows only expose the first four columns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FlatpakRow {
    pub id: String,
    pub name: String,
    pub version: Option<String>,
    pub description: Option<String>,
    pub installed_bytes: Option<u64>,
    pub origin: Option<String>,
    pub arch: Option<String>,
}

/// Parse tab-separated `application,name,version,description` output.
pub(crate) fn parse_columns_output(output: &str) -> Vec<FlatpakRow> {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut rows = Vec::new();
    for line in output.lines() {
        // `flatpak search` prints human messages such as `No matches found`
        // instead of table rows; real rows are always tab-separated.
        if !line.contains('\t') {
            continue;
        }
        let Some(id) = line.split('\t').next() else {
            continue;
        };
        let id = id.trim();
        if id.is_empty() || !seen.insert(id) {
            continue;
        }
        let mut parts = line.splitn(7, '\t');
        let _id = parts.next();
        let field = |v: Option<&str>| {
            v.map(str::trim)
                .filter(|value| !value.is_empty() && *value != "-")
                .map(str::to_string)
        };
        rows.push(FlatpakRow {
            id: id.to_string(),
            name: field(parts.next()).unwrap_or_else(|| id.to_string()),
            version: field(parts.next()),
            description: field(parts.next()),
            // Search rows end here: size, origin and arch parse as None.
            installed_bytes: parts.next().and_then(parse_human_size),
            origin: field(parts.next()),
            arch: field(parts.next()),
        });
    }
    rows
}

/// Rows of currently installed flatpak apps (one flatpak call).
pub(crate) fn installed_rows() -> Result<Vec<FlatpakRow>, ManagerError> {
    let output = shell::run_managed(ManagerKind::Flatpak, LIST_CMD)?;
    Ok(parse_columns_output(&output))
}

fn row_to_app(row: &FlatpakRow, installed: bool, description: Option<String>) -> App {
    App {
        usage: Some(format!("flatpak run {}", row.id)),
        install: Some(format!("flatpak install {}", row.id)),
        installed_bytes: row.installed_bytes,
        download_bytes: None,
        homepage: None,
        license: None,
        origin: row.origin.clone(),
        arch: row.arch.clone(),
        maintainer: None,
        section: None,
        depends: None,
        install_date: None,
        available_version: None,
        name: row.name.clone(),
        manager: ManagerKind::Flatpak,
        installed,
        version: row.version.clone(),
        description: row.description.clone().or(description),
    }
}

impl Provider for Flatpak {
    fn kind(&self) -> ManagerKind {
        ManagerKind::Flatpak
    }

    fn list_installed(&self) -> Result<Vec<App>, ManagerError> {
        Ok(installed_rows()?
            .iter()
            .map(|row| row_to_app(row, true, None))
            .collect())
    }

    fn search(&self, query: &str) -> Result<Vec<App>, ManagerError> {
        let tokens = query_tokens(query);
        if tokens.is_empty() {
            return Ok(Vec::new());
        }
        let installed = installed_rows()?;
        let installed_ids: HashSet<String> = installed.iter().map(|row| row.id.clone()).collect();
        // Installed apps must be searchable even when the remote full-text
        // index does not surface them for the same query.
        let installed_apps: Vec<App> = installed
            .iter()
            .filter(|row| {
                app_matches_query(&row.id, row.description.as_deref(), &tokens)
                    || app_matches_query(&row.name, row.description.as_deref(), &tokens)
            })
            .map(|row| row_to_app(row, true, None))
            .collect();
        let cmd = format!(
            "LC_ALL=C flatpak search {} {SEARCH_COLUMNS} 2>/dev/null || true",
            shell::quote(query)
        );
        let output = shell::run_managed(ManagerKind::Flatpak, &cmd)?;
        let catalog: Vec<App> = parse_columns_output(&output)
            .iter()
            .map(|row| {
                let is_installed = installed_ids.contains(&row.id);
                row_to_app(row, is_installed, None)
            })
            .collect();
        Ok(merge_installed_and_catalog(installed_apps, catalog))
    }
    fn outdated(&self) -> Result<Vec<App>, ManagerError> {
        let cmd = "LC_ALL=C flatpak remote-ls --updates --columns=application,version 2>/dev/null || true";
        let output = shell::run_managed(ManagerKind::Flatpak, cmd)?;
        let updates = parse_remote_updates(&output);
        if updates.is_empty() {
            return Ok(Vec::new());
        }
        let installed = installed_rows()?;
        Ok(updates
            .into_iter()
            .filter_map(|(id, available_version)| {
                let row = installed.iter().find(|row| row.id == id)?;
                let mut app = row_to_app(row, true, None);
                app.available_version = available_version.filter(|value| !value.is_empty());
                Some(app)
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST_FIXTURE: &str = concat!(
        "com.mojang.Minecraft\tMinecraft Launcher\t2.1.3\tCrea tu propio mundo\t70,2 MB\tflathub\tx86_64\n",
        "com.mojang.Minecraft\tMinecraft Launcher\t2.1.3\tCrea tu propio mundo\t70,2 MB\tflathub\tx86_64\n",
        "org.mozilla.firefox\tFirefox\t156.0\tFast, Private & Safe Web Browser\t336.8 MB\tflathub\tx86_64\n",
        "org.cutwire.Drift\tDrift\t\tEdit and export videos easily\t63.7 MB\tflathub\tx86_64\n",
    );

    const SEARCH_FIXTURE: &str = concat!(
        "org.vim.Vim\tVim\tv9.2.1025-1-g5c9c5a43c\tThe ubiquitous text editor\n",
        "io.neovim.nvim\tNeovim\t0.12.5\tVim-fork focused on extensibility\n",
    );

    #[test]
    fn parses_list_rows_and_dedupes_by_id() {
        let rows = parse_columns_output(LIST_FIXTURE);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].id, "com.mojang.Minecraft");
        assert_eq!(rows[1].id, "org.mozilla.firefox");
    }

    #[test]
    fn parses_human_sizes_from_size_column() {
        let rows = parse_columns_output(LIST_FIXTURE);
        assert_eq!(rows[0].installed_bytes, Some(70_200_000));
        assert_eq!(rows[1].installed_bytes, Some(336_800_000));
        assert_eq!(rows[2].installed_bytes, Some(63_700_000));
        assert_eq!(rows[2].origin.as_deref(), Some("flathub"));
        assert_eq!(rows[2].arch.as_deref(), Some("x86_64"));
    }

    #[test]
    fn empty_version_and_description_become_none() {
        let rows = parse_columns_output(LIST_FIXTURE);
        let drift = rows.iter().find(|r| r.id == "org.cutwire.Drift").unwrap();
        assert_eq!(drift.version, None);
        assert_eq!(
            drift.description.as_deref(),
            Some("Edit and export videos easily")
        );
    }

    #[test]
    fn skips_blank_lines() {
        assert!(parse_columns_output("\n\n").is_empty());
    }

    #[test]
    fn skips_no_matches_message_without_tabs() {
        assert!(parse_columns_output("No matches found\n").is_empty());
    }

    #[test]
    fn parses_search_rows_without_size_column() {
        let rows = parse_columns_output(SEARCH_FIXTURE);
        assert_eq!(rows[0].name, "Vim");
        assert_eq!(rows[0].version.as_deref(), Some("v9.2.1025-1-g5c9c5a43c"));
        assert_eq!(rows[1].id, "io.neovim.nvim");
        // Catalog search has no size/origin/arch columns.
        assert_eq!(rows[0].installed_bytes, None);
        assert_eq!(rows[0].origin, None);
        assert_eq!(rows[0].arch, None);
    }
}

/// Parse `flatpak remote-ls --updates` (id, version) rows.
pub(crate) fn parse_remote_updates(output: &str) -> Vec<(String, Option<String>)> {
    output
        .lines()
        .filter_map(|line| {
            if !line.contains('\t') {
                return None;
            }
            let mut parts = line.splitn(2, '\t');
            let id = parts.next()?.trim().to_string();
            if id.is_empty() {
                return None;
            }
            let version = parts
                .next()
                .map(str::trim)
                .filter(|value| !value.is_empty() && *value != "-")
                .map(str::to_string);
            Some((id, version))
        })
        .collect()
}

#[cfg(test)]
mod outdated_tests {
    use super::*;

    #[test]
    fn parses_remote_update_rows() {
        let fixture = concat!(
            "org.vim.Vim\t9.1.0\n",
            "org.mozilla.firefox\t-\n",
            "No updates\n",
        );
        let rows = parse_remote_updates(fixture);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, "org.vim.Vim");
        assert_eq!(rows[0].1.as_deref(), Some("9.1.0"));
        assert_eq!(rows[1].1, None);
    }
}

//! flatpak provider: installed apps and remote catalog search.
//!
//! Both list and search use tab-separated `--columns` output; rows can be
//! duplicated when an app is exposed by more than one remote, so results are
//! deduplicated by application ID keeping the first row.

use std::collections::HashSet;

use crate::model::{App, ManagerError, ManagerKind};
use crate::provider::Provider;
use crate::shell;

pub struct Flatpak;

const LIST_CMD: &str = "LC_ALL=C flatpak list --app --columns=application,name,version,description";
const SEARCH_COLUMNS: &str = "--columns=application,name,version,description";

/// One parsed flatpak row (already deduplicated).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FlatpakRow {
    pub id: String,
    pub name: String,
    pub version: Option<String>,
    pub description: Option<String>,
}

/// Parse tab-separated `application,name,version,description` output.
pub(crate) fn parse_columns_output(output: &str) -> Vec<FlatpakRow> {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut rows = Vec::new();
    for line in output.lines() {
        let Some(id) = line.split('\t').next() else {
            continue;
        };
        let id = id.trim();
        if id.is_empty() || !seen.insert(id) {
            continue;
        }
        let mut parts = line.splitn(4, '\t');
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
        let cmd = format!(
            "LC_ALL=C flatpak search {} {SEARCH_COLUMNS} 2>/dev/null || true",
            shell::quote(query)
        );
        let output = shell::run_managed(ManagerKind::Flatpak, &cmd)?;
        let rows = parse_columns_output(&output);
        if rows.is_empty() {
            return Ok(Vec::new());
        }
        let installed_ids: HashSet<String> =
            installed_rows()?.into_iter().map(|row| row.id).collect();
        Ok(rows
            .iter()
            .map(|row| {
                let installed = installed_ids.contains(&row.id);
                row_to_app(row, installed, None)
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST_FIXTURE: &str = concat!(
        "com.mojang.Minecraft\tMinecraft Launcher\t2.1.3\tCrea tu propio mundo\n",
        "com.mojang.Minecraft\tMinecraft Launcher\t2.1.3\tCrea tu propio mundo\n",
        "org.mozilla.firefox\tFirefox\t156.0\tFast, Private & Safe Web Browser\n",
        "org.cutwire.Drift\tDrift\t\tEdit and export videos easily\n",
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
    fn parses_search_rows() {
        let rows = parse_columns_output(SEARCH_FIXTURE);
        assert_eq!(rows[0].name, "Vim");
        assert_eq!(rows[0].version.as_deref(), Some("v9.2.1025-1-g5c9c5a43c"));
        assert_eq!(rows[1].id, "io.neovim.nvim");
    }
}

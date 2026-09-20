//! Provider abstraction: one implementation per package manager, plus the
//! shared query-matching and merge helpers used by every `search`.

use crate::model::{App, ManagerError, ManagerKind};
use crate::shell;

/// A package manager backend the CLI can query.
pub trait Provider {
    fn kind(&self) -> ManagerKind;

    /// Whether the manager's binary is present on this system.
    fn is_available(&self) -> bool {
        shell::which(self.kind().binary())
    }

    /// Applications currently installed through this manager.
    fn list_installed(&self) -> Result<Vec<App>, ManagerError>;

    /// Search installed and available applications matching `query`.
    fn search(&self, query: &str) -> Result<Vec<App>, ManagerError>;
}

/// All providers, in canonical `ManagerKind::ALL` order.
pub fn registry() -> Vec<Box<dyn Provider>> {
    use crate::providers::{
        apt::Apt, brew::Brew, dnf::Dnf, dpkg::Dpkg, flatpak::Flatpak, pacman::Pacman, snap::Snap,
    };
    vec![
        Box::new(Apt),
        Box::new(Dpkg),
        Box::new(Flatpak),
        Box::new(Snap),
        Box::new(Brew),
        Box::new(Pacman),
        Box::new(Dnf),
    ]
}

/// Kinds of the providers that are usable on this system, in registry order.
pub fn detect_available(registry: &[Box<dyn Provider>]) -> Vec<ManagerKind> {
    registry
        .iter()
        .filter(|p| p.is_available())
        .map(|p| p.kind())
        .collect()
}

/// Split a user query into lowercase tokens for AND matching.
pub(crate) fn query_tokens(query: &str) -> Vec<String> {
    query
        .split_whitespace()
        .map(str::to_lowercase)
        .filter(|token| !token.is_empty())
        .collect()
}

/// True when every token appears (case-insensitively) in the name or the
/// description; token order is irrelevant and substrings count (`video`
/// matches "videos").
pub(crate) fn app_matches_query(name: &str, description: Option<&str>, tokens: &[String]) -> bool {
    if tokens.is_empty() {
        return false;
    }
    let name_lower = name.to_lowercase();
    let description_lower = description.unwrap_or("").to_lowercase();
    tokens
        .iter()
        .all(|token| name_lower.contains(token) || description_lower.contains(token))
}

/// Merge installed and catalog results for one manager. Installed entries win
/// (their version is what the user actually has); missing description or
/// version fields are filled from the catalog copy; catalog-only entries are
/// appended.
pub(crate) fn merge_installed_and_catalog(mut installed: Vec<App>, catalog: Vec<App>) -> Vec<App> {
    for remote in catalog {
        if let Some(local) = installed.iter_mut().find(|app| app.name == remote.name) {
            local.installed = true;
            if local.description.is_none() {
                local.description = remote.description;
            }
            if local.version.is_none() {
                local.version = remote.version;
            }
        } else {
            installed.push(remote);
        }
    }
    installed
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeProvider(ManagerKind, bool);

    impl Provider for FakeProvider {
        fn kind(&self) -> ManagerKind {
            self.0
        }
        fn is_available(&self) -> bool {
            self.1
        }
        fn list_installed(&self) -> Result<Vec<App>, ManagerError> {
            Ok(vec![])
        }
        fn search(&self, _query: &str) -> Result<Vec<App>, ManagerError> {
            Ok(vec![])
        }
    }

    fn app(name: &str, installed: bool, description: Option<&str>) -> App {
        App {
            name: name.to_string(),
            manager: ManagerKind::Flatpak,
            installed,
            version: None,
            description: description.map(str::to_string),
            usage: None,
            install: None,
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
        }
    }

    #[test]
    fn detect_available_filters_by_availability() {
        let registry: Vec<Box<dyn Provider>> = vec![
            Box::new(FakeProvider(ManagerKind::Apt, true)),
            Box::new(FakeProvider(ManagerKind::Brew, false)),
        ];
        assert_eq!(detect_available(&registry), vec![ManagerKind::Apt]);
    }

    #[test]
    fn tokens_are_lowercased_and_require_all() {
        assert_eq!(query_tokens("Video  EDITOR"), vec!["video", "editor"]);
    }

    #[test]
    fn app_matches_when_every_token_hits_name_or_description() {
        let tokens = query_tokens("video editor");
        assert!(app_matches_query(
            "kdenlive",
            Some("non-linear video editor"),
            &tokens
        ));
        assert!(app_matches_query("VideoEditor", None, &tokens));
        // Substring counting: "videos" contains "video" but there is no "editor".
        assert!(!app_matches_query(
            "Drift",
            Some("Edit and export videos easily"),
            &tokens
        ));
        assert!(app_matches_query(
            "Drift",
            Some("Edit and export videos easily"),
            &query_tokens("video")
        ));
    }

    #[test]
    fn empty_tokens_never_match() {
        assert!(!app_matches_query("anything", Some("text"), &[]));
    }

    #[test]
    fn merge_prefers_installed_and_fills_gaps() {
        let mut installed = app("drift", true, None);
        installed.version = Some("0.6.0".into());
        let catalog = vec![
            app("drift", false, Some("Edit and export videos easily")),
            app("openshot", false, Some("video editor")),
        ];
        let merged = merge_installed_and_catalog(vec![installed], catalog);
        assert_eq!(merged.len(), 2);
        let drift = merged.iter().find(|a| a.name == "drift").unwrap();
        assert!(drift.installed);
        assert_eq!(drift.version.as_deref(), Some("0.6.0"));
        assert_eq!(
            drift.description.as_deref(),
            Some("Edit and export videos easily")
        );
        let openshot = merged.iter().find(|a| a.name == "openshot").unwrap();
        assert!(!openshot.installed);
    }
}

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

    /// Installed applications that have a newer version available. `version`
    /// keeps the installed one and `available_version` carries the candidate;
    /// managers that cannot detect upgrades return an empty list.
    fn outdated(&self) -> Result<Vec<App>, ManagerError> {
        Ok(Vec::new())
    }
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

/// True when `needle` appears in `hay` delimited by non-alphanumeric
/// characters (word boundary), which ranks higher than a bare substring hit.
pub(crate) fn contains_word(hay: &str, needle: &str) -> bool {
    if needle.is_empty() || hay.is_empty() {
        return false;
    }
    let mut start = 0;
    while let Some(offset) = hay[start..].find(needle) {
        let abs = start + offset;
        let before_ok = abs == 0 || !hay[..abs].ends_with(|c: char| c.is_alphanumeric());
        let after = abs + needle.len();
        let after_ok =
            after == hay.len() || !hay[after..].starts_with(|c: char| c.is_alphanumeric());
        if before_ok && after_ok {
            return true;
        }
        start = abs + 1;
    }
    false
}

/// Relevance score of an application for a tokenized query. Every token hit
/// adds points; name hits outweigh description hits and word-boundary hits
/// outweigh bare substrings, so AND matches naturally outrank OR matches.
pub(crate) fn relevance_score(name: &str, description: Option<&str>, tokens: &[String]) -> i64 {
    if tokens.is_empty() {
        return 0;
    }
    let name_lower = name.to_lowercase();
    let description_lower = description.unwrap_or("").to_lowercase();
    let mut score: i64 = 0;
    for token in tokens {
        if contains_word(&name_lower, token) {
            score += 50;
        } else if name_lower.contains(token) {
            score += 25;
        }
        if contains_word(&description_lower, token) {
            score += 10;
        } else if description_lower.contains(token) {
            score += 3;
        }
    }
    let phrase = tokens.join(" ");
    if name_lower == phrase {
        score += 200;
    } else if !phrase.is_empty() && name_lower.contains(&phrase) {
        score += 80;
    }
    score
}

/// Split a user query into lowercase tokens for AND matching.
pub(crate) fn query_tokens(query: &str) -> Vec<String> {
    query
        .split_whitespace()
        .map(str::to_lowercase)
        .filter(|token| !token.is_empty())
        .collect()
}

/// Permissive candidate gate: true when at least one token appears
/// (case-insensitively) in the name or the description. Ordering is decided
/// by [`relevance_score`], so multi-token matches naturally outrank
/// single-token ones.
pub(crate) fn app_matches_query(name: &str, description: Option<&str>, tokens: &[String]) -> bool {
    if tokens.is_empty() {
        return false;
    }
    let name_lower = name.to_lowercase();
    let description_lower = description.unwrap_or("").to_lowercase();
    tokens
        .iter()
        .any(|token| name_lower.contains(token) || description_lower.contains(token))
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
            available_version: None,
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
    fn app_matches_when_any_token_hits_name_or_description() {
        let tokens = query_tokens("video editor");
        assert!(app_matches_query(
            "kdenlive",
            Some("non-linear video editor"),
            &tokens
        ));
        assert!(app_matches_query("VideoEditor", None, &tokens));
        // OR gate: a single token hit is enough (ranking orders the rest).
        assert!(app_matches_query(
            "Drift",
            Some("Edit and export videos easily"),
            &tokens
        ));
        assert!(!app_matches_query("Drift", Some("unrelated text"), &tokens));
    }

    #[test]
    fn word_boundary_beats_substring() {
        assert!(contains_word("gnu tar archiving utility", "tar"));
        // `videos` contains `video` but not as a whole word: substring only.
        assert!(!contains_word("the videos collection", "video"));
        assert!(contains_word("a video editor", "video"));
        assert!(!contains_word("", "video"));
    }

    #[test]
    fn relevance_prefers_name_over_description_and_words_over_substrings() {
        let tokens = query_tokens("tar");
        let name_word = relevance_score("tar", Some("unrelated"), &tokens);
        let name_substring = relevance_score("startar", Some("unrelated"), &tokens);
        let description_word = relevance_score("zzz", Some("a tar utility"), &tokens);
        let description_substring = relevance_score("zzz", Some("it started"), &tokens);
        assert!(name_word > name_substring);
        assert!(name_substring > description_word);
        assert!(description_word > description_substring);
    }

    #[test]
    fn relevance_rewards_phrase_in_name_and_multi_token_hits() {
        let tokens = query_tokens("video editor");
        let phrase_name = relevance_score("video editor", None, &tokens);
        let both_tokens = relevance_score("kdenlive", Some("non-linear video editor"), &tokens);
        let one_token = relevance_score("drift", Some("export videos"), &tokens);
        assert!(phrase_name > both_tokens);
        assert!(both_tokens > one_token);
        assert!(one_token > 0);
        assert_eq!(relevance_score("zzz", None, &[]), 0);
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
